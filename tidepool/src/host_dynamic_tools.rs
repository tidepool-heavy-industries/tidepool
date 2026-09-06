//! Actor-scoped host dynamic tools served as HTTP/1.1 over a Unix socket.
//!
//! The socket directory is the authority membrane: it is owner-only, created
//! for one actor incarnation, and mounted into only that actor's interactive
//! process. Registration is immutable, and the v3 `/session` callback certifies
//! that exactly one Codex thread is durably queue-ready before any invocation
//! can be dispatched.

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use tidepool_actor::{ResidentToolEndpoint, ResidentToolError};
use tidepool_agent::{
    accept_interactive_session_binding, BackendThreadId, HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
};
use tidepool_tool::{HostedTool, ToolArguments, ToolInvocation, ToolInvocationContext};
use tokio::net::UnixListener;
use tokio::sync::Mutex;

const PROTOCOL_VERSION: u32 = HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION;
const NAMESPACE: &str = "tidepool_actor";
const REQUEST_LIMIT: usize = 4 * 1024 * 1024;
const DESCRIPTION_LIMIT: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolKind {
    Custom,
    Function,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostToolPhase {
    Serving,
    Quiescing,
    Draining,
}

/// One service's HTTP admission control, never resident-effect custody.
#[derive(Clone)]
pub(crate) struct HostToolControl {
    phase: tokio::sync::watch::Sender<HostToolPhase>,
}

impl HostToolControl {
    // Parent host integration is staged separately from this owning primitive.
    #[allow(dead_code)]
    pub(crate) fn quiesce(&self) {
        self.phase.send_modify(|phase| {
            if *phase == HostToolPhase::Serving {
                *phase = HostToolPhase::Quiescing;
            }
        });
    }

    #[allow(dead_code)]
    pub(crate) fn drain(&self) {
        self.phase.send_replace(HostToolPhase::Draining);
    }

    fn admits(&self, completion: bool) -> bool {
        // The watch read lock linearizes admission against phase publication.
        // An admitted handler may finish; this guard never spans endpoint await.
        match *self.phase.borrow() {
            HostToolPhase::Serving => true,
            HostToolPhase::Quiescing => completion,
            HostToolPhase::Draining => false,
        }
    }
}

#[derive(Clone)]
struct HostState {
    control: HostToolControl,
    registration: Arc<Registration>,
    tools: Arc<HashMap<String, ToolKind>>,
    endpoint: Arc<dyn ResidentToolEndpoint>,
    binding_path: PathBuf,
    expected_thread: Option<BackendThreadId>,
    bound_thread: Arc<Mutex<Option<BackendThreadId>>>,
}

/// Immutable actor-specific service inputs.
pub(crate) struct HostDynamicToolService {
    state: HostState,
}

impl HostDynamicToolService {
    pub(crate) fn new(
        endpoint: Arc<dyn ResidentToolEndpoint>,
        binding_path: PathBuf,
        expected_thread: Option<BackendThreadId>,
    ) -> Result<Self, String> {
        let mut identities = HashMap::new();
        let mut wire_tools = Vec::new();
        for tool in endpoint.tools() {
            let kind = match tool {
                HostedTool::Custom(tool) => {
                    wire_tools.push(NamespaceTool::Custom {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        defer_loading: false,
                    });
                    ToolKind::Custom
                }
                HostedTool::Function(tool) => {
                    wire_tools.push(NamespaceTool::Function {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: tool.input_schema.clone(),
                        defer_loading: false,
                    });
                    ToolKind::Function
                }
            };
            if identities.insert(tool.name().to_owned(), kind).is_some() {
                return Err(format!("duplicate resident tool `{}`", tool.name()));
            }
        }
        if wire_tools.is_empty() {
            return Err("resident tool registration is empty".into());
        }
        let description = endpoint
            .instructions()
            .unwrap_or("Actor-scoped Tidepool tools")
            .to_owned();
        validate_description("dynamic-tool namespace", NAMESPACE, &description)?;
        for tool in endpoint.tools() {
            validate_description("dynamic tool", tool.name(), tool.description())?;
        }
        let registration = Registration {
            protocol_version: PROTOCOL_VERSION,
            dynamic_tools: vec![DynamicTool::Namespace {
                model_only: true,
                name: NAMESPACE.into(),
                description,
                tools: wire_tools,
            }],
            scope: RegistrationScope::PrimaryThread,
        };
        Ok(Self {
            state: HostState {
                control: HostToolControl {
                    phase: tokio::sync::watch::channel(HostToolPhase::Serving).0,
                },
                registration: Arc::new(registration),
                tools: Arc::new(identities),
                endpoint,
                binding_path,
                expected_thread,
                bound_thread: Arc::new(Mutex::new(None)),
            },
        })
    }

    /// Retain this control before moving the service into its server task.
    #[allow(dead_code)]
    pub(crate) fn control(&self) -> HostToolControl {
        self.state.control.clone()
    }

    /// HTTP-only drain. Quiesce first, retain completion access until the owner
    /// has reconciled native/resident work, then drain and await this future.
    /// On timeout retain the server JoinHandle; abort is not successful drain.
    pub(crate) async fn serve(self, listener: UnixListener) -> Result<(), std::io::Error> {
        let mut phase = self.state.control.phase.subscribe();
        let shutdown = async move {
            loop {
                if *phase.borrow_and_update() == HostToolPhase::Draining {
                    return;
                }
                if phase.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        };
        let app = Router::new()
            .route("/v1/dynamic-tools/registration", get(registration))
            .route("/v1/dynamic-tools/session", post(attach_session))
            .route("/v1/dynamic-tools/call", post(call))
            .route("/v1/dynamic-tools/completed", post(completed))
            .layer(DefaultBodyLimit::max(REQUEST_LIMIT))
            .with_state(self.state);
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
    }
}

fn validate_description(kind: &str, name: &str, description: &str) -> Result<(), String> {
    if description.chars().count() <= DESCRIPTION_LIMIT {
        return Ok(());
    }
    Err(format!(
        "{kind} `{name}` description exceeds the {DESCRIPTION_LIMIT}-character provider limit"
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    protocol_version: u32,
    dynamic_tools: Vec<DynamicTool>,
    scope: RegistrationScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum RegistrationScope {
    PrimaryThread,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum DynamicTool {
    Namespace {
        #[serde(rename = "modelOnly")]
        model_only: bool,
        name: String,
        description: String,
        tools: Vec<NamespaceTool>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum NamespaceTool {
    Custom {
        name: String,
        description: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        defer_loading: bool,
    },
    Function {
        name: String,
        description: String,
        input_schema: serde_json::Value,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        defer_loading: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompletionRequest {
    protocol_version: u32,
    thread_id: String,
    context_call_id: String,
}

async fn completed(
    State(state): State<HostState>,
    Json(request): Json<CompletionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !state.control.admits(true) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "tool host is draining".into(),
        ));
    }
    if request.protocol_version != PROTOCOL_VERSION
        || request.context_call_id.is_empty()
        || request.context_call_id.len() > 256
        || state
            .bound_thread
            .lock()
            .await
            .as_ref()
            .is_none_or(|thread| thread.0 != request.thread_id)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid tool completion identity".into(),
        ));
    }
    state
        .endpoint
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary {
            thread_id: request.thread_id,
            call_id: request.context_call_id,
        })
        .await
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

async fn registration(State(state): State<HostState>) -> Result<Json<Registration>, StatusCode> {
    if !state.control.admits(true) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(Json((*state.registration).clone()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionRequest {
    protocol_version: u32,
    thread_id: String,
}

async fn attach_session(
    State(state): State<HostState>,
    Json(request): Json<SessionRequest>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    if !state.control.admits(false) {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "tool host is quiescing"));
    }
    let thread = parse_thread(request.thread_id)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid thread id"))?;
    if state
        .expected_thread
        .as_ref()
        .is_some_and(|expected| expected != &thread)
    {
        return Err((
            StatusCode::CONFLICT,
            "thread does not match retained binding",
        ));
    }

    let mut bound = state.bound_thread.lock().await;
    match bound.as_ref() {
        // Repeated callbacks still enter the acceptance owner below so an
        // unsupported protocol version cannot inherit an earlier acceptance.
        Some(existing) if existing == &thread => {}
        Some(_) => return Err((StatusCode::CONFLICT, "actor is already bound")),
        None => {}
    }
    accept_interactive_session_binding(
        &state.binding_path,
        request.protocol_version,
        thread.clone(),
    )
    .await
    .map_err(|error| match &error {
        tidepool_agent::AgentBackendError::ProtocolRejected { .. } => {
            tracing::warn!(%error, "rejected host dynamic-tool session binding");
            (StatusCode::BAD_REQUEST, "unsupported protocol version")
        }
        _ => {
            tracing::error!(%error, "failed to persist host dynamic-tool session binding");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not persist session binding",
            )
        }
    })?;
    if bound.is_some() {
        state.endpoint.reattach_boxed().await.map_err(|error| {
            tracing::error!(%error, "could not settle queued forks on reattachment");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not settle queued forks",
            )
        })?;
    }
    *bound = Some(thread);
    Ok(StatusCode::NO_CONTENT)
}

fn parse_thread(raw: String) -> Result<BackendThreadId, uuid::Error> {
    uuid::Uuid::parse_str(&raw)?;
    Ok(BackendThreadId(raw))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CallRequest {
    context_call_id: Option<String>,
    protocol_version: u32,
    thread_id: String,
    turn_id: String,
    call_id: String,
    namespace: Option<String>,
    tool: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CallResponse {
    content_items: Vec<CallContent>,
    success: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum CallContent {
    InputText { text: String },
}

impl CallResponse {
    fn domain(kind: ToolKind, value: serde_json::Value) -> Self {
        let text = match kind {
            ToolKind::Custom => workbench_transcript(&value).unwrap_or_else(|| serialize(value)),
            ToolKind::Function => serialize(value),
        };
        Self {
            content_items: vec![CallContent::InputText { text }],
            success: true,
        }
    }

    fn failure(error: &HostToolFailure) -> Self {
        Self {
            content_items: vec![CallContent::InputText {
                text: error.to_string(),
            }],
            success: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum HostToolFailure {
    #[error("tool host is quiescing")]
    Quiescing,
    #[error("unsupported dynamic-tool protocol version {actual}; expected {expected}")]
    UnsupportedProtocol { expected: u32, actual: u32 },
    #[error("dynamic-tool namespace mismatch: received {actual:?}; expected {expected:?}")]
    NamespaceMismatch {
        expected: &'static str,
        actual: Option<String>,
    },
    #[error("unknown actor-scoped tool `{0}`")]
    UnknownTool(String),
    #[error("dynamic-tool call thread {actual:?} does not match bound thread {expected:?}")]
    ThreadMismatch {
        expected: Option<String>,
        actual: String,
    },
    #[error("tool `{tool}` expected {expected} arguments, received {actual}")]
    ArgumentKindMismatch {
        tool: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error(transparent)]
    Dispatch(#[from] ResidentToolError),
    #[error("resident tool dispatch panicked before returning its future: {0}")]
    PanicBeforeFuture(String),
    #[error("resident tool dispatch panicked while polling its future: {0}")]
    PanicInFuture(String),
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn serialize(value: serde_json::Value) -> String {
    serde_json::to_string(&value).unwrap_or_else(|_| "{\"status\":\"rejected\"}".into())
}

fn workbench_transcript(value: &serde_json::Value) -> Option<String> {
    let response = value.as_object()?;
    let status = response.get("status")?.as_str()?;
    let next_index = response.get("nextIndex")?.as_u64()?;
    let total = response.get("total")?.as_u64()?;
    let items = response.get("items")?.as_array()?;
    let mut transcript = String::new();
    for item in items {
        let item = item.as_object()?;
        item.get("index")?.as_u64()?;
        item.get("status")?.as_str()?;
        let output = item.get("output")?.as_str()?;
        if !transcript.is_empty() && !transcript.ends_with('\n') && !output.is_empty() {
            transcript.push('\n');
        }
        transcript.push_str(output);
    }
    let processed = match status {
        "completed" => next_index,
        "rejected" => {
            items
                .last()
                .and_then(|item| item.get("index"))
                .and_then(serde_json::Value::as_u64)?
                + 1
        }
        _ => total,
    };
    let not_run = total.saturating_sub(processed);
    if not_run > 0 {
        if !transcript.is_empty() && !transcript.ends_with('\n') {
            transcript.push('\n');
        }
        match status {
            "rejected" => transcript.push_str(&format!(
                "[stopped after GHCi input unit {processed} of {total}; {not_run} not run]"
            )),
            "completed" => transcript.push_str(&format!(
                "[actor completed after GHCi input unit {processed} of {total}; {not_run} not run]"
            )),
            _ => {}
        }
    }
    Some(transcript)
}

async fn call(
    State(state): State<HostState>,
    Json(request): Json<CallRequest>,
) -> Json<CallResponse> {
    if !state.control.admits(false) {
        return Json(CallResponse::failure(&HostToolFailure::Quiescing));
    }
    if request.protocol_version != PROTOCOL_VERSION {
        return Json(CallResponse::failure(
            &HostToolFailure::UnsupportedProtocol {
                expected: PROTOCOL_VERSION,
                actual: request.protocol_version,
            },
        ));
    }
    if request.namespace.as_deref() != Some(NAMESPACE) {
        return Json(CallResponse::failure(&HostToolFailure::NamespaceMismatch {
            expected: NAMESPACE,
            actual: request.namespace,
        }));
    }
    let Some(kind) = state.tools.get(&request.tool).copied() else {
        return Json(CallResponse::failure(&HostToolFailure::UnknownTool(
            request.tool,
        )));
    };
    let bound = state.bound_thread.lock().await.clone();
    if bound.as_ref().map(|thread| thread.0.as_str()) != Some(request.thread_id.as_str()) {
        return Json(CallResponse::failure(&HostToolFailure::ThreadMismatch {
            expected: bound.map(|thread| thread.0),
            actual: request.thread_id,
        }));
    }
    let actual_argument_kind = json_kind(&request.arguments);
    let arguments = match (kind, request.arguments) {
        (ToolKind::Custom, serde_json::Value::String(source)) => ToolArguments::Raw(source),
        (ToolKind::Function, value @ serde_json::Value::Object(_)) => {
            ToolArguments::Structured(value)
        }
        _ => {
            let expected = match kind {
                ToolKind::Custom => "string",
                ToolKind::Function => "object",
            };
            return Json(CallResponse::failure(
                &HostToolFailure::ArgumentKindMismatch {
                    tool: request.tool,
                    expected,
                    actual: actual_argument_kind,
                },
            ));
        }
    };
    let invocation = ToolInvocation {
        context: Some(ToolInvocationContext {
            context_call_id: request.context_call_id.clone(),
            thread_id: request.thread_id.clone(),
            turn_id: request.turn_id.clone(),
            call_id: request.call_id.clone(),
            namespace: request.namespace.clone(),
        }),
        name: request.tool,
        arguments,
    };
    let future = std::panic::catch_unwind(AssertUnwindSafe(|| {
        state.endpoint.dispatch_boxed(invocation)
    }));
    let result = match future {
        Ok(future) => AssertUnwindSafe(future).catch_unwind().await,
        Err(payload) => {
            let failure = HostToolFailure::PanicBeforeFuture(
                tidepool_runtime::panic_payload_message(payload),
            );
            tracing::error!(
                turn_id = %request.turn_id,
                call_id = %request.call_id,
                error = %failure,
                "resident tool dispatch panicked before returning its future"
            );
            return Json(CallResponse::failure(&failure));
        }
    };
    match result {
        Ok(Ok(value)) => Json(CallResponse::domain(kind, value)),
        Ok(Err(error)) => {
            let failure = HostToolFailure::Dispatch(error);
            tracing::error!(
                turn_id = %request.turn_id,
                call_id = %request.call_id,
                error = %failure,
                "resident tool dispatch failed"
            );
            Json(CallResponse::failure(&failure))
        }
        Err(payload) => {
            let failure =
                HostToolFailure::PanicInFuture(tidepool_runtime::panic_payload_message(payload));
            tracing::error!(
                turn_id = %request.turn_id,
                call_id = %request.call_id,
                error = %failure,
                "resident tool dispatch panicked"
            );
            Json(CallResponse::failure(&failure))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_actor::ResidentToolFuture;
    use tidepool_tool::{CustomToolDeclaration, ToolDeclaration};

    struct EchoEndpoint {
        tools: Vec<HostedTool>,
    }

    impl ResidentToolEndpoint for EchoEndpoint {
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn instructions(&self) -> Option<&str> {
            Some("raw Haskell")
        }

        fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
            Box::pin(async move {
                let context = invocation.context;
                match invocation.arguments {
                    ToolArguments::Raw(source) => Ok(serde_json::json!({
                        "source": source,
                        "threadId": context.as_ref().map(|value| &value.thread_id),
                        "turnId": context.as_ref().map(|value| &value.turn_id),
                        "callId": context.as_ref().map(|value| &value.call_id),
                        "contextCallId": context.as_ref().and_then(|value| value.context_call_id.as_ref()),
                        "namespace": context.as_ref().and_then(|value| value.namespace.as_ref()),
                    })),
                    ToolArguments::Structured(_) => Err(ResidentToolError::InvalidInvocation(
                        "unexpected structured call".into(),
                    )),
                }
            })
        }
    }

    fn endpoint() -> Arc<dyn ResidentToolEndpoint> {
        Arc::new(EchoEndpoint {
            tools: vec![HostedTool::Custom(CustomToolDeclaration {
                name: "haskell".into(),
                description: "Run Haskell".into(),
            })],
        })
    }

    #[derive(Clone, Copy)]
    enum ControlledOutcome {
        Reject,
        Error,
        PanicBeforeFuture,
        PanicInFuture,
    }

    struct ControlledEndpoint {
        tools: Vec<HostedTool>,
        outcome: ControlledOutcome,
    }

    impl ResidentToolEndpoint for ControlledEndpoint {
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, _invocation: ToolInvocation) -> ResidentToolFuture {
            match self.outcome {
                ControlledOutcome::Reject => {
                    Box::pin(async { Ok(serde_json::json!({"status": "rejected"})) })
                }
                ControlledOutcome::Error => Box::pin(async {
                    Err(ResidentToolError::InvalidInvocation(
                        "controlled dispatch failure".into(),
                    ))
                }),
                ControlledOutcome::PanicBeforeFuture => panic!("panic before future"),
                ControlledOutcome::PanicInFuture => {
                    Box::pin(async { panic!("panic while polling future") })
                }
            }
        }
    }

    fn controlled_endpoint(outcome: ControlledOutcome) -> Arc<dyn ResidentToolEndpoint> {
        Arc::new(ControlledEndpoint {
            tools: vec![HostedTool::Custom(CustomToolDeclaration {
                name: "haskell".into(),
                description: "Run Haskell".into(),
            })],
            outcome,
        })
    }

    async fn attached_state(endpoint: Arc<dyn ResidentToolEndpoint>) -> HostState {
        let dir = tempfile::tempdir().unwrap();
        let state = HostDynamicToolService::new(endpoint, dir.path().join("binding"), None)
            .unwrap()
            .state;
        attach_session(
            State(state.clone()),
            Json(SessionRequest {
                protocol_version: PROTOCOL_VERSION,
                thread_id: "01a05a16-97f5-7722-aa8d-467e01e2e5b4".into(),
            }),
        )
        .await
        .unwrap();
        state
    }

    async fn call_haskell(state: HostState, arguments: serde_json::Value) -> CallResponse {
        call(State(state), Json(call_request(arguments))).await.0
    }

    fn call_request(arguments: serde_json::Value) -> CallRequest {
        CallRequest {
            context_call_id: None,
            protocol_version: PROTOCOL_VERSION,
            thread_id: "01a05a16-97f5-7722-aa8d-467e01e2e5b4".into(),
            turn_id: "turn".into(),
            call_id: "call".into(),
            namespace: Some(NAMESPACE.into()),
            tool: "haskell".into(),
            arguments,
        }
    }

    #[test]
    fn registration_is_namespaced_and_custom() {
        let service = HostDynamicToolService::new(endpoint(), "/tmp/binding".into(), None).unwrap();
        let value = serde_json::to_value(&*service.state.registration).unwrap();
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["scope"], "primaryThread");
        assert_eq!(value["dynamicTools"][0]["type"], "namespace");
        assert_eq!(value["dynamicTools"][0]["name"], NAMESPACE);
        assert_eq!(value["dynamicTools"][0]["modelOnly"], true);
        assert_eq!(value["dynamicTools"][0]["tools"][0]["type"], "custom");
        assert_eq!(value["dynamicTools"][0]["tools"][0]["name"], "haskell");
    }

    #[test]
    fn provider_description_limit_is_checked_before_launch() {
        let boundary = "x".repeat(DESCRIPTION_LIMIT);
        assert!(validate_description("dynamic tool", "boundary", &boundary).is_ok());

        let too_long = "x".repeat(DESCRIPTION_LIMIT + 1);
        assert_eq!(
            validate_description("dynamic tool", "oversized", &too_long).unwrap_err(),
            "dynamic tool `oversized` description exceeds the 1024-character provider limit"
        );
    }

    #[test]
    fn custom_workbench_receipt_projects_only_ghci_output() {
        let response = CallResponse::domain(
            ToolKind::Custom,
            serde_json::json!({
                "items": [{
                    "index": 0,
                    "output": "response :: Response ReviewReport",
                    "status": "committed"
                }, {
                    "index": 1,
                    "output": "readiness :: Watch ReviewReport",
                    "status": "committed"
                }],
                "nextIndex": 2,
                "status": "committed",
                "total": 2
            }),
        );
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "response :: Response ReviewReport\nreadiness :: Watch ReviewReport"
        );
    }

    #[test]
    fn custom_workbench_receipt_marks_unrun_suffix_compactly() {
        let response = CallResponse::domain(
            ToolKind::Custom,
            serde_json::json!({
                "items": [{
                    "index": 0,
                    "output": "Not in scope: `missing`",
                    "status": "rejected"
                }],
                "nextIndex": 0,
                "status": "rejected",
                "total": 3
            }),
        );
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "Not in scope: `missing`\n[stopped after GHCi input unit 1 of 3; 2 not run]"
        );
    }

    #[tokio::test]
    async fn raw_custom_source_survives_json_decoding_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let state = HostDynamicToolService::new(endpoint(), dir.path().join("binding"), None)
            .unwrap()
            .state;
        let thread = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
        attach_session(
            State(state.clone()),
            Json(SessionRequest {
                protocol_version: PROTOCOL_VERSION,
                thread_id: thread.into(),
            }),
        )
        .await
        .unwrap();
        let source = "let quoted = \"{\\\"json\\\":true}\\\\λ\"\n    in quoted";
        let response = call(
            State(state),
            Json(CallRequest {
                context_call_id: None,
                protocol_version: PROTOCOL_VERSION,
                thread_id: thread.into(),
                turn_id: "turn".into(),
                call_id: "call".into(),
                namespace: Some(NAMESPACE.into()),
                tool: "haskell".into(),
                arguments: serde_json::Value::String(source.into()),
            }),
        )
        .await;
        assert!(response.0.success);
        let CallContent::InputText { text } = &response.0.content_items[0];
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text).unwrap()["source"],
            source
        );
    }

    #[tokio::test]
    async fn kind_mismatch_and_handler_panics_fail_closed() {
        let function = Arc::new(EchoEndpoint {
            tools: vec![HostedTool::Function(ToolDeclaration {
                name: "haskell".into(),
                description: "Structured tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                kind: tidepool_tool::ToolKind::Call,
            })],
        });
        let mismatch = call_haskell(
            attached_state(function).await,
            serde_json::Value::String("pure ()".into()),
        )
        .await;
        assert!(!mismatch.success);
        let CallContent::InputText { text } = &mismatch.content_items[0];
        assert_eq!(
            text,
            "tool `haskell` expected object arguments, received string"
        );

        let dispatch_error = call_haskell(
            attached_state(controlled_endpoint(ControlledOutcome::Error)).await,
            serde_json::Value::String("pure ()".into()),
        )
        .await;
        assert!(!dispatch_error.success);
        let CallContent::InputText { text } = &dispatch_error.content_items[0];
        assert_eq!(
            text,
            "invalid resident tool invocation: controlled dispatch failure"
        );

        for (outcome, expected) in [
            (
                ControlledOutcome::PanicBeforeFuture,
                "resident tool dispatch panicked before returning its future: panic before future",
            ),
            (
                ControlledOutcome::PanicInFuture,
                "resident tool dispatch panicked while polling its future: panic while polling future",
            ),
        ] {
            let response = call_haskell(
                attached_state(controlled_endpoint(outcome)).await,
                serde_json::Value::String("pure ()".into()),
            )
            .await;
            assert!(!response.success);
            let CallContent::InputText { text } = &response.content_items[0];
            assert_eq!(text, expected);
        }
    }

    #[tokio::test]
    async fn routing_failures_name_the_rejected_boundary() {
        let state = HostDynamicToolService::new(endpoint(), "/tmp/unused-binding".into(), None)
            .unwrap()
            .state;
        let mut request = call_request(serde_json::Value::String("pure ()".into()));
        request.protocol_version = 9;
        let response = call(State(state.clone()), Json(request)).await.0;
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "unsupported dynamic-tool protocol version 9; expected 3"
        );

        let mut request = call_request(serde_json::Value::String("pure ()".into()));
        request.namespace = Some("wrong".into());
        let response = call(State(state.clone()), Json(request)).await.0;
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "dynamic-tool namespace mismatch: received Some(\"wrong\"); expected \"tidepool_actor\""
        );

        let mut request = call_request(serde_json::Value::String("pure ()".into()));
        request.tool = "missing".into();
        let response = call(State(state.clone()), Json(request)).await.0;
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(text, "unknown actor-scoped tool `missing`");

        let response = call(
            State(state),
            Json(call_request(serde_json::Value::String("pure ()".into()))),
        )
        .await
        .0;
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "dynamic-tool call thread \"01a05a16-97f5-7722-aa8d-467e01e2e5b4\" does not match bound thread None"
        );
    }

    #[tokio::test]
    async fn domain_rejection_remains_a_successful_tool_response() {
        let response = call_haskell(
            attached_state(controlled_endpoint(ControlledOutcome::Reject)).await,
            serde_json::Value::String("bad Haskell".into()),
        )
        .await;
        assert!(response.success);
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text).unwrap()["status"],
            "rejected"
        );
    }

    #[tokio::test]
    async fn uds_http_requires_session_before_exact_raw_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("host-tools.sock");
        let binding = dir.path().join("binding.json");
        let listener = UnixListener::bind(&socket).unwrap();
        let service = HostDynamicToolService::new(endpoint(), binding.clone(), None).unwrap();
        let server = tokio::spawn(service.serve(listener));
        let client = reqwest::Client::builder()
            .unix_socket(socket.clone())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .http1_only()
            .build()
            .unwrap();

        let registration: serde_json::Value = client
            .get("http://localhost/v1/dynamic-tools/registration")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(registration["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(registration["dynamicTools"][0]["name"], NAMESPACE);

        let thread = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
        let source = "let x = \"quotes \\\" and \\\\ and λ\"\n    in x";
        let request = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "threadId": thread,
            "turnId": "turn-1",
            "callId": "call-1",
            "contextCallId": "outer-exec",
            "namespace": NAMESPACE,
            "tool": "haskell",
            "arguments": source,
        });
        let before: serde_json::Value = client
            .post("http://localhost/v1/dynamic-tools/call")
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(before["success"], false);

        let legacy_session = client
            .post("http://localhost/v1/dynamic-tools/session")
            .json(&serde_json::json!({"protocolVersion": 1, "threadId": thread}))
            .send()
            .await
            .unwrap();
        assert_eq!(legacy_session.status(), StatusCode::BAD_REQUEST);
        assert!(!binding.exists());

        let session = serde_json::json!({"protocolVersion": PROTOCOL_VERSION, "threadId": thread});
        for _ in 0..2 {
            assert_eq!(
                client
                    .post("http://localhost/v1/dynamic-tools/session")
                    .json(&session)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NO_CONTENT
            );
        }
        assert_eq!(
            tidepool_agent::read_interactive_binding(&binding)
                .await
                .unwrap()
                .id(),
            &BackendThreadId(thread.into())
        );

        let response: serde_json::Value = client
            .post("http://localhost/v1/dynamic-tools/call")
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["success"], true);
        let receipt: serde_json::Value =
            serde_json::from_str(response["contentItems"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(receipt["source"], source);
        assert_eq!(receipt["threadId"], thread);
        assert_eq!(receipt["turnId"], "turn-1");
        assert_eq!(receipt["callId"], "call-1");
        assert_eq!(receipt["contextCallId"], "outer-exec");
        assert_eq!(receipt["namespace"], NAMESPACE);

        let conflict = client
            .post("http://localhost/v1/dynamic-tools/session")
            .json(&serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "threadId": "019fe92a-1a66-7820-9481-c0a2d108aba1"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);

        server.abort();
        let _ = server.await;
    }
}
