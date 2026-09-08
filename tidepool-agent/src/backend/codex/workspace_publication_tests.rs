use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn exact_publication_request_and_lost_reply_are_not_retried() {
    let identity = crate::interactive::PublicationIdentity {
        pid: 123,
        start_ticks: 42,
        mount_namespace_inode: 1234,
    };
    for operation in [
        PublicationOperation::Begin { expected: None },
        PublicationOperation::Begin {
            expected: Some(identity),
        },
        PublicationOperation::Finish { expected: identity },
    ] {
        for respond in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("native.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let payload = loop {
                    let mut chunk = [0; 2048];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(count, 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&bytes[..end]).unwrap();
                        assert!(header.starts_with("POST /v1/workspace/publication HTTP/1.1\r\n"));
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break serde_json::from_slice::<serde_json::Value>(
                                &bytes[end + 4..end + 4 + length],
                            )
                            .unwrap();
                        }
                    }
                };
                let mut expected = serde_json::json!({"threadId":"bound-thread", "sequence":7, "operation":"begin"});
                if matches!(operation, PublicationOperation::Finish { .. }) {
                    expected["operation"] = "finish".into();
                }
                if !matches!(operation, PublicationOperation::Begin { expected: None }) {
                    expected["expectedIdentity"] =
                        serde_json::json!({"pid":123,"startTicks":42,"mountNamespaceInode":1234});
                }
                assert_eq!(payload, expected);
                if respond {
                    let body = r#"{"status":"ready","pid":123,"startTicks":42,"mountNamespaceInode":1234,"cgroupPath":"/sys/fs/cgroup/writers"}"#;
                    let body = if matches!(operation, PublicationOperation::Finish { .. }) {
                        r#"{"status":"settled"}"#
                    } else {
                        body
                    };
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
                drop(stream);
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err()
                );
            });
            let thread = QueueReadyThread::new(crate::BackendThreadId("bound-thread".into()))
                .with_input_control(Some(socket));
            let reply = request(&thread, NonZeroU64::new(7).unwrap(), operation).await;
            if respond {
                match operation {
                    PublicationOperation::Begin { .. } => assert!(matches!(
                        reply,
                        Ok(PublicationReply::Ready {
                            pid: 123,
                            start_ticks: 42,
                            mount_namespace_inode: 1234,
                            ..
                        })
                    )),
                    PublicationOperation::Finish { .. } => {
                        assert!(matches!(reply, Ok(PublicationReply::Settled)))
                    }
                }
            } else {
                assert!(matches!(
                    reply,
                    Err(AgentBackendError::BackendUnavailable { .. })
                ));
            }
            server.await.unwrap();
        }
    }
}
