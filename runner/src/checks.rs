use crate::scenario::Check;
use serde_json::Value;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailedCheck {
    pub path: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<Value>,
}

/// Resolves a simplified JSONPath ($.a.b[0].c) against a value.
pub fn resolve_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let rest = path.strip_prefix('$')?;
    let mut current = root;
    for raw_segment in rest.split('.').filter(|s| !s.is_empty()) {
        // Each segment is `name` optionally followed by one or more [idx].
        let (name, indexes) = match raw_segment.find('[') {
            Some(bracket) => (&raw_segment[..bracket], &raw_segment[bracket..]),
            None => (raw_segment, ""),
        };
        if !name.is_empty() {
            current = current.get(name)?;
        }
        let mut idx_part = indexes;
        while let Some(stripped) = idx_part.strip_prefix('[') {
            let close = stripped.find(']')?;
            let index: usize = stripped[..close].parse().ok()?;
            current = current.get(index)?;
            idx_part = &stripped[close + 1..];
        }
    }
    Some(current)
}

pub fn evaluate(target: &Value, checks: &[Check]) -> Vec<FailedCheck> {
    let mut failures = Vec::new();
    for check in checks {
        let found = resolve_path(target, &check.path);
        if let Some(true) = check.exists {
            if found.is_none() {
                failures.push(FailedCheck {
                    path: check.path.clone(),
                    reason: "expected path to exist".into(),
                    expected: None,
                    actual: None,
                });
            }
            continue;
        }
        if let Some(false) = check.exists {
            if let Some(actual) = found {
                failures.push(FailedCheck {
                    path: check.path.clone(),
                    reason: "expected path to be absent".into(),
                    expected: None,
                    actual: Some(actual.clone()),
                });
            }
            continue;
        }
        let Some(actual) = found else {
            failures.push(FailedCheck {
                path: check.path.clone(),
                reason: "path not found".into(),
                expected: check.equals.clone(),
                actual: None,
            });
            continue;
        };
        if let Some(expected) = &check.equals {
            if actual != expected {
                failures.push(FailedCheck {
                    path: check.path.clone(),
                    reason: "value mismatch".into(),
                    expected: Some(expected.clone()),
                    actual: Some(actual.clone()),
                });
            }
        }
        if let Some(count) = check.count {
            match actual.as_array() {
                Some(items) if items.len() == count => {}
                Some(items) => failures.push(FailedCheck {
                    path: check.path.clone(),
                    reason: format!("expected array of {count}, got {}", items.len()),
                    expected: None,
                    actual: None,
                }),
                None => failures.push(FailedCheck {
                    path: check.path.clone(),
                    reason: "expected an array".into(),
                    expected: None,
                    actual: Some(actual.clone()),
                }),
            }
        }
        if let Some(substring) = &check.contains {
            match actual.as_str() {
                Some(s) if s.contains(substring.as_str()) => {}
                Some(s) => failures.push(FailedCheck {
                    path: check.path.clone(),
                    reason: format!("string does not contain {substring:?}"),
                    expected: None,
                    actual: Some(Value::String(s.to_string())),
                }),
                None => failures.push(FailedCheck {
                    path: check.path.clone(),
                    reason: "expected a string".into(),
                    expected: None,
                    actual: Some(actual.clone()),
                }),
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_nested_paths() {
        let v = json!({"task": {"artifacts": [{"parts": [{"text": "hi"}]}]}});
        assert_eq!(
            resolve_path(&v, "$.task.artifacts[0].parts[0].text"),
            Some(&json!("hi"))
        );
        assert_eq!(resolve_path(&v, "$.task.missing"), None);
    }

    #[test]
    fn evaluates_count_and_equals() {
        let v = json!({"history": [1, 2]});
        let checks = vec![
            Check {
                path: "$.history".into(),
                equals: None,
                exists: None,
                count: Some(2),
                contains: None,
            },
            Check {
                path: "$.history[1]".into(),
                equals: Some(json!(2)),
                exists: None,
                count: None,
                contains: None,
            },
        ];
        assert!(evaluate(&v, &checks).is_empty());
    }
}
