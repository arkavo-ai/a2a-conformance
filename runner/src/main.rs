mod adapter;
mod checks;
mod proxy;
mod report;
mod scenario;

use adapter::{ClientHandle, MatrixConfig, ServerHandle};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use report::{ResultLine, Wire};
use scenario::Scenario;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "a2a-conformance-runner", about = "Pairwise A2A SDK interop matrix")]
struct Cli {
    /// Repo root (default: current directory)
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Validate all scenario files against the schema and structural rules.
    Validate,
    /// Run the matrix (default: every enabled cell).
    Run {
        /// Restrict to one cell: client=NAME,server=NAME (repeatable)
        #[arg(long)]
        cell: Vec<String>,
        /// Restrict to scenarios carrying this tag (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Report output directory
        #[arg(long, default_value = "reports")]
        report: PathBuf,
        /// Skip building the adapters first
        #[arg(long)]
        no_build: bool,
        /// Compare against a baseline NDJSON and exit non-zero on regressions
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Per-scenario timeout in milliseconds
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Validate => validate(&cli.root),
        Commands::Run {
            cell,
            tag,
            report,
            no_build,
            baseline,
            timeout_ms,
        } => tokio::runtime::Runtime::new()?.block_on(run(
            &cli.root, cell, tag, report, no_build, baseline, timeout_ms,
        )),
    }
}

fn validate(root: &std::path::Path) -> Result<()> {
    let schema_text = std::fs::read_to_string(root.join("schema/scenario.schema.json"))?;
    let schema: Value = serde_json::from_str(&schema_text)?;
    let compiled = jsonschema::validator_for(&schema).context("compiling scenario schema")?;

    let scenarios = scenario::load_all(&root.join("scenarios"))?;
    let mut problems = Vec::new();
    for s in &scenarios {
        let path = root.join("scenarios").join(format!("{}.json", s.id));
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        for error in compiled.iter_errors(&raw) {
            problems.push(format!("{}: {} (at {})", s.id, error, error.instance_path));
        }
        match s.expect.kind.as_str() {
            "error" => {
                // errorCode null/absent is permitted: it asserts only that a
                // protocol-level error is surfaced (e.g. transport-layer auth
                // failures with no JSON-RPC code).
            }
            "stream" => {
                if s.expect.stream_order.is_none() {
                    problems.push(format!("{}: expect.kind=stream requires streamOrder", s.id));
                }
            }
            _ => {}
        }
        if s.client.op == "RawRequest" && s.client.raw_body.is_none() {
            problems.push(format!("{}: RawRequest requires client.rawBody", s.id));
        }
    }
    if problems.is_empty() {
        println!("ok: {} scenarios valid", scenarios.len());
        Ok(())
    } else {
        for p in &problems {
            eprintln!("invalid: {p}");
        }
        bail!("{} validation problem(s)", problems.len());
    }
}

fn parse_cells(specs: &[String], config: &MatrixConfig) -> Result<Vec<(String, String)>> {
    if specs.is_empty() {
        let mut cells = Vec::new();
        for client in &config.matrix.clients {
            for server in &config.matrix.servers {
                cells.push((client.clone(), server.clone()));
            }
        }
        return Ok(cells);
    }
    let mut cells = Vec::new();
    for spec in specs {
        let mut client = None;
        let mut server = None;
        for part in spec.split(',') {
            match part.split_once('=') {
                Some(("client", v)) => client = Some(v.to_string()),
                Some(("server", v)) => server = Some(v.to_string()),
                _ => bail!("bad --cell {spec:?}; expected client=NAME,server=NAME"),
            }
        }
        cells.push((
            client.context("--cell missing client=")?,
            server.context("--cell missing server=")?,
        ));
    }
    Ok(cells)
}

#[allow(clippy::too_many_arguments)]
async fn run(
    root: &std::path::Path,
    cell_specs: Vec<String>,
    tags: Vec<String>,
    report_dir: PathBuf,
    no_build: bool,
    baseline: Option<PathBuf>,
    timeout_ms: u64,
) -> Result<()> {
    let config = adapter::load_matrix(&root.join("matrix.toml"))?;
    let scenarios = scenario::load_all(&root.join("scenarios"))?;
    let cells = parse_cells(&cell_specs, &config)?;
    let timeout = Duration::from_millis(config.runner.timeout_ms.unwrap_or(timeout_ms));

    if !no_build {
        let mut built = std::collections::BTreeSet::new();
        for (client, server) in &cells {
            for name in [client, server] {
                if built.insert(name.clone()) {
                    adapter::run_build(name, &config.implementations[name], root)?;
                }
            }
        }
    }

    let selected: Vec<&Scenario> = scenarios
        .iter()
        .filter(|s| tags.is_empty() || tags.iter().all(|t| s.has_tag(t)))
        .collect();

    let mut results: Vec<ResultLine> = Vec::new();
    for (client_name, server_name) in &cells {
        eprintln!("[cell] client={client_name} server={server_name}");
        let cell_results = run_cell(
            root,
            &config,
            client_name,
            server_name,
            &selected,
            timeout,
        )
        .await
        .with_context(|| format!("cell {client_name} -> {server_name}"))?;
        results.extend(cell_results);
    }

    std::fs::create_dir_all(&report_dir)?;
    report::write_ndjson(&report_dir.join("results.ndjson"), &results)?;
    let mut groups: Vec<String> = scenarios
        .iter()
        .map(|s| s.group().to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    // Keep the core corpus groups in their conventional order, extensions after.
    let conventional = ["core", "streaming", "errors", "discovery", "edge"];
    groups.sort_by_key(|g| {
        (conventional.iter().position(|c| c == g).unwrap_or(usize::MAX), g.clone())
    });
    let groups: Vec<&str> = groups.iter().map(String::as_str).collect();
    let clients: Vec<String> = cells.iter().map(|c| c.0.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let servers: Vec<String> = cells.iter().map(|c| c.1.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    report::write_markdown(&report_dir, &results, &groups, &clients, &servers)?;

    let pass = results.iter().filter(|r| r.status == "pass").count();
    let fail = results.iter().filter(|r| r.status == "fail").count();
    let error = results.iter().filter(|r| r.status == "error").count();
    let skip = results.iter().filter(|r| r.status == "skip").count();
    println!(
        "done: {pass} pass, {fail} fail, {error} error, {skip} skip ({} total) -> {}",
        results.len(),
        report_dir.join("matrix.md").display()
    );

    if let Some(baseline_path) = baseline {
        let baseline_results = report::load_ndjson(&baseline_path)?;
        let regressions = report::regressions(&baseline_results, &results);
        if !regressions.is_empty() {
            for r in &regressions {
                eprintln!("REGRESSION: {r}");
            }
            bail!("{} regression(s) against baseline", regressions.len());
        }
        println!("no regressions against {}", baseline_path.display());
    }
    if error > 0 {
        bail!("{error} scenario(s) ended in harness error");
    }
    Ok(())
}

async fn run_cell(
    root: &std::path::Path,
    config: &MatrixConfig,
    client_name: &str,
    server_name: &str,
    scenarios: &[&Scenario],
    timeout: Duration,
) -> Result<Vec<ResultLine>> {
    let client_impl = &config.implementations[client_name];
    let server_impl = &config.implementations[server_name];

    let (proxy, proxy_port) = proxy::CaptureProxy::bind().await?;
    let public_base_url = format!("http://127.0.0.1:{proxy_port}");

    let server = ServerHandle::spawn(
        &root.join(&server_impl.server),
        &root.join("scenarios"),
        &public_base_url,
    )
    .await?;
    let server_port: u16 = server
        .ready
        .base_url
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .context("parsing server port from baseUrl")?;
    proxy.set_target(server_port);

    let mut client = ClientHandle::spawn(&root.join(&client_impl.client))?;

    let mut results = Vec::new();
    for s in scenarios {
        let start = Instant::now();
        let mut line = ResultLine {
            scenario: s.id.clone(),
            client: client_name.to_string(),
            server: server_name.to_string(),
            status: String::new(),
            detail: None,
            failed_checks: Vec::new(),
            wire: None,
            duration_ms: 0.0,
            client_impl: None,
        };

        if !s.applies(client_name, server_name) {
            line.status = "n/a".into();
            line.duration_ms = 0.0;
            results.push(line);
            continue;
        }

        proxy.select_scenario(&s.id);
        let (ok, reason) = server.select(&s.id).await?;
        if !ok {
            line.status = "skip".into();
            line.detail = Some(format!(
                "server harness: {}",
                reason.unwrap_or_else(|| "unsupported".into())
            ));
            results.push(line);
            continue;
        }

        let input = json!({
            "scenario": s.id,
            "baseUrl": public_base_url,
            "op": s.client.op,
            "params": s.client.params,
            "rawBody": s.client.raw_body,
            "timeoutMs": timeout.as_millis() as u64,
        });
        let outcome = client.run_scenario(&input, timeout + Duration::from_secs(5)).await?;
        line.duration_ms = start.elapsed().as_secs_f64() * 1000.0;

        let Some(outcome) = outcome else {
            line.status = "error".into();
            line.detail = Some("client harness timed out (killed and respawned)".into());
            attach_wire(&mut line, &proxy, &s.id);
            results.push(line);
            continue;
        };
        line.client_impl = outcome.get("impl").cloned();

        let observed = if s.expect_request.is_some() {
            Some(server.observed().await.unwrap_or(Value::Null))
        } else {
            None
        };

        evaluate_outcome(s, &outcome, observed.as_ref(), &mut line);
        if line.status == "fail" || line.status == "error" {
            attach_wire(&mut line, &proxy, &s.id);
        }
        results.push(line);
    }

    client.shutdown().await;
    server.shutdown().await;
    Ok(results)
}

fn attach_wire(line: &mut ResultLine, proxy: &proxy::CaptureProxy, scenario: &str) {
    if let Some(capture) = proxy.capture_for(scenario) {
        line.wire = Some(Wire::from_capture(&capture));
    }
}

fn evaluate_outcome(
    scenario: &Scenario,
    outcome_line: &Value,
    observed: Option<&Value>,
    line: &mut ResultLine,
) {
    let outcome = &outcome_line["outcome"];
    let kind = outcome["kind"].as_str().unwrap_or("");

    match kind {
        "harness-error" => {
            line.status = "error".into();
            line.detail = outcome["detail"].as_str().map(String::from)
                .or_else(|| outcome["errorMessage"].as_str().map(String::from))
                .or(Some("harness error".into()));
            return;
        }
        "unsupported" => {
            line.status = "skip".into();
            line.detail = Some(
                outcome["detail"]
                    .as_str()
                    .unwrap_or("client harness: unsupported op")
                    .to_string(),
            );
            return;
        }
        _ => {}
    }

    let expect = &scenario.expect;
    match expect.kind.as_str() {
        "error" => {
            if kind != "error" {
                line.status = "fail".into();
                line.detail = Some(format!("expected protocol error, got outcome kind {kind:?}"));
                return;
            }
            let code = outcome["errorCode"].as_i64();
            if expect.error_code.is_some() && code != expect.error_code {
                line.status = "fail".into();
                line.detail = Some(format!(
                    "expected error code {:?}, got {:?} ({})",
                    expect.error_code,
                    code,
                    outcome["errorMessage"].as_str().unwrap_or("")
                ));
                return;
            }
        }
        "stream" => {
            if kind != "stream" {
                line.status = "fail".into();
                line.detail = Some(format!(
                    "expected stream, got outcome kind {kind:?} ({})",
                    outcome["errorMessage"].as_str().unwrap_or("")
                ));
                return;
            }
            let events = outcome["events"].as_array().cloned().unwrap_or_default();
            let kinds: Vec<&str> = events
                .iter()
                .map(|e| e["kind"].as_str().unwrap_or(""))
                .collect();
            if let Some(order) = &expect.stream_order {
                if kinds != order.iter().map(String::as_str).collect::<Vec<_>>() {
                    line.status = "fail".into();
                    line.detail = Some(format!(
                        "stream order mismatch: expected {:?}, got {:?}",
                        order, kinds
                    ));
                    return;
                }
            }
            // Synthesize {"events":[{kind: value}]} for path checks.
            let synthesized = json!({
                "events": events
                    .iter()
                    .map(|e| json!({ e["kind"].as_str().unwrap_or("?"): e["value"] }))
                    .collect::<Vec<_>>()
            });
            let failures = checks::evaluate(&synthesized, &expect.checks);
            if !failures.is_empty() {
                line.status = "fail".into();
                line.detail = Some("stream event checks failed".into());
                line.failed_checks = failures;
                return;
            }
        }
        expected_kind @ ("result" | "card" | "interface") => {
            if kind != expected_kind {
                line.status = "fail".into();
                line.detail = Some(format!(
                    "expected {expected_kind}, got outcome kind {kind:?} (code {:?}, {})",
                    outcome["errorCode"],
                    outcome["errorMessage"].as_str().unwrap_or("")
                ));
                return;
            }
            let failures = checks::evaluate(&outcome["value"], &expect.checks);
            if !failures.is_empty() {
                line.status = "fail".into();
                line.detail = Some("value checks failed".into());
                line.failed_checks = failures;
                return;
            }
        }
        other => {
            line.status = "error".into();
            line.detail = Some(format!("scenario has unknown expect.kind {other:?}"));
            return;
        }
    }

    if let Some(expect_request) = &scenario.expect_request {
        let observed = observed.cloned().unwrap_or(Value::Null);
        if observed["params"].is_null() {
            line.status = "skip".into();
            line.detail =
                Some("server harness cannot observe request params (expectRequest unverifiable)".into());
            return;
        }
        let failures = checks::evaluate(&observed, &expect_request.checks);
        if !failures.is_empty() {
            line.status = "fail".into();
            line.detail = Some("request-side checks failed (observed at server SDK)".into());
            line.failed_checks = failures;
            return;
        }
    }

    line.status = "pass".into();
}
