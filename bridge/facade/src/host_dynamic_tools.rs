//! Actor-scoped host dynamic tools served as HTTP/1.1 over a Unix socket.
//!
//! The socket directory is the authority membrane: it is owner-only, created
//! for one actor incarnation, and mounted into only that actor's interactive
//! process. Registration is immutable, and the v4 `/session` callback certifies
//! that exactly one Codex thread is durably queue-ready before any invocation
//! can be dispatched.

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use exomonad_actor::{
    ResidentToolEndpoint, ResidentToolError, WorkbenchBoundaryReconciliation,
    WorkbenchCancellationOutcome,
};
use exomonad_agent::backend::codex::dynamic_tools::DynamicToolFunctionSpec;
use exomonad_agent::{
    accept_interactive_session_binding, BackendThreadId, InteractiveSessionBinding,
    HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
};
use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation, ToolInvocationContext};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Mutex;

mod operation_journal;

pub(crate) fn validate_operation_recovery(path: PathBuf) -> Result<(), String> {
    operation_journal::OperationJournal::open_existing(path)
        .map(|_| ())
        .map_err(|error| format!("hosted-operation recovery evidence is unavailable: {error}"))
}

const PROTOCOL_VERSION: u32 = HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION;
const REQUEST_LIMIT: usize = 4 * 1024 * 1024;
const DESCRIPTION_LIMIT: usize = 1024;
pub(crate) const MODEL_OUTPUT_LIMIT: usize = 64 * 1024;

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

#[derive(Clone, Copy)]
enum AdmissionKind {
    NewWork,
    CompletionOrRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostBoundaryState {
    Active,
    Reconciling,
    Pending,
    Recoverable,
    Settled,
}

/// One service's HTTP admission control, never resident-effect custody.
#[derive(Clone)]
pub(crate) struct HostToolControl {
    bound_thread: Arc<Mutex<Option<BackendThreadId>>>,
    challenged_binding: Arc<Mutex<Option<InteractiveSessionBinding>>>,
    phase: tokio::sync::watch::Sender<HostToolPhase>,
    endpoint: Arc<dyn ResidentToolEndpoint>,
}

pub(crate) type HostToolSealFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<exomonad_actor::HostedWorkSeal, HostToolSealError>>
            + Send,
    >,
>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HostToolSealError {
    #[error("HTTP tool service is already draining")]
    AlreadyDraining,
    #[error(transparent)]
    Endpoint(#[from] ResidentToolError),
    #[error("resident seal belongs to {actual:?}, expected {expected:?}")]
    ForeignActor {
        expected: exomonad_actor::ActorRef,
        actual: exomonad_actor::ActorRef,
    },
}

impl HostToolControl {
    pub(crate) fn session_attached(&self) -> bool {
        self.bound_thread
            .try_lock()
            .is_ok_and(|bound| bound.is_some())
    }

    pub(crate) fn challenged_binding(&self) -> Option<InteractiveSessionBinding> {
        self.challenged_binding
            .try_lock()
            .ok()
            .and_then(|binding| binding.clone())
    }

    /// Immediately quiesce HTTP admission, then return the exact endpoint barrier
    /// future. The host must retain this future (or its owned task/result) across
    /// bounded waits; dropping it is uncertainty, not cancellation or permission
    /// to retry. Already-Draining admission is rejected without invoking the
    /// endpoint. The host must separately serialize raw drain against pending
    /// seals/completions; this operation does not reserve completion access.
    /// A seal is not cleanup.
    pub(crate) fn quiesce_and_seal(
        &self,
        expected: exomonad_actor::ActorRef,
    ) -> HostToolSealFuture {
        let mut already_draining = false;
        self.phase.send_modify(|phase| match phase {
            HostToolPhase::Draining => already_draining = true,
            HostToolPhase::Serving | HostToolPhase::Quiescing => {
                *phase = HostToolPhase::Quiescing;
            }
        });
        if already_draining {
            return Box::pin(async { Err(HostToolSealError::AlreadyDraining) });
        }
        let endpoint = Arc::clone(&self.endpoint);
        Box::pin(async move {
            let seal = endpoint.seal_hosted_work_boxed().await?;
            if seal.actor() != expected {
                return Err(HostToolSealError::ForeignActor {
                    expected,
                    actual: seal.actor(),
                });
            }
            Ok(seal)
        })
    }

    pub(crate) fn quiesce(&self) {
        self.phase.send_modify(|phase| {
            if *phase == HostToolPhase::Serving {
                *phase = HostToolPhase::Quiescing;
            }
        });
    }

    pub(crate) fn drain(&self) {
        self.phase.send_replace(HostToolPhase::Draining);
    }

    fn admits(&self, kind: AdmissionKind) -> bool {
        // The watch read lock linearizes admission against phase publication.
        // An admitted handler may finish; this guard never spans endpoint await.
        match *self.phase.borrow() {
            HostToolPhase::Serving => true,
            HostToolPhase::Quiescing => matches!(kind, AdmissionKind::CompletionOrRead),
            HostToolPhase::Draining => false,
        }
    }
}

#[derive(Clone)]
struct HostState {
    command_resources: Option<(
        Arc<exomonad_node::command_resources::CommandResourceClient>,
        String,
    )>,
    control: HostToolControl,
    registration: Arc<Registration>,
    tools: Arc<HashMap<String, ToolKind>>,
    endpoint: Arc<dyn ResidentToolEndpoint>,
    binding_path: PathBuf,
    expected_thread: Option<BackendThreadId>,
    boundaries: Arc<Mutex<HashMap<(String, String), HostBoundaryState>>>,
    operations: Option<Arc<parking_lot::Mutex<operation_journal::OperationJournal>>>,
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
                    wire_tools.push(DynamicTool::Custom {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        defer_loading: false,
                    });
                    ToolKind::Custom
                }
                HostedTool::Function(tool) => {
                    wire_tools.push(DynamicTool::Function(DynamicToolFunctionSpec {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input_schema: tool.input_schema.clone(),
                        defer_loading: false,
                    }));
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
        for tool in endpoint.tools() {
            validate_description("dynamic tool", tool.name(), tool.description())?;
        }
        let registration = Registration {
            protocol_version: PROTOCOL_VERSION,
            dynamic_tools: wire_tools,
            scope: codex_shoal_protocol::HostedRegistrationScope::PrimaryThread,
            input_control_socket: None,
            launch_id: uuid::Uuid::new_v4().to_string(),
            input_control_nonce: uuid::Uuid::new_v4().to_string(),
        };
        Ok(Self {
            state: HostState {
                command_resources: None,
                control: HostToolControl {
                    phase: tokio::sync::watch::channel(HostToolPhase::Serving).0,
                    bound_thread: Arc::new(Mutex::new(None)),
                    challenged_binding: Arc::new(Mutex::new(None)),
                    endpoint: Arc::clone(&endpoint),
                },
                registration: Arc::new(registration),
                tools: Arc::new(identities),
                endpoint,
                binding_path,
                expected_thread,
                boundaries: Arc::default(),
                operations: None,
            },
        })
    }

    pub(crate) fn with_operation_journal(
        mut self,
        path: PathBuf,
        require_existing: bool,
    ) -> Result<Self, String> {
        let journal = if require_existing {
            operation_journal::OperationJournal::open_existing(path)
        } else {
            operation_journal::OperationJournal::open(path)
        }
        .map_err(|error| format!("cannot open hosted-operation journal: {error}"))?;
        let mut boundaries = journal
            .uncertain_boundaries()
            .map(|boundary| {
                (
                    (boundary.thread_id, boundary.context_call_id),
                    HostBoundaryState::Pending,
                )
            })
            .collect::<HashMap<_, _>>();
        boundaries.extend(journal.settled_boundaries().map(|boundary| {
            (
                (boundary.thread_id.clone(), boundary.context_call_id.clone()),
                HostBoundaryState::Settled,
            )
        }));
        self.state.boundaries = Arc::new(Mutex::new(boundaries));
        self.state.operations = Some(Arc::new(parking_lot::Mutex::new(journal)));
        Ok(self)
    }

    pub(crate) fn with_command_resources(
        mut self,
        resources: Option<(
            Arc<exomonad_node::command_resources::CommandResourceClient>,
            String,
        )>,
    ) -> Self {
        self.state.command_resources = resources;
        self
    }

    /// Retain this control before moving the service into its server task.
    pub(crate) fn control(&self) -> HostToolControl {
        self.state.control.clone()
    }

    /// HTTP-only drain. Quiesce first, retain completion access until the owner
    /// has reconciled native/resident work, then drain and await this future.
    /// On timeout retain the server JoinHandle; abort is not successful drain.
    pub(crate) async fn serve(mut self, listener: UnixListener) -> Result<(), std::io::Error> {
        // The existing socket-directory owner retains filesystem custody.
        let endpoint = listener
            .local_addr()?
            .as_pathname()
            .map(|path| path.with_file_name("input.sock"));
        Arc::make_mut(&mut self.state.registration).input_control_socket = endpoint;
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
            .route("/v1/commands/resources", post(command_resources))
            .route("/v1/dynamic-tools/registration", get(registration))
            .route("/v1/dynamic-tools/session", post(attach_session))
            .route("/v1/dynamic-tools/call", post(call))
            .route("/v1/dynamic-tools/cancel", post(cancel_workbench))
            .route("/v1/dynamic-tools/interrupted", post(interrupted))
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

type Registration = codex_shoal_protocol::HostedRegistration<DynamicTool, PathBuf>;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum DynamicTool {
    Custom {
        name: String,
        description: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        defer_loading: bool,
    },
    Function(DynamicToolFunctionSpec),
}

type CompletionRequest = codex_shoal_protocol::HostedCompletionRequest<String>;

/// Exact native/application custody supplied by the challenged session plus
/// the complete hosted invocation coordinate. The resident owner derives its
/// opaque execution identity from this coordinate; the HTTP host never issues
/// or substitutes one.
type WorkbenchCancellationRequest = codex_shoal_protocol::HostedCancellationRequest<String>;

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum WorkbenchCancellationResponse {
    Cancelled {
        execution: tidepool_runtime::session::WorkbenchExecutionId,
        reply: CallResponse,
    },
    Expired {
        execution: tidepool_runtime::session::WorkbenchExecutionId,
        reply: CallResponse,
    },
    Unconfirmed {
        execution: tidepool_runtime::session::WorkbenchExecutionId,
    },
    NotSleeping {
        execution: tidepool_runtime::session::WorkbenchExecutionId,
    },
    UnknownEvaluation {
        execution: tidepool_runtime::session::WorkbenchExecutionId,
    },
}

impl WorkbenchCancellationResponse {
    fn from_outcome(outcome: WorkbenchCancellationOutcome) -> Self {
        match outcome {
            WorkbenchCancellationOutcome::Cancelled { execution, reply } => Self::Cancelled {
                execution,
                reply: workbench_reply(reply),
            },
            WorkbenchCancellationOutcome::Expired { execution, reply } => Self::Expired {
                execution,
                reply: workbench_reply(reply),
            },
            WorkbenchCancellationOutcome::Unconfirmed { execution } => {
                Self::Unconfirmed { execution }
            }
            WorkbenchCancellationOutcome::NotSleeping { execution } => {
                Self::NotSleeping { execution }
            }
            WorkbenchCancellationOutcome::UnknownEvaluation { execution } => {
                Self::UnknownEvaluation { execution }
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum WorkbenchInterruptionResponse {
    Pending,
    Recovered { reply: CallResponse },
    Settled,
}

impl WorkbenchInterruptionResponse {
    fn from_reconciliation(reconciliation: WorkbenchBoundaryReconciliation) -> Self {
        match reconciliation {
            WorkbenchBoundaryReconciliation::Pending => Self::Pending,
            WorkbenchBoundaryReconciliation::Recovered { reply } => Self::Recovered {
                reply: workbench_reply(reply),
            },
            WorkbenchBoundaryReconciliation::Settled => Self::Settled,
        }
    }
}

#[allow(
    clippy::expect_used,
    reason = "WorkbenchResponse serialization is infallible"
)]
fn workbench_reply(reply: exomonad_actor::KernelWorkbenchReply) -> CallResponse {
    match reply {
        Ok(response) => CallResponse::domain(
            ToolKind::Custom,
            serde_json::to_value(response).expect("WorkbenchResponse serialization is infallible"),
        ),
        Err(error) => CallResponse::failure(&HostToolFailure::Dispatch(
            ResidentToolError::Invocation(error),
        )),
    }
}

async fn cancel_workbench(
    State(state): State<HostState>,
    Json(request): Json<WorkbenchCancellationRequest>,
) -> Result<Json<WorkbenchCancellationResponse>, (StatusCode, String)> {
    if !state.control.admits(AdmissionKind::CompletionOrRead) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "tool host is draining".into(),
        ));
    }
    let invocation = validate_exact_workbench_request(&state, request).await?;
    let (thread_id, call_id) = (invocation.thread_id.clone(), invocation.call_id.clone());
    let outcome = state.endpoint.cancel_workbench_boxed(invocation).await;
    // One line per validated request. `thread_id` names the actor through its
    // binding; `run-map` reads these as hosted-call cancellations.
    match &outcome {
        Ok(outcome) => {
            let (label, execution) = match outcome {
                WorkbenchCancellationOutcome::Cancelled { execution, .. } => {
                    ("Cancelled", execution)
                }
                WorkbenchCancellationOutcome::Expired { execution, .. } => ("Expired", execution),
                WorkbenchCancellationOutcome::Unconfirmed { execution } => {
                    ("Unconfirmed", execution)
                }
                WorkbenchCancellationOutcome::NotSleeping { execution } => {
                    ("NotSleeping", execution)
                }
                WorkbenchCancellationOutcome::UnknownEvaluation { execution } => {
                    ("UnknownEvaluation", execution)
                }
            };
            tracing::info!(%thread_id, %call_id, %execution, outcome = label, "hosted workbench cancellation");
        }
        Err(error) => {
            tracing::info!(%thread_id, %call_id, outcome = "Failed", %error, "hosted workbench cancellation");
        }
    }
    outcome
        .map(WorkbenchCancellationResponse::from_outcome)
        .map(Json)
        .map_err(|error| {
            let status = match error {
                ResidentToolError::CancellationUnsupported => StatusCode::NOT_IMPLEMENTED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })
}

async fn interrupted(
    State(state): State<HostState>,
    Json(request): Json<CompletionRequest>,
) -> Result<Json<WorkbenchInterruptionResponse>, (StatusCode, String)> {
    if !state.control.admits(AdmissionKind::CompletionOrRead) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "tool host is draining".into(),
        ));
    }
    let boundary = validate_completion_boundary(&state, request).await?;
    let key = (boundary.thread_id.clone(), boundary.call_id.clone());
    let previous = {
        let mut boundaries = state.boundaries.lock().await;
        match boundaries.get(&key).copied() {
            Some(HostBoundaryState::Reconciling) => {
                return Ok(Json(WorkbenchInterruptionResponse::Pending));
            }
            Some(HostBoundaryState::Settled) => {
                return Ok(Json(WorkbenchInterruptionResponse::Settled));
            }
            previous @ (None
            | Some(
                HostBoundaryState::Active
                | HostBoundaryState::Pending
                | HostBoundaryState::Recoverable,
            )) => {
                boundaries.insert(key.clone(), HostBoundaryState::Reconciling);
                previous
            }
        }
    };
    let response = match state.endpoint.reconcile_workbench_boxed(boundary).await {
        Ok(response) => WorkbenchInterruptionResponse::from_reconciliation(response),
        Err(error) => {
            let mut boundaries = state.boundaries.lock().await;
            match previous {
                Some(previous) => {
                    boundaries.insert(key, previous);
                }
                None => {
                    boundaries.remove(&key);
                }
            }
            let status = match error {
                ResidentToolError::CancellationUnsupported => StatusCode::NOT_IMPLEMENTED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            return Err((status, error.to_string()));
        }
    };
    let state_after = match &response {
        WorkbenchInterruptionResponse::Pending => HostBoundaryState::Pending,
        WorkbenchInterruptionResponse::Recovered { .. } => HostBoundaryState::Recoverable,
        WorkbenchInterruptionResponse::Settled => HostBoundaryState::Settled,
    };
    state.boundaries.lock().await.insert(key, state_after);
    Ok(Json(response))
}

async fn validate_exact_workbench_request(
    state: &HostState,
    request: WorkbenchCancellationRequest,
) -> Result<ToolInvocationContext, (StatusCode, String)> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unsupported dynamic-tool protocol version {}; expected {PROTOCOL_VERSION}",
                request.protocol_version
            ),
        ));
    }
    if request.namespace.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid workbench cancellation namespace".into(),
        ));
    }
    let bound = state.control.bound_thread.lock().await.clone();
    if bound.as_ref().map(|thread| thread.0.as_str()) != Some(request.thread_id.as_str()) {
        return Err((
            StatusCode::CONFLICT,
            "workbench cancellation thread does not match bound thread".into(),
        ));
    }
    let challenged = state.control.challenged_binding.lock().await.clone();
    let Some(challenged) = challenged else {
        return Err((
            StatusCode::CONFLICT,
            "workbench cancellation requires a challenged native session".into(),
        ));
    };
    if challenged.launch_id != request.launch_id
        || challenged.instance_id != request.application_instance_id
        || challenged.generation.get() != request.session_generation
        || challenged.nonce != request.input_control_nonce
    {
        return Err((
            StatusCode::CONFLICT,
            "workbench cancellation session identity does not match current generation".into(),
        ));
    }
    if request.turn_id.is_empty()
        || request.call_id.is_empty()
        || request
            .context_call_id
            .as_ref()
            .is_some_and(String::is_empty)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "incomplete workbench cancellation identity".into(),
        ));
    }

    Ok(ToolInvocationContext {
        context_call_id: request.context_call_id,
        thread_id: request.thread_id,
        turn_id: request.turn_id,
        call_id: request.call_id,
        namespace: request.namespace,
    })
}

async fn completed(
    State(state): State<HostState>,
    Json(request): Json<CompletionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !state.control.admits(AdmissionKind::CompletionOrRead) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "tool host is draining".into(),
        ));
    }
    let boundary = validate_completion_boundary(&state, request).await?;
    let key = (boundary.thread_id.clone(), boundary.call_id.clone());
    let previous = {
        let mut boundaries = state.boundaries.lock().await;
        match boundaries.get(&key).copied() {
            Some(HostBoundaryState::Settled) => return Ok(Json(serde_json::Value::Null)),
            Some(
                HostBoundaryState::Active
                | HostBoundaryState::Reconciling
                | HostBoundaryState::Pending,
            ) => {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    "tool completion boundary is still settling; retry acknowledgment".into(),
                ));
            }
            previous => {
                boundaries.insert(key.clone(), HostBoundaryState::Reconciling);
                previous
            }
        }
    };
    if let Err(error) = state.endpoint.complete_boxed(boundary).await {
        let mut boundaries = state.boundaries.lock().await;
        match previous {
            Some(previous) => {
                boundaries.insert(key, previous);
            }
            None => {
                boundaries.remove(&key);
            }
        }
        return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string()));
    }
    if let Some(operations) = &state.operations {
        operations
            .lock()
            .settle_boundary(operation_journal::BoundaryKey {
                thread_id: key.0.clone(),
                context_call_id: key.1.clone(),
            })
            .map_err(|error| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("hosted-operation acknowledgment is unconfirmed: {error}"),
                )
            })?;
    }
    state
        .boundaries
        .lock()
        .await
        .insert(key, HostBoundaryState::Settled);
    Ok(Json(serde_json::Value::Null))
}

async fn validate_completion_boundary(
    state: &HostState,
    request: CompletionRequest,
) -> Result<tidepool_runtime::session::WorkbenchForkBoundary, (StatusCode, String)> {
    if request.protocol_version != PROTOCOL_VERSION
        || request.context_call_id.is_empty()
        || request.context_call_id.len() > 256
        || state
            .control
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
    Ok(tidepool_runtime::session::WorkbenchForkBoundary {
        thread_id: request.thread_id,
        call_id: request.context_call_id,
    })
}

async fn registration(State(state): State<HostState>) -> Result<Json<Registration>, StatusCode> {
    if !state.control.admits(AdmissionKind::CompletionOrRead) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(Json((*state.registration).clone()))
}

type SessionRequest = codex_shoal_protocol::HostedSessionRequest<String, PathBuf>;

async fn attach_session(
    State(state): State<HostState>,
    Json(request): Json<SessionRequest>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    if !state.control.admits(AdmissionKind::NewWork) {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "tool host is quiescing"));
    }
    if request.input_control_socket.is_some()
        && request.input_control_socket != state.registration.input_control_socket
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "input endpoint does not match actor socket",
        ));
    }
    let challenged_binding = if request.input_control_socket.is_some() {
        let generation = request
            .session_generation
            .and_then(std::num::NonZeroU64::new)
            .ok_or((StatusCode::BAD_REQUEST, "missing native session generation"))?;
        let launch_id = request
            .launch_id
            .filter(|value| value == &state.registration.launch_id)
            .ok_or((StatusCode::CONFLICT, "launch challenge does not match"))?;
        let nonce = request
            .input_control_nonce
            .filter(|value| value == &state.registration.input_control_nonce)
            .ok_or((StatusCode::CONFLICT, "input challenge does not match"))?;
        let instance_id = request
            .application_instance_id
            .filter(|value| !value.is_empty())
            .ok_or((
                StatusCode::BAD_REQUEST,
                "missing native application instance",
            ))?;
        Some(InteractiveSessionBinding {
            launch_id,
            instance_id,
            generation,
            nonce,
        })
    } else {
        None
    };
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

    let mut bound = state.control.bound_thread.lock().await;
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
        request.input_control_socket,
    )
    .await
    .map_err(|error| match &error {
        exomonad_agent::AgentBackendError::ProtocolRejected { .. } => {
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
    if let Some(binding) = challenged_binding {
        *state.control.challenged_binding.lock().await = Some(binding);
    }
    *bound = Some(thread);
    Ok(StatusCode::NO_CONTENT)
}

fn parse_thread(raw: String) -> Result<BackendThreadId, uuid::Error> {
    uuid::Uuid::parse_str(&raw)?;
    Ok(BackendThreadId(raw))
}

type CallRequest = codex_shoal_protocol::HostedCallRequest<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallResponse {
    content_items: Vec<CallContent>,
    success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum CallContent {
    InputText { text: String },
}

impl CallResponse {
    fn workbench(value: serde_json::Value) -> Self {
        Self::text(workbench_transcript(&value).unwrap_or_else(|| serialize(value)))
    }

    fn domain(kind: ToolKind, value: serde_json::Value) -> Self {
        Self::text(match (kind, value) {
            (ToolKind::Custom, serde_json::Value::String(text)) => text,
            (_, value) => serialize(value),
        })
    }

    fn text(text: String) -> Self {
        Self {
            content_items: vec![CallContent::InputText {
                text: exomonad_actor::bound_workbench_display(&text, MODEL_OUTPUT_LIMIT),
            }],
            success: true,
        }
    }

    fn failure(error: &HostToolFailure) -> Self {
        let text = match error {
            HostToolFailure::Dispatch(ResidentToolError::Invocation(
                exomonad_actor::KernelInvocationFailure::Workbench(failure),
            )) => workbench_failure_transcript(failure),
            _ => exomonad_actor::bound_workbench_display(&error.to_string(), MODEL_OUTPUT_LIMIT),
        };
        Self {
            content_items: vec![CallContent::InputText { text }],
            success: false,
        }
    }
}

fn workbench_failure_transcript(failure: &exomonad_actor::KernelWorkbenchFailure) -> String {
    let detail = exomonad_actor::bound_workbench_display(&failure.detail, 2048);
    let mut text = format!(
        "actor {:?} workbench input unit {} of {} failed: {detail}\n",
        failure.actor,
        failure.failed_index + 1,
        failure.total
    );
    // Command observations reserve 4 KiB for diagnostics. Present those
    // observations before optional operation metadata, without repeating detail.
    for receipt in &failure.receipts {
        if receipt.output.is_empty() {
            continue;
        }
        let allowance = MODEL_OUTPUT_LIMIT.saturating_sub(text.len() + 256);
        if allowance == 0 {
            text.push_str("\n[additional receipt output omitted]");
            break;
        }
        text.push_str(&exomonad_actor::bound_workbench_display(
            &receipt.output,
            allowance,
        ));
        text.push('\n');
    }
    for operation in failure
        .receipts
        .iter()
        .flat_map(|receipt| &receipt.operations)
    {
        let effect = exomonad_actor::bound_workbench_display(&operation.effect, 256);
        let line = format!(
            "operation {}:{}:{} {:?} ({effect})\n",
            operation.id.execution,
            operation.id.input_unit_index + 1,
            operation.id.effect_ordinal + 1,
            operation.disposition
        );
        if text.len() + line.len() + 128 > MODEL_OUTPUT_LIMIT {
            text.push_str("[additional operation receipts omitted]\n");
            break;
        }
        text.push_str(&line);
    }
    text
}

#[derive(Debug, thiserror::Error)]
enum HostToolFailure {
    #[error("tool host is quiescing")]
    Quiescing,
    #[error("tool completion boundary was already settled")]
    SettledBoundary,
    #[error("tool completion boundary already has an active call")]
    ActiveBoundary,
    #[error("the original hosted operation outcome is uncertain; it was not rerun")]
    OperationUncertain,
    #[error("hosted-operation durability failed: {0}")]
    OperationJournal(String),
    #[error("unsupported dynamic-tool protocol version {actual}; expected {expected}")]
    UnsupportedProtocol { expected: u32, actual: u32 },
    #[error("dynamic-tool namespace mismatch: received {actual:?}; expected {expected:?}")]
    NamespaceMismatch {
        expected: Option<&'static str>,
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

/// What a processed unit did that a reader stopped at a later failure would
/// otherwise have no way to know: it handed off the turn's terminal
/// transfer, or it installed bindings into the persistent environment.
/// `None` when the unit did neither (an ordinary committed expression, or a
/// unit not yet run).
fn unit_effect_description(item: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(transfer) = item.get("terminalTransfer").and_then(|v| v.as_str()) {
        let phrase = match transfer {
            "replyAccepted" => Some("submitted the reply"),
            "commandBackgrounded" => Some("backgrounded the command"),
            "cancellationAcknowledged" => Some("acknowledged the cancellation"),
            _ => None,
        };
        parts.extend(phrase.map(str::to_string));
    }
    if let Some(bindings) = item.get("installedBindings").and_then(|v| v.as_array()) {
        let names: Vec<&str> = bindings.iter().filter_map(|v| v.as_str()).collect();
        if !names.is_empty() {
            parts.push(format!("bound {}", names.join(", ")));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" and "))
    }
}

fn workbench_transcript(value: &serde_json::Value) -> Option<String> {
    let response = value.as_object()?;
    let status = response.get("status")?.as_str()?;
    let next_index = response.get("nextIndex")?.as_u64()?;
    let total = response.get("total")?.as_u64()?;
    let items = response.get("items")?.as_array()?;
    let mut transcript = String::new();
    let mut unit_effects: Vec<(u64, String)> = Vec::new();
    for item in items {
        let item = item.as_object()?;
        let index = item.get("index")?.as_u64()?;
        item.get("status")?.as_str()?;
        let output = item.get("output")?.as_str()?;
        if !transcript.is_empty() && !transcript.ends_with('\n') && !output.is_empty() {
            transcript.push('\n');
        }
        transcript.push_str(output);
        if let Some(effect) = unit_effect_description(item) {
            unit_effects.push((index, effect));
        }
    }
    let processed = match status {
        "completed" => next_index,
        "rejected" | "backgrounded" | "replied" => next_index.saturating_add(1).min(total),
        _ => total,
    };
    let not_run = total.saturating_sub(processed);
    if not_run > 0 {
        if !transcript.is_empty() && !transcript.ends_with('\n') {
            transcript.push('\n');
        }
        for (index, effect) in &unit_effects {
            if *index < processed {
                transcript.push_str(&format!("unit {} {effect}\n", index + 1));
            }
        }
        match status {
            "rejected" | "backgrounded" | "replied" => transcript.push_str(&format!(
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

/// The tool-call level of the run's span tree. Its identity is the model
/// provider's own call id, which is what joins a reconstructed cell back to
/// the provider transcript; `turn_id` is the coarser turn and is a field
/// here, never the identity.
#[tracing::instrument(
    name = "tool_call",
    skip_all,
    fields(
        call_id = %request.call_id,
        context_call_id = request.context_call_id.as_deref().unwrap_or(""),
        tool = %request.tool,
        thread_id = %request.thread_id,
        turn_id = %request.turn_id,
    )
)]
async fn call(
    State(state): State<HostState>,
    Json(request): Json<CallRequest>,
) -> Json<CallResponse> {
    let journal_request = request.clone();
    if !state.control.admits(AdmissionKind::NewWork) {
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
    if request.namespace.is_some() {
        return Json(CallResponse::failure(&HostToolFailure::NamespaceMismatch {
            expected: None,
            actual: request.namespace,
        }));
    }
    let Some(kind) = state.tools.get(&request.tool).copied() else {
        return Json(CallResponse::failure(&HostToolFailure::UnknownTool(
            request.tool,
        )));
    };
    let bound = state.control.bound_thread.lock().await.clone();
    if bound.as_ref().map(|thread| thread.0.as_str()) != Some(request.thread_id.as_str()) {
        return Json(CallResponse::failure(&HostToolFailure::ThreadMismatch {
            expected: bound.map(|thread| thread.0),
            actual: request.thread_id,
        }));
    }
    let actual_argument_kind = json_kind(&request.arguments);
    let arguments = match (kind, request.arguments) {
        (ToolKind::Custom, serde_json::Value::String(source)) => ToolArguments::Raw(source),
        (ToolKind::Function, value) => ToolArguments::Structured(value),
        (ToolKind::Custom, _) => {
            return Json(CallResponse::failure(
                &HostToolFailure::ArgumentKindMismatch {
                    tool: request.tool,
                    expected: "string",
                    actual: actual_argument_kind,
                },
            ));
        }
    };
    let boundary_key = request
        .context_call_id
        .as_ref()
        .map(|call_id| (request.thread_id.clone(), call_id.clone()));
    if let Some(key) = &boundary_key {
        let mut boundaries = state.boundaries.lock().await;
        match boundaries.get(key) {
            Some(HostBoundaryState::Settled) => {
                return Json(CallResponse::failure(&HostToolFailure::SettledBoundary));
            }
            Some(
                HostBoundaryState::Active
                | HostBoundaryState::Reconciling
                | HostBoundaryState::Pending
                | HostBoundaryState::Recoverable,
            ) => {
                return Json(CallResponse::failure(&HostToolFailure::ActiveBoundary));
            }
            None => {
                boundaries.insert(key.clone(), HostBoundaryState::Active);
            }
        }
    }
    if let Some(operations) = &state.operations {
        let admission = operations.lock().admit(&journal_request);
        match admission {
            Ok(operation_journal::Admission::New) => {}
            Ok(operation_journal::Admission::Known(response)) => {
                if let Some(key) = &boundary_key {
                    state.boundaries.lock().await.remove(key);
                }
                return Json(response);
            }
            Ok(operation_journal::Admission::Uncertain) => {
                if let Some(key) = &boundary_key {
                    state.boundaries.lock().await.remove(key);
                }
                return Json(CallResponse::failure(&HostToolFailure::OperationUncertain));
            }
            Err(error) => {
                if let Some(key) = &boundary_key {
                    state.boundaries.lock().await.remove(key);
                }
                return Json(CallResponse::failure(&HostToolFailure::OperationJournal(
                    error.to_string(),
                )));
            }
        }
    }
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
            if let Some(key) = &boundary_key {
                state.boundaries.lock().await.remove(key);
            }
            return Json(record_operation_response(
                &state,
                &journal_request,
                CallResponse::failure(&failure),
            ));
        }
    };
    let response = match result {
        Ok(Ok(value)) => match state.endpoint.output_format() {
            exomonad_actor::ResidentToolOutput::Value => CallResponse::domain(kind, value),
            exomonad_actor::ResidentToolOutput::Workbench => CallResponse::workbench(value),
        },
        Ok(Err(error)) => {
            let failure = HostToolFailure::Dispatch(error);
            tracing::error!(
                turn_id = %request.turn_id,
                call_id = %request.call_id,
                error = %failure,
                "resident tool dispatch failed"
            );
            CallResponse::failure(&failure)
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
            CallResponse::failure(&failure)
        }
    };
    let response = record_operation_response(&state, &journal_request, response);
    if let Some(key) = &boundary_key {
        let mut boundaries = state.boundaries.lock().await;
        if boundaries.get(key) == Some(&HostBoundaryState::Active) {
            boundaries.remove(key);
        }
    }
    Json(response)
}

fn record_operation_response(
    state: &HostState,
    request: &CallRequest,
    response: CallResponse,
) -> CallResponse {
    let Some(operations) = &state.operations else {
        return response;
    };
    match operations.lock().finish(request, &response) {
        Ok(()) => response,
        Err(error) => CallResponse::failure(&HostToolFailure::OperationJournal(format!(
            "terminal outcome is unconfirmed: {error}"
        ))),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use exomonad_actor::ResidentToolFuture;
    use exomonad_tool::{CustomToolDeclaration, ToolDeclaration};
    use std::num::NonZeroU64;
    use std::sync::Mutex as StdMutex;
    use tidepool_runtime::session::{WorkbenchExecutionId, WorkbenchResponse, WorkbenchRunStatus};

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

    pub(crate) fn endpoint() -> Arc<dyn ResidentToolEndpoint> {
        Arc::new(EchoEndpoint {
            tools: vec![HostedTool::Custom(CustomToolDeclaration {
                name: "haskell".into(),
                description: "Run Haskell".into(),
            })],
        })
    }

    struct CancellationEndpoint {
        tools: Vec<HostedTool>,
        calls: Arc<StdMutex<Vec<ToolInvocationContext>>>,
    }

    impl ResidentToolEndpoint for CancellationEndpoint {
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, _invocation: ToolInvocation) -> ResidentToolFuture {
            Box::pin(async {
                Err(ResidentToolError::InvalidInvocation(
                    "dispatch must not be used for cancellation".into(),
                ))
            })
        }

        fn cancel_workbench_boxed(
            &self,
            invocation: ToolInvocationContext,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<WorkbenchCancellationOutcome, ResidentToolError>,
                    > + Send
                    + 'static,
            >,
        > {
            self.calls.lock().unwrap().push(invocation.clone());
            Box::pin(async move {
                let execution =
                    WorkbenchExecutionId::from_digest(if invocation.call_id == "call-a" {
                        [0x0a; 16]
                    } else {
                        [0x0b; 16]
                    });
                if invocation.call_id == "call-a" {
                    Ok(WorkbenchCancellationOutcome::Cancelled {
                        execution,
                        reply: Ok(WorkbenchResponse {
                            status: WorkbenchRunStatus::RequestCancelled,
                            summary: None,
                            items: vec![],
                            next_index: 0,
                            total: 2,
                        }),
                    })
                } else {
                    Ok(WorkbenchCancellationOutcome::UnknownEvaluation { execution })
                }
            })
        }

        fn reconcile_workbench_boxed(
            &self,
            boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<WorkbenchBoundaryReconciliation, ResidentToolError>,
                    > + Send
                    + 'static,
            >,
        > {
            self.calls.lock().unwrap().push(ToolInvocationContext {
                context_call_id: Some(boundary.call_id.clone()),
                thread_id: boundary.thread_id,
                turn_id: String::new(),
                call_id: boundary.call_id.clone(),
                namespace: None,
            });
            Box::pin(async move {
                Ok(match boundary.call_id.as_str() {
                    "call-a" => WorkbenchBoundaryReconciliation::Recovered {
                        reply: Ok(WorkbenchResponse {
                            status: WorkbenchRunStatus::Committed,
                            summary: None,
                            items: vec![],
                            next_index: 1,
                            total: 1,
                        }),
                    },
                    "call-b" => WorkbenchBoundaryReconciliation::Pending,
                    _ => WorkbenchBoundaryReconciliation::Settled,
                })
            })
        }
    }

    fn cancellation_endpoint() -> (
        Arc<CancellationEndpoint>,
        Arc<StdMutex<Vec<ToolInvocationContext>>>,
    ) {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        (
            Arc::new(CancellationEndpoint {
                tools: vec![HostedTool::Custom(CustomToolDeclaration {
                    name: "haskell".into(),
                    description: "Run Haskell".into(),
                })],
                calls: Arc::clone(&calls),
            }),
            calls,
        )
    }

    async fn challenged_cancellation_state(endpoint: Arc<dyn ResidentToolEndpoint>) -> HostState {
        let state = HostDynamicToolService::new(endpoint, "/tmp/cancellation-binding".into(), None)
            .unwrap()
            .state;
        *state.control.bound_thread.lock().await = Some(BackendThreadId(
            "01a05a16-97f5-7722-aa8d-467e01e2e5b4".into(),
        ));
        *state.control.challenged_binding.lock().await = Some(InteractiveSessionBinding {
            launch_id: "launch".into(),
            instance_id: "application".into(),
            generation: NonZeroU64::new(7).unwrap(),
            nonce: "nonce".into(),
        });
        state
    }

    fn cancellation_request(call_id: &str) -> WorkbenchCancellationRequest {
        WorkbenchCancellationRequest {
            protocol_version: PROTOCOL_VERSION,
            thread_id: "01a05a16-97f5-7722-aa8d-467e01e2e5b4".into(),
            turn_id: "turn".into(),
            call_id: call_id.into(),
            context_call_id: Some("outer".into()),
            namespace: None,
            launch_id: "launch".into(),
            application_instance_id: "application".into(),
            session_generation: 7,
            input_control_nonce: "nonce".into(),
        }
    }

    #[tokio::test]
    async fn exact_cancellation_reconciles_while_quiescing_without_redispatch() {
        let (endpoint, calls) = cancellation_endpoint();
        let state = challenged_cancellation_state(endpoint).await;

        let mut stale = cancellation_request("call-a");
        stale.session_generation = 6;
        let error = cancel_workbench(State(state.clone()), Json(stale))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert!(calls.lock().unwrap().is_empty());

        state.control.quiesce();
        for (call_id, expected_status, expected_execution) in [
            (
                "call-a",
                "cancelled",
                "exec-0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
            ),
            (
                "call-b",
                "unknownEvaluation",
                "exec-0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
            ),
            (
                "call-a",
                "cancelled",
                "exec-0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
            ),
        ] {
            let response =
                cancel_workbench(State(state.clone()), Json(cancellation_request(call_id)))
                    .await
                    .unwrap();
            let response = serde_json::to_value(response.0).unwrap();
            assert_eq!(response["status"], expected_status);
            assert_eq!(response["execution"], expected_execution);
            if expected_status == "cancelled" {
                assert_eq!(response["reply"]["success"], true);
                let reply: serde_json::Value = serde_json::from_str(
                    response["reply"]["contentItems"][0]["text"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(reply["status"], "requestCancelled");
                assert_eq!(reply["items"], serde_json::json!([]));
            }
        }

        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0], calls[2]);
            assert_ne!(calls[0].call_id, calls[1].call_id);
        }

        state.control.drain();
        let error = cancel_workbench(State(state), Json(cancellation_request("call-a")))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn completion_during_interruption_is_retryable_until_settled() {
        let (endpoint, _) = cancellation_endpoint();
        let state = challenged_cancellation_state(endpoint).await;
        let thread_id = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
        let key = (thread_id.to_owned(), "call-a".to_owned());
        for boundary_state in [
            HostBoundaryState::Active,
            HostBoundaryState::Reconciling,
            HostBoundaryState::Pending,
        ] {
            state
                .boundaries
                .lock()
                .await
                .insert(key.clone(), boundary_state);
            let error = completed(
                State(state.clone()),
                Json(CompletionRequest {
                    protocol_version: PROTOCOL_VERSION,
                    thread_id: thread_id.into(),
                    context_call_id: "call-a".into(),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                state.boundaries.lock().await.get(&key),
                Some(&boundary_state)
            );
        }
        state
            .boundaries
            .lock()
            .await
            .insert(key.clone(), HostBoundaryState::Recoverable);
        let response = completed(
            State(state.clone()),
            Json(CompletionRequest {
                protocol_version: PROTOCOL_VERSION,
                thread_id: thread_id.into(),
                context_call_id: "call-a".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.0, serde_json::Value::Null);
        assert_eq!(
            state.boundaries.lock().await.get(&key),
            Some(&HostBoundaryState::Settled)
        );
    }

    #[tokio::test]
    async fn interrupted_call_returns_exact_retry_safe_receipt_while_quiescing() {
        let (endpoint, calls) = cancellation_endpoint();
        let state = challenged_cancellation_state(endpoint).await;
        state.control.quiesce();

        for (call_id, status) in [
            ("call-a", "recovered"),
            ("call-b", "pending"),
            ("call-c", "settled"),
            ("call-a", "recovered"),
            ("call-c", "settled"),
        ] {
            let response = interrupted(
                State(state.clone()),
                Json(CompletionRequest {
                    protocol_version: PROTOCOL_VERSION,
                    thread_id: "01a05a16-97f5-7722-aa8d-467e01e2e5b4".into(),
                    context_call_id: call_id.into(),
                }),
            )
            .await
            .unwrap();
            let response = serde_json::to_value(response.0).unwrap();
            assert_eq!(response["status"], status);
            if status == "recovered" {
                assert_eq!(response["reply"]["success"], true);
                let reply: serde_json::Value = serde_json::from_str(
                    response["reply"]["contentItems"][0]["text"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(reply["status"], "committed");
                assert_eq!(reply["nextIndex"], 1);
            }
        }

        {
            let observed = calls.lock().unwrap();
            assert_eq!(
                observed.len(),
                4,
                "settled receipt is served from the host tombstone"
            );
        }

        state.control.phase.send_replace(HostToolPhase::Serving);
        let mut delayed = call_request(serde_json::Value::String("pure ()".into()));
        delayed.context_call_id = Some("call-c".into());
        delayed.call_id = "late-inner-call".into();
        let response = call(State(state), Json(delayed)).await.0;
        assert!(!response.success);
        let CallContent::InputText { text } = &response.content_items[0];
        assert!(text.contains("completion boundary was already settled"));
        assert_eq!(
            calls.lock().unwrap().len(),
            4,
            "a delayed call must be rejected before endpoint dispatch"
        );
    }

    struct BlockingReconciliationEndpoint {
        tools: Vec<HostedTool>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        dispatches: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ResidentToolEndpoint for BlockingReconciliationEndpoint {
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, _invocation: ToolInvocation) -> ResidentToolFuture {
            self.dispatches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(serde_json::Value::Null) })
        }

        fn reconcile_workbench_boxed(
            &self,
            _boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<WorkbenchBoundaryReconciliation, ResidentToolError>,
                    > + Send
                    + 'static,
            >,
        > {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Ok(WorkbenchBoundaryReconciliation::Settled)
            })
        }
    }

    #[tokio::test]
    async fn reconciliation_claim_rejects_a_delayed_call_before_settlement() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let endpoint = Arc::new(BlockingReconciliationEndpoint {
            tools: vec![HostedTool::Custom(CustomToolDeclaration {
                name: "haskell".into(),
                description: "Run Haskell".into(),
            })],
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            dispatches: Arc::clone(&dispatches),
        });
        let state = HostDynamicToolService::new(endpoint, "/tmp/reconcile-binding".into(), None)
            .unwrap()
            .state;
        let thread = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
        *state.control.bound_thread.lock().await = Some(BackendThreadId(thread.into()));
        let entered_wait = entered.notified();
        let reconcile = tokio::spawn(interrupted(
            State(state.clone()),
            Json(CompletionRequest {
                protocol_version: PROTOCOL_VERSION,
                thread_id: thread.into(),
                context_call_id: "boundary".into(),
            }),
        ));
        entered_wait.await;

        let mut delayed = call_request(serde_json::Value::String("pure ()".into()));
        delayed.context_call_id = Some("boundary".into());
        let response = call(State(state), Json(delayed)).await.0;
        assert!(!response.success);
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 0);

        release.notify_one();
        let response = reconcile.await.unwrap().unwrap().0;
        assert!(matches!(response, WorkbenchInterruptionResponse::Settled));
    }

    #[tokio::test]
    async fn cancellation_rejects_foreign_protocol_thread_and_legacy_session() {
        let (endpoint, calls) = cancellation_endpoint();
        let state = challenged_cancellation_state(endpoint).await;

        let mut foreign_protocol = cancellation_request("call-a");
        foreign_protocol.protocol_version += 1;
        assert_eq!(
            cancel_workbench(State(state.clone()), Json(foreign_protocol))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );

        let mut foreign_namespace = cancellation_request("call-a");
        foreign_namespace.namespace = Some("exomonad_actor".into());
        assert_eq!(
            cancel_workbench(State(state.clone()), Json(foreign_namespace))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );

        let mut foreign_thread = cancellation_request("call-a");
        foreign_thread.thread_id = "019fe92a-1a66-7820-9481-c0a2d108aba1".into();
        assert_eq!(
            cancel_workbench(State(state.clone()), Json(foreign_thread))
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );

        *state.control.challenged_binding.lock().await = None;
        assert_eq!(
            cancel_workbench(State(state), Json(cancellation_request("call-a")))
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );
        assert!(calls.lock().unwrap().is_empty());
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
                input_control_socket: None,
                launch_id: None,
                application_instance_id: None,
                session_generation: None,
                input_control_nonce: None,
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
            namespace: None,
            tool: "haskell".into(),
            arguments,
        }
    }

    struct CountingEndpoint {
        tools: Vec<HostedTool>,
        dispatches: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ResidentToolEndpoint for CountingEndpoint {
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
            self.dispatches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                let ToolArguments::Raw(source) = invocation.arguments else {
                    return Err(ResidentToolError::InvalidInvocation(
                        "expected raw source".into(),
                    ));
                };
                Ok(serde_json::json!({ "source": source }))
            })
        }
    }

    fn counting_endpoint(
        dispatches: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Arc<dyn ResidentToolEndpoint> {
        Arc::new(CountingEndpoint {
            tools: vec![HostedTool::Custom(CustomToolDeclaration {
                name: "haskell".into(),
                description: "Run Haskell".into(),
            })],
            dispatches,
        })
    }

    async fn bind_test_thread(state: &HostState) {
        *state.control.bound_thread.lock().await = Some(BackendThreadId(
            "01a05a16-97f5-7722-aa8d-467e01e2e5b4".into(),
        ));
    }

    #[tokio::test]
    async fn restart_returns_durable_outcome_without_redispatch() {
        let directory = tempfile::tempdir().unwrap();
        let journal = directory.path().join("operations.jsonl");
        let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first = HostDynamicToolService::new(
            counting_endpoint(Arc::clone(&dispatches)),
            directory.path().join("binding-a"),
            None,
        )
        .unwrap()
        .with_operation_journal(journal.clone(), false)
        .unwrap()
        .state;
        bind_test_thread(&first).await;
        let request = call_request(serde_json::Value::String("effect".into()));
        let expected = call(State(first), Json(request.clone())).await.0;
        assert!(expected.success);
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 1);

        let recovered = HostDynamicToolService::new(
            counting_endpoint(Arc::clone(&dispatches)),
            directory.path().join("binding-b"),
            None,
        )
        .unwrap()
        .with_operation_journal(journal, true)
        .unwrap()
        .state;
        bind_test_thread(&recovered).await;
        assert_eq!(call(State(recovered), Json(request)).await.0, expected);
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn restart_reports_accepted_operation_as_uncertain_without_dispatch() {
        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("operations.jsonl");
        let request = call_request(serde_json::Value::String("effect".into()));
        let mut journal = operation_journal::OperationJournal::open(journal_path.clone()).unwrap();
        assert!(matches!(
            journal.admit(&request).unwrap(),
            operation_journal::Admission::New
        ));
        drop(journal);

        let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recovered = HostDynamicToolService::new(
            counting_endpoint(Arc::clone(&dispatches)),
            directory.path().join("binding"),
            None,
        )
        .unwrap()
        .with_operation_journal(journal_path, true)
        .unwrap()
        .state;
        bind_test_thread(&recovered).await;
        let response = call(State(recovered), Json(request)).await.0;
        assert!(!response.success);
        let CallContent::InputText { text } = &response.content_items[0];
        assert!(text.contains("outcome is uncertain"));
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn restart_fences_sibling_calls_under_an_uncertain_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("operations.jsonl");
        let mut request = call_request(serde_json::Value::String("effect-a".into()));
        request.context_call_id = Some("outer-call".into());
        let mut journal = operation_journal::OperationJournal::open(journal_path.clone()).unwrap();
        assert!(matches!(
            journal.admit(&request).unwrap(),
            operation_journal::Admission::New
        ));
        drop(journal);

        let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = HostDynamicToolService::new(
            counting_endpoint(Arc::clone(&dispatches)),
            directory.path().join("binding.json"),
            None,
        )
        .unwrap()
        .with_operation_journal(journal_path, true)
        .unwrap()
        .state;
        bind_test_thread(&state).await;
        let mut sibling = call_request(serde_json::Value::String("effect-b".into()));
        sibling.context_call_id = request.context_call_id.clone();
        sibling.call_id = "sibling-call".into();
        let response = call(State(state), Json(sibling)).await.0;
        assert!(!response.success);
        let CallContent::InputText { text } = &response.content_items[0];
        assert!(text.contains("active call"));
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn registration_is_flat_and_custom() {
        let service = HostDynamicToolService::new(endpoint(), "/tmp/binding".into(), None).unwrap();
        let value = serde_json::to_value(&*service.state.registration).unwrap();
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["scope"], "primaryThread");
        assert_eq!(
            value["dynamicTools"],
            serde_json::json!([{
                "type": "custom", "name": "haskell", "description": "Run Haskell"
            }])
        );
    }

    struct WorkbenchFunctionEndpoint {
        tools: Vec<HostedTool>,
        expected_arguments: serde_json::Value,
    }

    impl ResidentToolEndpoint for WorkbenchFunctionEndpoint {
        fn tools(&self) -> &[HostedTool] {
            &self.tools
        }

        fn output_format(&self) -> exomonad_actor::ResidentToolOutput {
            exomonad_actor::ResidentToolOutput::Workbench
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
            assert_eq!(
                invocation.arguments,
                ToolArguments::Structured(self.expected_arguments.clone())
            );
            Box::pin(async {
                Ok(serde_json::json!({
                    "status": "committed",
                    "nextIndex": 1,
                    "total": 1,
                    "items": [{
                        "index": 0,
                        "status": "committed",
                        "output": ":: Int -> Int\n  id :: a -> a"
                    }]
                }))
            })
        }
    }

    #[tokio::test]
    async fn workbench_function_result_uses_endpoint_owned_text_boundary() {
        let endpoint = Arc::new(WorkbenchFunctionEndpoint {
            expected_arguments: serde_json::json!({"queries": [":: Int -> Int"]}),
            tools: vec![HostedTool::Function(ToolDeclaration {
                name: "lookup".into(),
                description: "Search current Haskell scope; returns deterministic text.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"queries": {"type": "array"}},
                    "required": ["queries"]
                }),
                output_schema: None,
                kind: exomonad_tool::ToolKind::Call,
            })],
        });
        let state = attached_state(endpoint).await;
        let mut request = call_request(serde_json::json!({"queries": [":: Int -> Int"]}));
        request.tool = "lookup".into();
        let response = call(State(state), Json(request)).await.0;
        assert!(response.success);
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(text, ":: Int -> Int\n  id :: a -> a");
    }

    #[tokio::test]
    async fn function_arguments_retain_non_object_json_for_endpoint_validation() {
        for arguments in [
            serde_json::json!("awaitSettled"),
            serde_json::json!(["awaitSettled"]),
            serde_json::Value::Null,
        ] {
            let endpoint = Arc::new(WorkbenchFunctionEndpoint {
                expected_arguments: arguments.clone(),
                tools: vec![HostedTool::Function(ToolDeclaration {
                    name: "inspect".into(),
                    description: "Inspect a value".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                    output_schema: None,
                    kind: exomonad_tool::ToolKind::Call,
                })],
            });
            let state = attached_state(endpoint).await;
            let mut request = call_request(arguments);
            request.tool = "inspect".into();
            let response = call(State(state), Json(request)).await.0;
            assert!(response.success);
            let CallContent::InputText { text } = &response.content_items[0];
            assert_eq!(text, ":: Int -> Int\n  id :: a -> a");
        }
    }

    #[test]
    fn flat_registration_rejects_cross_kind_name_collisions() {
        let duplicate = Arc::new(EchoEndpoint {
            tools: vec![
                HostedTool::Custom(CustomToolDeclaration {
                    name: "execute".into(),
                    description: "Raw".into(),
                }),
                HostedTool::Function(ToolDeclaration {
                    name: "execute".into(),
                    description: "Structured".into(),
                    input_schema: serde_json::json!({"type":"object"}),
                    output_schema: None,
                    kind: exomonad_tool::ToolKind::Call,
                }),
            ],
        });
        let error = HostDynamicToolService::new(duplicate, "/tmp/unused-binding".into(), None)
            .err()
            .expect("duplicate rejected before serving");
        assert_eq!(error, "duplicate resident tool `execute`");
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
    fn every_hosted_result_path_obeys_the_final_display_budget() {
        let huge = "λ-output\n".repeat(100_000);
        let responses = [
            CallResponse::domain(ToolKind::Custom, serde_json::json!({"large": huge})),
            CallResponse::domain(ToolKind::Function, serde_json::json!({"large": huge})),
            CallResponse::failure(&HostToolFailure::PanicInFuture(huge.clone())),
            CallResponse::workbench(serde_json::json!({
                "status": "committed", "nextIndex": 2, "total": 2,
                "items": [
                    {"index": 0, "status": "committed", "output": huge},
                    {"index": 1, "status": "committed", "output": "final evidence"}
                ]
            })),
        ];
        for response in responses {
            let CallContent::InputText { text } = &response.content_items[0];
            assert!(text.len() <= MODEL_OUTPUT_LIMIT, "{}", text.len());
            assert!(text.contains("bytes not displayed"));
        }
    }

    #[test]
    fn workbench_failure_preserves_command_output_before_large_diagnostics() {
        use tidepool_runtime::session::{
            WorkbenchExecutionId, WorkbenchItemReceipt, WorkbenchItemStatus,
            WorkbenchOperationDisposition, WorkbenchOperationId, WorkbenchOperationReceipt,
        };
        let output = format!(
            "begin-command\n{}\nmiddle-evidence\n{}\nend-command\n",
            "λ".repeat(15_000),
            "x".repeat(30_000)
        );
        let failure = exomonad_actor::KernelWorkbenchFailure {
            actor: exomonad_actor::ActorRef::first(exomonad_actor::ActorId(7)),
            failed_index: 1,
            total: 3,
            detail: "large diagnostic λ\n".repeat(20_000),
            receipts: vec![WorkbenchItemReceipt {
                index: 1,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: WorkbenchItemStatus::Rejected,
                output: output.clone(),
                diagnostics: Vec::new(),
                warnings: vec![],
                installed_bindings: vec![],
                terminal_transfer: None,
                failure_layer: None,
                operations: (0..1000)
                    .map(|effect_ordinal| WorkbenchOperationReceipt {
                        id: WorkbenchOperationId {
                            execution: WorkbenchExecutionId::from_digest([1; 16]),
                            input_unit_index: 1,
                            effect_ordinal,
                        },
                        effect: "command observation".repeat(100),
                        disposition: WorkbenchOperationDisposition::Committed,
                    })
                    .collect(),
            }],
        };
        let response =
            CallResponse::failure(&HostToolFailure::Dispatch(ResidentToolError::Invocation(
                exomonad_actor::KernelInvocationFailure::Workbench(failure),
            )));
        let CallContent::InputText { text } = &response.content_items[0];
        assert!(!response.success);
        assert!(text.len() <= MODEL_OUTPUT_LIMIT, "{}", text.len());
        assert!(
            text.contains(&output),
            "command output was clipped by diagnostics"
        );
        assert!(text.contains("input unit 2 of 3 failed"));
        assert!(text.contains("additional operation receipts omitted"));
    }

    /// A structured field that never reaches the model is worthless. This
    /// takes a receipt through the same `workbench_reply` the host serves
    /// every workbench reply through, and reads the diagnostics back out of
    /// the JSON — span, severity, and message addressable on their own, with
    /// the rendered `output` still present and unchanged beside them.
    #[test]
    fn workbench_reply_json_carries_structured_diagnostics_beside_the_rendered_output() {
        use tidepool_runtime::session::{
            WorkbenchItemReceipt, WorkbenchItemStatus, WorkbenchResponse, WorkbenchRunStatus,
        };
        use tidepool_toolchain::diag::{DiagnosticLevel, DiagnosticLocation, StructuredDiagnostic};
        let rendered = "<cell>:1:1-8: error:\n    Variable not in scope: missing";
        let reply = Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Rejected,
            summary: None,
            items: vec![WorkbenchItemReceipt {
                index: 0,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: WorkbenchItemStatus::Rejected,
                output: rendered.into(),
                diagnostics: vec![
                    StructuredDiagnostic {
                        severity: DiagnosticLevel::Error,
                        location: DiagnosticLocation::Authored {
                            label: "<cell>".into(),
                            start_line: 1,
                            start_col: 1,
                            end_line: 1,
                            end_col: 8,
                        },
                        message: "Variable not in scope: missing".into(),
                    },
                    // The unrecognised case travels too, message intact.
                    StructuredDiagnostic {
                        severity: DiagnosticLevel::Error,
                        location: DiagnosticLocation::Unlocated,
                        message: "compiler worker stderr: panic".into(),
                    },
                ],
                warnings: Vec::new(),
                installed_bindings: Vec::new(),
                operations: Vec::new(),
                terminal_transfer: None,
                failure_layer: None,
            }],
            next_index: 0,
            total: 1,
        });
        let response = workbench_reply(reply);
        let CallContent::InputText { text } = &response.content_items[0];
        let json: serde_json::Value = serde_json::from_str(text).expect("a JSON reply");
        let item = &json["items"][0];
        assert_eq!(item["output"], rendered, "the rendered text still stands");
        let diagnostics = item["diagnostics"]
            .as_array()
            .expect("diagnostics reach the model as an array");
        assert_eq!(diagnostics.len(), 2, "{json}");
        assert_eq!(diagnostics[0]["severity"], "error");
        assert_eq!(diagnostics[0]["message"], "Variable not in scope: missing");
        assert_eq!(diagnostics[0]["location"]["kind"], "authored");
        assert_eq!(diagnostics[0]["location"]["label"], "<cell>");
        assert_eq!(diagnostics[0]["location"]["startLine"], 1);
        assert_eq!(diagnostics[0]["location"]["startCol"], 1);
        assert_eq!(diagnostics[0]["location"]["endCol"], 8);
        assert_eq!(diagnostics[1]["location"]["kind"], "unlocated");
        assert_eq!(diagnostics[1]["message"], "compiler worker stderr: panic");
    }

    #[test]
    fn custom_workbench_receipt_projects_only_ghci_output() {
        let response = CallResponse::workbench(serde_json::json!({
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
        }));
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "response :: Response ReviewReport\nreadiness :: Watch ReviewReport"
        );
    }

    #[test]
    fn ordinary_domain_values_do_not_acquire_workbench_meaning_from_their_shape() {
        let value = serde_json::json!({
            "items": [{"index":0, "output":"application data", "status":"committed"}],
            "status":"committed", "nextIndex":1, "total":1,
        });
        let response = CallResponse::domain(ToolKind::Custom, value.clone());
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text).unwrap(),
            value
        );
        let response = CallResponse::domain(ToolKind::Custom, "literal λ\ntext".into());
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(text, "literal λ\ntext");
    }

    #[test]
    fn custom_workbench_receipt_marks_unrun_suffix_compactly() {
        let response = CallResponse::workbench(serde_json::json!({
            "items": [{
                "index": 0,
                "output": "Not in scope: `missing`",
                "status": "rejected"
            }],
            "nextIndex": 0,
            "status": "rejected",
            "total": 3
        }));
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "Not in scope: `missing`\n[stopped after GHCi input unit 1 of 3; 2 not run]"
        );
    }

    /// The regression from run 8a782b2b: unit 1's `respond` submitted the
    /// reply, but unit 2 then failed at check. A reader who sees only the
    /// failing unit's error, plus a bare "K not run", cannot tell whether
    /// the reply already went out and would retry into "respond not in
    /// scope". The transcript must say so before the stopped-suffix line,
    /// and a `replied`-status run that still leaves later units un-run must
    /// carry the same suffix the `rejected`/`backgrounded` statuses already
    /// get.
    #[test]
    fn custom_workbench_receipt_reports_a_processed_units_reply_before_a_later_rejection() {
        let response = CallResponse::workbench(serde_json::json!({
            "items": [{
                "index": 0,
                "output": "Reply submitted.",
                "status": "committed",
                "terminalTransfer": "replyAccepted"
            }, {
                "index": 1,
                "output": "Not in scope: `respond`",
                "status": "rejected"
            }],
            "nextIndex": 1,
            "status": "rejected",
            "total": 3
        }));
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "Reply submitted.\nNot in scope: `respond`\nunit 1 submitted the reply\n\
             [stopped after GHCi input unit 2 of 3; 1 not run]"
        );
    }

    /// A `replied` run status previously fell into the catch-all `_ =>
    /// total` branch, which reported every unit as processed even when
    /// later units never ran. A retried actor would see no suffix at all.
    #[test]
    fn custom_workbench_receipt_marks_unrun_suffix_for_a_replied_run() {
        let response = CallResponse::workbench(serde_json::json!({
            "items": [{
                "index": 0,
                "output": "Reply submitted.",
                "status": "committed",
                "terminalTransfer": "replyAccepted"
            }],
            "nextIndex": 0,
            "status": "replied",
            "total": 2
        }));
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "Reply submitted.\nunit 1 submitted the reply\n\
             [stopped after GHCi input unit 1 of 2; 1 not run]"
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
                input_control_socket: None,
                launch_id: None,
                application_instance_id: None,
                session_generation: None,
                input_control_nonce: None,
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
                namespace: None,
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

    #[test]
    fn structured_registration_uses_native_function_wire_schema() {
        let service = HostDynamicToolService::new(
            Arc::new(EchoEndpoint {
                tools: vec![HostedTool::Function(ToolDeclaration {
                    name: "execute".into(),
                    description: "Run a command".into(),
                    input_schema: serde_json::json!({"type":"object"}),
                    output_schema: None,
                    kind: exomonad_tool::ToolKind::Call,
                })],
            }),
            PathBuf::from("unused-binding"),
            None,
        )
        .unwrap();
        let registration = serde_json::to_value(&*service.state.registration).unwrap();
        assert_eq!(
            registration["dynamicTools"][0],
            serde_json::json!({
                "type":"function", "name":"execute", "description":"Run a command",
                "inputSchema":{"type":"object"},
            })
        );
    }

    #[tokio::test]
    async fn endpoint_rejection_and_handler_panics_fail_closed() {
        let function = Arc::new(EchoEndpoint {
            tools: vec![HostedTool::Function(ToolDeclaration {
                name: "haskell".into(),
                description: "Structured tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
                kind: exomonad_tool::ToolKind::Call,
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
            "invalid resident tool invocation: unexpected structured call"
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
            &format!(
                "unsupported dynamic-tool protocol version 9; expected {}",
                HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION
            )
        );

        let mut request = call_request(serde_json::Value::String("pure ()".into()));
        request.namespace = Some("wrong".into());
        let response = call(State(state.clone()), Json(request)).await.0;
        let CallContent::InputText { text } = &response.content_items[0];
        assert_eq!(
            text,
            "dynamic-tool namespace mismatch: received Some(\"wrong\"); expected None"
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
        assert_eq!(registration["dynamicTools"][0]["name"], "haskell");
        let input = socket.with_file_name("input.sock");
        assert_eq!(registration["inputControlSocket"], serde_json::json!(input));

        let thread = "01a05a16-97f5-7722-aa8d-467e01e2e5b4";
        let source = "let x = \"quotes \\\" and \\\\ and λ\"\n    in x";
        let request = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "threadId": thread,
            "turnId": "turn-1",
            "callId": "call-1",
            "contextCallId": "outer-exec",
            "namespace": null,
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

        let foreign = client
            .post("http://localhost/v1/dynamic-tools/session")
            .json(&serde_json::json!({"protocolVersion": PROTOCOL_VERSION, "threadId": thread, "inputControlSocket": "/tmp/foreign.sock"}))
            .send().await.unwrap();
        assert_eq!(foreign.status(), StatusCode::BAD_REQUEST);
        assert!(!binding.exists());

        let stale = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "threadId": thread,
            "inputControlSocket": input,
            "launchId": registration["launchId"],
            "applicationInstanceId": "native-instance-1",
            "sessionGeneration": 1,
            "inputControlNonce": "stale-nonce",
        });
        assert_eq!(
            client
                .post("http://localhost/v1/dynamic-tools/session")
                .json(&stale)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert!(!binding.exists());

        let session = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "threadId": thread,
            "inputControlSocket": input,
            "launchId": registration["launchId"],
            "applicationInstanceId": "native-instance-1",
            "sessionGeneration": 1,
            "inputControlNonce": registration["inputControlNonce"],
        });
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
            exomonad_agent::read_interactive_binding(&binding)
                .await
                .unwrap()
                .id(),
            &BackendThreadId(thread.into())
        );
        let persisted: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&binding).await.unwrap()).unwrap();
        assert_eq!(persisted["input_control_socket"], serde_json::json!(input));

        let cancellation = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "threadId": thread,
            "turnId": "turn-1",
            "callId": "call-1",
            "contextCallId": "outer-exec",
            "namespace": null,
            "launchId": registration["launchId"],
            "applicationInstanceId": "native-instance-1",
            "sessionGeneration": 1,
            "inputControlNonce": registration["inputControlNonce"],
        });
        assert_eq!(
            client
                .post("http://localhost/v1/dynamic-tools/cancel")
                .json(&cancellation)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_IMPLEMENTED
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
        assert!(receipt["namespace"].is_null());

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
        // best-effort: task is aborted; the join result is expected to be Cancelled.
        server.await.ok();
    }
}

#[cfg(test)]
#[path = "host_dynamic_tools_drain_tests.rs"]
mod drain_tests;

#[cfg(test)]
pub(crate) use tests::endpoint as test_endpoint;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandResourceRequest {
    id: String,
    operation: CommandResourceOperation,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandResourceOperation {
    Acquire,
    Started,
    Status,
    Finished,
    Cancel,
}
async fn command_resources(
    axum::extract::State(state): axum::extract::State<HostState>,
    Json(request): Json<CommandResourceRequest>,
) -> Result<
    Json<exomonad_node::command_resources::CommandResourceStatus>,
    (axum::http::StatusCode, String),
> {
    let (owner, actor) = state.command_resources.as_ref().ok_or((
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "command resources unavailable".into(),
    ))?;
    let result = match request.operation {
        CommandResourceOperation::Acquire => {
            let mut phase = state.control.phase.subscribe();
            if *phase.borrow_and_update() != HostToolPhase::Serving {
                owner.cancel(actor, &request.id).await
            } else {
                tokio::select! {
                    result = owner.acquire(actor, &request.id) => {
                        if *phase.borrow() == HostToolPhase::Serving {
                            result
                        } else {
                            // best-effort: cancel is attempted regardless of its
                            // own outcome; the more specific quiescing error
                            // below is returned either way.
                            owner.cancel(actor, &request.id).await.ok();
                            Err(std::io::Error::other("host quiescing; command not started"))
                        }
                    },
                    _ = phase.changed() => owner.cancel(actor, &request.id).await,
                }
            }
        }
        CommandResourceOperation::Started => owner.started(actor, &request.id).await,
        CommandResourceOperation::Status => owner.status(actor, &request.id).await,
        CommandResourceOperation::Finished => match owner.started(actor, &request.id).await {
            Ok(_) => owner.status(actor, &request.id).await,
            Err(error) => Err(error),
        },
        CommandResourceOperation::Cancel => owner.cancel(actor, &request.id).await,
    };
    result.map(Json).map_err(|error| {
        (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            error.to_string(),
        )
    })
}

#[cfg(test)]
mod resource_tests;

#[cfg(test)]
mod tui_workspace_tests;
