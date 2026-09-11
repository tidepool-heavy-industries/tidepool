//! Protected local attachment transport. Execution belongs to the resident actor.
pub mod wire;

use axum::{
    body::{to_bytes, Body},
    extract::{Path as RoutePath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tidepool_actor::{
    ActorExitKind, ActorTerminal, KernelInvocationFailure, KernelMessage, LocalActorRef,
};
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus};
use tokio::{net::UnixListener, sync::Mutex};
use wire::*;

type InspectGraph =
    dyn Fn(tidepool_actor::ActorRef) -> Option<Vec<tidepool_actor::ActorGraphNode>> + Send + Sync;

type Provision = dyn Fn() -> BoxFuture<'static, Result<LocalActorRef, String>> + Send + Sync;
#[derive(Clone)]
struct AttachmentState {
    sessions: Arc<Mutex<BTreeMap<String, LocalActorRef>>>,
    open: Arc<std::sync::atomic::AtomicBool>,
    provision: Arc<Provision>,
    inspect_graph: Arc<InspectGraph>,
    socket: PathBuf,
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
    ) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let parent = socket
            .parent()
            .ok_or_else(|| std::io::Error::other("socket needs a parent"))?;
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        let listener = UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let state = AttachmentState {
            sessions: Arc::default(),
            open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            provision,
            inspect_graph,
            socket,
        };
        let app = Router::new()
            .route("/host/operators", get(list).post(new))
            .route("/host/operators/{session}/stop", post(stop))
            .route("/v1/sessions/{session}", get(inspect))
            .route("/v1/sessions/{session}/submit", post(submit))
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
        let _ = std::fs::remove_file(&self.state.socket);
    }
}
fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            code: code.into(),
            message: message.into(),
        }),
    )
        .into_response()
}
async fn new(State(state): State<AttachmentState>) -> Response {
    // Provisioning, like execution, survives loss of the HTTP observer.
    let task = tokio::spawn(async move {
        match (state.provision)().await {
            Ok(actor) => {
                let mut sessions = state.sessions.lock().await;
                if !state.open.load(std::sync::atomic::Ordering::SeqCst) {
                    drop(sessions);
                    let _ = actor
                        .shutdown(ActorTerminal {
                            kind: ActorExitKind::Cancelled,
                            summary: "host attachment service closed".into(),
                        })
                        .await;
                    return error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "Host is shutting down",
                    );
                }
                let session = format!(
                    "operator-{}-{}",
                    actor.identity().id.0,
                    actor.identity().incarnation.0
                );
                sessions.insert(session.clone(), actor);
                Json(Attachment {
                    session,
                    socket: state.socket,
                })
                .into_response()
            }
            Err(detail) => error(StatusCode::SERVICE_UNAVAILABLE, "unavailable", detail),
        }
    });
    task.await.unwrap_or_else(|e| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )
    })
}
async fn list(State(state): State<AttachmentState>) -> Response {
    let sessions = state.sessions.lock().await;
    Json(
        sessions
            .iter()
            .filter(|(_, actor)| actor.terminal().get().is_none())
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
    let Some(actor) = state.sessions.lock().await.remove(&session) else {
        return error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session no longer exists",
        );
    };
    let task = tokio::spawn(async move {
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "operator stopped workbench".into(),
            })
            .await
    });
    match task.await {
        Ok(Ok(_)) => Json(serde_json::json!({"stopped": session})).into_response(),
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Retirement failed",
        ),
    }
}
async fn inspect(
    State(state): State<AttachmentState>,
    RoutePath(session): RoutePath<String>,
) -> Response {
    if state
        .sessions
        .lock()
        .await
        .get(&session)
        .is_some_and(|a| a.terminal().get().is_none())
    {
        Json(SessionInfo {
            protocol_version: PROTOCOL_VERSION,
            session,
        })
        .into_response()
    } else {
        error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "Session no longer exists",
        )
    }
}
async fn graph(
    State(state): State<AttachmentState>,
    RoutePath(session): RoutePath<String>,
) -> Response {
    let sessions = state.sessions.lock().await;
    let Some(actor) = sessions
        .get(&session)
        .filter(|a| a.terminal().get().is_none())
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
    let request = match WorkbenchRequest::from_ghci_input(&input.source) {
        Ok(request) => request,
        Err(e) => return error(StatusCode::BAD_REQUEST, "invalid_request", e.to_string()),
    };
    let sessions = state.sessions.lock().await;
    let Some(actor) = sessions
        .get(&session)
        .filter(|a| a.terminal().get().is_none())
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
            request,
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
            let mut response = map_result(WorkbenchResponse {
                status: WorkbenchRunStatus::Rejected,
                items: failure.receipts,
                next_index: failure.failed_index,
                total: failure.total,
            });
            response
                .blocks
                .push(Block::Diagnostic(failure.detail.clone()));
            if let Some(receipt) = &mut response.receipt {
                if let Some(structured) = &mut receipt.structured {
                    structured["failure"] = failure.detail.into();
                }
            }
            bounded_response(response)
        }
        Ok(Err(e)) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            e.to_string(),
        ),
        Err(e) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            e.to_string(),
        ),
    }
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
            )
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
    Stop { session: String },
}

pub async fn command(
    socket: &Path,
    action: OperatorAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()?;
    let url = match &action {
        OperatorAction::New | OperatorAction::List => "http://localhost/host/operators".to_owned(),
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
    let response = if matches!(action, OperatorAction::List) {
        client.get(url)
    } else {
        client.post(url)
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
mod tests;
