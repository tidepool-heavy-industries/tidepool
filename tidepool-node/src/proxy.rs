//! Authenticated actor-scoped handoff from a stdio MCP child to the daemon.
//!
//! The sidecar performs one typed line handshake and then gets out of the way:
//! every subsequent byte is the original MCP stream. Policy and tool dispatch
//! remain in the daemon.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tidepool_actor::ActorRef;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const NODE_PROTOCOL_VERSION: u32 = 1;

/// Launch-scoped bearer credential minted by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeCredential(pub String);

/// The only pre-MCP frame on a node connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeHandshake {
    pub version: u32,
    pub actor: ActorRef,
    pub credential: NodeCredential,
}

impl NodeHandshake {
    pub fn current(actor: ActorRef, credential: NodeCredential) -> Self {
        Self {
            version: NODE_PROTOCOL_VERSION,
            actor,
            credential,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum HandshakeReply {
    Accepted { version: u32 },
    Rejected { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum NodeProxyError {
    #[error("node proxy io: {0}")]
    Io(#[from] std::io::Error),
    #[error("node proxy protocol: {0}")]
    Protocol(String),
    #[error("node proxy refused: {0}")]
    Refused(String),
}

/// Authenticate an accepted socket and return the untouched MCP byte stream.
///
/// `authorize` is the daemon's registry/grant decision. This transport helper
/// deliberately has no fallback policy of its own.
pub async fn accept_proxy(
    stream: UnixStream,
    authorize: impl FnOnce(&NodeHandshake) -> Result<(), String>,
) -> Result<(NodeHandshake, UnixStream), NodeProxyError> {
    let mut reader = BufReader::new(stream);
    let handshake: NodeHandshake = read_json_line(&mut reader).await?;
    let validation = if handshake.version == NODE_PROTOCOL_VERSION {
        authorize(&handshake)
    } else {
        Err(format!(
            "unsupported node protocol version {} (expected {})",
            handshake.version, NODE_PROTOCOL_VERSION
        ))
    };
    match validation {
        Ok(()) => {
            write_json_line(
                reader.get_mut(),
                &HandshakeReply::Accepted {
                    version: NODE_PROTOCOL_VERSION,
                },
            )
            .await?;
            Ok((handshake, reader.into_inner()))
        }
        Err(reason) => {
            write_json_line(
                reader.get_mut(),
                &HandshakeReply::Rejected {
                    reason: reason.clone(),
                },
            )
            .await?;
            Err(NodeProxyError::Refused(reason))
        }
    }
}

/// Connect and authenticate the sidecar before exposing its MCP stream.
pub async fn connect_proxy(
    endpoint: &Path,
    handshake: &NodeHandshake,
) -> Result<UnixStream, NodeProxyError> {
    let mut stream = UnixStream::connect(endpoint).await?;
    write_json_line(&mut stream, handshake).await?;
    match read_json_line::<_, HandshakeReply>(&mut BufReader::new(&mut stream)).await? {
        HandshakeReply::Accepted { version } if version == NODE_PROTOCOL_VERSION => Ok(stream),
        HandshakeReply::Accepted { version } => Err(NodeProxyError::Protocol(format!(
            "daemon accepted with protocol version {version}, expected {NODE_PROTOCOL_VERSION}"
        ))),
        HandshakeReply::Rejected { reason } => Err(NodeProxyError::Refused(reason)),
    }
}

/// Run the thin stdio sidecar after its actor identity and credential have
/// been supplied by the daemon-owned launch environment.
pub async fn proxy_stdio(endpoint: &Path, handshake: &NodeHandshake) -> Result<(), NodeProxyError> {
    let stream = connect_proxy(endpoint, handshake).await?;
    let (mut socket_read, mut socket_write) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let upload = async {
        tokio::io::copy(&mut stdin, &mut socket_write).await?;
        socket_write.shutdown().await
    };
    let download = async {
        tokio::io::copy(&mut socket_read, &mut stdout).await?;
        stdout.flush().await
    };
    tokio::try_join!(upload, download)?;
    Ok(())
}

async fn read_json_line<R, T>(reader: &mut R) -> Result<T, NodeProxyError>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Err(NodeProxyError::Protocol(
            "connection closed during node handshake".to_string(),
        ));
    }
    serde_json::from_str(&line)
        .map_err(|error| NodeProxyError::Protocol(format!("invalid handshake frame: {error}")))
}

async fn write_json_line<T: Serialize>(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
) -> Result<(), NodeProxyError> {
    let mut line = serde_json::to_vec(value)
        .map_err(|error| NodeProxyError::Protocol(format!("cannot encode handshake: {error}")))?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_actor::{ActorId, ActorRef};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    fn hello() -> NodeHandshake {
        NodeHandshake::current(
            ActorRef::first(ActorId(7)),
            NodeCredential("launch-secret".to_string()),
        )
    }

    #[tokio::test]
    async fn accepted_handshake_leaves_following_mcp_bytes_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("node.sock");
        let listener = UnixListener::bind(&endpoint).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (handshake, mut stream) = accept_proxy(stream, |candidate| {
                (candidate.credential.0 == "launch-secret")
                    .then_some(())
                    .ok_or_else(|| "bad credential".to_string())
            })
            .await
            .unwrap();
            let mut body = [0; 12];
            stream.read_exact(&mut body).await.unwrap();
            (handshake, body)
        });

        let mut client = connect_proxy(&endpoint, &hello()).await.unwrap();
        client.write_all(b"mcp bytes!!!").await.unwrap();
        let (handshake, body) = server.await.unwrap();
        assert_eq!(handshake, hello());
        assert_eq!(&body, b"mcp bytes!!!");
    }

    #[tokio::test]
    async fn refusal_is_observed_by_both_ends() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("node.sock");
        let listener = UnixListener::bind(&endpoint).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            accept_proxy(stream, |_| Err("stale actor".to_string())).await
        });
        assert!(matches!(
            connect_proxy(&endpoint, &hello()).await,
            Err(NodeProxyError::Refused(reason)) if reason == "stale actor"
        ));
        assert!(matches!(
            server.await.unwrap(),
            Err(NodeProxyError::Refused(reason)) if reason == "stale actor"
        ));
    }
}
