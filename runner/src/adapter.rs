use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixConfig {
    pub implementations: std::collections::BTreeMap<String, Implementation>,
    pub matrix: MatrixSelection,
    #[serde(default)]
    pub runner: RunnerConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Implementation {
    #[serde(default)]
    pub build: Option<String>,
    pub client: String,
    pub server: String,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixSelection {
    pub clients: Vec<String>,
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RunnerConfig {
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

pub fn load_matrix(path: &Path) -> Result<MatrixConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let config: MatrixConfig = toml::from_str(&text)?;
    for name in config.matrix.clients.iter().chain(&config.matrix.servers) {
        if !config.implementations.contains_key(name) {
            bail!("matrix references unknown implementation {name:?}");
        }
    }
    Ok(config)
}

pub fn run_build(name: &str, implementation: &Implementation, repo_root: &Path) -> Result<()> {
    let Some(build) = &implementation.build else {
        return Ok(());
    };
    eprintln!("[build] {name}: {build}");
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(build)
        .current_dir(repo_root)
        .status()?;
    if !status.success() {
        bail!("build failed for {name}");
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyInfo {
    #[allow(dead_code)]
    pub port: u16,
    pub control_port: u16,
    pub base_url: String,
}

pub struct ServerHandle {
    child: Child,
    pub ready: ReadyInfo,
    control: reqwest::Client,
}

impl ServerHandle {
    pub async fn spawn(
        executable: &Path,
        scenarios_dir: &Path,
        public_base_url: &str,
    ) -> Result<ServerHandle> {
        let mut child = Command::new(executable)
            .arg("--scenarios")
            .arg(scenarios_dir)
            .arg("--public-base-url")
            .arg(public_base_url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {}", executable.display()))?;
        let stdout = child.stdout.take().context("server stdout")?;
        let mut lines = BufReader::new(stdout).lines();
        let ready_line = tokio::time::timeout(Duration::from_secs(60), lines.next_line())
            .await
            .context("timed out waiting for READY")??
            .context("server harness exited before READY")?;
        let payload = ready_line
            .strip_prefix("READY ")
            .with_context(|| format!("expected READY line, got {ready_line:?}"))?;
        let ready: ReadyInfo = serde_json::from_str(payload)?;
        // Drain any further stdout to avoid blocking the child.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Ok(ServerHandle {
            child,
            ready,
            control: reqwest::Client::new(),
        })
    }

    pub async fn select(&self, scenario: &str) -> Result<(bool, Option<String>)> {
        let url = format!("http://127.0.0.1:{}/select", self.ready.control_port);
        let response: Value = self
            .control
            .post(&url)
            .json(&json!({ "scenario": scenario }))
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .json()
            .await?;
        let ok = response["ok"].as_bool().unwrap_or(false);
        let reason = response["reason"].as_str().map(String::from);
        Ok((ok, reason))
    }

    pub async fn observed(&self) -> Result<Value> {
        let url = format!("http://127.0.0.1:{}/observed", self.ready.control_port);
        Ok(self
            .control
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .json()
            .await?)
    }

    pub async fn shutdown(mut self) {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = stdin.shutdown().await;
        }
        drop(self.child.stdin.take());
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}

pub struct ClientHandle {
    executable: PathBuf,
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
}

impl ClientHandle {
    pub fn spawn(executable: &Path) -> Result<ClientHandle> {
        let mut child = Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {}", executable.display()))?;
        let stdin = child.stdin.take().context("client stdin")?;
        let stdout = child.stdout.take().context("client stdout")?;
        Ok(ClientHandle {
            executable: executable.to_path_buf(),
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        })
    }

    /// Sends one input line and awaits the outcome line. On timeout the child
    /// is killed and respawned so subsequent scenarios are unaffected.
    pub async fn run_scenario(&mut self, input: &Value, timeout: Duration) -> Result<Option<Value>> {
        let mut line = serde_json::to_string(input)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        match tokio::time::timeout(timeout, self.lines.next_line()).await {
            Err(_) => {
                self.respawn().await?;
                Ok(None) // timeout
            }
            Ok(Ok(Some(text))) => {
                let value: Value = serde_json::from_str(&text)
                    .with_context(|| format!("client outcome is not JSON: {text:?}"))?;
                Ok(Some(value))
            }
            Ok(Ok(None)) => bail!("client harness closed stdout"),
            Ok(Err(e)) => Err(e.into()),
        }
    }

    async fn respawn(&mut self) -> Result<()> {
        let _ = self.child.kill().await;
        let replacement = ClientHandle::spawn(&self.executable)?;
        *self = replacement;
        Ok(())
    }

    pub async fn shutdown(mut self) {
        let _ = self.stdin.shutdown().await;
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}
