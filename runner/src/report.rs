use crate::checks::FailedCheck;
use anyhow::Result;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultLine {
    pub scenario: String,
    pub client: String,
    pub server: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failed_checks: Vec<FailedCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire: Option<Wire>,
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_impl: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wire {
    pub request: Option<String>,
    pub response: Option<String>,
}

impl Wire {
    pub fn from_capture(capture: &crate::proxy::WireCapture) -> Wire {
        let b64 = base64::engine::general_purpose::STANDARD;
        Wire {
            request: (!capture.client_to_server.is_empty())
                .then(|| b64.encode(&capture.client_to_server)),
            response: (!capture.server_to_client.is_empty())
                .then(|| b64.encode(&capture.server_to_client)),
        }
    }
}

pub fn write_ndjson(path: &Path, results: &[ResultLine]) -> Result<()> {
    let mut out = String::new();
    for line in results {
        out.push_str(&serde_json::to_string(line)?);
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

pub fn load_ndjson(path: &Path) -> Result<Vec<ResultLine>> {
    let text = std::fs::read_to_string(path)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

/// Returns regressions: cells where baseline passed but the current run does not.
pub fn regressions(baseline: &[ResultLine], current: &[ResultLine]) -> Vec<String> {
    let key = |r: &ResultLine| format!("{}|{}|{}", r.scenario, r.client, r.server);
    let current_map: BTreeMap<String, &ResultLine> =
        current.iter().map(|r| (key(r), r)).collect();
    let mut out = Vec::new();
    for b in baseline {
        if b.status != "pass" {
            continue;
        }
        match current_map.get(&key(b)) {
            Some(c) if c.status == "pass" => {}
            Some(c) => out.push(format!(
                "{} [{} -> {}]: pass regressed to {}",
                b.scenario, b.client, b.server, c.status
            )),
            None => out.push(format!(
                "{} [{} -> {}]: missing from current run",
                b.scenario, b.client, b.server
            )),
        }
    }
    out
}

fn lossy_http(b64: &Option<String>) -> String {
    let Some(encoded) = b64 else {
        return "(none)".into();
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    let truncated: String = text.chars().take(4000).collect();
    truncated
}

pub fn write_markdown(
    report_dir: &Path,
    results: &[ResultLine],
    groups: &[&str],
    clients: &[String],
    servers: &[String],
) -> Result<()> {
    std::fs::create_dir_all(report_dir.join("cells"))?;

    let mut md = String::from("# A2A interop matrix\n\n");
    md.push_str("Cell format: `pass/applicable` (skip = harness reported unsupported; n/a = excluded by appliesTo). Self-pairs are sanity checks, not interop evidence.\n\n");

    for group in groups {
        md.push_str(&format!("## {group}\n\n| client \\ server |"));
        for server in servers {
            md.push_str(&format!(" {server} |"));
        }
        md.push_str("\n|---|");
        md.push_str(&"---|".repeat(servers.len()));
        md.push('\n');
        for client in clients {
            md.push_str(&format!("| **{client}** |"));
            for server in servers {
                let cell: Vec<&ResultLine> = results
                    .iter()
                    .filter(|r| {
                        &r.client == client
                            && &r.server == server
                            && r.scenario.starts_with(&format!("{group}/"))
                    })
                    .collect();
                let pass = cell.iter().filter(|r| r.status == "pass").count();
                let applicable = cell
                    .iter()
                    .filter(|r| r.status != "n/a" && r.status != "skip")
                    .count();
                let skip = cell.iter().filter(|r| r.status == "skip").count();
                let marker = if client == server { " (self)" } else { "" };
                let link = format!("cells/{client}--{server}.md");
                if cell.iter().all(|r| r.status == "n/a") {
                    md.push_str(" \u{2013} |");
                } else if applicable == 0 && skip > 0 {
                    md.push_str(&format!(" [skip×{skip}]({link}){marker} |"));
                } else {
                    let badge = if pass == applicable { "✅" } else { "❌" };
                    let skip_note = if skip > 0 {
                        format!(" +{skip}skip")
                    } else {
                        String::new()
                    };
                    md.push_str(&format!(
                        " [{badge} {pass}/{applicable}{skip_note}]({link}){marker} |"
                    ));
                }
            }
            md.push('\n');
        }
        md.push('\n');
    }
    std::fs::write(report_dir.join("matrix.md"), &md)?;

    // Per-cell detail files.
    for client in clients {
        for server in servers {
            let cell: Vec<&ResultLine> = results
                .iter()
                .filter(|r| &r.client == client && &r.server == server)
                .collect();
            if cell.is_empty() {
                continue;
            }
            let mut detail = format!("# {client} (client) → {server} (server)\n\n");
            detail.push_str("| scenario | status | detail |\n|---|---|---|\n");
            for r in &cell {
                detail.push_str(&format!(
                    "| `{}` | {} | {} |\n",
                    r.scenario,
                    r.status,
                    r.detail.as_deref().unwrap_or("")
                ));
            }
            for r in &cell {
                if r.status == "fail" || r.status == "error" {
                    detail.push_str(&format!("\n## {} — {}\n\n", r.scenario, r.status));
                    if !r.failed_checks.is_empty() {
                        detail.push_str("Failed checks:\n\n");
                        for f in &r.failed_checks {
                            detail.push_str(&format!(
                                "- `{}`: {} (expected `{}`, actual `{}`)\n",
                                f.path,
                                f.reason,
                                f.expected
                                    .as_ref()
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into()),
                                f.actual
                                    .as_ref()
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into()),
                            ));
                        }
                    }
                    if let Some(wire) = &r.wire {
                        detail.push_str("\n<details><summary>wire capture (request)</summary>\n\n```http\n");
                        detail.push_str(&lossy_http(&wire.request));
                        detail.push_str("\n```\n</details>\n");
                        detail.push_str("\n<details><summary>wire capture (response)</summary>\n\n```http\n");
                        detail.push_str(&lossy_http(&wire.response));
                        detail.push_str("\n```\n</details>\n");
                    }
                }
            }
            std::fs::write(
                report_dir.join("cells").join(format!("{client}--{server}.md")),
                detail,
            )?;
        }
    }
    Ok(())
}
