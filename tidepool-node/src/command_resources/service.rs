//! One private per-user resource authority; connections do not own command jobs.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MAX_FRAME: u64 = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const WAIT_WINDOW: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Policy,
    Directory {
        actor: String,
    },
    Submit {
        actor: String,
        id: String,
        bytes: u64,
    },
    SubmitNative {
        actor: String,
        id: String,
    },
    Wait {
        actor: String,
        id: String,
    },
    Status {
        actor: String,
        id: String,
    },
    Started {
        actor: String,
        id: String,
    },
    Cancel {
        actor: String,
        id: String,
    },
    ActorAdmission,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
enum Response {
    Policy(CommandResourcePolicy),
    Directory(PathBuf),
    Status(CommandResourceStatus),
    ActorAdmitted,
    Error(String),
}

async fn receive<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> std::io::Result<T> {
    let mut frame = String::new();
    let count = BufReader::new(stream.take(MAX_FRAME))
        .read_line(&mut frame)
        .await?;
    if count == 0 || count as u64 == MAX_FRAME || !frame.ends_with('\n') {
        return Err(io_error("invalid resource service frame"));
    }
    serde_json::from_str(&frame).map_err(|error| io_error(error.to_string()))
}
async fn send<T: Serialize>(stream: &mut UnixStream, value: &T) -> std::io::Result<()> {
    let mut frame = serde_json::to_vec(value).map_err(|error| io_error(error.to_string()))?;
    frame.push(b'\n');
    if frame.len() as u64 > MAX_FRAME {
        return Err(io_error("resource service frame exceeds limit"));
    }
    stream.write_all(&frame).await
}

pub async fn serve(listener: UnixListener, owner: Arc<CommandResources>) -> std::io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let owner = owner.clone();
        tokio::spawn(async move {
            let request = tokio::time::timeout(IO_TIMEOUT, receive::<Request>(&mut stream)).await;
            let Ok(Ok(request)) = request else { return };
            if matches!(request, Request::ActorAdmission) {
                match owner.admit_actor().await {
                    Ok(_reservation) => {
                        if send(&mut stream, &Response::ActorAdmitted).await.is_ok() {
                            // The connection itself is the startup reservation lease.
                            let _ = stream.read_u8().await;
                        }
                    }
                    Err(error) => {
                        let _ = send(&mut stream, &Response::Error(error.to_string())).await;
                    }
                }
                return;
            }
            let result = match request {
                Request::Policy => Ok(Response::Policy(owner.policy().clone())),
                Request::Directory { actor } => {
                    owner.actor_directory(&actor).map(Response::Directory)
                }
                Request::Submit { actor, id, bytes } => {
                    owner.submit(&actor, &id, bytes).map(Response::Status)
                }
                Request::SubmitNative { actor, id } => {
                    owner.submit_native(&actor, &id).map(Response::Status)
                }
                Request::Wait { actor, id } => {
                    match tokio::time::timeout(WAIT_WINDOW, owner.wait(&actor, &id)).await {
                        Ok(result) => result.map(Response::Status),
                        Err(_) => owner.status(&actor, &id).map(Response::Status),
                    }
                }
                Request::Status { actor, id } => owner.status(&actor, &id).map(Response::Status),
                Request::Started { actor, id } => owner.started(&actor, &id).map(Response::Status),
                Request::Cancel { actor, id } => owner.cancel(&actor, &id).map(Response::Status),
                Request::ActorAdmission => unreachable!("startup lease handled above"),
            };
            let response = result.unwrap_or_else(|error| Response::Error(error.to_string()));
            let _ = tokio::time::timeout(IO_TIMEOUT, send(&mut stream, &response)).await;
        });
    }
}

pub enum CommandResourceClient {
    Local(Arc<CommandResources>),
    Remote { socket: PathBuf, run: String },
}

impl CommandResourceClient {
    pub fn local(owner: Arc<CommandResources>) -> Arc<Self> {
        Arc::new(Self::Local(owner))
    }

    pub async fn connect(
        socket: PathBuf,
        run: String,
        policy: &CommandResourcePolicy,
    ) -> std::io::Result<Arc<Self>> {
        if !valid_key(&run) {
            return Err(io_error("invalid resource run identity"));
        }
        let client = Arc::new(Self::Remote { socket, run });
        match client.rpc(Request::Policy).await? {
            Response::Policy(actual) if actual == *policy => Ok(client),
            Response::Policy(_) => Err(io_error("shared command resource policy differs; stop allocations before changing the service policy")),
            _ => Err(io_error("invalid resource service policy response")),
        }
    }

    fn actor(&self, actor: &str) -> String {
        match self {
            Self::Local(_) => actor.to_owned(),
            Self::Remote { run, .. } => format!("{run}-{actor}"),
        }
    }

    async fn rpc(&self, request: Request) -> std::io::Result<Response> {
        let Self::Remote { socket, .. } = self else {
            return Err(io_error("local resources have no transport"));
        };
        tokio::time::timeout(WAIT_WINDOW + IO_TIMEOUT, async {
            let mut stream = UnixStream::connect(socket).await?;
            send(&mut stream, &request).await?;
            match receive(&mut stream).await? {
                Response::Error(error) => Err(io_error(error)),
                response => Ok(response),
            }
        })
        .await
        .map_err(|_| io_error("resource service unavailable; accepted commands remain retained"))?
    }

    async fn status_rpc(&self, request: Request) -> std::io::Result<CommandResourceStatus> {
        match self.rpc(request).await? {
            Response::Status(status) => Ok(status),
            _ => Err(io_error("invalid command resource response")),
        }
    }

    pub async fn actor_directory(&self, actor: &str) -> std::io::Result<PathBuf> {
        if let Self::Local(owner) = self {
            return owner.actor_directory(actor);
        }
        match self
            .rpc(Request::Directory {
                actor: self.actor(actor),
            })
            .await?
        {
            Response::Directory(path) => Ok(path),
            _ => Err(io_error("invalid resource directory response")),
        }
    }

    pub async fn submit(
        &self,
        actor: &str,
        id: &str,
        bytes: u64,
    ) -> std::io::Result<CommandResourceStatus> {
        if let Self::Local(owner) = self {
            return owner.submit(actor, id, bytes);
        }
        self.status_rpc(Request::Submit {
            actor: self.actor(actor),
            id: id.into(),
            bytes,
        })
        .await
    }

    pub async fn wait(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        if let Self::Local(owner) = self {
            return owner.wait(actor, id).await;
        }
        loop {
            let status = self
                .status_rpc(Request::Wait {
                    actor: self.actor(actor),
                    id: id.into(),
                })
                .await?;
            if !status.is_queued() {
                return Ok(status);
            }
        }
    }

    pub async fn acquire(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        match self {
            Self::Local(owner) => {
                owner.submit_native(actor, id)?;
            }
            _ => {
                self.status_rpc(Request::SubmitNative {
                    actor: self.actor(actor),
                    id: id.into(),
                })
                .await?;
            }
        }
        self.wait(actor, id).await
    }

    pub async fn started(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        if let Self::Local(owner) = self {
            return owner.started(actor, id);
        }
        self.status_rpc(Request::Started {
            actor: self.actor(actor),
            id: id.into(),
        })
        .await
    }

    pub async fn status(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        if let Self::Local(owner) = self {
            return owner.status(actor, id);
        }
        self.status_rpc(Request::Status {
            actor: self.actor(actor),
            id: id.into(),
        })
        .await
    }

    pub async fn cancel(&self, actor: &str, id: &str) -> std::io::Result<CommandResourceStatus> {
        if let Self::Local(owner) = self {
            return owner.cancel(actor, id);
        }
        self.status_rpc(Request::Cancel {
            actor: self.actor(actor),
            id: id.into(),
        })
        .await
    }

    pub async fn admit_actor(&self) -> std::io::Result<StartupLease> {
        match self {
            Self::Local(owner) => owner.admit_actor().await.map(StartupLease::Local),
            Self::Remote { socket, .. } => {
                let mut stream = UnixStream::connect(socket).await?;
                send(&mut stream, &Request::ActorAdmission).await?;
                match receive(&mut stream).await? {
                    Response::ActorAdmitted => Ok(StartupLease::Remote(stream)),
                    Response::Error(error) => Err(io_error(error)),
                    _ => Err(io_error("invalid actor admission response")),
                }
            }
        }
    }
}

/// Dropping this lease releases only the temporary actor-start reservation.
pub enum StartupLease {
    Local(ActorStartReservation),
    Remote(UnixStream),
}
