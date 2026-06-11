// Client harness for the a2a-rs SDK extended with the arkavo identity layer
// (arkavo-ext-rust). This is the core adapters/rust client harness, extended
// with scenario-keyed identity behavior (IDENTITY-HARNESS.md):
//
// - `arkavo/identity/cwt-auth-success`: resolve the card, read the target
//   DID from the aia-identity extension params, mint fresh CWTs through
//   arkavo-a2a-identity's CwtCallInterceptor, run the op;
// - `cwt-expired` / `cwt-wrong-audience`: sign a deliberately bad CWT and
//   attach it via a fixed-header CallInterceptor; the SDK's transport-level
//   auth failure surfaces as outcome error with errorCode null;
// - `card-signature-valid` / `-tampered`: ResolveCard with
//   verify_card(VerifyMode::RequireIdentity) — fail closed on any
//   non-verified outcome.
//
// Every other scenario behaves byte-identically to adapters/rust.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use a2a::*;
use a2a_client::agent_card::AgentCardResolver;
use a2a_client::jsonrpc::JsonRpcTransport;
use a2a_client::middleware::CallInterceptor;
use a2a_client::{A2AClient, Transport};
use a2a_pb::protojson_conv::{self, ProtoJsonPayload};
use arkavo_a2a_identity::{
    CardVerification, ConfirmationKey, CwtCallInterceptor, CwtClaims, CwtCredential, VerifyMode,
    did_key_from_verifying_key, random_cti, sign_cwt, verify_card,
};
use arkavo_a2a_policy::{REJECTION_TYPE_URL, TORG_POLICY_REFUSAL_CODE};
use arkavo_a2a_iroh::{A2AIrohClient, parse_node_addr};
use arkavo_a2a_ws::{A2AWsClient, WireFormat};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;

#[path = "../fixtures.rs"]
mod fixtures;
use fixtures::{EXTENSION_URI, IdentityFixtures};

#[path = "../tdf.rs"]
mod tdf;
use arkavo_a2a_tdf as tdfsdk;

const IMPL_NAME: &str = "arkavo-ext-rust";
const DEFAULT_TIMEOUT_MS: u64 = 30000;

/// Open-form protocol binding for the WebSocket interface (ws-binding-v1 §1).
const TRANSPORT_PROTOCOL_JSONRPC_WS: &str = "JSONRPC-WS";

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
    // torg-gate §3.2: an error carrying the `arkavo.torg.v1.Rejection` @type
    // detail IS the TorgPolicyRefusal (−32099), whatever code the generic
    // SDK decode surfaced. The a2a-rs server envelope auto-appends a
    // google.rpc.ErrorInfo whose fallback reason for non-core codes is
    // INTERNAL_ERROR, and the a2a-rs client lets that protocol-domain reason
    // override the wire code (−32099 → −32603); the extension-aware harness
    // restores the pinned code from the normative detail. Scenario-blind:
    // keyed on the wire shape, never on scenario ids.
    let is_torg_refusal = e
        .details
        .as_ref()
        .is_some_and(|details| details.iter().any(|d| d.type_url == REJECTION_TYPE_URL));
    if is_torg_refusal {
        return outcome_error(Some(TORG_POLICY_REFUSAL_CODE), e.message.clone());
    }
    outcome_error(Some(e.code), e.message.clone())
}

type Client = A2AClient<Box<dyn Transport>>;

/// Returns true when two URLs share scheme://host:port.
fn same_authority(a: &str, b: &str) -> bool {
    fn authority(u: &str) -> Option<(String, String, Option<u16>)> {
        let parsed = url::Url::parse(u).ok()?;
        Some((
            parsed.scheme().to_string(),
            parsed.host_str()?.to_string(),
            parsed.port_or_known_default(),
        ))
    }
    matches!((authority(a), authority(b)), (Some(x), Some(y)) if x == y)
}

/// Card-first construction, like a real SDK consumer: resolve the agent card
/// and build through the factory so SDK-side card-derived behavior (interface
/// selection, tenant echoing) engages. Falls back to a synthetic JSONRPC
/// transport when the card is unavailable or its JSONRPC interface points at
/// a different authority (guard against wandering off to fixture URLs).
async fn build_client(base_url: &str) -> Result<Client, A2AError> {
    let resolver = AgentCardResolver::new(None);
    if let Ok(card) = resolver.resolve(base_url).await {
        let jsonrpc_iface_local = card.supported_interfaces.iter().any(|i| {
            i.protocol_binding.eq_ignore_ascii_case(TRANSPORT_PROTOCOL_JSONRPC)
                && same_authority(&i.url, base_url)
        });
        if jsonrpc_iface_local {
            let factory = a2a_client::A2AClientFactory::builder()
                .preferred_bindings(vec![TRANSPORT_PROTOCOL_JSONRPC.to_string()])
                .build();
            if let Ok(client) = factory.create_from_card(&card).await {
                return Ok(client);
            }
        }
    }
    let http = a2a_client::default_reqwest_client(None)?;
    let transport: Box<dyn Transport> =
        Box::new(JsonRpcTransport::new(http, base_url.to_string()));
    Ok(A2AClient::new(transport))
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
    F: FnOnce(Client, Req) -> Fut,
    Fut: Future<Output = Result<Resp, A2AError>>,
{
    let req: Req = match decode_params(&input.params) {
        Ok(r) => r,
        Err(outcome) => return outcome,
    };
    let client = match build_client(&input.base_url).await {
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
    let client = match build_client(&input.base_url).await {
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

// ---------------------------------------------------------------------------
// arkavo-ext: WebSocket binding (ws-binding-v1, WS-HARNESS.md)
//
// ws/* and transport-equivalence/* scenarios resolve the card, select its
// JSONRPC-WS interface, connect the SDK A2AWsClient to the real ws port, and
// drive the op — emitting the SAME outcome shapes the HTTP path emits. The WS
// connection is opened inside each scenario's op handler and dropped at the end
// of that handler (per-op lifecycle). All multi-step WS behaviours (re-subscribe,
// concurrent multiplexing) happen WITHIN a single op-line handler on one live
// socket, so the connection never needs to persist across op lines — the harness
// has no /select signal. Per-op open/close honours WS-HARNESS.md's "torn down"
// deterministically (A2AWsClient is Arc<WsClientConnection>; dropping the last
// clone aborts its reader/writer) and removes any stale-connection risk.
// ---------------------------------------------------------------------------

/// The card's JSONRPC-WS interface URL (ws-binding-v1 §1 — interface
/// selection, exercising the real card-advertised endpoint).
async fn resolve_ws_url(base_url: &str) -> Result<String, Value> {
    let resolver = AgentCardResolver::new(None);
    let card = resolver
        .resolve(base_url)
        .await
        .map_err(|e| outcome_error(None, format!("card resolve failed: {e}")))?;
    card.supported_interfaces
        .iter()
        .find(|i| i.protocol_binding.eq_ignore_ascii_case(TRANSPORT_PROTOCOL_JSONRPC_WS))
        .map(|i| i.url.clone())
        .ok_or_else(|| {
            outcome_error(None, "card has no JSONRPC-WS interface".to_string())
        })
}

/// Open a fresh A2AWsClient for `ws_url`, offering `offer` in preference order
/// (ws-binding-v1 §2). The caller scopes the client to one op handler; when it
/// (and any clones) drop, the underlying connection's reader/writer abort —
/// per-op tear-down (see module header).
async fn ws_client(ws_url: &str, offer: &[WireFormat]) -> Result<A2AWsClient, Value> {
    A2AWsClient::connect(ws_url, offer)
        .await
        .map_err(|e| outcome_error(None, format!("ws connect failed: {e}")))
}

/// Split a `StreamResponse`-encoded value into the runner's {kind, value} shape.
fn split_stream_event(encoded: Value) -> Result<Value, Value> {
    let Some(obj) = encoded.as_object() else {
        return Err(outcome_harness_error("stream event is not an object".to_string()));
    };
    let Some((kind, value)) = obj.iter().next() else {
        return Err(outcome_harness_error("stream event has no variant key".to_string()));
    };
    Ok(json!({"kind": kind, "value": value}))
}

/// Drain a SDK WS stream into the runner's stream-outcome events list.
async fn drain_ws_stream(
    mut stream: BoxStream<'static, Result<StreamResponse, A2AError>>,
) -> Result<Vec<Value>, Value> {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(sr) => {
                let encoded = encode_value(&sr)?;
                events.push(split_stream_event(encoded)?);
            }
            Err(e) => {
                return Err(outcome_error(
                    Some(e.code),
                    format!("stream error after {} events: {}", events.len(), e.message),
                ));
            }
        }
    }
    Ok(events)
}

/// Run one op over a connected A2AWsClient, producing the runner outcome value
/// (result/stream/error) — the same shapes the HTTP path emits.
async fn ws_run_op(client: &A2AWsClient, input: &InputLine) -> Value {
    match input.op.as_str() {
        "SendMessage" => {
            let req: SendMessageRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(o) => return o,
            };
            match client.send_message(&req).await {
                Ok(resp) => match encode_value(&resp) {
                    Ok(v) => outcome_result(v),
                    Err(o) => o,
                },
                Err(e) => a2a_error(&e),
            }
        }
        "GetTask" => {
            let req: GetTaskRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(o) => return o,
            };
            match client.get_task(&req).await {
                Ok(resp) => match encode_value(&resp) {
                    Ok(v) => outcome_result(v),
                    Err(o) => o,
                },
                Err(e) => a2a_error(&e),
            }
        }
        "SendStreamingMessage" => {
            let req: SendMessageRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(o) => return o,
            };
            match client.send_streaming_message(&req).await {
                Ok(stream) => match drain_ws_stream(stream).await {
                    Ok(events) => json!({"kind": "stream", "events": events}),
                    Err(o) => o,
                },
                Err(e) => a2a_error(&e),
            }
        }
        "SubscribeToTask" => {
            let req: SubscribeToTaskRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(o) => return o,
            };
            match client.subscribe_to_task(&req).await {
                Ok(stream) => match drain_ws_stream(stream).await {
                    Ok(events) => json!({"kind": "stream", "events": events}),
                    Err(o) => o,
                },
                Err(e) => a2a_error(&e),
            }
        }
        other => json!({"kind": "unsupported",
            "detail": format!("ws path does not support op {other}")}),
    }
}

/// Every ws cell offers both codecs in preference order: the CBOR binary-frame
/// path is exercised wherever the server negotiates it, and
/// `ws-subprotocol-negotiation` asserts the server selects cbor from that
/// ordered offer (canonical-CBOR binary frame).
fn ws_codec_offer() -> Vec<WireFormat> {
    vec![WireFormat::Cbor, WireFormat::Json]
}

/// `arkavo/ws/*`: connect via the SDK WS client to the card's JSONRPC-WS
/// interface, drive the op, emit the HTTP-equivalent outcome shape.
async fn run_ws(input: &InputLine) -> Value {
    let ws_url = match resolve_ws_url(&input.base_url).await {
        Ok(u) => u,
        Err(o) => return o,
    };
    let offer = ws_codec_offer();
    let client = match ws_client(&ws_url, &offer).await {
        Ok(c) => c,
        Err(o) => return o,
    };

    // ws-subprotocol-negotiation: assert the negotiated codec via the SDK's
    // exposed format(), then run the op (result).
    if input.scenario == "arkavo/ws/ws-subprotocol-negotiation" {
        let negotiated = client.format();
        if negotiated != WireFormat::Cbor {
            return outcome_error(
                None,
                format!(
                    "subprotocol negotiation: offered [cbor, json], expected cbor, got {}",
                    negotiated.subprotocol()
                ),
            );
        }
        eprintln!(
            "client-harness: ws negotiated subprotocol {}",
            negotiated.subprotocol()
        );
        return ws_run_op(&client, input).await;
    }

    // ws-resubscribe-same-socket: subscribe, consume, then re-subscribe on the
    // SAME live socket with a fresh id (ws-binding-v1 §3). Emit the second run.
    if input.scenario == "arkavo/ws/ws-resubscribe-same-socket" {
        let first = ws_run_op(&client, input).await;
        if first["kind"] != "stream" {
            return first; // surface the failure as-is
        }
        eprintln!("client-harness: ws re-subscribing on the same socket (fresh id)");
        return ws_run_op(&client, input).await;
    }

    // ws-concurrent-streams-multiplexed: open TWO streaming exchanges on one
    // connection concurrently; both must complete in per-id order. Emit the
    // first stream's outcome (both share the scenario's expected order).
    if input.scenario == "arkavo/ws/ws-concurrent-streams-multiplexed" {
        let (a, b) = futures::join!(ws_run_op(&client, input), ws_run_op(&client, input));
        if b["kind"] != "stream" {
            return b;
        }
        return a;
    }

    ws_run_op(&client, input).await
}

/// `arkavo/transport-equivalence/*`: run the op over HTTP/JSON AND over WS, and
/// assert the decoded results are identical in the harness (WS-HARNESS). For
/// send-message-equivalent, also run a WS/CBOR leg (the canonical binary
/// frame). The WS run's outcome is emitted; divergence fails the outcome.
async fn run_transport_equivalence(input: &InputLine) -> Value {
    // HTTP/JSON leg (the equivalence oracle).
    let http_outcome = match input.op.as_str() {
        "SendStreamingMessage" | "SubscribeToTask" => run_streaming(input).await,
        _ => run_op_http_unary(input).await,
    };
    let comparable_http = comparable_outcome(&http_outcome);

    let ws_url = match resolve_ws_url(&input.base_url).await {
        Ok(u) => u,
        Err(o) => return o,
    };

    // WS legs: JSON first (always), then CBOR when negotiable (the binary
    // canonical-CBOR frame path) — for the unary send-message case.
    let mut ws_outcome = Value::Null;
    let legs: &[(&str, WireFormat)] = match input.op.as_str() {
        "SendMessage" => &[("ws/json", WireFormat::Json), ("ws/cbor", WireFormat::Cbor)],
        _ => &[("ws/json", WireFormat::Json)],
    };
    for (label, codec) in legs {
        let client = match ws_client(&ws_url, &[*codec]).await {
            Ok(c) => c,
            Err(o) => return o,
        };
        if client.format() != *codec {
            return outcome_error(
                None,
                format!(
                    "transport-equivalence {label}: server did not negotiate {}",
                    codec.subprotocol()
                ),
            );
        }
        let outcome = ws_run_op(&client, input).await;
        let comparable_ws = comparable_outcome(&outcome);
        if comparable_ws != comparable_http {
            return outcome_error(
                None,
                format!(
                    "transport-equivalence divergence ({label} vs http/json): {comparable_ws} != {comparable_http}"
                ),
            );
        }
        eprintln!(
            "client-harness: transport-equivalence {label} ({}) ≡ http/json",
            codec.subprotocol()
        );
        ws_outcome = outcome;
    }
    ws_outcome
}

/// The part of an outcome that must be byte-identical across transports: the
/// decoded result value, or the ordered stream event list. Error/harness
/// outcomes compare whole (a divergence there is itself a failure).
fn comparable_outcome(outcome: &Value) -> Value {
    match outcome.get("kind").and_then(Value::as_str) {
        Some("result") => outcome.get("value").cloned().unwrap_or(Value::Null),
        Some("stream") => outcome.get("events").cloned().unwrap_or(Value::Null),
        _ => outcome.clone(),
    }
}

/// HTTP unary path returning the runner outcome value (mirrors run_unary's
/// dispatch but reusable from the equivalence oracle).
async fn run_op_http_unary(input: &InputLine) -> Value {
    match input.op.as_str() {
        "SendMessage" => {
            run_unary(input, |c, req: SendMessageRequest| async move {
                c.send_message(&req).await
            })
            .await
        }
        "GetTask" => {
            run_unary(input, |c, req: GetTaskRequest| async move { c.get_task(&req).await })
                .await
        }
        other => json!({"kind": "unsupported",
            "detail": format!("transport-equivalence http oracle: unsupported op {other}")}),
    }
}

// ---------------------------------------------------------------------------
// arkavo-ext: scenario-keyed identity ops (IDENTITY-HARNESS.md)
// ---------------------------------------------------------------------------

fn load_fixtures() -> Result<&'static IdentityFixtures, Value> {
    static FIXTURES: std::sync::OnceLock<Result<IdentityFixtures, String>> =
        std::sync::OnceLock::new();
    match FIXTURES.get_or_init(IdentityFixtures::load) {
        Ok(fx) => Ok(fx),
        Err(e) => Err(outcome_harness_error(format!(
            "identity fixtures unavailable: {e}"
        ))),
    }
}

/// Fixed-header presenter for deliberately bad credentials (one token,
/// minted before the call, attached verbatim).
struct StaticBearerInterceptor(String);

#[async_trait::async_trait]
impl CallInterceptor for StaticBearerInterceptor {
    async fn before(
        &self,
        _method: &str,
        params: &mut a2a_client::transport::ServiceParams,
    ) -> Result<(), A2AError> {
        params
            .entry("authorization".to_string())
            .or_default()
            .push(self.0.clone());
        Ok(())
    }
}

/// TEST-ONLY throwaway DID for the wrong-audience credential, derived from
/// the same constant scalar as the fixture generator's.
fn throwaway_did() -> String {
    let secret =
        p256::SecretKey::from_slice(&[0x42u8; 32]).expect("constant scalar is in range");
    did_key_from_verifying_key(p256::ecdsa::SigningKey::from(secret).verifying_key())
}

/// The target DID (`aud`): the aia-identity extension's `params.did` on the
/// resolved card, falling back to the manifest serverDid.
async fn resolve_aud(base_url: &str, fx: &IdentityFixtures) -> String {
    let resolver = AgentCardResolver::new(None);
    if let Ok(card) = resolver.resolve(base_url).await
        && let Some(did) = card
            .capabilities
            .extensions
            .as_ref()
            .and_then(|exts| exts.iter().find(|e| e.uri == EXTENSION_URI))
            .and_then(|e| e.params.as_ref())
            .and_then(|p| p.get("did"))
            .and_then(Value::as_str)
    {
        return did.to_string();
    }
    fx.server_did.clone()
}

enum Credential {
    Fresh,
    Expired,
    WrongAudience,
}

async fn run_cwt_send(input: &InputLine, fx: &IdentityFixtures, cred: Credential) -> Value {
    if input.op != "SendMessage" {
        return json!({"kind": "unsupported",
            "detail": format!("cwt scenarios are pinned to SendMessage, got {}", input.op)});
    }
    let req: SendMessageRequest = match decode_params(&input.params) {
        Ok(r) => r,
        Err(outcome) => return outcome,
    };
    let cnf = ConfirmationKey::from_verifying_key(&fx.client_verifying);
    let interceptor: Arc<dyn CallInterceptor> = match &cred {
        Credential::Fresh => {
            let aud = resolve_aud(&input.base_url, fx).await;
            let credential = CwtCredential::new(
                fx.issuer_signing.clone(),
                fx.iss.clone(),
                fx.client_did.clone(),
                aud,
                cnf,
            ); // default TTL: 240 s, fresh random cti per call
            Arc::new(CwtCallInterceptor::new(credential))
        }
        Credential::Expired | Credential::WrongAudience => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let (aud, iat, exp) = match cred {
                // 60 s past expiry: beyond the ±30 s skew allowance.
                Credential::Expired => (fx.server_did.clone(), now - 300, now - 60),
                _ => (throwaway_did(), now, now + 240),
            };
            let cti = match random_cti() {
                Ok(c) => c,
                Err(e) => return outcome_harness_error(format!("cti mint failed: {e}")),
            };
            let claims = CwtClaims {
                iss: fx.iss.clone(),
                sub: fx.client_did.clone(),
                aud,
                exp,
                nbf: None,
                iat,
                cti,
                cnf,
            };
            let token = match sign_cwt(&claims, &fx.issuer_signing) {
                Ok(t) => t,
                Err(e) => return outcome_harness_error(format!("CWT sign failed: {e}")),
            };
            Arc::new(StaticBearerInterceptor(format!(
                "Bearer {}",
                URL_SAFE_NO_PAD.encode(token)
            )))
        }
    };

    let client = match build_client(&input.base_url).await {
        Ok(c) => c,
        Err(e) => return outcome_harness_error(format!("failed to build client: {e}")),
    }
    .with_interceptors(vec![interceptor]);

    match client.send_message(&req).await {
        Ok(resp) => match encode_value(&resp) {
            Ok(v) => outcome_result(v),
            Err(outcome) => outcome,
        },
        Err(e) => match cred {
            Credential::Fresh => a2a_error(&e),
            // Transport-level auth rejection. Empirical: a2a-rs surfaces the
            // 401 as a fabricated internal error (it never exposes the HTTP
            // status); per CONTRACT.md a non-protocol failure maps to
            // errorCode null with the description in errorMessage.
            _ => outcome_error(
                None,
                format!(
                    "transport-level auth rejection (expected HTTP 401; SDK surfaced code {}): {}",
                    e.code, e.message
                ),
            ),
        },
    }
}

async fn run_verified_resolve_card(input: &InputLine) -> Value {
    // Verify over the exact served bytes (the JWS covers their canonical
    // form — re-encoding through the SDK first could legally drop unknown
    // fields); on success, resolve through the SDK for the outcome value.
    let url = format!(
        "{}/.well-known/agent-card.json",
        input.base_url.trim_end_matches('/')
    );
    let raw: Value = match reqwest::Client::new().get(&url).send().await {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(e) => return outcome_error(None, format!("card fetch failed: {e}")),
        },
        Err(e) => return outcome_error(None, format!("card fetch failed: {e}")),
    };
    match verify_card(&raw, VerifyMode::RequireIdentity) {
        Ok(CardVerification::Verified { did, kid }) => {
            eprintln!("client-harness: card identity-verified (did {did}, kid {kid})");
            run_resolve_card(input).await
        }
        Ok(other) => outcome_error(None, format!("card not identity-verified: {other:?}")),
        Err(e) => outcome_error(None, format!("card verification failed (fail closed): {e}")),
    }
}

async fn run_identity(input: &InputLine) -> Value {
    let fx = match load_fixtures() {
        Ok(f) => f,
        Err(outcome) => return outcome,
    };
    match input
        .scenario
        .strip_prefix("arkavo/identity/")
        .unwrap_or("")
    {
        "cwt-auth-success" => run_cwt_send(input, fx, Credential::Fresh).await,
        "cwt-expired" => run_cwt_send(input, fx, Credential::Expired).await,
        "cwt-wrong-audience" => run_cwt_send(input, fx, Credential::WrongAudience).await,
        "card-signature-valid" | "card-signature-tampered" => {
            run_verified_resolve_card(input).await
        }
        other => json!({"kind": "unsupported",
            "detail": format!("unknown arkavo/identity scenario: {other}")}),
    }
}

// ---------------------------------------------------------------------------
// arkavo-ext: scenario-keyed TDF parts (tdf-parts-v1, TDF-HARNESS.md)
// ---------------------------------------------------------------------------

fn load_tdf_fixtures() -> Result<&'static tdf::TdfFixtures, Value> {
    static FIXTURES: std::sync::OnceLock<Result<tdf::TdfFixtures, String>> =
        std::sync::OnceLock::new();
    match FIXTURES.get_or_init(tdf::TdfFixtures::load) {
        Ok(fx) => Ok(fx),
        Err(e) => Err(outcome_harness_error(format!("tdf fixtures unavailable: {e}"))),
    }
}

/// Normalize integral floats (e.g. `25.0`) back to JSON integers anywhere in a
/// value. The a2a-rs wire layer carries `Part.data` as a protobuf Struct, whose
/// numbers are all f64 — so `ciphertextLength: 25` arrives as `25.0`, which the
/// strict `u64` manifest parser rejects. Both languages float Struct numbers,
/// so this normalization is harness-side (the SDK crate stays untouched) and is
/// safe: it only rewrites floats with no fractional part.
fn normalize_ints(v: &mut Value) {
    match v {
        Value::Number(n) => {
            if n.as_i64().is_none() && n.as_u64().is_none() {
                if let Some(f) = n.as_f64() {
                    if f.fract() == 0.0 && f.abs() < 9.007e15 {
                        *n = serde_json::Number::from(f as i64);
                    }
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_ints),
        Value::Object(map) => map.values_mut().for_each(normalize_ints),
        _ => {}
    }
}

/// Return a copy of `part` whose Data value has integral floats normalized to
/// integers (see `normalize_ints`), so the strict manifest parser accepts it.
fn normalized_part(part: &Part) -> Part {
    let mut p = part.clone();
    if let a2a::PartContent::Data(value) = &mut p.content {
        normalize_ints(value);
    }
    p
}

/// Collect the parts the server delivered (artifact parts), and the SDK-encoded
/// result value (for the runner's id check). For SendMessage the response is a
/// Task or Message; for GetTask/CancelTask it is a Task.
fn collect_parts(parts: &[Part]) -> (Option<&Part>, Vec<String>) {
    let mut enc_part = None;
    let mut plaintext_siblings = Vec::new();
    for p in parts {
        let is_enc = p
            .metadata
            .as_ref()
            .is_some_and(|m| m.contains_key(tdfsdk::ENC_KEY));
        if is_enc {
            enc_part = Some(p);
        } else if let a2a::PartContent::Text(t) = &p.content {
            plaintext_siblings.push(t.clone());
        }
    }
    (enc_part, plaintext_siblings)
}

/// The artifact parts from a typed Task.
fn task_parts(task: &Task) -> Vec<Part> {
    task.artifacts
        .as_ref()
        .map(|arts| arts.iter().flat_map(|a| a.parts.clone()).collect())
        .unwrap_or_default()
}

async fn run_tdf(input: &InputLine) -> Value {
    let fx = match load_tdf_fixtures() {
        Ok(f) => f,
        Err(o) => return o,
    };
    let suffix = match input.scenario.strip_prefix("arkavo/tdf/") {
        Some(s) => s,
        None => return outcome_harness_error("not a tdf scenario".to_string()),
    };

    let client = match build_client(&input.base_url).await {
        Ok(c) => c,
        Err(e) => return outcome_harness_error(format!("failed to build client: {e}")),
    };

    // Run the op and obtain the typed result + its parts + the encoded value.
    let (result_value, parts): (Value, Vec<Part>) = match input.op.as_str() {
        "SendMessage" => {
            let req: SendMessageRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(o) => return o,
            };
            match client.send_message(&req).await {
                Ok(resp) => {
                    let encoded = match encode_value(&resp) {
                        Ok(v) => v,
                        Err(o) => return o,
                    };
                    let parts = match &resp {
                        SendMessageResponse::Task(t) => task_parts(t),
                        SendMessageResponse::Message(m) => m.parts.clone(),
                    };
                    (encoded, parts)
                }
                Err(e) => return a2a_error(&e),
            }
        }
        "GetTask" => {
            let req: GetTaskRequest = match decode_params(&input.params) {
                Ok(r) => r,
                Err(o) => return o,
            };
            match client.get_task(&req).await {
                Ok(task) => {
                    let encoded = match encode_value(&task) {
                        Ok(v) => v,
                        Err(o) => return o,
                    };
                    (encoded, task_parts(&task))
                }
                Err(e) => return a2a_error(&e),
            }
        }
        other => {
            return json!({"kind": "unsupported",
                "detail": format!("tdf path does not support op {other}")});
        }
    };

    let (enc_part, siblings) = collect_parts(&parts);
    let Some(enc_part) = enc_part else {
        return outcome_harness_error("no #enc part in the delivered artifact".to_string());
    };
    let sibling = siblings.into_iter().next().unwrap_or_default();

    let tdf_outcome = match suffix {
        "nanotdf-part-roundtrip" => {
            let enc_part = &normalized_part(enc_part);
            match tdfsdk::decrypt_inline(enc_part, &fx.test_dek) {
                Ok(pt) => {
                    let plaintext = String::from_utf8_lossy(&pt).to_string();
                    json!({
                        "scheme": "nanotdf",
                        "plaintext": plaintext,
                        "sibling": sibling,
                    })
                }
                Err(e) => {
                    return outcome_error(None, format!("roundtrip decrypt failed: {e}"));
                }
            }
        }
        "tdf-part-wrong-key-fails-closed" => {
            // Decrypt with the WRONG dek -> GCM auth failure -> fail closed.
            // The harness maps the wrong-key (KAS) denial to kas-denied.
            let enc_part = &normalized_part(enc_part);
            match tdfsdk::decrypt_inline(enc_part, &fx.wrong_dek) {
                Ok(_pt) => {
                    return outcome_error(
                        None,
                        "fail-closed violated: wrong key yielded plaintext".to_string(),
                    );
                }
                Err(_e) => {
                    // Surface #error.code = kas-denied on the part (local-only);
                    // assert no plaintext present, sibling still delivered.
                    let mut surfaced = enc_part.clone();
                    tdfsdk::attach_error(&mut surfaced, "kas-denied", Some("unwrap denied"));
                    let code = surfaced
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get(tdfsdk::ERROR_KEY))
                        .and_then(|e| e.get("code"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    json!({
                        "errorCode": code,
                        "plaintextPresent": false,
                        "sibling": sibling,
                    })
                }
            }
        }
        "b3-url-artifact-integrity" => match run_b3(enc_part, &fx.test_dek, &sibling).await {
            Ok(v) => v,
            Err(o) => return o,
        },
        other => {
            return json!({"kind": "unsupported",
                "detail": format!("unknown tdf scenario: {other}")});
        }
    };

    // Merge the tdf object into the SDK-encoded result so the runner can check
    // both the id (SDK result) AND the TDF behavior.
    let mut value = result_value;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("tdf".to_string(), tdf_outcome);
    } else {
        value = json!({ "result": value, "tdf": tdf_outcome });
    }
    outcome_result(value)
}

/// b3-url-artifact-integrity: read the url part, verify /b3/<hex> shape, FETCH
/// the blob over HTTP, verify BLAKE3==#enc.b3, then decrypt the fetched inline
/// archive. payload-first holds because the server wrote the blob before serving
/// the message, so the fetch succeeds (the blob was readable on first receipt).
async fn run_b3(enc_part: &Part, dek: &tdfsdk::Dek, sibling: &str) -> Result<Value, Value> {
    let a2a::PartContent::Url(url) = &enc_part.content else {
        return Err(outcome_error(None, "b3 part is not a url part".to_string()));
    };
    let enc_meta = enc_part
        .metadata
        .as_ref()
        .and_then(|m| m.get(tdfsdk::ENC_KEY))
        .cloned()
        .unwrap_or(Value::Null);
    let b3_expected = enc_meta
        .get("b3")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // Verify the pinned /b3/<hex> shape (64 lowercase hex chars).
    let url_shape_ok = url
        .rsplit_once("/b3/")
        .map(|(_, hex)| hex == b3_expected && hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or(false);
    if !url_shape_ok {
        return Err(outcome_error(None, format!("b3 url shape invalid: {url}")));
    }

    // FETCH the blob over HTTP (through the capture proxy).
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| outcome_error(None, format!("b3 fetch failed: {e}")))?;
    if !resp.status().is_success() {
        // payload-first violated -> the blob was not readable (404 race).
        return Err(outcome_error(
            None,
            format!("b3 fetch returned {} (payload-first violated)", resp.status()),
        ));
    }
    let fetched = resp
        .bytes()
        .await
        .map_err(|e| outcome_error(None, format!("b3 body read failed: {e}")))?
        .to_vec();

    // Verify integrity: BLAKE3(fetched) == #enc.b3 (fail closed on mismatch).
    let b3_verified = tdfsdk::verify_b3(&fetched, &b3_expected).is_ok();
    if !b3_verified {
        let mut surfaced = enc_part.clone();
        tdfsdk::attach_error(&mut surfaced, "integrity-failed", Some("b3 mismatch"));
        return Ok(json!({
            "urlShapeOk": true,
            "b3Verified": false,
            "payloadFirst": true,
            "errorCode": "integrity-failed",
            "sibling": sibling,
        }));
    }

    // The blob is the inline NanoTDF archive (manifest+ciphertext) as JSON; wrap
    // it back into a data part and decrypt to the known plaintext.
    let mut inline: Value = serde_json::from_slice(&fetched)
        .map_err(|e| outcome_error(None, format!("blob is not inline JSON: {e}")))?;
    normalize_ints(&mut inline);
    let mut data_part = a2a::Part::data(inline).with_media_type(tdfsdk::NANOTDF_MEDIA_TYPE);
    let mut md = std::collections::HashMap::new();
    md.insert(tdfsdk::ENC_KEY.to_string(), json!({"scheme": "nanotdf", "v": 1}));
    data_part.metadata = Some(md);
    let plaintext = match tdfsdk::decrypt_inline(&data_part, dek) {
        Ok(pt) => String::from_utf8_lossy(&pt).to_string(),
        Err(e) => return Err(outcome_error(None, format!("b3 decrypt failed: {e}"))),
    };

    Ok(json!({
        "urlShapeOk": true,
        "b3Verified": true,
        "payloadFirst": true,
        "plaintext": plaintext,
        "sibling": sibling,
    }))
}

// ---------------------------------------------------------------------------
// arkavo-ext: iroh discovery (iroh-discovery-v1, IROH-HARNESS.md)
//
// Transport selection by impl: the Rust ext client runs arkavo/discovery/* over
// NATIVE iroh (rs↔rs), dialing the iroh endpoint directly. The dialable node
// address arrives in the HTTP-resolved card's iroh-discovery extension params as
// the test-only `nodeAddr` (the runner forwards only baseUrl; iroh:// has no
// direct addresses under the hermetic no-discovery topology). For
// relay-fallback-equivalence the op also runs through the relay gateway and the
// decoded results are compared byte-identically in-harness.
// ---------------------------------------------------------------------------

const IROH_DISCOVERY_EXTENSION_URI: &str = "https://arkavo.social/ext/a2a/iroh-discovery/v1";

/// The iroh-discovery extension params from the HTTP-resolved card:
/// `(nodeId, gateway, nodeAddr)`. `nodeAddr` is the test-only dialable carriage
/// (IROH-HARNESS.md); `gateway` is the relay base.
struct IrohParams {
    node_id: String,
    gateway: String,
    node_addr: String,
}

/// Resolve the card over HTTP and read the iroh-discovery extension params.
async fn resolve_iroh_params(base_url: &str) -> Result<(AgentCard, IrohParams), Value> {
    let resolver = AgentCardResolver::new(None);
    let card = resolver
        .resolve(base_url)
        .await
        .map_err(|e| outcome_error(None, format!("card resolve failed: {e}")))?;
    let ext = card
        .capabilities
        .extensions
        .as_ref()
        .and_then(|exts| exts.iter().find(|e| e.uri == IROH_DISCOVERY_EXTENSION_URI))
        .ok_or_else(|| outcome_error(None, "card has no iroh-discovery extension".to_string()))?;
    let params = ext
        .params
        .as_ref()
        .ok_or_else(|| outcome_error(None, "iroh-discovery extension has no params".to_string()))?;
    let get = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| outcome_error(None, format!("iroh-discovery params missing {k}")))
    };
    let node_id = get("nodeId")?;
    let gateway = get("gateway")?;
    let node_addr = get("nodeAddr")?;
    Ok((card, IrohParams { node_id, gateway, node_addr }))
}

/// Native dial: connect to the iroh endpoint with the given codec offer.
async fn iroh_connect(node_addr: &str, offer: &[WireFormat]) -> Result<A2AIrohClient, Value> {
    let addr = parse_node_addr(node_addr)
        .map_err(|e| outcome_error(None, format!("bad iroh node addr: {e}")))?;
    A2AIrohClient::connect(addr, offer)
        .await
        .map_err(|e| outcome_error(None, format!("iroh connect failed: {e}")))
}

/// JCS-ish canonical comparison: serde re-serialization with sorted keys is
/// enough here (both sides are the same SDK's AgentCard), so compare the
/// serde_json::Value forms directly.
fn cards_match(a: &AgentCard, b: &AgentCard) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

/// `arkavo/discovery/card-by-node-id`: resolve the card by NodeId over native
/// iroh under BOTH ALPNs, assert each matches the HTTP well-known card. Emit the
/// resolved card (kind=card) for the runner's name check.
async fn run_discovery_card_by_node_id(input: &InputLine) -> Value {
    let (http_card, p) = match resolve_iroh_params(&input.base_url).await {
        Ok(v) => v,
        Err(o) => return o,
    };

    // Run under both ALPNs (cbor then json): the resolved card must match the
    // HTTP card after canonicalization in both codecs.
    let mut last_card: Option<AgentCard> = None;
    for codec in [WireFormat::Cbor, WireFormat::Json] {
        let client = match iroh_connect(&p.node_addr, &[codec]).await {
            Ok(c) => c,
            Err(o) => return o,
        };
        if client.format() != codec {
            return outcome_error(
                None,
                format!("iroh ALPN negotiation: expected {codec:?}, got {:?}", client.format()),
            );
        }
        let resolved = match client.resolve_card().await {
            Ok(c) => c,
            Err(e) => return a2a_error(&e),
        };
        if !cards_match(&resolved, &http_card) {
            return outcome_error(
                None,
                format!(
                    "native iroh card ({codec:?}) does not match HTTP well-known card after canonicalization"
                ),
            );
        }
        eprintln!(
            "client-harness: iroh card-by-node-id matched under ALPN {codec:?} (node {})",
            p.node_id
        );
        last_card = Some(resolved);
    }

    let card = last_card.unwrap_or(http_card);
    match serde_json::to_value(&card) {
        Ok(v) => json!({"kind": "card", "value": v}),
        Err(e) => outcome_harness_error(format!("failed to re-encode card: {e}")),
    }
}

/// `arkavo/discovery/relay-fallback-equivalence`: run the op natively over iroh
/// AND through the relay gateway, assert byte-identical decoded results, and
/// emit the (shared) result.
async fn run_discovery_relay_equivalence(input: &InputLine) -> Value {
    if input.op != "SendMessage" {
        return json!({"kind": "unsupported",
            "detail": format!("relay-fallback-equivalence is pinned to SendMessage, got {}", input.op)});
    }
    let (_card, p) = match resolve_iroh_params(&input.base_url).await {
        Ok(v) => v,
        Err(o) => return o,
    };
    let req: SendMessageRequest = match decode_params(&input.params) {
        Ok(r) => r,
        Err(o) => return o,
    };

    // Native leg: dial iroh, send the message, encode the response.
    let native_client = match iroh_connect(&p.node_addr, &[WireFormat::Cbor, WireFormat::Json]).await {
        Ok(c) => c,
        Err(o) => return o,
    };
    let native_resp = match native_client.send_message(&req).await {
        Ok(r) => r,
        Err(e) => return a2a_error(&e),
    };
    let native_value = match encode_value(&native_resp) {
        Ok(v) => v,
        Err(o) => return o,
    };

    // Relay leg: POST the JSON-RPC request to <gateway>/<nodeId>/ (the node
    // base, iroh-discovery-v1 §5) and decode the response through the SDK.
    let relay_value = match relay_send_message(&p, &req).await {
        Ok(v) => v,
        Err(o) => return o,
    };

    if native_value != relay_value {
        return outcome_error(
            None,
            format!(
                "relay-fallback divergence: native iroh result != relay result ({native_value} != {relay_value})"
            ),
        );
    }
    eprintln!(
        "client-harness: relay-fallback-equivalence native iroh ≡ relay (node {})",
        p.node_id
    );
    outcome_result(native_value)
}

/// Run SendMessage through the relay gateway: build the op URL from the
/// `<gateway>/<nodeId>/` base (the relay client MUST NOT parse a sub-path out of
/// the card — iroh-discovery-v1 §5), POST the JSON-RPC request, and decode the
/// SDK response so the decoded result is comparable to the native leg.
async fn relay_send_message(p: &IrohParams, req: &SendMessageRequest) -> Result<Value, Value> {
    let params = protojson_conv::to_value(req)
        .map_err(|e| outcome_harness_error(format!("failed to encode params: {e}")))?;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": params,
    });
    let url = format!("{}/{}/", p.gateway.trim_end_matches('/'), p.node_id);
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| outcome_error(None, format!("relay POST failed: {e}")))?;
    let envelope: Value = resp
        .json()
        .await
        .map_err(|e| outcome_error(None, format!("relay response not JSON: {e}")))?;
    if let Some(err) = envelope.get("error") {
        let code = err.get("code").and_then(Value::as_i64).map(|c| c as i32);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Err(outcome_error(code, format!("relay returned error: {message}")));
    }
    let result = envelope
        .get("result")
        .cloned()
        .ok_or_else(|| outcome_error(None, "relay envelope has no result".to_string()))?;
    // Decode through the SDK and re-encode so the comparison matches the native
    // leg's encode_value(SendMessageResponse) output exactly.
    let decoded: SendMessageResponse = protojson_conv::from_value(result)
        .map_err(|e| outcome_error(None, format!("relay result decode failed: {e}")))?;
    encode_value(&decoded)
}

async fn run_discovery(input: &InputLine) -> Value {
    match input.scenario.strip_prefix("arkavo/discovery/").unwrap_or("") {
        "card-by-node-id" => run_discovery_card_by_node_id(input).await,
        "relay-fallback-equivalence" => run_discovery_relay_equivalence(input).await,
        other => json!({"kind": "unsupported",
            "detail": format!("unknown arkavo/discovery scenario: {other}")}),
    }
}

async fn run_op(input: &InputLine) -> Value {
    // Scenario-keyed identity behavior lives here in the harness, exactly
    // like the tolgaki harness's loopback seeding: SDK code paths never see
    // scenario ids.
    if input.scenario.starts_with("arkavo/identity/") {
        return run_identity(input).await;
    }
    // TDF parts (tdf-parts-v1, TDF-HARNESS.md): the client decrypts/verifies the
    // encrypted part and surfaces fail-closed #error metadata. Scenario-keyed in
    // the harness; the SDK code path stays scenario-blind.
    if input.scenario.starts_with("arkavo/tdf/") {
        return run_tdf(input).await;
    }
    // WS binding (ws-binding-v1, WS-HARNESS.md): transport selection by
    // scenario. ws/* drives the SDK WS client; transport-equivalence/* runs the
    // op over both HTTP and WS and asserts identical decoded results.
    if input.scenario.starts_with("arkavo/ws/") {
        return run_ws(input).await;
    }
    if input.scenario.starts_with("arkavo/transport-equivalence/") {
        return run_transport_equivalence(input).await;
    }
    // iroh discovery (iroh-discovery-v1, IROH-HARNESS.md): the Rust ext client
    // runs arkavo/discovery/* over NATIVE iroh (rs↔rs) + the relay leg.
    if input.scenario.starts_with("arkavo/discovery/") {
        return run_discovery(input).await;
    }
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
    // The iroh dep pulls rustls(tls-ring) into the tree, unifying reqwest onto
    // rustls-no-provider; install the ring crypto provider once so
    // reqwest::Client construction (and any rustls use) does not panic.
    let _ = rustls::crypto::ring::default_provider().install_default();

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
