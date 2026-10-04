//! Real Store admission on both sides of the native idle-retirement claim.
//! The barriers only pause production wake/probe boundaries; neither supplies authority.

use super::embedded_harness::EmbeddedAdmissionTestHooks;
use super::hosted_test_context::{HostedActorContext, HostedTestRuntime};
use super::*;
use crate::exomonad::EmbeddedLaunchConfig;
use async_trait::async_trait;
use futures_util::FutureExt;
use harness::{
    embedding::InputObservation,
    engine::ResponsesTransport,
    model::RequestId,
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
};
use std::sync::{Condvar, Mutex as StdMutex};
use tokio::sync::oneshot;

const BOUNDARY_BUDGET: Duration = Duration::from_secs(15);

struct ResumeWake(Option<oneshot::Sender<()>>);
impl Drop for ResumeWake {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

struct ResumeProbe(Arc<(StdMutex<bool>, Condvar)>);
impl Drop for ResumeProbe {
    fn drop(&mut self) {
        let (released, changed) = &*self.0;
        *released.lock().unwrap() = true;
        changed.notify_all();
    }
}

struct WakePause {
    reached: oneshot::Sender<i64>,
    resume: oneshot::Receiver<()>,
}
struct ProbePause {
    reached: oneshot::Sender<()>,
    resume: Arc<(StdMutex<bool>, Condvar)>,
}

#[derive(Default)]
struct Boundaries {
    wake: StdMutex<Option<WakePause>>,
    probe: StdMutex<Option<ProbePause>>,
}

impl Boundaries {
    fn pause_wake(&self) -> (oneshot::Receiver<i64>, ResumeWake) {
        let (reached, observation) = oneshot::channel();
        let (resume, release) = oneshot::channel();
        assert!(self
            .wake
            .lock()
            .unwrap()
            .replace(WakePause {
                reached,
                resume: release
            })
            .is_none());
        (observation, ResumeWake(Some(resume)))
    }

    fn pause_probe(&self) -> (oneshot::Receiver<()>, ResumeProbe) {
        let (reached, observation) = oneshot::channel();
        let resume = Arc::new((StdMutex::new(false), Condvar::new()));
        assert!(self
            .probe
            .lock()
            .unwrap()
            .replace(ProbePause {
                reached,
                resume: resume.clone()
            })
            .is_none());
        (observation, ResumeProbe(resume))
    }

    fn hooks(self: &Arc<Self>) -> Arc<EmbeddedAdmissionTestHooks> {
        let wake = self.clone();
        let probe = self.clone();
        Arc::new(EmbeddedAdmissionTestHooks {
            before_input_wake: Some(Arc::new(move |envelope| {
                let pause = wake.wake.lock().unwrap().take();
                async move {
                    if let Some(pause) = pause {
                        let _ = pause.reached.send(envelope);
                        // Dropping the test's release guard also unblocks cleanup.
                        tokio::time::timeout(BOUNDARY_BUDGET, pause.resume)
                            .await
                            .expect("test must release committed input wake")
                            .ok();
                    }
                }
                .boxed()
            })),
            before_idle_probe: Some(Arc::new(move || {
                let pause = probe.probe.lock().unwrap().take();
                if let Some(pause) = pause {
                    let _ = pause.reached.send(());
                    let (released, changed) = &*pause.resume;
                    let (held, timeout) = changed
                        .wait_timeout_while(released.lock().unwrap(), BOUNDARY_BUDGET, |released| {
                            !*released
                        })
                        .unwrap();
                    drop(held);
                    assert!(!timeout.timed_out(), "test must release real Store probe");
                }
            })),
        })
    }
}

struct ScriptedRound {
    request_id: RequestId,
    request: ResponsesRequest,
    response: oneshot::Sender<ResponsesTurn>,
}
impl ScriptedRound {
    fn succeed(self) {
        self.response
            .send(ResponsesTurn {
                response_id: format!("idle-retirement-{}", self.request_id.0),
                items: vec![harness::item::Item(serde_json::json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text", "text":"input received"}],
                }))],
                usage: Usage::default(),
            })
            .unwrap();
    }
}

struct IdleTransport(mpsc::UnboundedSender<ScriptedRound>);
#[async_trait]
impl ResponsesTransport for IdleTransport {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("Engine must provide its actual durable request ID")
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let (response, completed) = oneshot::channel();
        self.0
            .send(ScriptedRound {
                request_id: request_id.clone(),
                request,
                response,
            })
            .map_err(|_| TransportError::Stream("test round observer closed".into()))?;
        let turn = tokio::time::timeout(Duration::from_secs(60), completed)
            .await
            .map_err(|_| TransportError::Stream("test provider response timed out".into()))?
            .map_err(|_| TransportError::Stream("test provider response cancelled".into()))?;
        for item in &turn.items {
            sink.send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await
                .map_err(|_| TransportError::Stream("Engine output stream closed".into()))?;
        }
        Ok(turn)
    }
}

async fn start_host(
    boundaries: Arc<Boundaries>,
) -> (
    HostedTestRuntime,
    mpsc::UnboundedReceiver<ScriptedRound>,
    tempfile::TempDir,
) {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret = files.path().join("browser-secret");
    std::fs::write(&secret, "native-idle-retirement-test-secret-32-bytes").unwrap();
    let credentials = files.path().join("offline-auth.json");
    std::fs::write(&credentials, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: credentials,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };
    let (rounds, received) = mpsc::unbounded_channel();
    let host = HostedTestRuntime::start_with_factory(
        &settings,
        |_| {},
        move |runtime, _| {
            runtime
                .configure_admission_test_hooks(boundaries.hooks())
                .unwrap();
            Arc::new(IdleTransport(rounds))
        },
    )
    .await
    .unwrap();
    let binding = host.context.binding(host.context.actor.identity()).unwrap();
    binding
        .conversation()
        .unwrap()
        .input(
            "initial-round",
            "acceptance",
            "establish a completed provider round",
        )
        .await
        .expect("real host admits the initial user input");
    (host, received, files)
}

async fn next_round(rounds: &mut mpsc::UnboundedReceiver<ScriptedRound>) -> ScriptedRound {
    tokio::time::timeout(Duration::from_secs(90), rounds.recv())
        .await
        .expect("real Engine reaches scripted provider")
        .expect("provider remains attached")
}

async fn idle(context: &HostedActorContext) {
    tokio::time::timeout(BOUNDARY_BUDGET, async {
        loop {
            let graph = context.forest.inspect_host_graph();
            if graph.iter().any(|node| {
                node.actor == context.actor.identity()
                    && !node.provider_observation_stale
                    && node.provider_turn.as_ref().is_some_and(|turn| {
                        turn.state == exomonad_model::ProviderTurnState::Succeeded
                    })
                    && node.active_requests.is_empty()
                    && node.queued_requests.is_empty()
            }) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real provider success is observed after durable round settlement");
}

fn assert_confirmed_cleanup(actor: &exomonad_actor::LocalActorRef) {
    let cleanup = actor
        .terminal()
        .cleanup()
        .expect("actual lifecycle owner retains cleanup proof");
    assert_eq!(cleanup.actor(), actor.identity());
    assert!(cleanup.is_confirmed(), "{cleanup:?}");
}

fn requested_retirement() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "native idle Store acceptance".into(),
    }
}

fn assert_refused(
    result: Result<ActorTerminal, exomonad_actor::KernelInvocationFailure>,
    reason: &str,
) {
    assert!(
        matches!(result, Err(exomonad_actor::KernelInvocationFailure::Rejected { detail, .. }) if detail.contains(reason))
    );
}

#[tokio::test]
async fn committed_input_before_wake_refuses_idle_retirement_and_runs_after_release() {
    let boundaries = Arc::new(Boundaries::default());
    let (host, mut rounds, _files) = start_host(boundaries.clone()).await;
    let context = &host.context;
    let actor = context.actor.clone();
    let binding = context.binding(actor.identity()).unwrap();
    let conversation = binding.conversation().unwrap();
    let store = host.runtime.store();
    let first = next_round(&mut rounds).await;
    assert_eq!(
        store
            .embedded_round_frontier(conversation.identity())
            .unwrap()
            .pending_head,
        Some(first.request_id.clone())
    );
    assert_refused(
        actor
            .retire_idle_by(actor.identity(), requested_retirement())
            .await,
        "not confirmed idle",
    );
    first.succeed();
    idle(context).await;

    let (committed, resume_wake) = boundaries.pause_wake();
    let admitted = conversation.clone();
    let input = tokio::spawn(async move {
        admitted
            .input(
                "commit-before-wake",
                "acceptance",
                "durable input before wake",
            )
            .await
    });
    let envelope = tokio::time::timeout(BOUNDARY_BUDGET, committed)
        .await
        .unwrap()
        .unwrap();
    assert!(store
        .unread(&conversation.identity().actor.0)
        .unwrap()
        .iter()
        .any(|item| item.id == envelope));
    assert_eq!(
        conversation.input_observation(envelope).unwrap(),
        InputObservation::Admitted
    );
    assert!(
        rounds.try_recv().is_err(),
        "wake has not reached the real Engine"
    );

    let held_admission = actor.admit_transaction().unwrap();
    assert_refused(
        actor
            .retire_idle_by(actor.identity(), requested_retirement())
            .await,
        "admitted transaction",
    );
    drop(held_admission);
    // The provider is still genuinely idle. Store alone refuses this claim,
    // even though the original input's short admission lease is already gone.
    assert_refused(
        actor
            .retire_idle_by(actor.identity(), requested_retirement())
            .await,
        "pending durable work",
    );
    assert!(actor.terminal().get().is_none());
    drop(
        actor
            .admit_transaction()
            .expect("refused Store probe reopens live admission"),
    );

    drop(resume_wake);
    let receipt = tokio::time::timeout(BOUNDARY_BUDGET, input)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(receipt.envelope_id, envelope);
    assert!(receipt.wake_error.is_none());
    let next = next_round(&mut rounds).await;
    assert!(next
        .request
        .input
        .iter()
        .any(|item| item.0["content"] == "durable input before wake"));
    assert_eq!(
        conversation.input_observation(envelope).unwrap(),
        InputObservation::Included(next.request_id.clone())
    );
    next.succeed();
    idle(context).await;
    let terminal = tokio::time::timeout(
        BOUNDARY_BUDGET,
        actor.retire_idle_by(actor.identity(), requested_retirement()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(terminal.kind, ActorExitKind::Cancelled);
    assert!(actor
        .terminal()
        .retirement_acknowledged_by(actor.identity()));
    assert_confirmed_cleanup(&actor);
    assert!(store
        .unread(&conversation.identity().actor.0)
        .unwrap()
        .is_empty());
    host.stop()
        .await
        .expect("actual host confirms retirement and resource cleanup");
}

#[tokio::test]
async fn idle_claim_before_input_refuses_store_mutation_and_confirms_cleanup() {
    let boundaries = Arc::new(Boundaries::default());
    let (host, mut rounds, _files) = start_host(boundaries.clone()).await;
    let actor = host.context.actor.clone();
    let binding = host.context.binding(actor.identity()).unwrap();
    let conversation = binding.conversation().unwrap();
    let observer = conversation.input_observer();
    let store = host.runtime.store();
    next_round(&mut rounds).await.succeed();
    idle(&host.context).await;
    assert!(store
        .unread(&conversation.identity().actor.0)
        .unwrap()
        .is_empty());
    let before = store
        .embedded_round_frontier(conversation.identity())
        .unwrap();
    let (claimed, resume_probe) = boundaries.pause_probe();
    let retiring_actor = actor.clone();
    let executor = tokio::runtime::Handle::current();
    // The synchronous Store probe pauses on a separate executor thread, never
    // the production host's current-thread executor or the Store's mutex.
    let retirement = tokio::task::spawn_blocking(move || {
        executor.block_on(
            retiring_actor.retire_idle_by(retiring_actor.identity(), requested_retirement()),
        )
    });
    tokio::time::timeout(BOUNDARY_BUDGET, claimed)
        .await
        .unwrap()
        .unwrap();
    assert!(conversation
        .input("claim-before-input", "acceptance", "must not commit")
        .await
        .is_err());
    assert_eq!(
        observer
            .input_observation_by_operation("claim-before-input")
            .unwrap(),
        None
    );
    assert!(store
        .unread(&conversation.identity().actor.0)
        .unwrap()
        .is_empty());
    let held = store
        .embedded_round_frontier(conversation.identity())
        .unwrap();
    assert_eq!(held.settled_head, before.settled_head);
    assert_eq!(held.pending_head, before.pending_head);
    assert!(
        actor.terminal().get().is_none(),
        "claim has not yet decided retirement"
    );
    drop(resume_probe);
    let terminal = tokio::time::timeout(BOUNDARY_BUDGET, retirement)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(terminal.kind, ActorExitKind::Cancelled);
    assert!(actor
        .terminal()
        .retirement_acknowledged_by(actor.identity()));
    assert_confirmed_cleanup(&actor);
    assert!(conversation
        .input("after-retirement", "acceptance", "must remain refused")
        .await
        .is_err());
    assert_eq!(
        observer
            .input_observation_by_operation("after-retirement")
            .unwrap(),
        None
    );
    assert!(store
        .unread(&conversation.identity().actor.0)
        .unwrap()
        .is_empty());
    host.stop()
        .await
        .expect("actual host confirms closed admission and resource cleanup");
}
