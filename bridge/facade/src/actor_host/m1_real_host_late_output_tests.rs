use super::*;
use async_trait::async_trait;
use harness::{
    engine::ResponsesTransport,
    model::{AgentPath, CallId},
    store::ClaimState,
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
};
use serde_json::json;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

const CELL_CALL_ID: &str = "m1-late-output-cell";
const FIRST_INPUT: &str = "Start the resident Haskell operation.";
const CONTINUE_INPUT: &str = "Continue after the resident operation settles.";

struct PendingCellTransport {
    normal_rounds: AtomicUsize,
    declared_tools: StdMutex<Option<Vec<serde_json::Value>>>,
    compaction_seen: mpsc::UnboundedSender<()>,
    compaction_release: AsyncMutex<Option<oneshot::Receiver<()>>>,
    successor: mpsc::UnboundedSender<(ResponsesRequest, oneshot::Sender<ResponsesTurn>)>,
    late_output: mpsc::UnboundedSender<ResponsesRequest>,
}

fn final_turn(response_id: &str, text: &str) -> ResponsesTurn {
    ResponsesTurn {
        response_id: response_id.into(),
        items: vec![harness::item::Item(json!({
            "type":"message",
            "role":"assistant",
            "phase":"final_answer",
            "content":[{"type":"output_text","text":text}]
        }))],
        usage: Usage::default(),
    }
}

fn output_for_call(request: &ResponsesRequest) -> impl Iterator<Item = &harness::item::Item> {
    request.input.iter().filter(|item| {
        item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == CELL_CALL_ID
    })
}

fn retains_declared_tools(transport: &PendingCellTransport, request: &ResponsesRequest) -> bool {
    let current = request
        .tools
        .iter()
        .map(|tool| tool.0.clone())
        .collect::<Vec<_>>();
    transport.declared_tools.lock().unwrap().as_ref() == Some(&current)
}

#[async_trait]
impl ResponsesTransport for PendingCellTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        if request.tools_allowed.as_ref().is_some_and(Vec::is_empty) {
            self.compaction_seen.send(()).map_err(|_| {
                TransportError::Stream("test closed before compaction began".into())
            })?;
            let release = self.compaction_release.lock().await.take().ok_or_else(|| {
                TransportError::Stream("compaction was requested more than once".into())
            })?;
            release.await.map_err(|_| {
                TransportError::Stream("test dropped the compaction release".into())
            })?;
            return Ok(final_turn(
                "m1-late-cell-compaction",
                "The resident operation is still pending.",
            ));
        }

        match self.normal_rounds.fetch_add(1, Ordering::SeqCst) + 1 {
            1 => {
                if !request.tools.iter().any(|tool| tool.0["name"] == "haskell") {
                    return Err(TransportError::Stream(
                        "the real root declaration did not expose the Haskell tool".into(),
                    ));
                }
                *self.declared_tools.lock().unwrap() =
                    Some(request.tools.iter().map(|tool| tool.0.clone()).collect());
                Ok(ResponsesTurn {
                    response_id: "m1-late-cell-start".into(),
                    items: vec![harness::item::Item(json!({
                        "type":"custom_tool_call",
                        "call_id":CELL_CALL_ID,
                        "name":"haskell",
                        "input":"do { sleep (seconds 8); pure (40 + 2 :: Int) }"
                    }))],
                    usage: Usage {
                        input_tokens: 100_001,
                        ..Usage::default()
                    },
                })
            }
            2 => {
                if !retains_declared_tools(self, &request) {
                    return Err(TransportError::Stream(
                        "post-compaction tool declarations changed during the resident call".into(),
                    ));
                }
                if !request
                    .input
                    .iter()
                    .any(|item| item.0["call_id"] == CELL_CALL_ID)
                {
                    return Err(TransportError::Stream(
                        "post-compaction request lost the pending Haskell call".into(),
                    ));
                }
                if output_for_call(&request).next().is_some() {
                    return Err(TransportError::Stream(
                        "pending Haskell call already had an output".into(),
                    ));
                }
                let (reply, response) = oneshot::channel();
                self.successor
                    .send((request, reply))
                    .map_err(|_| TransportError::Stream("test dropped successor request".into()))?;
                response
                    .await
                    .map_err(|_| TransportError::Stream("test dropped successor response".into()))
            }
            3 => {
                if !retains_declared_tools(self, &request) {
                    return Err(TransportError::Stream(
                        "late-output tool declarations changed during the resident call".into(),
                    ));
                }
                let outputs = output_for_call(&request).count();
                if outputs != 1 {
                    return Err(TransportError::Stream(format!(
                        "late Haskell output must be delivered exactly once, got {outputs}"
                    )));
                }
                let output = output_for_call(&request).next().expect("count checked");
                let serialized = output.0.to_string();
                if !serialized.contains("42") {
                    return Err(TransportError::Stream(format!(
                        "late Haskell output did not contain the real result: {serialized}"
                    )));
                }
                self.late_output.send(request).map_err(|_| {
                    TransportError::Stream("test dropped late-output request".into())
                })?;
                Ok(final_turn(
                    "m1-late-cell-finished",
                    "The late resident result was delivered once.",
                ))
            }
            other => Err(TransportError::Stream(format!(
                "unexpected normal provider round {other}"
            ))),
        }
    }
}

async fn wait_for_cell_state(host: &RunningBrowserHost, computing: bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while host.campaign.actor.hosted_cell_computing() != computing {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("resident cell did not reach computing={computing}"));
}

async fn submit_host_input(
    client: &reqwest::Client,
    address: std::net::SocketAddr,
    origin: &str,
    cookie: &str,
    target: &harness::embedding::HostIdentity,
    text: &str,
) {
    let response = client
        .post(format!("http://{address}/api/commands"))
        .header("Origin", origin)
        .header(reqwest::header::COOKIE, cookie)
        .json(&browser_input(target, text))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn real_host_retains_one_late_haskell_output_across_compaction() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret = "m1-late-output-secret-is-long-enough";
    let secret_file = files.path().join("browser-secret");
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("unused-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = crate::exomonad::EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };

    let (compaction_tx, mut compaction_rx) = mpsc::unbounded_channel();
    let (compaction_release_tx, compaction_release_rx) = oneshot::channel();
    let (successor_tx, mut successor_rx) = mpsc::unbounded_channel();
    let (late_output_tx, mut late_output_rx) = mpsc::unbounded_channel();
    let transport: Arc<dyn ResponsesTransport> = Arc::new(PendingCellTransport {
        normal_rounds: AtomicUsize::new(0),
        declared_tools: StdMutex::new(None),
        compaction_seen: compaction_tx,
        compaction_release: AsyncMutex::new(Some(compaction_release_rx)),
        successor: successor_tx,
        late_output: late_output_tx,
    });
    let host = RunningBrowserHost::start(&settings, &transport)
        .await
        .expect("production embedded host should start");
    let client = reqwest::Client::new();
    let origin = format!("https://{}", host.address);
    let login = client
        .post(format!("http://{}/api/session", host.address))
        .header("Origin", &origin)
        .json(&json!({ "secret": secret }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let target = browser_target(&host.campaign);

    submit_host_input(
        &client,
        host.address,
        &origin,
        &cookie,
        &target,
        FIRST_INPUT,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(30), compaction_rx.recv())
        .await
        .expect("provider did not enter compaction")
        .expect("provider dropped compaction signal");
    wait_for_cell_state(&host, true).await;
    compaction_release_tx
        .send(())
        .expect("compaction request was no longer waiting");

    let (successor_request, successor_reply) =
        tokio::time::timeout(Duration::from_secs(30), successor_rx.recv())
            .await
            .expect("provider did not receive the post-compaction request")
            .expect("provider dropped post-compaction request");
    assert!(
        successor_request
            .input
            .iter()
            .any(|item| item.0["call_id"] == CELL_CALL_ID)
    );
    assert_eq!(output_for_call(&successor_request).count(), 0);
    wait_for_cell_state(&host, true).await;
    successor_reply
        .send(final_turn(
            "m1-late-cell-waiting",
            "Waiting for the resident Haskell result.",
        ))
        .expect("Engine stopped before pending output could settle");

    wait_for_cell_state(&host, false).await;
    submit_host_input(
        &client,
        host.address,
        &origin,
        &cookie,
        &target,
        CONTINUE_INPUT,
    )
    .await;
    let late_request = tokio::time::timeout(Duration::from_secs(30), late_output_rx.recv())
        .await
        .expect("late output was not sent to the model")
        .expect("provider dropped late-output request");
    let outputs = output_for_call(&late_request).collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1, "late output must be retained once");
    assert!(outputs[0].0.to_string().contains("42"));
    let claims = host
        .runtime
        .store()
        .claims(&CallId(CELL_CALL_ID.into()))
        .expect("read resident tool claim");
    assert_eq!(claims.len(), 1, "one real Haskell call must have one claim");
    assert_eq!(claims[0].state, ClaimState::Settled);
    assert_eq!(target.actor, AgentPath("/root".into()));

    host.stop()
        .await
        .expect("production host cleanup should succeed");
}
