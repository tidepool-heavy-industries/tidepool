//! Native start-or-steer input, confirmed at the persisted user-message boundary.
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt};

use super::super::process::Session;
use super::super::transport::Transport;
use crate::{
    AgentBackendError, InteractiveAgentInstallation, QueueReadyThread, UpdatePresentationError,
};

fn failure(error: impl std::fmt::Display) -> AgentBackendError {
    AgentBackendError::RunFailed {
        detail: format!("active update: {error}"),
    }
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
    #[serde(other)]
    Other,
}

fn confirms(line: &str, key: &str) -> bool {
    matches!(serde_json::from_str::<RolloutRecord>(line),
        Ok(RolloutRecord::EventMsg(InputEvent::UserMessage { client_id: Some(id) })) if id == key)
}

async fn submit<T: Transport>(
    session: &mut Session<T>,
    thread: &str,
    key: &str,
    message: &str,
) -> Result<(), AgentBackendError> {
    // The fork's turn/start is atomic start_or_steer_turn: it steers the active
    // turn, or wakes the same conversation if idle. It never enters the queue
    // for a second assignment, and leaves thread configuration unchanged.
    let _: serde_json::Value = session
        .request(
            "turn/start",
            &serde_json::json!({
                "threadId": thread,
                "clientUserMessageId": key,
                "input": [{ "type": "text", "text": message, "textElements": [] }],
            }),
        )
        .await
        .map_err(failure)?;
    Ok(())
}

// Only complete JSONL records count. EOF on a live rollout means wait for
// more bytes; a partially appended correlation record is not confirmation.
async fn await_confirmation<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    key: &str,
) -> Result<(), AgentBackendError> {
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

pub(super) async fn present(
    installation: &InteractiveAgentInstallation,
    cwd: &Path,
    thread: &QueueReadyThread,
    key: &str,
    message: &str,
) -> Result<(), UpdatePresentationError> {
    use UpdatePresentationError::{NotSubmitted, Unconfirmed};
    let prepared = async {
        let sessions = super::super::isolation::codex_home().join("sessions");
        let thread_id = thread.id().0.clone();
        let path =
            tokio::task::spawn_blocking(move || super::find_rollout(&sessions, &thread_id, 4))
                .await
                .map_err(failure)??
                .ok_or_else(|| failure("bound conversation rollout is unavailable"))?;
        let mut file = tokio::fs::File::open(path).await.map_err(failure)?;
        file.seek(std::io::SeekFrom::End(0))
            .await
            .map_err(failure)?;
        let reader = tokio::io::BufReader::new(file);
        let session = tokio::time::timeout(
            super::CLI_DEADLINE,
            Session::connect_proxy(installation.executable(), cwd),
        )
        .await
        .map_err(|_| failure("timed out connecting to the interactive daemon"))?
        .map_err(failure)?;
        Ok::<_, AgentBackendError>((reader, session))
    }
    .await
    .map_err(NotSubmitted)?;
    let (mut reader, mut session) = prepared;
    let result = tokio::time::timeout(Duration::from_secs(300), async {
        submit(&mut session, &thread.id().0, key, message).await?;
        // Pending input is recorded after the current sampling/tool boundary,
        // before building the next model request. RPC acceptance alone is not
        // presentation. Neither uncertain submission nor observation is retried.
        await_confirmation(&mut reader, key).await
    })
    .await
    .map_err(|_| failure("timed out awaiting model-visible input"))
    .and_then(|result| result);
    if let Err(error) = session.shutdown().await {
        tracing::warn!(%error, "active-update proxy cleanup failed");
    }
    result.map_err(Unconfirmed)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    // A controlled peer tests request/error plumbing, not provider behavior.
    #[derive(Default)]
    struct Peer {
        sent: Vec<serde_json::Value>,
        reject: bool,
    }

    impl Transport for Peer {
        async fn send(&mut self, frame: &serde_json::Value) -> Result<(), codex_codes::Error> {
            self.sent.push(frame.clone());
            Ok(())
        }
        async fn next_line(&mut self) -> Result<Option<String>, codex_codes::Error> {
            let id = self.sent.last().unwrap()["id"].clone();
            Ok(Some(
                if self.reject {
                    serde_json::json!({"id": id, "error": {"code": -32600, "message": "rejected"}})
                } else {
                    serde_json::json!({"id": id, "result": {"turn": {"id": "current-turn"}}})
                }
                .to_string(),
            ))
        }
        fn pid(&self) -> Option<u32> {
            None
        }
        async fn shutdown(self) -> Result<(), codex_codes::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn submits_atomic_start_or_steer_without_changing_configuration_or_retrying() {
        for reject in [false, true] {
            let mut session = Session::over(Peer {
                reject,
                ..Default::default()
            });
            assert_eq!(
                submit(&mut session, "thread-1", "update-7", "clickable tabs")
                    .await
                    .is_err(),
                reject
            );
            let sent = &session.transport().sent;
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0]["method"], "turn/start");
            assert_eq!(
                sent[0]["params"],
                serde_json::json!({
                    "threadId": "thread-1", "clientUserMessageId": "update-7",
                    "input": [{"type": "text", "text": "clickable tabs", "textElements": []}]
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
