//! Native start-or-steer input, confirmed at the persisted user-message boundary.
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt};

use crate::{QueueReadyThread, UpdatePresentationError};

fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
enum RolloutRecord {
    EventMsg(InputEvent),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InputEvent {
    UserMessage {
        client_id: Option<String>,
    },
    ItemCompleted {
        item: InputItem,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum InputItem {
    UserMessage {
        client_id: Option<String>,
    },
    #[serde(other)]
    Other,
}

fn confirms(line: &str, key: &str) -> bool {
    matches!(serde_json::from_str::<RolloutRecord>(line),
        Ok(RolloutRecord::EventMsg(InputEvent::UserMessage { client_id: Some(id) }
            | InputEvent::ItemCompleted { item: InputItem::UserMessage { client_id: Some(id) } })) if id == key)
}

fn input_client(socket: &std::path::Path) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .unix_socket(socket)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .http1_only()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(failure)
}

async fn submit(
    client: &reqwest::Client,
    thread: &str,
    key: &str,
    message: &str,
) -> Result<(), UpdatePresentationError> {
    // This endpoint belongs to the already-running TUI. It forwards through
    // that TUI's app-server handle; no daemon discovery or thread resume occurs.
    let response = client
        .post("http://localhost/v1/input")
        .json(&serde_json::json!({
            "threadId": thread,
            "clientUserMessageId": key,
            "message": message,
        }))
        .send()
        .await
        .map_err(|error| {
            if error.is_connect() {
                UpdatePresentationError::NotSubmitted(failure(error))
            } else {
                UpdatePresentationError::Unconfirmed(failure(error))
            }
        })?;
    if response.status() != reqwest::StatusCode::ACCEPTED {
        return Err(UpdatePresentationError::Unconfirmed(format!(
            "native input returned HTTP {}",
            response.status()
        )));
    }
    Ok(())
}

// Only complete JSONL records count. EOF on a live rollout means wait for
// more bytes; a partially appended correlation record is not confirmation.
async fn await_confirmation<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    key: &str,
) -> Result<(), String> {
    let mut line = String::new();
    loop {
        let count = reader.read_line(&mut line).await.map_err(failure)?;
        if line.ends_with('\n') {
            if confirms(&line, key) {
                return Ok(());
            }
            line.clear();
        }
        if count == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[tracing::instrument(skip_all, fields(thread = %thread.id().0, update_key = key), err(level = "warn"))]
pub(super) async fn present(
    thread: &QueueReadyThread,
    key: &str,
    message: &str,
) -> Result<(), UpdatePresentationError> {
    use UpdatePresentationError::{NotSubmitted, Unconfirmed};
    let prepared = async {
        let socket = thread.input_control_socket()
            .ok_or_else(|| "bound TUI did not advertise active-input support; queue readiness is not steering readiness".to_string())?;
        let client = input_client(socket)?;
        let sessions = super::super::isolation::codex_home().join("sessions");
        let thread_id = thread.id().0.clone();
        let path =
            tokio::task::spawn_blocking(move || super::find_rollout(&sessions, &thread_id, 4))
                .await
                .map_err(failure)?
                .map_err(failure)?
                .ok_or_else(|| failure("bound conversation rollout is unavailable"))?;
        let mut file = tokio::fs::File::open(path).await.map_err(failure)?;
        file.seek(std::io::SeekFrom::End(0))
            .await
            .map_err(failure)?;
        let reader = tokio::io::BufReader::new(file);
        Ok::<_, String>((reader, client))
    }
    .await
    .map_err(NotSubmitted)?;
    let (mut reader, client) = prepared;
    let result = tokio::time::timeout(Duration::from_secs(300), async {
        submit(&client, &thread.id().0, key, message).await?;
        // Pending input is recorded after the current sampling/tool boundary,
        // before building the next model request. RPC acceptance alone is not
        // presentation. Neither uncertain submission nor observation is retried.
        await_confirmation(&mut reader, key)
            .await
            .map_err(Unconfirmed)
    })
    .await
    .map_err(|_| Unconfirmed(failure("timed out awaiting model-visible input")))
    .and_then(|result| result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queue_only_binding_rejects_input_before_submission() {
        let thread = QueueReadyThread::new(crate::BackendThreadId("thread-1".into()));
        assert!(matches!(
            present(&thread, "update-7", "contract").await,
            Err(UpdatePresentationError::NotSubmitted(_))
        ));
    }

    #[tokio::test]
    async fn unavailable_socket_is_not_submitted() {
        let directory = tempfile::tempdir().unwrap();
        let client = input_client(&directory.path().join("missing.sock")).unwrap();
        assert!(matches!(
            submit(&client, "thread-1", "update-7", "contract").await,
            Err(UpdatePresentationError::NotSubmitted(_))
        ));
    }

    #[test]
    fn only_the_exact_persisted_user_message_confirms_presentation() {
        assert!(confirms(
            r#"{"type":"event_msg","payload":{"type":"user_message","client_id":"update-7","message":"clickable tabs"}}"#,
            "update-7"
        ));
        assert!(!confirms(
            r#"{"type":"event_msg","payload":{"type":"user_message","client_id":"other"}}"#,
            "update-7"
        ));
        assert!(!confirms(
            r#"{"type":"event_msg","payload":{"type":"agent_message","client_id":"update-7"}}"#,
            "update-7"
        ));
        assert!(!confirms(
            r#"{"id":1,"result":{"turn":{"id":"turn-1"}}}"#,
            "update-7"
        ));
        assert!(confirms(
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","client_id":"update-7"}}}"#,
            "update-7"
        ));
        for line in [
            r#"{"type":"event_msg","payload":{"type":"item_started","item":{"type":"UserMessage","client_id":"update-7"}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","client_id":"other"}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","client_id":"update-7"}}}"#,
        ] {
            assert!(!confirms(line, "update-7"));
        }
    }

    #[tokio::test]
    async fn native_input_is_sent_once_with_exact_correlation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for status in [Some("202 Accepted"), Some("409 Conflict"), None] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("input.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let peer = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let size = stream.read(&mut chunk).await.unwrap();
                    assert!(size > 0);
                    bytes.extend_from_slice(&chunk[..size]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&bytes[..end]).unwrap();
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                if let Some(status) = status {
                    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                }
                drop(stream);
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "uncertain input must not be retried"
                );
                bytes
            });
            let client = input_client(&socket).unwrap();
            let result = submit(&client, "thread-1", "update-7", "clickable tabs").await;
            assert_eq!(result.is_ok(), status == Some("202 Accepted"));
            if status.is_none() {
                assert!(matches!(
                    result,
                    Err(UpdatePresentationError::Unconfirmed(_))
                ));
            }
            let bytes = peer.await.unwrap();
            let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            assert!(bytes.starts_with(b"POST /v1/input HTTP/1.1"));
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[end + 4..]).unwrap(),
                serde_json::json!({
                    "threadId": "thread-1", "clientUserMessageId": "update-7", "message": "clickable tabs"
                })
            );
        }
    }

    #[tokio::test]
    async fn confirmation_waits_across_eof_and_partial_appends_for_exact_input() {
        use tokio::io::AsyncWriteExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        let mut writer = tokio::fs::File::create(&path).await.unwrap();
        let file = tokio::fs::File::open(&path).await.unwrap();
        let mut reader = tokio::io::BufReader::new(file);
        let observer =
            tokio::spawn(async move { await_confirmation(&mut reader, "update-7").await });
        writer.write_all(b"{\"id\":1,\"result\":{}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"client_id\":\"other\"}}\n").await.unwrap();
        writer.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !observer.is_finished(),
            "RPC acceptance and unrelated input cannot confirm"
        );
        writer.write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"client_id\":\"update-").await.unwrap();
        writer.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!observer.is_finished(), "partial input cannot confirm");
        writer.write_all(b"7\"}}\n").await.unwrap();
        writer.flush().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), observer)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
