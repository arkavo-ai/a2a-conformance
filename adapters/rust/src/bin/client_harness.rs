// Client harness for the a2a-rs SDK (Linux Foundation Rust SDK).
//
// Reads NDJSON op lines on stdin and performs each op through the SDK's
// native client API, emitting one outcome line per input on stdout
// (schema/harness-outcome.schema.json). Logs go to stderr.

use std::io::Write;
use std::time::{Duration, Instant};

use a2a::*;
use a2a_client::A2AClient;
use a2a_client::agent_card::AgentCardResolver;
use a2a_client::jsonrpc::JsonRpcTransport;
use a2a_pb::protojson_conv::{self, ProtoJsonPayload};
use futures::{FutureExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;

const IMPL_NAME: &str = "rust-a2a";
const DEFAULT_TIMEOUT_MS: u64 = 30000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputLine {
    scenario: String,
    base_url: String,
    op: String,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    raw_body: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

fn outcome_result(value: Value) -> Value {
    json!({"kind": "result", "value": value})
}

fn outcome_error(code: Option<i32>, message: String) -> Value {
    json!({"kind": "error", "errorCode": code, "errorMessage": message})
}

fn outcome_harness_error(detail: String) -> Value {
    json!({"kind": "harness-error", "detail": detail})
}

fn a2a_error(e: &A2AError) -> Value {
    outcome_error(Some(e.code), e.message.clone())
}

fn build_client(base_url: &str) -> Result<A2AClient<JsonRpcTransport>, A2AError> {
    let http = a2a_client::default_reqwest_client(None)?;
    Ok(A2AClient::new(JsonRpcTransport::new(http, base_url.to_string())))
}

fn decode_params<T: ProtoJsonPayload>(params: &Option<Value>) -> Result<T, Value> {
    let raw = params.clone().unwrap_or_else(|| json!({}));
    protojson_conv::from_value(raw)
        .map_err(|e| outcome_error(None, format!("SDK could not decode params: {e}")))
}

fn encode_value<T: ProtoJsonPayload>(value: &T) -> Result<Value, Value> {
    protojson_conv::to_value(value)
        .map_err(|e| outcome_harness_error(format!("failed to re-encode result: {e}")))
}

async fn run_unary<Req, Resp, F, Fut>(input: &InputLine, call: F) -> Value
where
    Req: ProtoJsonPayload,
    Resp: ProtoJsonPayload,
    F: FnOnce(A2AClient<JsonRpcTransport>, Req) -> Fut,
    Fut: Future<Output = Result<Resp, A2AError>>,
{
    let req: Req = match decode_params(&input.params) {
        Ok(r) => r,
        Err(outcome) => return outcome,
    };
    let client = match build_client(&input.base_url) {
        Ok(c) => c,
        Err(e) => return outcome_harness_error(format!("failed to build client: {e}")),
    };
    match call(client, req).await {
        Ok(resp) => match encode_value(&resp) {
            Ok(v) => outcome_result(v),
            Err(outcome) => outcome,
        },
        Err(e) => a2a_error(&e),
    }
}

async fn run_streaming(input: &InputLine) -> Value {
    let client = match build_client(&input.base_url) {
        Ok(c) => c,
        Err(e) => return outcome_harness_error(format!("failed to build client: {e}")),
    };

    let stream = match input.op.as_str() {
        "SendStreamingMessage" => {
            let req: SendMessageRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(outcome) => return outcome,
            };
            client.send_streaming_message(&req).await
        }
        _ => {
            let req: SubscribeToTaskRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(outcome) => return outcome,
            };
            client.subscribe_to_task(&req).await
        }
    };

    let mut stream = match stream {
        Ok(s) => s,
        Err(e) => return a2a_error(&e),
    };

    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(sr) => {
                // The SDK's wire encoder produces the single-key oneof object
                // ({"task": ...} etc.); split it into (kind, value).
                let encoded = match encode_value(&sr) {
                    Ok(v) => v,
                    Err(outcome) => return outcome,
                };
                let Some(obj) = encoded.as_object() else {
                    return outcome_harness_error("stream event is not an object".to_string());
                };
                let Some((kind, value)) = obj.iter().next() else {
                    return outcome_harness_error("stream event has no variant key".to_string());
                };
                events.push(json!({"kind": kind, "value": value}));
            }
            Err(e) => {
                return outcome_error(
                    Some(e.code),
                    format!("stream error after {} events: {}", events.len(), e.message),
                );
            }
        }
    }
    json!({"kind": "stream", "events": events})
}

async fn run_resolve_card(input: &InputLine) -> Value {
    let resolver = AgentCardResolver::new(None);
    match resolver.resolve(&input.base_url).await {
        Ok(card) => match serde_json::to_value(&card) {
            Ok(v) => json!({"kind": "card", "value": v}),
            Err(e) => outcome_harness_error(format!("failed to re-encode card: {e}")),
        },
        Err(e) => a2a_error(&e),
    }
}

/// Mirrors A2AClientFactory::create_from_card's candidate ranking
/// (a2a-client/src/factory.rs): default registered factories are JSONRPC and
/// HTTP+JSON at protocol major version 1, default preference order
/// [JSONRPC, HTTP+JSON], stable sort by preference. The factory does not
/// expose which interface it picked, so the ranking is replicated here and
/// create_from_card is still invoked to let the SDK do the actual selection
/// and connection.
fn select_interface(card: &AgentCard) -> Option<&AgentInterface> {
    let registered: [(&str, u64); 2] = [
        (TRANSPORT_PROTOCOL_JSONRPC, 1),
        (TRANSPORT_PROTOCOL_HTTP_JSON, 1),
    ];
    let preferred = [TRANSPORT_PROTOCOL_JSONRPC, TRANSPORT_PROTOCOL_HTTP_JSON];

    let mut candidates: Vec<(usize, &AgentInterface)> = Vec::new();
    for iface in &card.supported_interfaces {
        let major = iface
            .protocol_version
            .split('.')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);
        if registered.contains(&(iface.protocol_binding.as_str(), major)) {
            let priority = preferred
                .iter()
                .position(|b| *b == iface.protocol_binding)
                .unwrap_or(usize::MAX);
            candidates.push((priority, iface));
        }
    }
    candidates.sort_by_key(|(prio, _)| *prio);
    candidates.first().map(|(_, iface)| *iface)
}

async fn run_select_interface(input: &InputLine) -> Value {
    let resolver = AgentCardResolver::new(None);
    let card = match resolver.resolve(&input.base_url).await {
        Ok(c) => c,
        Err(e) => return a2a_error(&e),
    };

    let factory = a2a_client::A2AClientFactory::builder().build();
    if let Err(e) = factory.create_from_card(&card).await {
        return a2a_error(&e);
    }

    match select_interface(&card) {
        Some(iface) => match serde_json::to_value(iface) {
            Ok(v) => json!({"kind": "interface", "value": v}),
            Err(e) => outcome_harness_error(format!("failed to re-encode interface: {e}")),
        },
        None => outcome_error(None, "no compatible interface selected".to_string()),
    }
}

async fn run_raw_request(input: &InputLine) -> Value {
    let Some(raw_body) = &input.raw_body else {
        return outcome_harness_error("RawRequest without rawBody".to_string());
    };
    let client = reqwest::Client::new();
    let resp = match client
        .post(&input.base_url)
        .header("Content-Type", "application/json")
        .body(raw_body.clone())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return outcome_error(None, format!("HTTP request failed: {e}")),
    };
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return outcome_error(None, format!("failed to read response body: {e}")),
    };

    match serde_json::from_str::<Value>(&text) {
        Ok(envelope) => {
            if let Some(error) = envelope.get("error") {
                let code = error.get("code").and_then(|c| c.as_i64()).map(|c| c as i32);
                let message = error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                outcome_error(code, message)
            } else if let Some(result) = envelope.get("result") {
                outcome_result(result.clone())
            } else {
                outcome_error(None, format!("HTTP {status}: no result or error in envelope"))
            }
        }
        Err(e) => outcome_error(None, format!("HTTP {status}: non-JSON response: {e}")),
    }
}

async fn run_op(input: &InputLine) -> Value {
    match input.op.as_str() {
        "SendMessage" => {
            run_unary(input, |c, req: SendMessageRequest| async move {
                c.send_message(&req).await
            })
            .await
        }
        "GetTask" => {
            run_unary(input, |c, req: GetTaskRequest| async move {
                c.get_task(&req).await
            })
            .await
        }
        "ListTasks" => {
            run_unary(input, |c, req: ListTasksRequest| async move {
                c.list_tasks(&req).await
            })
            .await
        }
        "CancelTask" => {
            run_unary(input, |c, req: CancelTaskRequest| async move {
                c.cancel_task(&req).await
            })
            .await
        }
        "CreateTaskPushNotificationConfig" => {
            run_unary(input, |c, req: TaskPushNotificationConfig| async move {
                c.create_push_config(&req).await
            })
            .await
        }
        "GetExtendedAgentCard" => {
            run_unary(input, |c, req: GetExtendedAgentCardRequest| async move {
                c.get_extended_agent_card(&req).await
            })
            .await
        }
        "SendStreamingMessage" | "SubscribeToTask" => run_streaming(input).await,
        "ResolveCard" => run_resolve_card(input).await,
        "SelectInterface" => run_select_interface(input).await,
        "RawRequest" => run_raw_request(input).await,
        other => json!({"kind": "unsupported", "detail": format!("unknown op: {other}")}),
    }
}

async fn handle_line(line: &str) -> Value {
    let input: InputLine = match serde_json::from_str(line) {
        Ok(i) => i,
        Err(e) => {
            return json!({
                "scenario": "unknown",
                "outcome": outcome_harness_error(format!("bad input line: {e}")),
                "durationMs": 0,
                "impl": {"name": IMPL_NAME, "version": a2a::VERSION},
            });
        }
    };

    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let start = Instant::now();

    let outcome = match tokio::time::timeout(
        timeout,
        std::panic::AssertUnwindSafe(run_op(&input)).catch_unwind(),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_panic)) => outcome_harness_error("op panicked".to_string()),
        Err(_) => outcome_harness_error(format!("timeout after {}ms", timeout.as_millis())),
    };

    json!({
        "scenario": input.scenario,
        "outcome": outcome,
        "durationMs": start.elapsed().as_millis() as u64,
        "impl": {"name": IMPL_NAME, "version": a2a::VERSION},
    })
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = std::io::stdout();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let outcome = handle_line(&line).await;
                let serialized =
                    serde_json::to_string(&outcome).unwrap_or_else(|e| {
                        format!(
                            r#"{{"scenario":"unknown","outcome":{{"kind":"harness-error","detail":"serialize failure: {e}"}},"durationMs":0,"impl":{{"name":"{IMPL_NAME}","version":"unknown"}}}}"#
                        )
                    });
                if writeln!(stdout, "{serialized}").is_err() {
                    break;
                }
                stdout.flush().ok();
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("client-harness: stdin read error: {e}");
                break;
            }
        }
    }
    eprintln!("client-harness: stdin EOF, exiting");
}
