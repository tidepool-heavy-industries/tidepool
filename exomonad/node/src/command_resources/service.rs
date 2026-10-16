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
    Policy {},
    Observe {},
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
    SealProducer {
        producer: String,
    },
    Acknowledge {
        actor: String,
        id: String,
    },
    ActorAdmission {},
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
    Error(String),
}

async fn receive<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> std::io::Result<T> {
    let mut frame = String::new();
    let count = BufReader::new(stream.take(MAX_FRAME + 1))
        .read_line(&mut frame)
        .await?;
    if count == 0 || count as u64 > MAX_FRAME || !frame.ends_with('\n') {
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
            if matches!(request, Request::ActorAdmission {}) {
                match owner.admit_actor().await {
                    Ok(_reservation) => {
                        if send(&mut stream, &Response::ActorAdmitted).await.is_ok() {
                            // The connection itself is the startup reservation lease;
                            // best-effort: only its close (any read outcome) matters.
                            stream.read_u8().await.ok();
                        }
                    }
                    Err(error) => {
                        // best-effort: the client may already have disconnected.
                        send(&mut stream, &Response::Error(error.to_string()))
                            .await
                            .ok();
                    }
                }
                return;
            }
            let result = match request {
                Request::Policy {} => Ok(Response::Policy(owner.policy().clone())),
                Request::Observe {} => Ok(Response::Observation(owner.observation())),
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
                Request::SealProducer { producer } => owner
                    .seal_producer(&producer)
                    .map(|()| Response::Acknowledged),
                Request::Acknowledge { actor, id } => owner
                    .acknowledge(&actor, &id)
                    .map(|()| Response::Acknowledged),
                Request::ActorAdmission {} => unreachable!("startup lease handled above"),
            };
            let response = result.unwrap_or_else(|error| Response::Error(error.to_string()));
            // best-effort: the client may already have disconnected or timed out.
            tokio::time::timeout(IO_TIMEOUT, send(&mut stream, &response))
                .await
                .ok();
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
        match client.rpc(Request::Policy {}).await? {
            Response::Policy(actual) if actual == *policy => {
                let observation = client.observation().await?;
                tracing::info!(
                    active = observation.active,
                    historical = observation.historical,
                    retained_allocations = observation.retained_allocations,
                    cleanup_failures = observation.cleanup_failures,
                    sealed_producers = observation.sealed_producers,
                    process_count = observation.process_count,
                    memory_bytes = observation.memory_bytes,
                    memory_pressure_avg10_micros = observation.memory_pressure_avg10_micros,
                    cpu_usage_micros = observation.cpu_usage_micros,
                    io_read_bytes = observation.io_read_bytes,
                    io_write_bytes = observation.io_write_bytes,
                    "command resource service healthy"
                );
                Ok(client)
            }
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

    pub async fn observation(&self) -> std::io::Result<CommandResourceObservation> {
        if let Self::Local(owner) = self {
            return Ok(owner.observation());
        }
        match self.rpc(Request::Observe {}).await? {
            Response::Observation(observation) => Ok(observation),
            _ => Err(io_error("invalid command resource observation response")),
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

    pub async fn seal_producer(&self, producer: &str) -> std::io::Result<()> {
        if let Self::Local(owner) = self {
            return owner.seal_producer(producer);
        }
        match self
            .rpc(Request::SealProducer {
                producer: self.actor(producer),
            })
            .await?
        {
            Response::Acknowledged => Ok(()),
            _ => Err(io_error("invalid producer seal response")),
        }
    }

    pub async fn acknowledge(&self, actor: &str, id: &str) -> std::io::Result<()> {
        if let Self::Local(owner) = self {
            return owner.acknowledge(actor, id);
        }
        match self
            .rpc(Request::Acknowledge {
                actor: self.actor(actor),
                id: id.into(),
            })
            .await?
        {
            Response::Acknowledged => Ok(()),
            _ => Err(io_error("invalid command acknowledgment response")),
        }
    }

    pub async fn admit_actor(&self) -> std::io::Result<StartupLease> {
        match self {
            Self::Local(owner) => owner.admit_actor().await.map(StartupLease::Local),
            Self::Remote { socket, .. } => {
                let mut stream = UnixStream::connect(socket).await?;
                send(&mut stream, &Request::ActorAdmission {}).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        for operation in ["policy", "observe", "actor_admission"] {
            let valid = serde_json::json!({"operation": operation});
            let request: Request = serde_json::from_value(valid.clone()).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), valid);
            let mut stream = UnixStream::connect(&socket).await.unwrap();
            send(&mut stream, &valid).await.unwrap();
            let response: Response = receive(&mut stream).await.unwrap();
            match (operation, response) {
                ("policy", Response::Policy(policy)) => assert_eq!(policy, *owner.policy()),
                ("observe", Response::Observation(_)) => {}
                // The test owner has no real actor slice, so valid admission reaches
                // the authority and reports its unavailable cgroup boundary.
                ("actor_admission", Response::Error(_)) => {}
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
                    send(&mut stream, &invalid).await.unwrap();
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
