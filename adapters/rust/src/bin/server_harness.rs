// Server harness for the a2a-rs SDK (Linux Foundation Rust SDK).
//
// Serves the SDK's own JSON-RPC router on an ephemeral A2A port and a plain
// control API (POST /select, GET /observed) on a second ephemeral port, per
// CONTRACT.md. The scripted RequestHandler answers from the currently
// selected scenario's `server` section.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use a2a::*;
use a2a_pb::protojson_conv::{self, ProtoJsonPayload};
use a2a_server::agent_card::{AgentCardProducer, agent_card_router};
use a2a_server::jsonrpc::jsonrpc_router;
use a2a_server::{RequestHandler, ServiceParams};
use async_trait::async_trait;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::{self, BoxStream};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

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
}

impl ScriptedHandler {
    fn observe<T: ProtoJsonPayload>(&self, req: &T) {
        let value = protojson_conv::to_value(req).ok();
        self.state.lock().unwrap().observed = value;
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

fn default_card(public_base_url: &str) -> AgentCard {
    AgentCard {
        name: "Rust Conformance Harness".to_string(),
        description: "Scripted a2a-rs server harness.".to_string(),
        version: "0.1.0".to_string(),
        supported_interfaces: vec![AgentInterface::new(
            public_base_url,
            TRANSPORT_PROTOCOL_JSONRPC,
        )],
        capabilities: AgentCapabilities {
            streaming: Some(true),
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
    }
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
}

#[derive(Deserialize)]
struct SelectBody {
    scenario: String,
}

async fn handle_select(State(ctrl): State<CtrlState>, Json(body): Json<SelectBody>) -> Json<Value> {
    let Some(scenario) = ctrl.scenarios.get(&body.scenario) else {
        return Json(json!({"ok": false, "reason": format!("unknown scenario: {}", body.scenario)}));
    };

    match arm(scenario, &ctrl.public_base_url) {
        Ok((scripted, card)) => {
            {
                let mut state = ctrl.state.lock().unwrap();
                state.scripted = scripted;
                state.observed = None;
            }
            *ctrl.card.write().unwrap() = card.unwrap_or_else(|| ctrl.default_card.clone());
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

    let state: SharedState = Arc::new(Mutex::new(HarnessState::default()));
    let default = default_card(&public_base_url);
    let card = Arc::new(RwLock::new(default.clone()));

    let handler = Arc::new(ScriptedHandler { state: state.clone() });
    let producer = Arc::new(SwappableCard { card: card.clone() });
    let a2a_app: Router = jsonrpc_router(handler).merge(agent_card_router(producer));

    let ctrl_state = CtrlState {
        scenarios: Arc::new(scenarios),
        state,
        card,
        default_card: default,
        public_base_url,
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
}
