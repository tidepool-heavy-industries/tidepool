//! Protected local attachment transport. Execution belongs to the resident actor.
pub mod proxy;
pub mod wire;

use axum::{
    body::{to_bytes, Body},
    extract::{Path as RoutePath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use exomonad_actor::{ActorExitKind, ActorTerminal, KernelInvocationFailure, LocalActorRef};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus};
use tokio::{
    net::UnixListener,
    sync::{watch, Mutex},
};
use uuid::Uuid;
use wire::*;

/// Keep the directory descriptor alive while a client can open connections.
/// Linux resolves this short address to the socket's existing filesystem inode,
/// so durable run paths need not fit in sockaddr_un and retain their permissions.
struct SocketAddress {
    path: PathBuf,
    #[cfg(target_os = "linux")]
    _directory: std::fs::File,
}

impl SocketAddress {
    fn new(socket: &Path) -> std::io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let parent = socket
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = socket.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "socket needs a file name")
            })?;
            let directory = std::fs::File::open(parent)?;
            let path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
            Ok(Self {
                path,
                _directory: directory,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Self {
                path: socket.to_owned(),
            })
        }
    }
}

fn client_for(socket: &Path) -> std::io::Result<(reqwest::Client, SocketAddress)> {
    let address = SocketAddress::new(socket)?;
    let client = reqwest::Client::builder()
        .unix_socket(address.path.as_path())
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(std::io::Error::other)?;
    Ok((client, address))
}

type InspectGraph = dyn Fn(
        exomonad_actor::ActorRef,
    ) -> BoxFuture<
        'static,
        Result<Option<Vec<exomonad_actor::ActorGraphNode>>, exomonad_actor::DirectoryError>,
    > + Send
    + Sync;
type InspectArtifact = dyn Fn(exomonad_actor::ActorRef, PathBuf) -> BoxFuture<'static, crate::run_map::ArtifactProvenance>
    + Send
    + Sync;

type Provision = dyn Fn() -> BoxFuture<'static, Result<LocalActorRef, String>> + Send + Sync;

#[derive(Clone)]
enum LocalOperator {
    Available(LocalActorRef),
    StopRequested(LocalActorRef),
}

type ProvisionCompletion = watch::Receiver<Option<Result<Attachment, String>>>;

#[derive(Clone)]
struct AttachmentState {
    sessions: Arc<Mutex<BTreeMap<String, LocalOperator>>>,
    service: ServiceIdentity,
    provisions: Arc<Mutex<BTreeMap<Uuid, ProvisionCompletion>>>,
    open: Arc<std::sync::atomic::AtomicBool>,
    provision: Arc<Provision>,
    input_bridge: Option<harness::server::HostInputBridge>,
    inspect_graph: Arc<InspectGraph>,
    inspect_artifact: Arc<InspectArtifact>,
    socket: PathBuf,
}
/// This process-local owner never carries admissions across a service restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceIdentity {
    incarnation: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvisionRequest {
    service: ServiceIdentity,
    operation: Uuid,
}

impl ProvisionRequest {
    fn new(service: ServiceIdentity) -> Self {
        Self {
            service,
            operation: Uuid::new_v4(),
        }
    }
}

async fn service_identity(client: &reqwest::Client) -> Result<ServiceIdentity, proxy::ProxyError> {
    let response = client
        .get("http://localhost/host/operators/service")
        .send()
        .await
        .map_err(|error| format!("cannot inspect operator service: {error}"))?
        .error_for_status()
        .map_err(|error| format!("cannot inspect operator service: {error}"))?;
    response
        .json()
        .await
        .map_err(|error| format!("cannot decode operator service identity: {error}").into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub session: String,
    pub socket: PathBuf,
}

pub struct OperatorService {
    state: AttachmentState,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl OperatorService {
    pub async fn bind(
        socket: PathBuf,
        input_bridge: Option<harness::server::HostInputBridge>,
        provision: Arc<Provision>,
        inspect_graph: Arc<InspectGraph>,
        inspect_artifact: Arc<InspectArtifact>,
    ) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let parent = socket
            .parent()
            .ok_or_else(|| std::io::Error::other("socket needs a parent"))?;
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        let address = SocketAddress::new(&socket)?;
        let listener = UnixListener::bind(&address.path)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let state = AttachmentState {
            sessions: Arc::default(),
            service: ServiceIdentity {
                incarnation: Uuid::new_v4(),
            },
            provisions: Arc::default(),
            open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            provision,
            input_bridge,
            inspect_graph,
            inspect_artifact,
            socket,
        };
        let app = Router::new()
            .route("/host/operators", get(list).post(new))
            .route("/host/operators/service", get(service))
            .route("/host/operators/{session}/stop", post(stop))
            .route("/host/artifacts", post(artifact))
            .route("/host/input", post(input))
            .route("/host/input/status", get(input_status))
            .route("/v1/sessions/{session}", get(inspect))
            .route("/v1/sessions/{session}/submit", post(submit))
            .route(
                "/v1/sessions/{session}/display/expand",
                post(expand_display),
            )
            .route("/v1/sessions/{session}/actors", get(graph))
            .fallback(|| async {
                error(StatusCode::NOT_FOUND, "session_not_found", "Unknown route")
            })
            .layer(axum::middleware::map_response(
                |response: Response| async move {
                    if (response.status().is_client_error() || response.status().is_server_error())
                        && response
                            .headers()
                            .get(axum::http::header::CONTENT_TYPE)
                            .is_none_or(|v| v != "application/json")
                    {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "Invalid HTTP route or request encoding",
                        );
                    }
                    response
                },
            ))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await });
        Ok(Self { state, task })
    }
    pub async fn shutdown(self) {
        self.task.abort();
        self.state
            .open
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.state.sessions.lock().await.clear();
        // best-effort: cleanup of the listen socket file; a leftover file
        // does not affect a later run, which binds its own fresh socket.
        std::fs::remove_file(&self.state.socket).ok();
    }
}

async fn artifact(State(state): State<AttachmentState>, body: Body) -> Response {
    let bytes = match to_bytes(body, 8 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "Artifact query exceeds 8 KiB",
            );
        }
    };
    let request: ArtifactRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(error_value) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                error_value.to_string(),
            );
        }
    };
    if !crate::actor_host::valid_artifact_path(&request.relative_path) {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_artifact_path",
            "Use a relative build artifact path without traversal",
        );
    }
    Json((state.inspect_artifact)(request.actor, request.relative_path).await).into_response()
}
fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            code: code.into(),
            message: message.into(),
            receipt: None,
        }),
    )
        .into_response()
}
async fn service(State(state): State<AttachmentState>) -> Json<ServiceIdentity> {
    Json(state.service)
}

async fn provision_once(state: AttachmentState) -> Result<Attachment, String> {
    let actor = (state.provision)().await?;
    let mut sessions = state.sessions.lock().await;
    if !state.open.load(std::sync::atomic::Ordering::SeqCst) {
        drop(sessions);
        if let Err(error) = actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "host attachment service closed".into(),
                diagnostic: None,
            })
            .await
        {
            tracing::warn!(%error, "cannot shut down actor provisioned after attachment service closed");
        }
        return Err("Host is shutting down".into());
    }
    let session = format!(
        "operator-{}-{}-{}",
        state.service.incarnation,
        actor.identity().id.0,
        actor.identity().incarnation.0
    );
    sessions.insert(session.clone(), LocalOperator::Available(actor));
    Ok(Attachment {
        session,
        socket: state.socket,
    })
}

async fn new(
    State(state): State<AttachmentState>,
    Json(request): Json<ProvisionRequest>,
) -> Response {
    if request.service != state.service {
        return error(
            StatusCode::CONFLICT,
            "service_changed",
            "Operator service incarnation changed; reconcile the saved operation before provisioning",
        );
    }
    if !state.open.load(std::sync::atomic::Ordering::SeqCst) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Host is shutting down",
        );
    }
    let mut result = {
        let mut provisions = state.provisions.lock().await;
        provisions
            .entry(request.operation)
            .or_insert_with(|| {
                let (completion, result) = watch::channel(None);
                let owner = state.clone();
                // Admission is retained before spawning. Observer loss, provider
                // failure, and retirement never free the identity for another actor.
                tokio::spawn(async move {
                    let outcome = match tokio::spawn(provision_once(owner)).await {
                        Ok(outcome) => outcome,
                        Err(error) => Err(format!(
                            "operator provisioning outcome unavailable: {error}"
                        )),
                    };
                    completion.send_replace(Some(outcome));
                });
                result
            })
            .clone()
    };
    loop {
        let observed = result.borrow().clone();
        if let Some(observed) = observed {
            return match observed {
                Ok(attachment) => Json(attachment).into_response(),
                Err(detail) => error(StatusCode::SERVICE_UNAVAILABLE, "unavailable", detail),
            };
        }
        if result.changed().await.is_err() {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Operator provisioning outcome unavailable",
            );
        }
    }
}
async fn list(State(state): State<AttachmentState>) -> Response {
    let sessions = state.sessions.lock().await;
    Json(
        sessions
            .iter()
            .filter(|(_, operator)| matches!(operator, LocalOperator::Available(actor) if actor.terminal().get().is_none()))
            .map(|(session, _)| Attachment {
                session: session.clone(),
                socket: state.socket.clone(),
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}
async fn stop(
    State(state): State<AttachmentState>,
    RoutePath(session): RoutePath<String>,
) -> Response {
    let (actor, initiate_shutdown) = {
        let mut sessions = state.sessions.lock().await;
        let Some(operator) = sessions.get_mut(&session) else {
            return error(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "Session no longer exists",
            );
        };
        let (actor, initiate_shutdown) = match operator {
            LocalOperator::Available(actor) => (actor.clone(), true),
            LocalOperator::StopRequested(actor) => (actor.clone(), false),
        };
        *operator = LocalOperator::StopRequested(actor.clone());
        (actor, initiate_shutdown)
    };
    if !initiate_shutdown {
        let cleanup_confirmed = actor.terminal().get().is_some()
            && actor
                .terminal()
                .cleanup()
                .is_some_and(|cleanup| cleanup.is_confirmed());
        return if cleanup_confirmed {
            let mut sessions = state.sessions.lock().await;
            if matches!(sessions.get(&session), Some(LocalOperator::StopRequested(current)) if current.identity() == actor.identity())
            {
                sessions.remove(&session);
            }
            Json(serde_json::json!({"stopped": session})).into_response()
        } else {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Retirement failed or cleanup remains unconfirmed",
            )
        };
    }
    // The detached task owns shutdown after admission, even if the HTTP
    // observer disconnects. Keep the session until actor cleanup is proven.
    let task_state = state.clone();
    let task_session = session.clone();
    let task = tokio::spawn(async move {
        let result = actor
            .shutdown_with_cleanup(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "operator stopped workbench".into(),
                diagnostic: None,
            })
            .await;
        if matches!(&result, Ok(shutdown) if shutdown.cleanup.is_confirmed()) {
            let mut sessions = task_state.sessions.lock().await;
            if matches!(sessions.get(&task_session), Some(LocalOperator::StopRequested(current)) if current.identity() == actor.identity())
            {
                sessions.remove(&task_session);
            }
        }
        result
    });
    match task.await {
        Ok(Ok(shutdown)) if shutdown.cleanup.is_confirmed() => {
            Json(serde_json::json!({"stopped": session})).into_response()
        }
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Retirement failed or cleanup remains unconfirmed",
        ),
    }
}

async fn inspect(
    State(state): State<AttachmentState>,
    RoutePath(session): RoutePath<String>,
) -> Response {
    let sessions = state.sessions.lock().await;
    match sessions.get(&session) {
        Some(LocalOperator::Available(actor)) if actor.terminal().get().is_none() => {
            Json(SessionInfo {
                protocol_version: PROTOCOL_VERSION,
                session,
            })
            .into_response()
        }
        Some(LocalOperator::Available(actor))
            if !actor
                .terminal()
                .cleanup()
                .is_some_and(|cleanup| cleanup.is_confirmed()) =>
        {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Session retirement cleanup remains unconfirmed",
            )
        }
        Some(LocalOperator::StopRequested(_)) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Session shutdown is pending or cleanup remains unconfirmed",
        ),
        _ => error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session no longer exists",
        ),
    }
}

async fn graph(
    State(state): State<AttachmentState>,
    RoutePath(session): RoutePath<String>,
) -> Response {
    let sessions = state.sessions.lock().await;
    let Some(LocalOperator::Available(actor)) = sessions
        .get(&session)
        .filter(|operator| matches!(operator, LocalOperator::Available(actor) if actor.terminal().get().is_none()))
    else {
        return error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session no longer exists",
        );
    };
    let actor = actor.identity();
    drop(sessions);
    let nodes = match (state.inspect_graph)(actor).await {
        Ok(Some(nodes)) => nodes,
        Ok(None) => {
            return error(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "Session no longer exists",
            )
        }
        Err(error_value) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "inspection_unavailable",
                error_value.to_string(),
            )
        }
    };
    let value = serde_json::json!({"protocol_version": PROTOCOL_VERSION, "session": session, "actors": nodes});
    let bytes = match serde_json::to_vec(&value) {
        Ok(bytes) => bytes,
        Err(error_value) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error_value.to_string(),
            );
        }
    };
    if bytes.len() > MAX_RESPONSE_BYTES {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Actor snapshot exceeds response budget",
        );
    }
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}

async fn expand_display(
    State(state): State<AttachmentState>,
    RoutePath(session): RoutePath<String>,
    body: Body,
) -> Response {
    let bytes = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "Display expansion request exceeds the request budget",
            );
        }
    };
    let input: DisplayExpansionRequest = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(error_value) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                error_value.to_string(),
            );
        }
    };
    submit_message(state, session, move |reply| KernelMessage::ExpandDisplay {
        request: exomonad_actor::DisplayExpansionRequest {
            identity: input.identity,
            key: input.key,
        },
        control: None,
        reply: reply.into(),
    })
    .await
}

async fn submit(
    State(state): State<AttachmentState>,
    RoutePath(session): RoutePath<String>,
    body: Body,
) -> Response {
    let bytes = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "Request body exceeds 1 MiB or could not be read",
            );
        }
    };
    let input: SubmitRequest = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(e) => return error(StatusCode::BAD_REQUEST, "invalid_request", e.to_string()),
    };
    let request = WorkbenchRequest::from_cell_input(&input.source);
    submit_invocation(
        state,
        session,
        exomonad_actor::ActorWorkbenchInvocation::unbound(request),
    )
    .await
}

async fn submit_invocation(
    state: AttachmentState,
    session: String,
    invocation: exomonad_actor::ActorWorkbenchInvocation,
) -> Response {
    submit_message(state, session, move |reply| KernelMessage::Workbench {
        invocation,
        control: None,
        reply: reply.into(),
    })
    .await
}

async fn submit_message(
    state: AttachmentState,
    session: String,
    message: impl FnOnce(
        tokio::sync::oneshot::Sender<exomonad_actor::KernelWorkbenchReply>,
    ) -> KernelMessage,
) -> Response {
    let sessions = state.sessions.lock().await;
    let Some(LocalOperator::Available(actor)) = sessions
        .get(&session)
        .filter(|operator| matches!(operator, LocalOperator::Available(actor) if actor.terminal().get().is_none()))
    else {
        return error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session no longer exists",
        );
    };
    let (reply, receive) = tokio::sync::oneshot::channel();
    let actor = actor.clone();
    drop(sessions);
    let admitted = match message(reply) {
        KernelMessage::Workbench {
            invocation,
            control,
            reply,
        } => actor.submit_workbench(invocation, control, reply).await,
        KernelMessage::ExpandDisplay {
            request,
            control,
            reply,
        } => {
            actor
                .submit_display_expansion(request, control, reply)
                .await
        }
        _ => unreachable!("operator submits only workbench or display expansion"),
    };
    if let Err(failure) = admitted {
        return match failure {
            exomonad_actor::KernelCallFailure::MailboxClosed(_)
            | exomonad_actor::KernelCallFailure::TargetExited(_) => error(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "Session no longer exists",
            ),
            failure => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "workbench_unavailable",
                failure.to_string(),
            ),
        };
    }
    // Ractor now owns the request, including when this response future is dropped.
    match receive.await {
        Ok(Ok(result)) => bounded_response(map_result(result)),
        Ok(Err(KernelInvocationFailure::Workbench(failure))) => {
            bounded_response(map_failure(failure))
        }
        Ok(Err(e)) => invocation_error(e),
        Err(e) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            e.to_string(),
        ),
    }
}

fn invocation_error(failure: KernelInvocationFailure) -> Response {
    let receipts = failure.receipts();
    if receipts.is_empty() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            failure.to_string(),
        );
    }
    let response = ApiError {
        code: "unavailable".into(),
        message: failure.to_string(),
        receipt: Some(Receipt {
            display: exomonad_actor::bound_workbench_display(&failure.to_string(), 2048),
            structured: Some(
                serde_json::json!({"items": receipts, "publication": failure.publication()}),
            ),
        }),
    };
    let bytes = match serde_json::to_vec(&response) {
        Ok(bytes) if bytes.len() <= MAX_RESPONSE_BYTES => bytes,
        Ok(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Execution ended but its required receipt exceeds the response budget; execution is unknown to this connection",
            );
        }
        Err(error_value) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error_value.to_string(),
            );
        }
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}

fn map_failure(failure: exomonad_actor::KernelWorkbenchFailure) -> SubmitResponse {
    let recovery_required = matches!(
        failure.publication.as_ref(),
        Some(tidepool_runtime::session::WorkbenchPublicationOutcome::RecoveryRequired { .. })
    );
    let mut response = map_result(WorkbenchResponse {
        status: WorkbenchRunStatus::Rejected,
        summary: None,
        items: failure.receipts,
        next_index: failure.point.next_index(),
        total: failure.total,
        publication: failure.publication,
    });
    if let Some(receipt) = &mut response.receipt {
        receipt.display = match failure.point {
            tidepool_runtime::session::WorkbenchFailurePoint::InputUnit { index } => {
                format!("Rejected: {} of {} input units", index, failure.total)
            }
            tidepool_runtime::session::WorkbenchFailurePoint::Publication {
                completed_input_units,
            } if recovery_required => {
                format!(
                    "Publication requires authenticated recovery after {completed_input_units} completed input units"
                )
            }
            tidepool_runtime::session::WorkbenchFailurePoint::Publication {
                completed_input_units,
            } => {
                format!(
                    "Rejected: publication rejected after {completed_input_units} completed input units"
                )
            }
            tidepool_runtime::session::WorkbenchFailurePoint::Finalization {
                completed_input_units,
            } => {
                format!(
                    "Rejected: finalization failed after {completed_input_units} completed input units"
                )
            }
        };
        if let Some(structured) = &mut receipt.structured {
            structured["failure"] = failure.detail.clone().into();
        }
    }
    response.blocks.push(Block::Diagnostic(failure.detail));
    response
}

fn map_result(result: WorkbenchResponse) -> SubmitResponse {
    let outcome = match result.status {
        WorkbenchRunStatus::Rejected | WorkbenchRunStatus::Backgrounded => Outcome::Rejected,
        WorkbenchRunStatus::Committed
        | WorkbenchRunStatus::Completed
        | WorkbenchRunStatus::Replied
        | WorkbenchRunStatus::RequestCancelled => Outcome::Completed,
    };
    let mut blocks = Vec::new();
    for item in &result.items {
        if !item.output.is_empty() {
            blocks.push(match item.status {
                tidepool_runtime::session::WorkbenchItemStatus::Diagnostic
                | tidepool_runtime::session::WorkbenchItemStatus::Stopped
                | tidepool_runtime::session::WorkbenchItemStatus::Rejected => {
                    Block::Diagnostic(item.output.clone())
                }
                _ => Block::Output(item.output.clone()),
            });
        }
        blocks.extend(item.warnings.iter().cloned().map(Block::Diagnostic));
    }
    SubmitResponse {
        blocks,
        outcome,
        receipt: Some(Receipt {
            display: format!(
                "{:?}: {} of {} input units",
                result.status, result.next_index, result.total
            ),
            structured: Some(serde_json::json!(result)),
        }),
    }
}
fn bounded_response(mut result: SubmitResponse) -> Response {
    let mut bytes = match serde_json::to_vec(&result) {
        Ok(bytes) => bytes,
        Err(error_value) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error_value.to_string(),
            );
        }
    };
    if bytes.len() > MAX_RESPONSE_BYTES {
        result.blocks = vec![Block::Diagnostic(
            "Display output truncated to fit the response budget.".into(),
        )];
        bytes = match serde_json::to_vec(&result) {
            Ok(bytes) => bytes,
            Err(error_value) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    error_value.to_string(),
                );
            }
        };
    }
    if bytes.len() > MAX_RESPONSE_BYTES {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Execution ended but its required receipt exceeds the response budget; execution is unknown to this connection",
        );
    }
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRequest {
    pub operation: Uuid,
    pub target: harness::server::HostActorIdentity,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputStatusRequest {
    pub run: String,
    pub operation: Uuid,
}

impl InputStatusRequest {
    fn url(&self) -> Result<reqwest::Url, Box<dyn std::error::Error>> {
        let mut url = reqwest::Url::parse("http://localhost/host/input/status")?;
        url.query_pairs_mut()
            .append_pair("run", &self.run)
            .append_pair("operation", &self.operation.to_string());
        Ok(url)
    }
}

fn input_refusal((code, reason): (harness::server::CommandRefusal, String)) -> Response {
    use harness::server::CommandRefusal;
    let status = match code {
        CommandRefusal::InvalidCommand => StatusCode::BAD_REQUEST,
        CommandRefusal::Conflict => StatusCode::CONFLICT,
        CommandRefusal::WrongRun => StatusCode::FORBIDDEN,
        CommandRefusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    (
        status,
        Json(serde_json::json!({"code": code, "reason": reason})),
    )
        .into_response()
}

async fn input(
    State(state): State<AttachmentState>,
    Json(request): Json<InputRequest>,
) -> Response {
    if !state.open.load(std::sync::atomic::Ordering::SeqCst) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Host is shutting down",
        );
    }
    let Some(bridge) = state.input_bridge else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "input_unavailable",
            "host model input is unavailable",
        );
    };
    match bridge.submit_input(
        harness::embedding::ClientOperationId(request.operation),
        request.target,
        request.text,
    ) {
        Ok(accepted) => (StatusCode::ACCEPTED, Json(accepted)).into_response(),
        Err(error) => input_refusal(error),
    }
}

async fn input_status(
    State(state): State<AttachmentState>,
    Query(request): Query<InputStatusRequest>,
) -> Response {
    let Some(bridge) = state.input_bridge else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "input_unavailable",
            "host model input is unavailable",
        );
    };
    match bridge.input_status(
        &request.run,
        harness::embedding::ClientOperationId(request.operation),
    ) {
        Ok(Some(record)) => Json(record).into_response(),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "input_not_found",
            "input operation is not retained",
        ),
        Err(error) => input_refusal(error),
    }
}

pub enum OperatorAction {
    Input(InputRequest),
    InputStatus(InputStatusRequest),
    New,
    List,
    Stop {
        session: String,
    },
    Artifact {
        actor: exomonad_actor::ActorRef,
        relative_path: PathBuf,
    },
}

pub async fn command(
    socket: &Path,
    action: OperatorAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let (client, _address) = client_for(socket)?;
    let url = match &action {
        OperatorAction::Input(_) => "http://localhost/host/input".to_owned(),
        OperatorAction::InputStatus(request) => request.url()?.to_string(),
        OperatorAction::New | OperatorAction::List => "http://localhost/host/operators".to_owned(),
        OperatorAction::Artifact { .. } => "http://localhost/host/artifacts".to_owned(),
        OperatorAction::Stop { session } => {
            let mut url = reqwest::Url::parse("http://localhost/host/operators/")?;
            url.path_segments_mut()
                .map_err(|_| "invalid operator URL")?
                .pop_if_empty()
                .push(session)
                .push("stop");
            url.to_string()
        }
    };
    let input_operation = match &action {
        OperatorAction::Input(request) => Some((request.target.run.clone(), request.operation)),
        _ => None,
    };
    let uncertain = |error: reqwest::Error| -> Box<dyn std::error::Error> {
        match &input_operation {
            Some((run, operation)) => format!("input acknowledgement is unknown: {error}; inspect input-status --run {run} --operation {operation} before any submission").into(),
            None => Box::new(error),
        }
    };
    let response = match action {
        OperatorAction::Input(request) => client.post(url).json(&request),
        OperatorAction::InputStatus(_) => client.get(url),
        OperatorAction::List => client.get(url),
        OperatorAction::Artifact {
            actor,
            relative_path,
        } => client.post(url).json(&ArtifactRequest {
            actor,
            relative_path,
        }),
        OperatorAction::New => client
            .post(url)
            .json(&ProvisionRequest::new(service_identity(&client).await?)),
        OperatorAction::Stop { .. } => client.post(url),
    }
    .send()
    .await
    .map_err(&uncertain)?;
    let status = response.status();
    let body = response.text().await.map_err(&uncertain)?;
    if !status.is_success() {
        if status.is_server_error() {
            if let Some((run, operation)) = input_operation {
                return Err(format!("{status}: {body}; input acknowledgement is unknown; inspect input-status --run {run} --operation {operation} before any submission").into());
            }
        }
        return Err(format!("{status}: {body}").into());
    }
    println!("{body}");
    Ok(())
}

#[cfg(test)]
mod input_tests {
    use super::*;

    fn request() -> InputRequest {
        InputRequest {
            operation: Uuid::new_v4(),
            target: harness::server::HostActorIdentity {
                run: "exact-run + & ü".into(),
                actor: harness::model::AgentPath("/root".into()),
                incarnation: "exact-incarnation".into(),
            },
            text: "ordinary user input".into(),
        }
    }

    #[tokio::test]
    async fn protected_input_preserves_shared_admission_and_exact_status() {
        let store = Arc::new(harness::store::Store::memory().unwrap());
        let (_, control, mut commands) = harness::server::server_with_config(
            harness::server::ServerConfig::new(PathBuf::from(".")).with_history_store(store),
        );
        control.set_snapshot(harness::server::Snapshot {
            host_run: Some("exact-run + & ü".into()),
            ..harness::server::Snapshot::default()
        });
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator/operator.sock");
        let service = OperatorService::bind(
            socket.clone(),
            Some(control.input_bridge()),
            Arc::new(|| panic!("input must not provision a workbench")),
            Arc::new(|_| panic!("input must not inspect graph")),
            Arc::new(|_, _| panic!("input must not inspect artifacts")),
        )
        .await
        .unwrap();
        let (client, _address) = client_for(&socket).unwrap();
        let request = request();
        // Discard the accepted response; status must retain the same command.
        let accepted = client
            .post("http://localhost/host/input")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        drop(accepted);
        let queued = commands.recv().await.unwrap();
        assert!(
            matches!(queued.command, harness::server::ClientCommand::Host { operation_id, command: harness::server::HostCommand::Input { target, text } }
            if operation_id.0 == request.operation && target == request.target && text == request.text)
        );
        let query = InputStatusRequest {
            run: request.target.run.clone(),
            operation: request.operation,
        };
        let response = client.get(query.url().unwrap()).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let record: harness::store::EmbeddedCommandRecord = response.json().await.unwrap();
        assert_eq!(record.operation_id.0, request.operation);
        assert_eq!(
            record.command,
            harness::server::HostCommand::Input {
                target: request.target.clone(),
                text: request.text.clone()
            }
        );
        assert_eq!(record.state, harness::store::EmbeddedCommandState::Queued);
        assert!(record.receipt.is_none());
        assert_eq!(
            client
                .post("http://localhost/host/input")
                .json(&request)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
        let malformed = serde_json::json!({
            "operation": request.operation,
            "target": request.target,
            "text": request.text,
            "command": "retire"
        });
        assert_eq!(
            client
                .post("http://localhost/host/input")
                .json(&malformed)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let mut conflicting = request.clone();
        conflicting.text = "different".into();
        assert_eq!(
            client
                .post("http://localhost/host/input")
                .json(&conflicting)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        let mut foreign = request;
        foreign.operation = Uuid::new_v4();
        foreign.target.run = "foreign-run".into();
        assert_eq!(
            client
                .post("http://localhost/host/input")
                .json(&foreign)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = client.get(query.url().unwrap()).send().await.unwrap();
        assert_eq!(
            response
                .json::<harness::store::EmbeddedCommandRecord>()
                .await
                .unwrap(),
            record
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn workbench_only_service_refuses_model_input() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator/operator.sock");
        let service = OperatorService::bind(
            socket.clone(),
            None,
            Arc::new(|| panic!("input must not provision a workbench")),
            Arc::new(|_| panic!("input must not inspect graph")),
            Arc::new(|_, _| panic!("input must not inspect artifacts")),
        )
        .await
        .unwrap();
        let (client, _address) = client_for(&socket).unwrap();
        let request = request();
        assert_eq!(
            client
                .post("http://localhost/host/input")
                .json(&request)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            client
                .get(
                    InputStatusRequest {
                        run: request.target.run,
                        operation: request.operation,
                    }
                    .url()
                    .unwrap()
                )
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn lost_input_transport_acknowledgement_requires_lookup_and_does_not_retry() {
        use tokio::io::AsyncReadExt;
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let request = request();
        let operation = request.operation;
        let client = tokio::spawn(async move {
            command(&socket, OperatorAction::Input(request))
                .await
                .map_err(|error| error.to_string())
        });
        let (mut connection, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(connection.read(&mut bytes).await.unwrap() > 0);
        drop(connection);
        let error = client.await.unwrap().unwrap_err();
        assert!(error.contains("acknowledgement is unknown"));
        assert!(error.contains("input-status"));
        assert!(error.contains(&operation.to_string()));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    use exomonad_actor::{ActorId, ActorRef};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn publication_rejection_keeps_completed_items_without_claiming_public_bindings() {
        use tidepool_runtime::session::{
            WorkbenchExecutionId, WorkbenchFailurePoint, WorkbenchItemReceipt, WorkbenchItemStatus,
            WorkbenchOperationDisposition, WorkbenchOperationId, WorkbenchOperationReceipt,
            WorkbenchPublicationOutcome,
        };
        let failure = exomonad_actor::KernelWorkbenchFailure {
            actor: ActorRef::first(ActorId(9)),
            receipts: (0..3)
                .map(|index| WorkbenchItemReceipt {
                    presentation: None,
                    index,
                    kind: None,
                    span: None,
                    source_items: Vec::new(),
                    status: WorkbenchItemStatus::Committed,
                    output: format!("private result {index}"),
                    value: None,
                    diagnostics: Vec::new(),
                    failure_layer: None,
                    warnings: Vec::new(),
                    installed_bindings: vec![format!("private{index}")],
                    operations: vec![WorkbenchOperationReceipt {
                        display: None,
                        display_publication: None,
                        id: WorkbenchOperationId {
                            execution: WorkbenchExecutionId::from_digest([index as u8; 16]),
                            input_unit_index: index,
                            effect_ordinal: 0,
                        },
                        effect: "receipt-bearing effect".into(),
                        disposition: WorkbenchOperationDisposition::Committed,
                    }],
                    terminal_transfer: None,
                })
                .collect(),
            point: WorkbenchFailurePoint::Publication {
                completed_input_units: 3,
            },
            total: 3,
            publication: Some(WorkbenchPublicationOutcome::Rejected {
                detail: "staged environment changed".into(),
            }),
            detail: "staged environment changed".into(),
            diagnostic: None,
        };
        let mut cleanup_receipts = failure.receipts.clone();
        let reference = tidepool_runtime::session::ActorOutputReference {
            run: "run".into(),
            sequence: 17,
        };
        cleanup_receipts[0].operations[0].display_publication = Some(
            tidepool_runtime::session::WorkbenchDisplayPublication::Published {
                publication: tidepool_runtime::session::WorkbenchDisplayPublicationIdentity {
                    display: (3, 5, 7),
                    page_ordinal: 11,
                },
                output: reference.clone(),
            },
        );
        cleanup_receipts[0].operations[0].display =
            Some(tidepool_runtime::session::WorkbenchDisplayOutput {
                page: tidepool_runtime::session::WorkbenchDisplayPage {
                    identity: (3, 5, 7),
                    text: "emitted before cleanup".into(),
                    expansions: Vec::new(),
                    unavailable: false,
                },
                output: reference,
            });
        let cleanup = invocation_error(KernelInvocationFailure::CleanupUnconfirmed {
            actor: failure.actor,
            detail: "cleanup remains unconfirmed".into(),
            receipts: cleanup_receipts,
            publication: Some(WorkbenchPublicationOutcome::NotPublished {
                reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Failed,
            }),
        });
        assert_eq!(cleanup.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(cleanup.into_body(), MAX_RESPONSE_BYTES)
            .await
            .unwrap();
        let error: ApiError = serde_json::from_slice(&body).unwrap();
        assert_eq!(error.code, "unavailable");
        let frozen = error.receipt.unwrap().structured.unwrap();
        assert_eq!(frozen["publication"]["status"], "notPublished");
        assert_eq!(frozen["publication"]["reason"], "failed");
        let operation = &frozen["items"][0]["operations"][0];
        assert_eq!(operation["displayPublication"]["status"], "published");
        assert_eq!(operation["display"]["output"]["sequence"], 17);
        assert_eq!(operation["display"]["text"], "emitted before cleanup");
        let legacy: ApiError =
            serde_json::from_value(serde_json::json!({"code":"unavailable","message":"old error"}))
                .unwrap();
        assert!(legacy.receipt.is_none());

        let response = map_failure(failure);
        let receipt = response.receipt.unwrap();
        assert_eq!(
            receipt.display,
            "Rejected: publication rejected after 3 completed input units"
        );
        let structured = receipt.structured.unwrap();
        assert_eq!(structured["nextIndex"], 3);
        assert_eq!(structured["items"].as_array().unwrap().len(), 3);
        assert_eq!(structured["publication"]["status"], "rejected");
        assert_eq!(
            structured["publication"]["detail"],
            "staged environment changed"
        );
        assert!(structured["publication"].get("bindings").is_none());
        let rejected = WorkbenchPublicationOutcome::Rejected {
            detail: "not published".into(),
        };
        assert!(rejected.public_bindings().is_empty());
        let unconfirmed = WorkbenchPublicationOutcome::RecoveryRequired {
            detail: "journal outcome unknown".into(),
        };
        assert!(unconfirmed.public_bindings().is_empty());
        let uncertain_failure = exomonad_actor::KernelWorkbenchFailure {
            actor: ActorRef::first(ActorId(9)),
            receipts: vec![],
            point: WorkbenchFailurePoint::Publication {
                completed_input_units: 3,
            },
            total: 3,
            publication: Some(unconfirmed),
            detail: "journal outcome unknown".into(),
            diagnostic: None,
        };
        assert!(uncertain_failure
            .to_string()
            .contains("publication requires authenticated recovery"));
        let uncertain_response = map_failure(uncertain_failure);
        assert!(uncertain_response
            .receipt
            .unwrap()
            .display
            .contains("Publication requires authenticated recovery"));
        assert_eq!(structured["items"][2]["output"], "private result 2");
        assert_eq!(
            structured["items"][2]["operations"][0]["effect"],
            "receipt-bearing effect"
        );
        assert!(!receipt.display.contains("unit 3 failed"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn operator_transport_supports_long_durable_paths() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let socket = directory
            .path()
            .join("long-state-path-".repeat(10))
            .join("operator.sock");
        assert!(socket.as_os_str().len() > 108);
        let service = OperatorService::bind(
            socket.clone(),
            None,
            Arc::new(|| Box::pin(async { Err("provision must not run".into()) })),
            Arc::new(|_| Box::pin(async { Ok(None) })),
            Arc::new(|_, _| panic!("artifact inspection must not run")),
        )
        .await
        .unwrap();
        let (client, _address) = client_for(&socket).unwrap();
        let response = client
            .get("http://localhost/host/operators")
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!([])
        );
        assert_eq!(
            std::fs::metadata(socket.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        service.shutdown().await;
        assert!(!socket.exists());
        assert!(socket.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn artifact_query_is_read_only_and_rejects_traversal_before_inspection() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator.sock");
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let service = OperatorService::bind(
            socket.clone(),
            None,
            Arc::new(|| Box::pin(async { Err("provision must not run".into()) })),
            Arc::new(|_| Box::pin(async { Ok(None) })),
            Arc::new(move |actor, relative_path| {
                observed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    crate::run_map::ArtifactProvenance {
                        run_id: "test-run".into(),
                        actor,
                        logical_path: relative_path.display().to_string(),
                        availability: crate::run_map::ArtifactAvailability::Unknown {
                            reason: "no active owner".into(),
                        },
                    }
                })
            }),
        )
        .await
        .unwrap();
        let client = reqwest::Client::builder()
            .unix_socket(socket.as_path())
            .build()
            .unwrap();
        let actor = ActorRef::first(ActorId(7));
        let query = |path: &str| ArtifactRequest {
            actor,
            relative_path: PathBuf::from(path),
        };
        let refused = client
            .post("http://localhost/host/artifacts")
            .json(&query("../outside"))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let accepted = client
            .post("http://localhost/host/artifacts")
            .json(&query("debug/evidence.json"))
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        let body: serde_json::Value = accepted.json().await.unwrap();
        assert_eq!(body["actor"], serde_json::json!({"id":7,"incarnation":1}));
        assert_eq!(body["availability"]["state"], "unknown");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        service.shutdown().await;
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use exomonad_actor::{
        ActorRef, ChildExitNotice, ExternalApplicationFailure, ExternalFailureDisposition,
        KernelBehavior, KernelBehaviorError, KernelContext, KernelStep, MailboxValue,
    };
    use futures_util::future::BoxFuture;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Semaphore;

    pub(super) struct LifecycleBehavior {
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
        shutdown_calls: Arc<AtomicUsize>,
        confirmed: bool,
        fail_shutdown: bool,
    }

    impl LifecycleBehavior {
        fn workbench<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: exomonad_actor::ActorWorkbenchInvocation,
            _: Option<Arc<exomonad_actor::WorkbenchExecutionControl>>,
        ) -> BoxFuture<
            'a,
            Result<KernelStep<WorkbenchResponse>, exomonad_actor::KernelInvocationFailure>,
        > {
            Box::pin(async { panic!("unexpected workbench") })
        }
    }

    impl KernelBehavior for LifecycleBehavior {
        fn start<'a>(
            &'a mut self,
            _: &'a KernelContext,
        ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async { Ok(KernelStep::Continue(())) })
        }
        fn cast<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: ActorRef,
            _: MailboxValue,
        ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async { panic!("unexpected cast") })
        }
        fn call<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: ActorRef,
            _: exomonad_actor::CallAncestry,
            _: MailboxValue,
        ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
            Box::pin(async { panic!("unexpected call") })
        }
        fn tool<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: exomonad_tool::ToolInvocation,
            _: Option<Arc<dyn exomonad_actor::HostedCheckpointCapture>>,
        ) -> BoxFuture<
            'a,
            Result<KernelStep<serde_json::Value>, exomonad_actor::KernelInvocationFailure>,
        > {
            Box::pin(async { panic!("unexpected tool") })
        }
        fn dispatch_workbench(
            &mut self,
            _context: &KernelContext,
            invocation: exomonad_actor::ActorWorkbenchInvocation,
            control: Option<Arc<exomonad_actor::WorkbenchExecutionControl>>,
        ) -> exomonad_actor::WorkbenchDispatch<Self> {
            exomonad_actor::WorkbenchDispatch::Sequential {
                invocation,
                control,
                executor: |mut behavior, context, invocation, control| {
                    Box::pin(async move {
                        let result = behavior.workbench(&context, invocation, control).await;
                        (
                            behavior,
                            exomonad_actor::OwnedActorCompletion::new(move |_| result),
                        )
                    })
                },
            }
        }
        fn external_application_failed<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: ExternalApplicationFailure,
        ) -> BoxFuture<'a, ExternalFailureDisposition> {
            Box::pin(async { panic!("unexpected external application failure") })
        }
        fn shutdown<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: &'a ActorTerminal,
        ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
            Box::pin(async move {
                self.shutdown_calls.fetch_add(1, Ordering::SeqCst);
                self.entered.add_permits(1);
                self.release.acquire().await.unwrap().forget();
                if self.fail_shutdown {
                    Err(KernelBehaviorError {
                        detail: "test shutdown failure".into(),
                        diagnostic: None,
                    })
                } else {
                    Ok(())
                }
            })
        }
        fn shutdown_components<'a>(
            &'a mut self,
            context: &'a KernelContext,
            terminal: &'a ActorTerminal,
            _: tokio::time::Instant,
        ) -> BoxFuture<
            'a,
            (
                exomonad_actor::CleanupComponentOutcome,
                exomonad_actor::CleanupComponentOutcome,
            ),
        > {
            Box::pin(async move {
                let hook = match self.shutdown(context, terminal).await {
                    Ok(()) if self.confirmed => exomonad_actor::CleanupComponentOutcome::Confirmed,
                    Ok(()) => exomonad_actor::CleanupComponentOutcome::Unconfirmed(
                        "test cleanup uncertain".into(),
                    ),
                    Err(error) => {
                        exomonad_actor::CleanupComponentOutcome::Unconfirmed(error.to_string())
                    }
                };
                let realm = if self.confirmed {
                    exomonad_actor::CleanupComponentOutcome::Confirmed
                } else {
                    exomonad_actor::CleanupComponentOutcome::Unconfirmed(
                        "test cleanup uncertain".into(),
                    )
                };
                (hook, realm)
            })
        }
        fn stopped<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: &'a ActorTerminal,
        ) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }
        fn child_exited(&mut self, _: ChildExitNotice) {
            panic!("unexpected child exit")
        }
    }

    async fn service_for_actor(
        socket: PathBuf,
        actor: LocalActorRef,
    ) -> (OperatorService, reqwest::Client, String) {
        let graph_actor = actor.identity();
        service_for_actor_with_graph(
            socket,
            actor,
            Arc::new(move |actor| {
                Box::pin(async move { Ok((actor == graph_actor).then(Vec::new)) })
            }),
        )
        .await
    }

    async fn service_for_actor_with_graph(
        socket: PathBuf,
        actor: LocalActorRef,
        inspect_graph: Arc<InspectGraph>,
    ) -> (OperatorService, reqwest::Client, String) {
        let service = OperatorService::bind(
            socket.clone(),
            None,
            Arc::new(move || {
                let actor = actor.clone();
                Box::pin(async move { Ok(actor) })
            }),
            inspect_graph,
            Arc::new(|_, _| Box::pin(async { panic!("unexpected artifact inspection") })),
        )
        .await
        .unwrap();
        let client = reqwest::Client::builder()
            .unix_socket(socket.as_path())
            .build()
            .unwrap();
        let attachment: Attachment = client
            .post("http://localhost/host/operators")
            .json(&ProvisionRequest::new(
                service_identity(&client).await.unwrap(),
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        (service, client, attachment.session)
    }

    pub(super) fn behavior(
        confirmed: bool,
        fail_shutdown: bool,
    ) -> (
        LifecycleBehavior,
        Arc<Semaphore>,
        Arc<Semaphore>,
        Arc<AtomicUsize>,
    ) {
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        (
            LifecycleBehavior {
                entered: entered.clone(),
                release: release.clone(),
                shutdown_calls: calls.clone(),
                confirmed,
                fail_shutdown,
            },
            entered,
            release,
            calls,
        )
    }

    #[tokio::test]
    async fn graph_inspection_distinguishes_absent_session_from_unavailable_directory() {
        let directory = tempfile::tempdir().unwrap();
        let (behavior, _, release, _) = behavior(true, false);
        let (actor, actor_task) = exomonad_actor::spawn_local_actor(None, behavior)
            .await
            .unwrap();
        let unavailable = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&unavailable);
        let (service, client, session) = service_for_actor_with_graph(
            directory.path().join("operator.sock"),
            actor,
            Arc::new(move |_| {
                let observed = Arc::clone(&observed);
                Box::pin(async move {
                    if observed.load(Ordering::SeqCst) {
                        Err(exomonad_actor::DirectoryError::Unavailable)
                    } else {
                        Ok(None)
                    }
                })
            }),
        )
        .await;
        let url = format!("http://localhost/v1/sessions/{session}/actors");
        let missing = client.get(&url).send().await.unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        unavailable.store(true, Ordering::SeqCst);
        let failed = client.get(&url).send().await.unwrap();
        assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            failed.json::<serde_json::Value>().await.unwrap()["code"],
            "inspection_unavailable"
        );

        release.add_permits(1);
        let stopped = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert_eq!(stopped.status(), StatusCode::OK);
        actor_task.await.unwrap();
        service.shutdown().await;
    }

    #[tokio::test]
    async fn stop_admission_survives_observer_loss_and_removes_only_confirmed_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator.sock");
        let (behavior, entered, release, calls) = behavior(true, false);
        let (actor, actor_task) = exomonad_actor::spawn_local_actor(None, behavior)
            .await
            .unwrap();
        let actor_observer = actor.clone();
        let (service, client, session) = service_for_actor(socket, actor).await;

        let stopping_client = client.clone();
        let stopping_session = session.clone();
        let observer = tokio::spawn(async move {
            stopping_client
                .post(format!(
                    "http://localhost/host/operators/{stopping_session}/stop"
                ))
                .send()
                .await
        });
        entered.acquire().await.unwrap().forget();
        let duplicate = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());

        let listed: Vec<Attachment> = client
            .get("http://localhost/host/operators")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(listed.is_empty());
        let submit = client
            .post(format!("http://localhost/v1/sessions/{session}/submit"))
            .json(&SubmitRequest {
                source: "()".into(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(submit.status(), StatusCode::NOT_FOUND);

        release.add_permits(1);
        actor_observer.terminal().wait().await;
        let stopped = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert!(matches!(
            stopped.status(),
            StatusCode::OK | StatusCode::NOT_FOUND
        ));
        let stopped_again = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert_eq!(stopped_again.status(), StatusCode::NOT_FOUND);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        actor_task.await.unwrap();
        assert_eq!(
            actor_observer.terminal().get().unwrap().kind,
            ActorExitKind::Cancelled
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn unconfirmed_cleanup_remains_tracked_and_inspection_refuses_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator.sock");
        let (behavior, _, release, _) = behavior(false, false);
        let (actor, actor_task) = exomonad_actor::spawn_local_actor(None, behavior)
            .await
            .unwrap();
        let actor_observer = actor.clone();
        let (service, client, session) = service_for_actor(socket, actor).await;
        release.add_permits(1);
        let stopped = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert_eq!(stopped.status(), StatusCode::SERVICE_UNAVAILABLE);
        let listed: Vec<Attachment> = client
            .get("http://localhost/host/operators")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(listed.is_empty());
        let inspected = client
            .get(format!("http://localhost/v1/sessions/{session}"))
            .send()
            .await
            .unwrap();
        assert_eq!(inspected.status(), StatusCode::SERVICE_UNAVAILABLE);
        let retry = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::SERVICE_UNAVAILABLE);
        actor_task.await.unwrap();
        assert_eq!(
            actor_observer.terminal().get().unwrap().kind,
            ActorExitKind::Cancelled
        );
        service.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_failure_keeps_the_session_for_reobservation() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("operator.sock");
        let (behavior, _, release, _) = behavior(false, true);
        let (actor, actor_task) = exomonad_actor::spawn_local_actor(None, behavior)
            .await
            .unwrap();
        let (service, client, session) = service_for_actor(socket, actor).await;
        release.add_permits(1);
        let stopped = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert_eq!(stopped.status(), StatusCode::SERVICE_UNAVAILABLE);
        let listed: Vec<Attachment> = client
            .get("http://localhost/host/operators")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(listed.is_empty());
        let retry = client
            .post(format!("http://localhost/host/operators/{session}/stop"))
            .send()
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::SERVICE_UNAVAILABLE);
        let _terminal = actor_task.await.unwrap();
        service.shutdown().await;
    }
}
