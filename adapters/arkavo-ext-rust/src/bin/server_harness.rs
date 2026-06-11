// Server harness for the a2a-rs SDK extended with the arkavo identity layer
// (arkavo-ext-rust). This is the core adapters/rust server harness, extended:
//
// - while an `arkavo/identity/cwt-*` scenario is selected, an axum middleware
//   fn verifies `Authorization: Bearer <CWT>` with arkavo-a2a-identity's
//   CwtVerifier (trust root = test-issuer.pub.pem; 401 per spec §8) before
//   requests reach the SDK's JSON-RPC router;
// - the `arkavo/identity/card-signature-*` scenarios serve a DID-signed
//   (and, for `-tampered`, post-signing mutated) agent card;
// - the default card advertises the aia-identity extension with
//   `params.did` = serverDid;
// - while an `arkavo/policy/*` scenario is selected (POLICY-HARNESS.md), the
//   dispatch path routes through arkavo-a2a-policy's GatedDispatcher over the
//   same scripted handler, with the reference evaluator (plus, for
//   gate-deny-task-op-32099 only, a harness-armed deny-all-task-ops rule).
//
// Every other scenario behaves byte-identically to adapters/rust.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use a2a::*;
use a2a_pb::protojson_conv::{self, ProtoJsonPayload};
use a2a_server::agent_card::{AgentCardProducer, agent_card_router};
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::{RequestHandler, ServiceParams};
use arkavo_a2a_identity::{CwtVerifier, did_key_url, sign_card};
use arkavo_a2a_policy::{
    Decision, GateContext, GatedDispatcher, PolicyEvaluator, ReferenceEvaluator, Rejection,
};
use arkavo_a2a_ws::A2AWsServer;
use async_trait::async_trait;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::{self, BoxStream};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

#[path = "../fixtures.rs"]
mod fixtures;
use fixtures::{EXTENSION_URI, IdentityFixtures};

#[path = "../tdf.rs"]
mod tdf;
use arkavo_a2a_tdf as tdfsdk;
use axum::extract::Path as AxumPath;

/// Open-form protocol binding for the WebSocket interface (ws-binding-v1 §1).
/// A2A §5.8 permits open-form bindings; the standard JSONRPC HTTP interface is
/// always listed first (degradation contract).
const TRANSPORT_PROTOCOL_JSONRPC_WS: &str = "JSONRPC-WS";

// ---------------------------------------------------------------------------
// Scenario files (only the parts the server harness needs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ScenarioFile {
    id: String,
    client: ClientSection,
    #[serde(default)]
    server: Option<ServerSection>,
}

#[derive(Deserialize)]
struct ClientSection {
    op: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerSection {
    #[serde(default)]
    respond: Option<Value>,
    #[serde(default)]
    error: Option<ErrorSection>,
    #[serde(default)]
    sse: Option<Vec<Value>>,
    #[serde(default)]
    card: Option<Value>,
    #[serde(default)]
    raw_result: Option<String>,
}

#[derive(Deserialize)]
struct ErrorSection {
    code: i32,
    message: String,
}

fn load_scenarios(dir: &Path, out: &mut HashMap<String, ScenarioFile>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            load_scenarios(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let text = std::fs::read_to_string(&path)?;
            match serde_json::from_str::<ScenarioFile>(&text) {
                Ok(s) => {
                    out.insert(s.id.clone(), s);
                }
                Err(e) => eprintln!("server-harness: skipping {}: {}", path.display(), e),
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scripted state shared between the control server and the SDK handler
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
enum Scripted {
    /// No scripted response (RawRequest probes; nothing selected yet).
    #[default]
    None,
    Error(A2AError),
    Send(SendMessageResponse),
    Task(Task),
    List(ListTasksResponse),
    Push(TaskPushNotificationConfig),
    ExtendedCard(AgentCard),
    Stream(Vec<StreamResponse>),
}

#[derive(Default)]
struct HarnessState {
    scripted: Scripted,
    observed: Option<Value>,
}

type SharedState = Arc<Mutex<HarnessState>>;

fn decode<T: serde::de::DeserializeOwned>(what: &str, v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("SDK cannot decode {what}: {e}"))
}

/// Build the scripted handler state (and the card to serve) for a scenario.
/// Err(reason) means this harness cannot serve the scenario (-> skip).
fn arm(scenario: &ScenarioFile, public_base_url: &str) -> Result<(Scripted, Option<AgentCard>), String> {
    let Some(server) = &scenario.server else {
        // RawRequest scenarios have no server section by design: the SDK's
        // own jsonrpc layer handles the probe.
        return Ok((Scripted::None, None));
    };

    if server.raw_result.is_some() {
        return Err("typed handler cannot inject raw JSON".to_string());
    }

    let card = match &server.card {
        Some(card_value) => {
            let text = serde_json::to_string(card_value)
                .map_err(|e| format!("card is not JSON: {e}"))?
                .replace("{{baseUrl}}", public_base_url);
            let card: AgentCard = serde_json::from_str(&text)
                .map_err(|e| format!("SDK AgentCard cannot decode scripted card: {e}"))?;
            Some(card)
        }
        None => None,
    };

    if let Some(err) = &server.error {
        return Ok((Scripted::Error(A2AError::new(err.code, err.message.clone())), card));
    }

    let scripted = match scenario.client.op.as_str() {
        "SendMessage" => {
            let v = server.respond.as_ref().ok_or("scenario has no server.respond")?;
            Scripted::Send(decode("SendMessageResponse", v)?)
        }
        "GetTask" | "CancelTask" => {
            let v = server.respond.as_ref().ok_or("scenario has no server.respond")?;
            Scripted::Task(decode("Task", v)?)
        }
        "ListTasks" => {
            let v = server.respond.as_ref().ok_or("scenario has no server.respond")?;
            Scripted::List(decode("ListTasksResponse", v)?)
        }
        "CreateTaskPushNotificationConfig" => {
            let v = server.respond.as_ref().ok_or("scenario has no server.respond")?;
            Scripted::Push(decode("TaskPushNotificationConfig", v)?)
        }
        "GetExtendedAgentCard" => {
            let v = server.respond.as_ref().ok_or("scenario has no server.respond")?;
            Scripted::ExtendedCard(decode("AgentCard", v)?)
        }
        "SendStreamingMessage" | "SubscribeToTask" => {
            let frames = server.sse.as_ref().ok_or("scenario has no server.sse")?;
            let mut events = Vec::with_capacity(frames.len());
            for frame in frames {
                events.push(decode::<StreamResponse>("StreamResponse", frame)?);
            }
            Scripted::Stream(events)
        }
        "ResolveCard" | "SelectInterface" | "RawRequest" => Scripted::None,
        other => return Err(format!("unknown op: {other}")),
    };

    Ok((scripted, card))
}

// ---------------------------------------------------------------------------
// Scripted RequestHandler
// ---------------------------------------------------------------------------

struct ScriptedHandler {
    state: SharedState,
    ext: Arc<ExtState>,
}

impl ScriptedHandler {
    fn observe<T: ProtoJsonPayload>(&self, req: &T) {
        let value = protojson_conv::to_value(req).ok();
        self.state.lock().unwrap().observed = value;
    }

    /// Record (once) the write_seq at which the referencing message is first
    /// dispatched to the SDK handler — the payload-first ordering check compares
    /// this against the blob's `blob_written_at` (TDF-HARNESS.md §b3).
    fn mark_message_dispatched(&self) {
        let mut b3 = self.ext.b3.lock().unwrap();
        if b3.message_dispatched_at.is_none() {
            b3.message_dispatched_at = Some(b3.write_seq);
        }
    }

    fn scripted(&self) -> Scripted {
        self.state.lock().unwrap().scripted.clone()
    }

    /// Error returned when a request reaches the handler without a script.
    /// An empty required id means the request was effectively missing it
    /// (proto3 default), which the application rejects as invalid params.
    fn unscripted(&self, required_id: Option<&str>) -> A2AError {
        match required_id {
            Some(id) if id.is_empty() => A2AError::invalid_params("missing required field: id"),
            _ => A2AError::internal("no scripted response for the selected scenario"),
        }
    }

    fn mismatch(&self, op: &str) -> A2AError {
        A2AError::internal(format!("scripted response does not match op {op}"))
    }

    fn scripted_stream(
        &self,
        op: &str,
        required_id: Option<&str>,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::Stream(events) => {
                Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
            }
            Scripted::None => Err(self.unscripted(required_id)),
            _ => Err(self.mismatch(op)),
        }
    }
}

#[async_trait]
impl RequestHandler for ScriptedHandler {
    async fn send_message(
        &self,
        _params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2AError> {
        self.observe(&req);
        self.mark_message_dispatched();
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::Send(r) => Ok(r),
            Scripted::None => Err(self.unscripted(None)),
            _ => Err(self.mismatch("SendMessage")),
        }
    }

    async fn send_streaming_message(
        &self,
        _params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.observe(&req);
        self.scripted_stream("SendStreamingMessage", None)
    }

    async fn get_task(
        &self,
        _params: &ServiceParams,
        req: GetTaskRequest,
    ) -> Result<Task, A2AError> {
        self.observe(&req);
        self.mark_message_dispatched();
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::Task(t) => Ok(t),
            Scripted::None => Err(self.unscripted(Some(&req.id))),
            _ => Err(self.mismatch("GetTask")),
        }
    }

    async fn list_tasks(
        &self,
        _params: &ServiceParams,
        req: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2AError> {
        self.observe(&req);
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::List(r) => Ok(r),
            Scripted::None => Err(self.unscripted(None)),
            _ => Err(self.mismatch("ListTasks")),
        }
    }

    async fn cancel_task(
        &self,
        _params: &ServiceParams,
        req: CancelTaskRequest,
    ) -> Result<Task, A2AError> {
        self.observe(&req);
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::Task(t) => Ok(t),
            Scripted::None => Err(self.unscripted(Some(&req.id))),
            _ => Err(self.mismatch("CancelTask")),
        }
    }

    async fn subscribe_to_task(
        &self,
        _params: &ServiceParams,
        req: SubscribeToTaskRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.observe(&req);
        let id = req.id.clone();
        self.scripted_stream("SubscribeToTask", Some(&id))
    }

    async fn create_push_config(
        &self,
        _params: &ServiceParams,
        req: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.observe(&req);
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::Push(r) => Ok(r),
            Scripted::None => Err(self.unscripted(None)),
            _ => Err(self.mismatch("CreateTaskPushNotificationConfig")),
        }
    }

    async fn get_push_config(
        &self,
        _params: &ServiceParams,
        req: GetTaskPushNotificationConfigRequest,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.observe(&req);
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::Push(r) => Ok(r),
            Scripted::None => Err(self.unscripted(None)),
            _ => Err(self.mismatch("GetTaskPushNotificationConfig")),
        }
    }

    async fn list_push_configs(
        &self,
        _params: &ServiceParams,
        req: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        self.observe(&req);
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::None => Err(self.unscripted(None)),
            _ => Err(self.mismatch("ListTaskPushNotificationConfigs")),
        }
    }

    async fn delete_push_config(
        &self,
        _params: &ServiceParams,
        req: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), A2AError> {
        self.observe(&req);
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::None => Err(self.unscripted(None)),
            _ => Err(self.mismatch("DeleteTaskPushNotificationConfig")),
        }
    }

    async fn get_extended_agent_card(
        &self,
        _params: &ServiceParams,
        req: GetExtendedAgentCardRequest,
    ) -> Result<AgentCard, A2AError> {
        self.observe(&req);
        match self.scripted() {
            Scripted::Error(e) => Err(e),
            Scripted::ExtendedCard(c) => Ok(c),
            Scripted::None => Err(self.unscripted(None)),
            _ => Err(self.mismatch("GetExtendedAgentCard")),
        }
    }
}

// ---------------------------------------------------------------------------
// arkavo-ext: scenario-keyed policy gate (POLICY-HARNESS.md)
// ---------------------------------------------------------------------------

/// The harness-side composite evaluator: while gate-deny-task-op-32099 is
/// selected, deny every non-message-carrying (task-management) op with the
/// TM-001 harness rule; otherwise fall through to the shared
/// ReferenceEvaluator. The arming lives HERE, in the harness — the extension
/// layer's evaluators stay scenario-blind (identity auth-arming pattern).
struct HarnessPolicyEvaluator {
    ext: Arc<ExtState>,
}

#[async_trait]
impl PolicyEvaluator for HarnessPolicyEvaluator {
    async fn evaluate(&self, ctx: &GateContext<'_>) -> Decision {
        if self.ext.policy_tm_deny.load(Ordering::SeqCst) && !ctx.op.is_message_carrying() {
            return Decision::Deny(Rejection::new(
                "conformance:reference@v1",
                "TM-001",
                "task-management refused by policy",
            ));
        }
        ReferenceEvaluator.evaluate(ctx).await
    }
}

/// Routing shim mounted in front of the SDK's jsonrpc router (the router is
/// built once at startup, so BOTH dispatch paths are built up front and this
/// outer handler delegates per the armed flag — the mirror of the identity
/// middleware approach at the handler layer):
///
/// - policy armed (`arkavo/policy/*` selected): record the decoded request
///   for `GET /observed` (a denied request never reaches the inner scripted
///   handler, which normally does the recording), then dispatch through the
///   GatedDispatcher over the scripted handler;
/// - not armed: the untouched vanilla path (degradation posture).
struct PolicyShim {
    state: SharedState,
    ext: Arc<ExtState>,
    plain: ScriptedHandler,
    gated: GatedDispatcher<ScriptedHandler, HarnessPolicyEvaluator>,
}

impl PolicyShim {
    fn armed(&self) -> bool {
        self.ext.policy_armed.load(Ordering::SeqCst)
    }

    fn observe<T: ProtoJsonPayload>(&self, req: &T) {
        let value = protojson_conv::to_value(req).ok();
        self.state.lock().unwrap().observed = value;
    }
}

// Each method: observe + gated path while a policy scenario is armed, plain
// scripted path otherwise. (Written out long-hand: a macro_rules shim cannot
// expand inside #[async_trait], which rewrites the impl before expansion.)
#[async_trait]
impl RequestHandler for PolicyShim {
    async fn send_message(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.send_message(params, req).await
        } else {
            self.plain.send_message(params, req).await
        }
    }

    async fn send_streaming_message(
        &self,
        params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.send_streaming_message(params, req).await
        } else {
            self.plain.send_streaming_message(params, req).await
        }
    }

    async fn get_task(
        &self,
        params: &ServiceParams,
        req: GetTaskRequest,
    ) -> Result<Task, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.get_task(params, req).await
        } else {
            self.plain.get_task(params, req).await
        }
    }

    async fn list_tasks(
        &self,
        params: &ServiceParams,
        req: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.list_tasks(params, req).await
        } else {
            self.plain.list_tasks(params, req).await
        }
    }

    async fn cancel_task(
        &self,
        params: &ServiceParams,
        req: CancelTaskRequest,
    ) -> Result<Task, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.cancel_task(params, req).await
        } else {
            self.plain.cancel_task(params, req).await
        }
    }

    async fn subscribe_to_task(
        &self,
        params: &ServiceParams,
        req: SubscribeToTaskRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.subscribe_to_task(params, req).await
        } else {
            self.plain.subscribe_to_task(params, req).await
        }
    }

    async fn create_push_config(
        &self,
        params: &ServiceParams,
        req: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.create_push_config(params, req).await
        } else {
            self.plain.create_push_config(params, req).await
        }
    }

    async fn get_push_config(
        &self,
        params: &ServiceParams,
        req: GetTaskPushNotificationConfigRequest,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.get_push_config(params, req).await
        } else {
            self.plain.get_push_config(params, req).await
        }
    }

    async fn list_push_configs(
        &self,
        params: &ServiceParams,
        req: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.list_push_configs(params, req).await
        } else {
            self.plain.list_push_configs(params, req).await
        }
    }

    async fn delete_push_config(
        &self,
        params: &ServiceParams,
        req: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.delete_push_config(params, req).await
        } else {
            self.plain.delete_push_config(params, req).await
        }
    }

    async fn get_extended_agent_card(
        &self,
        params: &ServiceParams,
        req: GetExtendedAgentCardRequest,
    ) -> Result<AgentCard, A2AError> {
        if self.armed() {
            self.observe(&req);
            self.gated.get_extended_agent_card(params, req).await
        } else {
            self.plain.get_extended_agent_card(params, req).await
        }
    }
}

// ---------------------------------------------------------------------------
// arkavo-ext: scenario-keyed identity layer (IDENTITY-HARNESS.md)
// ---------------------------------------------------------------------------

/// Shared state between /select and the identity middleware. CwtAuthLayer
/// cannot be re-armed after construction (it always enforces), so the harness
/// uses a middleware fn over the same CwtVerifier instead; the verifier (and
/// thus the §5 replay cache) lives for the whole process.
/// The b3 blob store for shape-(b) artifacts (tdf-parts-v1 §3). The ciphertext
/// blob is written here BEFORE the referencing message is served (payload-first
/// ordering); the `/b3/<hex>` route reads from it. `write_seq` is bumped on each
/// write so the payload-first ordering is observable: the harness records the
/// sequence at blob-write time vs. when the message is first dispatched.
#[derive(Default)]
struct B3Store {
    /// hex -> blob bytes.
    blobs: HashMap<String, Vec<u8>>,
    /// Monotonic counter: incremented at each blob write.
    write_seq: u64,
    /// The write_seq value at the moment the b3 blob for the current scenario
    /// was uploaded (payload-first marker).
    blob_written_at: Option<u64>,
    /// The write_seq value at the moment the referencing message was first
    /// dispatched to the SDK handler. Payload-first holds iff
    /// `blob_written_at < message_dispatched_at`.
    message_dispatched_at: Option<u64>,
}

struct ExtState {
    /// True while an `arkavo/identity/cwt-*` scenario is selected.
    armed: AtomicBool,
    /// shape-(b) blob store + payload-first ordering log (TDF-HARNESS.md).
    b3: Mutex<B3Store>,
    /// True while an `arkavo/policy/*` scenario is selected: dispatch routes
    /// through the GatedDispatcher (POLICY-HARNESS.md server arming table).
    policy_armed: AtomicBool,
    /// True while `arkavo/policy/gate-deny-task-op-32099` is selected: the
    /// harness composite evaluator denies every task-management op (TM-001).
    policy_tm_deny: AtomicBool,
    /// Exact bytes to serve for the card-signature scenarios: the signature
    /// covers the canonical form of precisely these bytes, so they bypass
    /// the SDK's card producer (which could re-encode).
    raw_card: RwLock<Option<Vec<u8>>>,
    verifier: CwtVerifier,
    fx: IdentityFixtures,
    /// `kid` DID URL for card signing: serverDid#<did:key fragment>.
    kid: String,
}

fn challenge_401(www_authenticate: &str) -> axum::response::Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::WWW_AUTHENTICATE,
            www_authenticate.to_string(),
        )],
    )
        .into_response()
}

fn invalid_token_401(description: &str) -> axum::response::Response {
    // Quotes stripped so the quoted-string parameter stays well-formed.
    let description = description.replace('"', "'");
    challenge_401(&format!(
        r#"Bearer realm="a2a", error="invalid_token", error_description="{description}""#
    ))
}

/// Pass-through when not armed; spec §8 Bearer-CWT enforcement when armed.
/// `GET /.well-known/agent-card.json` stays anonymous in both states: peers
/// must be able to read the extension advertisement to learn the `aud` (§1),
/// and the card-signature scenarios serve their exact signed bytes here.
async fn identity_middleware(
    State(ext): State<Arc<ExtState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if req.method() == axum::http::Method::GET
        && req.uri().path() == "/.well-known/agent-card.json"
    {
        let override_bytes = ext.raw_card.read().unwrap().clone();
        if let Some(bytes) = override_bytes {
            return (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response();
        }
        return next.run(req).await;
    }
    if !ext.armed.load(Ordering::SeqCst) {
        return next.run(req).await;
    }
    let Some(header) = req.headers().get(axum::http::header::AUTHORIZATION) else {
        return challenge_401(r#"Bearer realm="a2a", error="invalid_request""#);
    };
    let Ok(header) = header.to_str() else {
        return invalid_token_401("authorization header is not valid UTF-8");
    };
    match ext.verifier.verify_bearer(header) {
        Ok(_claims) => next.run(req).await,
        Err(e) => invalid_token_401(&e.to_string()),
    }
}

/// Builds the served bytes for the card-signature scenarios: scenario card
/// with `{{baseUrl}}` substituted, the identity extension advertised, signed
/// with the test-server-agent key — and, for `-tampered`, one character of
/// `description` mutated AFTER signing.
fn build_signed_card(
    scenario: &ScenarioFile,
    public_base_url: &str,
    ext: &ExtState,
) -> Result<Vec<u8>, String> {
    let card_value = scenario
        .server
        .as_ref()
        .and_then(|s| s.card.as_ref())
        .ok_or("card scenario has no server.card")?;
    let text = serde_json::to_string(card_value)
        .map_err(|e| e.to_string())?
        .replace("{{baseUrl}}", public_base_url);
    let mut card: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;

    let capabilities = card
        .as_object_mut()
        .ok_or("card is not an object")?
        .entry("capabilities")
        .or_insert_with(|| json!({}));
    let extensions = capabilities
        .as_object_mut()
        .ok_or("capabilities is not an object")?
        .entry("extensions")
        .or_insert_with(|| json!([]));
    extensions
        .as_array_mut()
        .ok_or("extensions is not an array")?
        .push(json!({
            "uri": EXTENSION_URI,
            "required": false,
            "params": {"did": ext.fx.server_did, "issuer": ext.fx.iss},
        }));

    sign_card(&mut card, &ext.fx.server_signing, &ext.kid).map_err(|e| e.to_string())?;

    if scenario.id.ends_with("card-signature-tampered") {
        let description = card
            .get("description")
            .and_then(Value::as_str)
            .ok_or("card has no description to tamper")?;
        let mut bytes = description.to_string().into_bytes();
        let last = bytes.len().checked_sub(1).ok_or("empty description")?;
        bytes[last] = if bytes[last] == b'!' { b'?' } else { b'!' };
        card["description"] =
            Value::String(String::from_utf8(bytes).map_err(|e| e.to_string())?);
    }

    serde_json::to_vec(&card).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// arkavo-ext: scenario-keyed TDF parts (TDF-HARNESS.md)
// ---------------------------------------------------------------------------

/// Build a sibling plaintext text part (proves mixed messages, §5).
fn sibling_part() -> Part {
    Part::text(tdf::SIBLING_PLAINTEXT)
}

/// Arm an `arkavo/tdf/*` scenario on the server side (TDF-HARNESS.md server
/// table). Returns the scripted response carrying the TDF artifact (+ sibling
/// plaintext part); for the b3 scenario it ALSO writes the ciphertext blob to
/// the b3 store payload-first and resets the ordering log. The served bytes are
/// identical for roundtrip and wrong-key (only the client's DEK differs).
fn arm_tdf(
    scenario: &ScenarioFile,
    public_base_url: &str,
    tdf_fx: &tdf::TdfFixtures,
    ext: &ExtState,
) -> Result<Scripted, String> {
    let server = scenario
        .server
        .as_ref()
        .ok_or("tdf scenario has no server section")?;
    let respond = server
        .respond
        .as_ref()
        .ok_or("tdf scenario has no server.respond")?;

    // Reset the b3 ordering log for this scenario window.
    {
        let mut b3 = ext.b3.lock().unwrap();
        b3.blob_written_at = None;
        b3.message_dispatched_at = None;
    }

    let suffix = scenario
        .id
        .strip_prefix("arkavo/tdf/")
        .ok_or("not a tdf scenario")?;

    // The TDF artifact part(s) injected into the response.
    let parts: Vec<Part> = match suffix {
        "nanotdf-part-roundtrip" | "tdf-part-wrong-key-fails-closed" => {
            // shape-(a): encrypt the known plaintext with the TEST dek (the
            // server always encrypts with the correct key; the wrong-key leg
            // differs only on the CLIENT's decrypt key).
            let enc = tdfsdk::encrypt_inline(
                tdf::KNOWN_PLAINTEXT.as_bytes(),
                &tdf_fx.test_dek,
                tdf::NONCE,
                tdf::KAS_URL,
            )
            .map_err(|e| format!("encrypt_inline failed: {e}"))?;
            vec![enc, sibling_part()]
        }
        "b3-url-artifact-integrity" => {
            // shape-(b): encrypt the known plaintext, take the wire ciphertext
            // bytes as the blob, write it payload-first, and reference it by
            // BLAKE3 hash. The b3 URL uses the PUBLIC base (the capture proxy),
            // so the client fetches through the proxy.
            let enc = tdfsdk::encrypt_inline(
                tdf::KNOWN_PLAINTEXT.as_bytes(),
                &tdf_fx.test_dek,
                tdf::NONCE,
                tdf::KAS_URL,
            )
            .map_err(|e| format!("encrypt_inline failed: {e}"))?;
            // The blob is the full inline data-part value (manifest+ciphertext)
            // serialized to JSON bytes: a self-describing TDF archive the client
            // can decrypt after fetching. (The harness IS the gateway.)
            let a2a::PartContent::Data(inline_value) = &enc.content else {
                return Err("encrypt_inline did not return a data part".into());
            };
            let blob = serde_json::to_vec(inline_value)
                .map_err(|e| format!("blob serialize failed: {e}"))?;
            let b3hex = tdfsdk::blake3_hex(&blob);

            // Payload-first: write the blob BEFORE the message is served and
            // record the write sequence.
            {
                let mut store = ext.b3.lock().unwrap();
                store.write_seq += 1;
                let seq = store.write_seq;
                store.blobs.insert(b3hex.clone(), blob);
                store.blob_written_at = Some(seq);
            }

            // The #manifest sidecar mirrors the inline manifest (so receivers can
            // evaluate KAS/policy before fetching).
            let manifest_sidecar = inline_value
                .get("manifest")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let url_part = tdfsdk::make_b3_url_part(&b3hex, public_base_url, manifest_sidecar)
                .expect("blake3_hex is valid b3 hex");
            vec![url_part, sibling_part()]
        }
        other => return Err(format!("unknown tdf scenario: {other}")),
    };

    let artifact = Artifact {
        artifact_id: "art-tdf-1".to_string(),
        name: Some("tdf-artifact".to_string()),
        description: None,
        parts,
        metadata: None,
        extensions: Some(vec![tdfsdk::EXTENSION_URI.to_string()]),
    };

    // Inject the artifact into the scripted task/message.
    match scenario.client.op.as_str() {
        "SendMessage" => {
            let mut resp: SendMessageResponse =
                decode("SendMessageResponse", respond)?;
            match &mut resp {
                SendMessageResponse::Task(t) => {
                    t.artifacts.get_or_insert_with(Vec::new).push(artifact);
                }
                SendMessageResponse::Message(m) => {
                    m.parts.extend(artifact.parts);
                }
            }
            Ok(Scripted::Send(resp))
        }
        "GetTask" | "CancelTask" => {
            let mut task: Task = decode("Task", respond)?;
            task.artifacts.get_or_insert_with(Vec::new).push(artifact);
            Ok(Scripted::Task(task))
        }
        other => Err(format!("tdf scenario unsupported op: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Swappable agent card producer
// ---------------------------------------------------------------------------

struct SwappableCard {
    card: Arc<RwLock<AgentCard>>,
}

impl AgentCardProducer for SwappableCard {
    fn card(&self) -> AgentCard {
        self.card.read().unwrap().clone()
    }
}

/// Append the JSONRPC-WS interface to a card's `supported_interfaces` if not
/// already present (ws-binding-v1 §1: dual-interface advertisement). The
/// standard JSONRPC HTTP interface stays first (degradation contract); the WS
/// interface points at the real ephemeral WS port. ws/* and
/// transport-equivalence/* clients resolve the card and select THIS interface.
fn advertise_ws_interface(card: &mut AgentCard, ws_base_url: &str) {
    let already = card
        .supported_interfaces
        .iter()
        .any(|i| i.protocol_binding.eq_ignore_ascii_case(TRANSPORT_PROTOCOL_JSONRPC_WS));
    if !already {
        card.supported_interfaces.push(AgentInterface::new(
            ws_base_url,
            TRANSPORT_PROTOCOL_JSONRPC_WS,
        ));
    }
}

/// tdf-parts-v1 §1 advertisement params (schemes REQUIRED; kas/gateway are
/// deployment knobs, not trust anchors).
fn tdf_extension_params() -> HashMap<String, Value> {
    let mut p: HashMap<String, Value> = HashMap::new();
    p.insert("schemes".to_string(), json!(["nanotdf", "tdf"]));
    p.insert("kas".to_string(), json!(["https://kas.arkavo.net"]));
    p.insert("gateway".to_string(), json!("https://tdf.arkavo.net"));
    p
}

fn default_card(public_base_url: &str, ws_base_url: &str, fx: &IdentityFixtures) -> AgentCard {
    // The default card always advertises the aia-identity extension with
    // params.did = serverDid (required: false — degradation contract):
    // cwt-auth-success clients read the aud from here, vanilla peers ignore it.
    let mut params: HashMap<String, Value> = HashMap::new();
    params.insert("did".to_string(), json!(fx.server_did));
    params.insert("issuer".to_string(), json!(fx.iss));
    let mut card = AgentCard {
        name: "Rust Conformance Harness".to_string(),
        description: "Scripted a2a-rs server harness.".to_string(),
        version: "0.1.0".to_string(),
        supported_interfaces: vec![AgentInterface::new(
            public_base_url,
            TRANSPORT_PROTOCOL_JSONRPC,
        )],
        capabilities: AgentCapabilities {
            streaming: Some(true),
            extensions: Some(vec![
                AgentExtension {
                    uri: EXTENSION_URI.to_string(),
                    description: None,
                    required: Some(false),
                    params: Some(params),
                },
                // tdf-parts-v1 advertisement (required: false — degradation
                // contract): schemes/kas/gateway per §1.
                AgentExtension {
                    uri: tdfsdk::EXTENSION_URI.to_string(),
                    description: None,
                    required: Some(false),
                    params: Some(tdf_extension_params()),
                },
            ]),
            ..AgentCapabilities::default()
        },
        default_input_modes: vec!["text/plain".to_string()],
        default_output_modes: vec!["text/plain".to_string()],
        skills: vec![],
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: None,
        security_requirements: None,
        signatures: None,
    };
    advertise_ws_interface(&mut card, ws_base_url);
    card
}

// ---------------------------------------------------------------------------
// Control API
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CtrlState {
    scenarios: Arc<HashMap<String, ScenarioFile>>,
    state: SharedState,
    card: Arc<RwLock<AgentCard>>,
    default_card: AgentCard,
    public_base_url: String,
    /// ws://127.0.0.1:<wsport>/ — the real WS endpoint advertised on every
    /// served card so ws/* and transport-equivalence/* clients select it.
    ws_base_url: String,
    ext: Arc<ExtState>,
    /// Test DEKs for the arkavo/tdf/* scenarios (TDF-HARNESS.md).
    tdf_fx: Arc<tdf::TdfFixtures>,
}

#[derive(Deserialize)]
struct SelectBody {
    scenario: String,
}

async fn handle_select(State(ctrl): State<CtrlState>, Json(body): Json<SelectBody>) -> Json<Value> {
    let Some(scenario) = ctrl.scenarios.get(&body.scenario) else {
        return Json(json!({"ok": false, "reason": format!("unknown scenario: {}", body.scenario)}));
    };

    // arkavo-ext: scenario-keyed arming (IDENTITY-HARNESS.md). CWT
    // verification is enforced ONLY while an identity/cwt-* scenario is
    // selected; everything else is the vanilla pass-through path.
    let id = scenario.id.as_str();
    ctrl.ext
        .armed
        .store(id.starts_with("arkavo/identity/cwt-"), Ordering::SeqCst);
    // arkavo-ext: scenario-keyed policy arming (POLICY-HARNESS.md). The gate
    // is armed ONLY while an arkavo/policy/* scenario is selected; the TM-001
    // deny-all-task-ops rule additionally only for gate-deny-task-op-32099.
    ctrl.ext
        .policy_armed
        .store(id.starts_with("arkavo/policy/"), Ordering::SeqCst);
    ctrl.ext.policy_tm_deny.store(
        id == "arkavo/policy/gate-deny-task-op-32099",
        Ordering::SeqCst,
    );
    let raw_card = if id == "arkavo/identity/card-signature-valid"
        || id == "arkavo/identity/card-signature-tampered"
    {
        match build_signed_card(scenario, &ctrl.public_base_url, &ctrl.ext) {
            Ok(bytes) => Some(bytes),
            Err(reason) => {
                eprintln!("server-harness: cannot sign card for {id}: {reason}");
                return Json(json!({"ok": false, "reason": reason}));
            }
        }
    } else {
        None
    };
    *ctrl.ext.raw_card.write().unwrap() = raw_card;

    if id == "arkavo/identity/cwt-expired"
        || id == "arkavo/identity/cwt-wrong-audience"
        || id == "arkavo/policy/gate-deny-task-op-32099"
    {
        // Scenarios that script nothing: the request must die before the
        // scripted handler (401 at the HTTP layer for the cwt pair; −32099
        // at the armed gate for the policy task-op denial), so the SDK
        // handler stays unscripted and the default card is served.
        {
            let mut state = ctrl.state.lock().unwrap();
            state.scripted = Scripted::None;
            state.observed = None;
        }
        *ctrl.card.write().unwrap() = ctrl.default_card.clone();
        return Json(json!({"ok": true}));
    }

    // arkavo-ext: scenario-keyed TDF arming (TDF-HARNESS.md). The harness
    // encrypts/serves the part(s); the default card (advertising the tdf
    // extension) is served.
    if id.starts_with("arkavo/tdf/") {
        match arm_tdf(scenario, &ctrl.public_base_url, &ctrl.tdf_fx, &ctrl.ext) {
            Ok(scripted) => {
                {
                    let mut state = ctrl.state.lock().unwrap();
                    state.scripted = scripted;
                    state.observed = None;
                }
                let mut served = ctrl.default_card.clone();
                advertise_ws_interface(&mut served, &ctrl.ws_base_url);
                *ctrl.card.write().unwrap() = served;
                return Json(json!({"ok": true}));
            }
            Err(reason) => {
                eprintln!("server-harness: cannot serve tdf {id}: {reason}");
                return Json(json!({"ok": false, "reason": reason}));
            }
        }
    }

    match arm(scenario, &ctrl.public_base_url) {
        Ok((scripted, card)) => {
            {
                let mut state = ctrl.state.lock().unwrap();
                state.scripted = scripted;
                state.observed = None;
            }
            // Every served card advertises BOTH interfaces (ws-binding-v1 §1).
            // The default card already carries the WS interface; a scripted
            // per-scenario card gets it appended here against the real ws port.
            let mut served = card.unwrap_or_else(|| ctrl.default_card.clone());
            advertise_ws_interface(&mut served, &ctrl.ws_base_url);
            *ctrl.card.write().unwrap() = served;
            Json(json!({"ok": true}))
        }
        Err(reason) => {
            eprintln!("server-harness: cannot serve {}: {}", body.scenario, reason);
            Json(json!({"ok": false, "reason": reason}))
        }
    }
}

async fn handle_observed(State(ctrl): State<CtrlState>) -> Json<Value> {
    let observed = ctrl.state.lock().unwrap().observed.clone();
    Json(json!({"params": observed}))
}

/// GET /b3/<hex>: serve the shape-(b) ciphertext blob the harness wrote
/// payload-first (tdf-parts-v1 §3). 404 if the blob is unknown (a sender
/// conformance failure would be a 404 race, which payload-first ordering
/// prevents). mediaType is the pinned TDF archive type.
async fn handle_b3(
    State(ext): State<Arc<ExtState>>,
    AxumPath(hex): AxumPath<String>,
) -> axum::response::Response {
    let blob = ext.b3.lock().unwrap().blobs.get(&hex).cloned();
    match blob {
        Some(bytes) => (
            [(axum::http::header::CONTENT_TYPE, tdfsdk::TDF_MEDIA_TYPE)],
            bytes,
        )
            .into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "unknown b3 blob").into_response(),
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let mut scenarios_dir: Option<String> = None;
    let mut public_base_url: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scenarios" => scenarios_dir = args.next(),
            "--public-base-url" => public_base_url = args.next(),
            other => {
                eprintln!("server-harness: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let (Some(scenarios_dir), Some(public_base_url)) = (scenarios_dir, public_base_url) else {
        eprintln!("usage: server-harness --scenarios <dir> --public-base-url <URL>");
        std::process::exit(2);
    };

    let mut scenarios = HashMap::new();
    if let Err(e) = load_scenarios(Path::new(&scenarios_dir), &mut scenarios) {
        eprintln!("server-harness: failed to load scenarios from {scenarios_dir}: {e}");
        std::process::exit(1);
    }
    eprintln!("server-harness: loaded {} scenarios", scenarios.len());

    let fx = match IdentityFixtures::load() {
        Ok(fx) => fx,
        Err(e) => {
            eprintln!("server-harness: identity fixtures unavailable: {e}");
            std::process::exit(1);
        }
    };
    let ext = Arc::new(ExtState {
        armed: AtomicBool::new(false),
        b3: Mutex::new(B3Store::default()),
        policy_armed: AtomicBool::new(false),
        policy_tm_deny: AtomicBool::new(false),
        raw_card: RwLock::new(None),
        verifier: CwtVerifier::new(fx.issuer_verifying, fx.iss.clone(), fx.server_did.clone()),
        kid: did_key_url(fx.server_signing.verifying_key()),
        fx,
    });

    let tdf_fx = match tdf::TdfFixtures::load() {
        Ok(fx) => Arc::new(fx),
        Err(e) => {
            eprintln!("server-harness: TDF fixtures unavailable: {e}");
            std::process::exit(1);
        }
    };

    let state: SharedState = Arc::new(Mutex::new(HarnessState::default()));

    // Both dispatch paths share the one scripted state; the PolicyShim routes
    // between them per the policy_armed flag (built once, like the jsonrpc
    // router itself). The SAME handler Arc backs BOTH the HTTP jsonrpc router
    // and the WS server, so the identity/policy arming wrappers gate WS
    // exchanges exactly as they gate HTTP ones (ws-binding-v1 §3 / WS-HARNESS:
    // a policy/identity scenario could in principle run over WS too).
    let handler = Arc::new(PolicyShim {
        state: state.clone(),
        ext: ext.clone(),
        plain: ScriptedHandler { state: state.clone(), ext: ext.clone() },
        gated: GatedDispatcher::new(
            ScriptedHandler { state: state.clone(), ext: ext.clone() },
            HarnessPolicyEvaluator { ext: ext.clone() },
        ),
    });

    // Bind the WS server FIRST so the real ephemeral ws port is known before
    // any card is built (the card advertises it). The returned A2AWsServer owns
    // the accept task and aborts it on Drop, so it must live for the process —
    // it is moved into the tokio::select! below to stay alive.
    let ws_server = A2AWsServer::serve("127.0.0.1:0", handler.clone())
        .await
        .expect("bind WS port");
    let ws_port = ws_server.local_addr().port();
    let ws_base_url = format!("ws://127.0.0.1:{ws_port}/");

    let default = default_card(&public_base_url, &ws_base_url, &ext.fx);
    let card = Arc::new(RwLock::new(default.clone()));

    let producer = Arc::new(SwappableCard { card: card.clone() });
    // shape-(b) blob route (tdf-parts-v1 §3): GET /b3/<hex> serves the ciphertext
    // blob the harness wrote payload-first at /select time. The client fetches it
    // through the capture proxy (the b3 URL uses the public base), so the GET is
    // wire-observable and the proxy sees it AFTER the message POST.
    let b3_router: Router = Router::new()
        .route("/b3/{hex}", get(handle_b3))
        .with_state(ext.clone());
    let a2a_app: Router = jsonrpc_router(handler)
        .merge(agent_card_router(producer))
        .merge(b3_router)
        .layer(axum::middleware::from_fn_with_state(
            ext.clone(),
            identity_middleware,
        ));

    let ctrl_state = CtrlState {
        scenarios: Arc::new(scenarios),
        state,
        card,
        default_card: default,
        public_base_url,
        ws_base_url: ws_base_url.clone(),
        ext,
        tdf_fx,
    };
    let ctrl_app = Router::new()
        .route("/select", post(handle_select))
        .route("/observed", get(handle_observed))
        .with_state(ctrl_state);

    let a2a_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind A2A port");
    let ctrl_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind control port");
    let a2a_port = a2a_listener.local_addr().unwrap().port();
    let ctrl_port = ctrl_listener.local_addr().unwrap().port();

    let ready = json!({
        "port": a2a_port,
        "controlPort": ctrl_port,
        "baseUrl": format!("http://127.0.0.1:{a2a_port}"),
        "wsBaseUrl": ws_base_url,
    });
    println!("READY {ready}");
    std::io::stdout().flush().ok();

    // Exit on stdin EOF.
    tokio::spawn(async {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        eprintln!("server-harness: stdin EOF, exiting");
        std::process::exit(0);
    });

    tokio::select! {
        r = axum::serve(a2a_listener, a2a_app) => {
            if let Err(e) = r { eprintln!("server-harness: A2A server exited: {e}"); }
        }
        r = axum::serve(ctrl_listener, ctrl_app) => {
            if let Err(e) = r { eprintln!("server-harness: control server exited: {e}"); }
        }
    }

    // Keep the WS server alive for the whole process lifetime: dropping it
    // aborts its accept task. (Unreachable in practice — stdin EOF exits the
    // process — but this pins the lifetime explicitly for the borrow checker.)
    drop(ws_server);
}
