//! One private per-user resource authority; connections do not own command jobs.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const PROTOCOL_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {
    version: u32,
    request: Request,
}

const MAX_FRAME: u64 = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const WAIT_WINDOW: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Connect {
        identity: CommandRunIdentity,
    },
    Policy {},
    Observe {},
    Directory {
        actor: CommandProducerIdentity,
    },
    Submit {
        actor: CommandProducerIdentity,
        id: String,
        bytes: u64,
    },
    SubmitNative {
        actor: CommandProducerIdentity,
        id: String,
    },
    Wait {
        actor: CommandProducerIdentity,
        id: String,
    },
    Status {
        actor: CommandProducerIdentity,
        id: String,
    },
    Started {
        actor: CommandProducerIdentity,
        id: String,
    },
    Cancel {
        actor: CommandProducerIdentity,
        id: String,
    },
    SealProducer {
        producer: CommandProducerIdentity,
    },
    Acknowledge {
        actor: CommandProducerIdentity,
        id: String,
    },
    ActorAdmission {
        identity: CommandRunIdentity,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
enum Response {
    Policy(CommandResourcePolicy),
    Observation(CommandResourceObservation),
    Directory(PathBuf),
    Status(CommandResourceStatus),
    ActorAdmitted,
    Acknowledged,
    Error {
        kind: WireErrorKind,
        message: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireErrorKind {
    InvalidInput,
    InvalidData,
    PermissionDenied,
    WouldBlock,
    Other,
}
impl WireErrorKind {
    fn from_io(kind: std::io::ErrorKind) -> Self {
        match kind {
            std::io::ErrorKind::InvalidInput => Self::InvalidInput,
            std::io::ErrorKind::InvalidData => Self::InvalidData,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::WouldBlock => Self::WouldBlock,
            _ => Self::Other,
        }
    }
    fn into_io(self) -> std::io::ErrorKind {
        match self {
            Self::InvalidInput => std::io::ErrorKind::InvalidInput,
            Self::InvalidData => std::io::ErrorKind::InvalidData,
            Self::PermissionDenied => std::io::ErrorKind::PermissionDenied,
            Self::WouldBlock => std::io::ErrorKind::WouldBlock,
            Self::Other => std::io::ErrorKind::Other,
        }
    }
}
impl Response {
    fn error(error: std::io::Error) -> Self {
        Self::Error {
            kind: WireErrorKind::from_io(error.kind()),
            message: error.to_string(),
        }
    }
}
fn invalid_response(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

async fn receive<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> std::io::Result<T> {
    let mut frame = String::new();
    let count = BufReader::new(stream.take(MAX_FRAME + 1))
        .read_line(&mut frame)
        .await?;
    if count == 0 || count as u64 > MAX_FRAME || !frame.ends_with('\n') {
        return Err(invalid_response("invalid resource service frame"));
    }
    serde_json::from_str(&frame)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
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
            let request =
                tokio::time::timeout(IO_TIMEOUT, receive::<RequestFrame>(&mut stream)).await;
            let Ok(Ok(request)) = request else { return };
            if request.version != PROTOCOL_VERSION {
                send(
                    &mut stream,
                    &Response::error(invalid_response(
                        "unsupported command resource protocol version",
                    )),
                )
                .await
                .ok();
                return;
            }
            let request = request.request;
            if let Request::ActorAdmission { identity } = &request {
                match owner.admit_actor_for(identity).await {
                    Ok(_reservation) => {
                        if send(&mut stream, &Response::ActorAdmitted).await.is_ok() {
                            // The connection itself is the startup reservation lease;
                            // best-effort: only its close (any read outcome) matters.
                            stream.read_u8().await.ok();
                        }
                    }
                    Err(error) => {
                        // best-effort: the client may already have disconnected.
                        send(&mut stream, &Response::error(error)).await.ok();
                    }
                }
                return;
            }
            let result = match request {
                Request::Connect { identity } => {
                    owner.admit_run(&identity).map(|()| Response::Acknowledged)
                }
                Request::Policy {} => Ok(Response::Policy(owner.policy().clone())),
                Request::Observe {} => Ok(Response::Observation(owner.observation())),
                Request::Directory { actor } => {
                    owner.producer_directory(&actor).map(Response::Directory)
                }
                Request::Submit { actor, id, bytes } => owner
                    .producer_key(&actor, true)
                    .and_then(|key| owner.submit(&key, &id, bytes))
                    .map(Response::Status),
                Request::SubmitNative { actor, id } => owner
                    .producer_key(&actor, true)
                    .and_then(|key| owner.submit_native(&key, &id))
                    .map(Response::Status),
                Request::Wait { actor, id } => {
                    let Ok(actor) = owner.producer_key(&actor, false) else {
                        return;
                    };
                    match tokio::time::timeout(WAIT_WINDOW, owner.wait(&actor, &id)).await {
                        Ok(result) => result.map(Response::Status),
                        Err(_) => owner.status(&actor, &id).map(Response::Status),
                    }
                }
                Request::Status { actor, id } => owner
                    .producer_key(&actor, false)
                    .and_then(|key| owner.status(&key, &id))
                    .map(Response::Status),
                Request::Started { actor, id } => owner
                    .producer_key(&actor, true)
                    .and_then(|key| owner.started(&key, &id))
                    .map(Response::Status),
                Request::Cancel { actor, id } => owner
                    .producer_key(&actor, false)
                    .and_then(|key| owner.cancel(&key, &id))
                    .map(Response::Status),
                Request::SealProducer { producer } => owner
                    .producer_key(&producer, false)
                    .and_then(|key| owner.seal_producer(&key))
                    .map(|()| Response::Acknowledged),
                Request::Acknowledge { actor, id } => owner
                    .producer_key(&actor, false)
                    .and_then(|key| owner.acknowledge(&key, &id))
                    .map(|()| Response::Acknowledged),
                Request::ActorAdmission { .. } => unreachable!("startup lease handled above"),
            };
            let response = result.unwrap_or_else(|error| Response::error(error));
            // best-effort: the client may already have disconnected or timed out.
            tokio::time::timeout(IO_TIMEOUT, send(&mut stream, &response))
                .await
                .ok();
        });
    }
}

pub struct CommandResourceClient {
    backend: ClientBackend,
    identity: CommandRunIdentity,
}
enum ClientBackend {
    Local(Arc<CommandResources>),
    Remote { socket: PathBuf },
}

impl CommandResourceClient {
    pub fn local(
        owner: Arc<CommandResources>,
        identity: CommandRunIdentity,
    ) -> std::io::Result<Arc<Self>> {
        owner.admit_run(&identity)?;
        Ok(Arc::new(Self {
            backend: ClientBackend::Local(owner),
            identity,
        }))
    }

    pub async fn connect(
        socket: PathBuf,
        identity: CommandRunIdentity,
        policy: &CommandResourcePolicy,
    ) -> std::io::Result<Arc<Self>> {
        identity.validate()?;
        let client = Arc::new(Self {
            backend: ClientBackend::Remote { socket },
            identity,
        });
        match client.rpc(Request::Policy {}).await? {
            Response::Policy(actual) if actual == *policy => {}
            Response::Policy(_) => return Err(invalid_response(
                "shared command resource policy differs; drain allocations before service upgrade",
            )),
            _ => return Err(invalid_response("invalid resource service policy response")),
        }
        match client
            .rpc(Request::Connect {
                identity: client.identity.clone(),
            })
            .await?
        {
            Response::Acknowledged => Ok(client),
            _ => Err(invalid_response(
                "invalid resource epoch admission response",
            )),
        }
    }

    fn actor(&self, actor: &CommandActorIdentity) -> CommandProducerIdentity {
        CommandProducerIdentity {
            run: self.identity.clone(),
            actor: *actor,
        }
    }

    async fn rpc(&self, request: Request) -> std::io::Result<Response> {
        let ClientBackend::Remote { socket } = &self.backend else {
            return Err(io_error("local resources have no transport"));
        };
        tokio::time::timeout(WAIT_WINDOW + IO_TIMEOUT, async {
            let mut stream = UnixStream::connect(socket).await?;
            send(
                &mut stream,
                &RequestFrame {
                    version: PROTOCOL_VERSION,
                    request,
                },
            )
            .await?;
            match receive(&mut stream).await? {
                Response::Error { kind, message } => {
                    Err(std::io::Error::new(kind.into_io(), message))
                }
                response => Ok(response),
            }
        })
        .await
        .map_err(|_| io_error("resource service unavailable; accepted commands remain retained"))?
    }

    async fn status_rpc(&self, request: Request) -> std::io::Result<CommandResourceStatus> {
        match self.rpc(request).await? {
            Response::Status(status) => Ok(status),
            _ => Err(invalid_response("invalid command resource response")),
        }
    }

    pub async fn actor_directory(&self, actor: &CommandActorIdentity) -> std::io::Result<PathBuf> {
        if let ClientBackend::Local(owner) = &self.backend {
            return owner.producer_directory(&self.actor(actor));
        }
        match self
            .rpc(Request::Directory {
                actor: self.actor(actor),
            })
            .await?
        {
            Response::Directory(path) => Ok(path),
            _ => Err(invalid_response("invalid resource directory response")),
        }
    }

    pub async fn observation(&self) -> std::io::Result<CommandResourceObservation> {
        if let ClientBackend::Local(owner) = &self.backend {
            return Ok(owner.observation());
        }
        match self.rpc(Request::Observe {}).await? {
            Response::Observation(observation) => Ok(observation),
            _ => Err(invalid_response(
                "invalid command resource observation response",
            )),
        }
    }

    pub async fn submit(
        &self,
        actor: &CommandActorIdentity,
        id: &str,
        bytes: u64,
    ) -> std::io::Result<CommandResourceStatus> {
        if let ClientBackend::Local(owner) = &self.backend {
            let key = owner.producer_key(&self.actor(actor), true)?;
            return owner.submit(&key, id, bytes);
        }
        self.status_rpc(Request::Submit {
            actor: self.actor(actor),
            id: id.into(),
            bytes,
        })
        .await
    }

    pub async fn wait(
        &self,
        actor: &CommandActorIdentity,
        id: &str,
    ) -> std::io::Result<CommandResourceStatus> {
        if let ClientBackend::Local(owner) = &self.backend {
            let key = owner.producer_key(&self.actor(actor), false)?;
            return owner.wait(&key, id).await;
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

    pub async fn acquire(
        &self,
        actor: &CommandActorIdentity,
        id: &str,
    ) -> std::io::Result<CommandResourceStatus> {
        match &self.backend {
            ClientBackend::Local(owner) => {
                let key = owner.producer_key(&self.actor(actor), true)?;
                owner.submit_native(&key, id)?;
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

    pub async fn started(
        &self,
        actor: &CommandActorIdentity,
        id: &str,
    ) -> std::io::Result<CommandResourceStatus> {
        if let ClientBackend::Local(owner) = &self.backend {
            let key = owner.producer_key(&self.actor(actor), true)?;
            return owner.started(&key, id);
        }
        self.status_rpc(Request::Started {
            actor: self.actor(actor),
            id: id.into(),
        })
        .await
    }

    pub async fn status(
        &self,
        actor: &CommandActorIdentity,
        id: &str,
    ) -> std::io::Result<CommandResourceStatus> {
        if let ClientBackend::Local(owner) = &self.backend {
            let key = owner.producer_key(&self.actor(actor), false)?;
            return owner.status(&key, id);
        }
        self.status_rpc(Request::Status {
            actor: self.actor(actor),
            id: id.into(),
        })
        .await
    }

    pub async fn cancel(
        &self,
        actor: &CommandActorIdentity,
        id: &str,
    ) -> std::io::Result<CommandResourceStatus> {
        if let ClientBackend::Local(owner) = &self.backend {
            let key = owner.producer_key(&self.actor(actor), false)?;
            return owner.cancel(&key, id);
        }
        self.status_rpc(Request::Cancel {
            actor: self.actor(actor),
            id: id.into(),
        })
        .await
    }

    pub async fn seal_producer(&self, producer: &CommandActorIdentity) -> std::io::Result<()> {
        if let ClientBackend::Local(owner) = &self.backend {
            let key = owner.producer_key(&self.actor(producer), false)?;
            return owner.seal_producer(&key);
        }
        match self
            .rpc(Request::SealProducer {
                producer: self.actor(producer),
            })
            .await?
        {
            Response::Acknowledged => Ok(()),
            _ => Err(invalid_response("invalid producer seal response")),
        }
    }

    pub async fn acknowledge(&self, actor: &CommandActorIdentity, id: &str) -> std::io::Result<()> {
        if let ClientBackend::Local(owner) = &self.backend {
            let key = owner.producer_key(&self.actor(actor), false)?;
            return owner.acknowledge(&key, id);
        }
        match self
            .rpc(Request::Acknowledge {
                actor: self.actor(actor),
                id: id.into(),
            })
            .await?
        {
            Response::Acknowledged => Ok(()),
            _ => Err(invalid_response("invalid command acknowledgment response")),
        }
    }

    pub async fn admit_actor(&self) -> std::io::Result<StartupLease> {
        match &self.backend {
            ClientBackend::Local(owner) => owner
                .admit_actor_for(&self.identity)
                .await
                .map(StartupLease::Local),
            ClientBackend::Remote { socket } => {
                let mut stream = UnixStream::connect(socket).await?;
                send(
                    &mut stream,
                    &RequestFrame {
                        version: PROTOCOL_VERSION,
                        request: Request::ActorAdmission {
                            identity: self.identity.clone(),
                        },
                    },
                )
                .await?;
                match receive(&mut stream).await? {
                    Response::ActorAdmitted => Ok(StartupLease::Remote(stream)),
                    Response::Error { kind, message } => {
                        Err(std::io::Error::new(kind.into_io(), message))
                    }
                    _ => Err(invalid_response("invalid actor admission response")),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_requires_version_and_exact_identity_fields() {
        assert!(
            serde_json::from_value::<RequestFrame>(serde_json::json!({"operation":"policy"}))
                .is_err()
        );
        assert!(serde_json::from_value::<RequestFrame>(
            serde_json::json!({"version":2,"request":{"operation":"actor_admission"}})
        )
        .is_err());
        assert!(serde_json::from_value::<RequestFrame>(
            serde_json::json!({"version":2,"request":{"operation":"policy","foreign":true}})
        )
        .is_err());
    }

    #[tokio::test]
    async fn complete_frame_boundary_round_trip_and_oversize_refusal() {
        for size in [MAX_FRAME - 1, MAX_FRAME, MAX_FRAME + 1] {
            let value = format!("λ{}", "x".repeat(size as usize - 5));
            assert_eq!(serde_json::to_vec(&value).unwrap().len() as u64 + 1, size);
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            if size <= MAX_FRAME {
                let (sent, received) =
                    tokio::join!(send(&mut writer, &value), receive::<String>(&mut reader));
                sent.unwrap();
                assert_eq!(received.unwrap(), value);
            } else {
                assert!(send(&mut writer, &value).await.is_err());
                drop(writer);
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes).await.unwrap();
                assert!(bytes.is_empty());
                let (mut writer, mut reader) = UnixStream::pair().unwrap();
                let mut bytes = serde_json::to_vec(&value).unwrap();
                bytes.push(b'\n');
                let (sent, received) =
                    tokio::join!(writer.write_all(&bytes), receive::<String>(&mut reader));
                sent.unwrap();
                assert!(received.is_err());
            }
        }
    }

    #[tokio::test]
    async fn empty_requests_refuse_surplus_fields_before_resource_dispatch() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("resources.sock");
        let owner = Arc::new(super::super::tests::owner(directory.path()));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, owner.clone()));
        for operation in ["policy", "observe"] {
            let valid = serde_json::json!({"operation": operation});
            let request: Request = serde_json::from_value(valid.clone()).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), valid);
            let mut stream = UnixStream::connect(&socket).await.unwrap();
            let valid_frame = serde_json::json!({"version": PROTOCOL_VERSION, "request": valid});
            send(&mut stream, &valid_frame).await.unwrap();
            let response: Response = receive(&mut stream).await.unwrap();
            match (operation, response) {
                ("policy", Response::Policy(policy)) => assert_eq!(policy, *owner.policy()),
                ("observe", Response::Observation(_)) => {}
                _ => panic!("valid request did not reach its resource authority"),
            }
            drop(stream);
            for field in ["foreign", "actor", "id", "bytes", "", "λ"] {
                for value in [
                    serde_json::Value::Null,
                    serde_json::json!(true),
                    serde_json::json!(1),
                    serde_json::json!("extra"),
                    serde_json::json!([]),
                    serde_json::json!({}),
                ] {
                    let mut invalid = valid.clone();
                    invalid[field] = value;
                    let mut stream = UnixStream::connect(&socket).await.unwrap();
                    let invalid_frame =
                        serde_json::json!({"version": PROTOCOL_VERSION, "request": invalid});
                    send(&mut stream, &invalid_frame).await.unwrap();
                    let result = tokio::time::timeout(Duration::from_secs(2), stream.read_u8())
                        .await
                        .unwrap();
                    assert_eq!(
                        result.unwrap_err().kind(),
                        std::io::ErrorKind::UnexpectedEof
                    );
                    assert_eq!(*owner.actor_starts.lock(), 0);
                }
            }
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}
