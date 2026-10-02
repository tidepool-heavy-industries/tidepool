//! Store-reopen proof through the production embedded driver.
use super::*;
use harness::{
    embedding::{
        AdmissionGuard, BindingSuccessorAuthority, Conversation, EmbeddedError, HostActor,
        HostControl, HostControlError, HostIdentity, ToolSurface,
    },
    item::Item,
    mailbox::DurableMailboxWake,
    provider::{Provider, ProviderError},
    transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError},
};
use std::{collections::VecDeque, sync::Mutex};

struct Offline;
impl Auth for Offline {
    fn access(&self) -> Result<(String, String), TransportError> {
        panic!("offline fixture")
    }
}
struct NoTools;
#[async_trait::async_trait]
impl Provider for NoTools {
    async fn call(
        &self,
        _: &str,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        panic!("no tool execution in restart fixture")
    }
    fn tools(&self) -> Vec<serde_json::Value> {
        vec![]
    }
}
struct Guard;
impl AdmissionGuard for Guard {}
struct Host {
    identity: HostIdentity,
    wakes: mpsc::UnboundedSender<DurableMailboxWake>,
    provider: Arc<dyn Provider>,
}
#[async_trait::async_trait]
impl HostActor for Host {
    fn identity(&self) -> &HostIdentity {
        &self.identity
    }
    fn admit(&self) -> Result<Box<dyn AdmissionGuard>, EmbeddedError> {
        Ok(Box::new(Guard))
    }
    fn tool_surface(&self) -> Result<Arc<ToolSurface>, EmbeddedError> {
        Ok(Arc::new(ToolSurface::new(
            "restart-fixture".into(),
            self.provider.tools(),
            self.provider.clone(),
        )?))
    }
    async fn wake(&self, id: i64) -> Result<(), String> {
        self.wakes
            .send(DurableMailboxWake { envelope_id: id })
            .map_err(|e| e.to_string())
    }
    async fn output_committed(
        &self,
        operation: &harness::model::OperationId,
    ) -> Result<(), String> {
        self.provider
            .output_committed(operation)
            .await
            .map_err(|e| e.to_string())
    }
    async fn control(&self, _: HostControl) -> Result<serde_json::Value, HostControlError> {
        panic!("unused control")
    }
}
struct Successor;
impl BindingSuccessorAuthority for Successor {
    fn validate_successor(&self, _: &HostIdentity, _: &HostIdentity) -> Result<bool, String> {
        Ok(true)
    }
}
struct Script {
    inputs: Arc<Mutex<Vec<ResponsesRequest>>>,
    turns: Mutex<VecDeque<ResponsesTurn>>,
}
#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for Script {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        self.inputs.lock().unwrap().push(request);
        Ok(self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected automatic provider request"))
    }
}
fn final_turn() -> ResponsesTurn {
    ResponsesTurn {
        response_id: "restart-final".into(),
        items: vec![Item(serde_json::json!({
            "type":"message","role":"assistant","phase":"final_answer",
            "content":[{"type":"output_text","text":"done"}]
        }))],
        usage: Default::default(),
    }
}
fn script() -> (Script, Arc<Mutex<Vec<ResponsesRequest>>>) {
    let inputs = Arc::new(Mutex::new(vec![]));
    (
        Script {
            inputs: inputs.clone(),
            turns: Mutex::new(VecDeque::from([final_turn()])),
        },
        inputs,
    )
}
fn attach(runtime: &EmbeddedHarnessRuntime, identity: HostIdentity) -> EmbeddedConversation {
    attach_with_provider(runtime, identity, Arc::new(NoTools))
}
fn attach_with_provider(
    runtime: &EmbeddedHarnessRuntime,
    identity: HostIdentity,
    provider: Arc<dyn Provider>,
) -> EmbeddedConversation {
    let (wakes, incoming) = mpsc::unbounded_channel();
    let host = Arc::new(Host {
        identity,
        wakes,
        provider,
    });
    EmbeddedConversation {
        conversation: Arc::new(Conversation::attach(runtime.store(), host, None).unwrap()),
        incoming,
        round_control: Arc::new(super::super::embedded_harness::EmbeddedRoundControl::default()),
    }
}
async fn wait_head(
    runtime: &EmbeddedHarnessRuntime,
    previous: Option<&harness::model::RequestId>,
) -> harness::model::RequestId {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let head = runtime
                .store()
                .agent(&harness::model::AgentPath("/root".into()))
                .unwrap()
                .unwrap()
                .head_request;
            if head.as_ref() != previous {
                if let Some(head) = head {
                    return head;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("driver must settle retained pending work")
}

#[tokio::test]
async fn driver_reopens_delivered_pending_round_without_wake_then_waits_for_explicit_input() {
    let files = tempfile::tempdir().unwrap();
    let old = HostIdentity {
        run: super::super::runtime_namespace(files.path()),
        actor: harness::model::AgentPath("/root".into()),
        incarnation: "old".into(),
    };
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Http,
        public_origin: None,
        asset_root: files.path().join("unused-assets"),
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(files.path().join("unused-secret")),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: files.path().join("unused-auth"),
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };
    let pending;
    {
        let runtime = EmbeddedHarnessRuntime::open(files.path(), 1).unwrap();
        let embedded = attach(&runtime, old.clone());
        embedded
            .conversation
            .input("first-input", "operator", "retained input")
            .await
            .unwrap();
        let (transport, inputs) = script();
        let engine = embedded
            .conversation
            .engine::<Offline, _>(
                transport,
                runtime.scheduler(),
                EngineConfig {
                    instructions: "offline".into(),
                    tools: vec![],
                    model: "test".into(),
                    effort: Effort::Low,
                    session_id: "before-crash".into(),
                    agent: old.actor.clone(),
                },
                NonZeroU64::new(settings.context_capacity_tokens).unwrap(),
            )
            .unwrap();
        let (_cancel, cancel) = watch::channel(false);
        pending = engine
            .run_embedded(None, vec![], cancel, embedded.incoming)
            .await
            .unwrap()
            .head_request;
        assert_eq!(inputs.lock().unwrap().len(), 1);
        assert!(runtime.store().unread("/root").unwrap().is_empty());
        assert!(
            runtime
                .store()
                .agent(&old.actor)
                .unwrap()
                .unwrap()
                .head_request
                .is_none()
        );
    }
    let runtime = Arc::new(EmbeddedHarnessRuntime::open(files.path(), 1).unwrap());
    let mut next = old.clone();
    next.incarnation = "new".into();
    runtime
        .store()
        .transfer_embedded_binding(&old, &next, &Successor)
        .unwrap();
    let embedded = attach(&runtime, next.clone());
    let conversation = embedded.conversation.clone();
    let (transport, inputs) = script();
    let (stop, cancel) = watch::channel(false);
    let (lifecycle, _observations) =
        watch::channel((None, harness::server::HostActorLifecycle::Waiting));
    let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(1));
    let task = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            drive_conversation_with_transport::<Offline, _>(
                embedded,
                runtime,
                &settings,
                "test".into(),
                Effort::Low,
                "offline".into(),
                cancel,
                lifecycle,
                actor,
                transport,
            )
            .await
        }
    });
    assert_eq!(wait_head(&runtime, None).await, pending);
    assert!(
        inputs.lock().unwrap().is_empty(),
        "durable final response must not be requested again"
    );
    assert!(!task.is_finished());
    conversation
        .input("explicit-next", "operator", "new explicit input")
        .await
        .unwrap();
    let successor = wait_head(&runtime, Some(&pending)).await;
    assert_eq!(inputs.lock().unwrap().len(), 1);
    let request = &inputs.lock().unwrap()[0];
    assert_eq!(
        request
            .input
            .iter()
            .filter(|item| item.0["content"] == "retained input")
            .count(),
        1
    );
    assert_eq!(
        request
            .input
            .iter()
            .filter(|item| item.0["content"] == "new explicit input")
            .count(),
        1
    );
    assert_eq!(
        runtime.store().request(&successor).unwrap().unwrap().parent,
        Some(pending)
    );
    stop.send_replace(true);
    tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

struct OnceEffect {
    calls: std::sync::atomic::AtomicUsize,
    completed: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl Provider for OnceEffect {
    fn tools(&self) -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"type":"function", "name":"effect", "strict":true,
            "parameters":{"type":"object", "properties":{}, "required":[], "additionalProperties":false}}),
        ]
    }
    async fn call(
        &self,
        _: &str,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(serde_json::json!({"effect":"completed once"}))
    }
    async fn output_committed(&self, _: &harness::model::OperationId) -> Result<(), ProviderError> {
        self.completed.notify_one();
        Ok(())
    }
}
struct InterruptedStream {
    inputs: Arc<Mutex<Vec<ResponsesRequest>>>,
    effect: Arc<OnceEffect>,
    emit_call: bool,
}
#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for InterruptedStream {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("fixture must use real streamed dispatch");
    }
    async fn create_streaming(
        &self,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let index = {
            let mut inputs = self.inputs.lock().unwrap();
            inputs.push(request);
            inputs.len()
        };
        if index != 1 {
            return Ok(final_turn());
        }
        let mut response = harness::transport::sse::ResponseAssembly::default();
        if self.emit_call {
            let event = response.accept(&serde_json::json!({
                "type":"response.output_item.done", "item": {
                    "type":"function_call", "call_id":"effect-before-eof", "name":"effect", "arguments":"{}"
                }
            }).to_string()).unwrap().unwrap();
            sink.send(event).await.unwrap();
            self.effect.completed.notified().await;
        }
        response.finish()
    }
}

#[tokio::test]
async fn driver_retains_actor_after_eof_and_explicit_input_never_redispatches_admitted_call() {
    for emit_call in [false, true] {
        let files = tempfile::tempdir().unwrap();
        let runtime = Arc::new(EmbeddedHarnessRuntime::open(files.path(), 1).unwrap());
        let identity = HostIdentity {
            run: super::super::runtime_namespace(files.path()),
            actor: harness::model::AgentPath("/root".into()),
            incarnation: "1".into(),
        };
        let effect = Arc::new(OnceEffect {
            calls: std::sync::atomic::AtomicUsize::new(0),
            completed: tokio::sync::Notify::new(),
        });
        let embedded = attach_with_provider(&runtime, identity.clone(), effect.clone());
        let conversation = embedded.conversation.clone();
        conversation
            .input("first", "operator", "first explicit input")
            .await
            .unwrap();
        let inputs = Arc::new(Mutex::new(vec![]));
        let transport = InterruptedStream {
            inputs: inputs.clone(),
            effect: effect.clone(),
            emit_call,
        };
        let settings = EmbeddedLaunchConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Http,
            public_origin: None,
            asset_root: files.path().join("unused-assets"),
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
            session_secret_file: Some(files.path().join("unused-secret")),
            provider: crate::exomonad::EmbeddedModelProvider::Codex,
            credential_file: files.path().join("unused-auth"),
            context_capacity_tokens: 200_000,
            concurrent_jobs: 1,
        };
        let (stop, cancel) = watch::channel(false);
        let (lifecycle, mut observations) =
            watch::channel((None, harness::server::HostActorLifecycle::Waiting));
        let actor = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(1));
        let task = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                drive_conversation_with_transport::<Offline, _>(
                    embedded,
                    runtime,
                    &settings,
                    "test".into(),
                    Effort::Low,
                    "offline".into(),
                    cancel,
                    lifecycle,
                    actor,
                    transport,
                )
                .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                observations.changed().await.unwrap();
                if observations.borrow().1 == harness::server::HostActorLifecycle::Waiting {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            !task.is_finished(),
            "provider EOF must leave the driver attached"
        );
        let interrupted = runtime.store().embedded_round_frontier(&identity).unwrap();
        assert!(interrupted.settled_head.is_none());
        let pending = interrupted.pending_head.unwrap();
        assert!(
            runtime
                .store()
                .events(Some(&pending))
                .unwrap()
                .iter()
                .any(|e| e.kind == "model_interrupted")
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(
            inputs.lock().unwrap().len(),
            1,
            "interruption must not retry itself"
        );
        conversation
            .input("next", "operator", "continue explicitly")
            .await
            .unwrap();
        let successor = wait_head(&runtime, None).await;
        assert_eq!(
            runtime.store().request(&successor).unwrap().unwrap().parent,
            Some(pending)
        );
        assert_eq!(inputs.lock().unwrap().len(), 2);
        assert_eq!(
            effect.calls.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(emit_call),
            "retained calls must never run twice"
        );
        let inputs = inputs.lock().unwrap();
        let continued = &inputs[1];
        assert_eq!(
            continued
                .input
                .iter()
                .filter(|i| i.0["content"] == "first explicit input")
                .count(),
            1
        );
        assert_eq!(
            continued
                .input
                .iter()
                .filter(|i| i.0["content"] == "continue explicitly")
                .count(),
            1
        );
        if emit_call {
            assert_eq!(
                continued
                    .input
                    .iter()
                    .filter(|i| i.0["type"] == "function_call_output"
                        && i.0["call_id"] == "effect-before-eof")
                    .count(),
                1
            );
        }
        drop(inputs);
        stop.send_replace(true);
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
