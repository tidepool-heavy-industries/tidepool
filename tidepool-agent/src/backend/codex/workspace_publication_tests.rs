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

#[test]
fn namespace_publication_server() {
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;
    let Some(socket) = std::env::var_os("SHOAL_TEST_PUBLICATION_SOCKET") else {
        return;
    };
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    let mut bytes = Vec::new();
    while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let headers = String::from_utf8(bytes).unwrap();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse().unwrap())
        })
        .unwrap();
    stream.read_exact(&mut vec![0; length]).unwrap();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let start: u64 = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    let body = serde_json::json!({"status":"ready", "pid":std::process::id(),
        "startTicks":start, "mountNamespaceInode":std::fs::metadata("/proc/self/ns/mnt").unwrap().ino(),
        "cgroupPath":"/sys/fs/cgroup/writers"}).to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    drop(stream);
    // Keep the exact peer alive until its caller has checked host-visible identity.
    let _ = std::io::stdin().read(&mut [0]);
}

#[tokio::test]
async fn publication_peer_pid_is_host_visible_across_pid_namespace() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("native.sock");
    let mut child = tokio::process::Command::new("bwrap")
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
            "--proc",
            "/proc",
            "--",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "backend::codex::workspace_publication::tests::namespace_publication_server",
            "--nocapture",
        ])
        .env("SHOAL_TEST_PUBLICATION_SOCKET", &socket)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !socket.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "namespace fixture exited before listening"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let thread = QueueReadyThread::new(crate::BackendThreadId("bound-thread".into()))
        .with_input_control(Some(socket));
    let reply = request(
        &thread,
        NonZeroU64::new(1).unwrap(),
        PublicationOperation::Begin { expected: None },
    )
    .await
    .unwrap();
    let PublicationReply::Ready {
        peer_pid,
        pid,
        start_ticks,
        mount_namespace_inode,
        ..
    } = reply
    else {
        panic!("{reply:?}")
    };
    assert_ne!(peer_pid, pid);
    assert_eq!(pid, 2);
    assert_eq!(
        std::fs::metadata(format!("/proc/{peer_pid}/ns/mnt"))
            .unwrap()
            .ino(),
        mount_namespace_inode
    );
    let stat = std::fs::read_to_string(format!("/proc/{peer_pid}/stat")).unwrap();
    assert_eq!(
        stat.rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse::<u64>()
            .unwrap(),
        start_ticks
    );
    drop(child.stdin.take());
    assert!(
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
