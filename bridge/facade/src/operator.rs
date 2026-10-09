//! Protected local attachment transport. Execution belongs to the resident actor.
pub mod proxy;
pub mod wire;

use axum::{
    body::{to_bytes, Body},
    extract::{Path as RoutePath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use exomonad_actor::{
    ActorExitKind, ActorTerminal, KernelInvocationFailure, KernelMessage, LocalActorRef,
};
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

type InspectGraph =
    dyn Fn(exomonad_actor::ActorRef) -> Option<Vec<exomonad_actor::ActorGraphNode>> + Send + Sync;
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
            inspect_graph,
            inspect_artifact,
            socket,
        };
        let app = Router::new()
            .route("/host/operators", get(list).post(new))
            .route("/host/operators/service", get(service))
            .route("/host/operators/{session}/stop", post(stop))
            .route("/host/artifacts", post(artifact))
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
            )
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
        return error(StatusCode::CONFLICT, "service_changed", "Operator service incarnation changed; reconcile the saved operation before provisioning");
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
    let Some(nodes) = (state.inspect_graph)(actor.identity()) else {
        return error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session no longer exists",
        );
    };
    let value = serde_json::json!({"protocol_version": PROTOCOL_VERSION, "session": session, "actors": nodes});
    let bytes = match serde_json::to_vec(&value) {
        Ok(bytes) => bytes,
        Err(error_value) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                error_value.to_string(),
            )
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
            )
        }
    };
    let input: DisplayExpansionRequest = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(error_value) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                error_value.to_string(),
            )
        }
    };
    submit_invocation(
        state,
        session,
        exomonad_actor::ActorWorkbenchInvocation::for_display_expansion(input.identity, input.key),
    )
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
            )
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
    if actor
        .address()
        .send_message(KernelMessage::Workbench {
            invocation,
            control: None,
            reply: reply.into(),
        })
        .is_err()
    {
        return error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session no longer exists",
        );
    }
    drop(sessions);
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
        Ok(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "Execution ended but its required receipt exceeds the response budget; execution is unknown to this connection"),
        Err(error_value) => return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", error_value.to_string()),
    };
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}

fn map_failure(failure: exomonad_actor::KernelWorkbenchFailure) -> SubmitResponse {
    let durability_unconfirmed = matches!(
        failure.publication.as_ref(),
        Some(tidepool_runtime::session::WorkbenchPublicationOutcome::DurabilityUnconfirmed { .. })
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
            } if durability_unconfirmed => {
                format!("Publication durability remains unconfirmed after {completed_input_units} completed input units")
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
                )
            }
        };
    }
    if bytes.len() > MAX_RESPONSE_BYTES {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "Execution ended but its required receipt exceeds the response budget; execution is unknown to this connection");
    }
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}

pub enum OperatorAction {
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
    let response = match action {
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
    .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(format!("{status}: {body}").into());
    }
    println!("{body}");
    Ok(())
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
        let unconfirmed = WorkbenchPublicationOutcome::DurabilityUnconfirmed {
            bindings: vec!["visibleWrite".into()],
            detail: "journal outcome unknown".into(),
        };
        assert_eq!(unconfirmed.public_bindings(), &["visibleWrite"]);
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
            .contains("publication durability remains unconfirmed"));
        let uncertain_response = map_failure(uncertain_failure);
        assert!(uncertain_response
            .receipt
            .unwrap()
            .display
            .contains("Publication durability remains unconfirmed"));
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
            Arc::new(|| Box::pin(async { Err("provision must not run".into()) })),
            Arc::new(|_| None),
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
            Arc::new(|| Box::pin(async { Err("provision must not run".into()) })),
            Arc::new(|_| None),
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
        let service = OperatorService::bind(
            socket.clone(),
            Arc::new(move || {
                let actor = actor.clone();
                Box::pin(async move { Ok(actor) })
            }),
            Arc::new(move |actor| (actor == graph_actor).then(Vec::new)),
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
