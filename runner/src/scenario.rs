use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    #[allow(dead_code)]
    pub spec: String,
    pub client: ClientSpec,
    #[serde(default)]
    pub server: Option<Value>,
    pub expect: Expect,
    #[serde(default)]
    pub expect_request: Option<ExpectRequest>,
    pub tags: Vec<String>,
    #[serde(default)]
    pub applies_to: Option<AppliesTo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSpec {
    pub op: String,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub raw_body: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expect {
    pub kind: String,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub error_code: Option<i64>,
    #[serde(default)]
    pub stream_order: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectRequest {
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Check {
    pub path: String,
    #[serde(default)]
    pub equals: Option<Value>,
    #[serde(default)]
    pub exists: Option<bool>,
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliesTo {
    #[serde(default)]
    pub clients: Option<Value>,
    #[serde(default)]
    pub servers: Option<Value>,
}

fn selector_matches(selector: &Option<Value>, name: &str) -> bool {
    match selector {
        None => true,
        Some(Value::String(s)) => s == "*",
        Some(Value::Array(items)) => items.iter().any(|v| v.as_str() == Some(name)),
        Some(_) => false,
    }
}

impl Scenario {
    pub fn applies(&self, client: &str, server: &str) -> bool {
        match &self.applies_to {
            None => true,
            Some(a) => selector_matches(&a.clients, client) && selector_matches(&a.servers, server),
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    pub fn group(&self) -> &str {
        self.id.rsplit_once('/').map(|(g, _)| g).unwrap_or("")
    }
}

fn collect_json_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_json_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

pub fn load_all(dir: &Path) -> Result<Vec<Scenario>> {
    let root = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    let mut files = Vec::new();
    collect_json_files(&root, &mut files)?;

    let mut scenarios = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        let scenario: Scenario =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let expected_id = path
            .with_extension("")
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if scenario.id != expected_id {
            bail!(
                "scenario id {:?} does not match its path ({})",
                scenario.id,
                expected_id
            );
        }
        scenarios.push(scenario);
    }
    scenarios.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(scenarios)
}
