//! Production Reflect reads during a gated mock provider response.
use async_trait::async_trait;
use exomonad_actor::{ActorId, ActorRef, ConversationReader, ConversationTurnState, TurnItem};
use harness::{
    embedding::{
        AdmissionGuard, Conversation, EmbeddedError, HostActor, HostControl, HostControlError,
        HostIdentity, ToolSurface,
    },
    engine::{EngineConfig, ResponsesTransport},
    item::Item,
    model::{AgentPath, Effort, RequestId},
    provider::{Provider, ProviderError},
    store::{EmbeddedRoundOutcome, Store},
    transport::{sse::StreamEvent, Auth, ResponsesRequest, ResponsesTurn, TransportError},
    turn::JobScheduler,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::sync::{mpsc, watch, Notify};

struct Lease;
impl AdmissionGuard for Lease {}
struct Host {
    identity: HostIdentity,
    surface: Arc<ToolSurface>,
}
#[async_trait]
impl HostActor for Host {
    fn identity(&self) -> &HostIdentity {
        &self.identity
    }
    fn admit(&self) -> Result<Box<dyn AdmissionGuard>, EmbeddedError> {
        Ok(Box::new(Lease))
    }
    fn tool_surface(&self) -> Result<Arc<ToolSurface>, EmbeddedError> {
        Ok(self.surface.clone())
    }
    async fn wake(&self, _: i64) -> Result<(), String> {
        Ok(())
    }
    async fn control(&self, _: HostControl) -> Result<Value, HostControlError> {
        unreachable!()
    }
}
struct NoAuth;
impl Auth for NoAuth {
    fn access(&self) -> Result<(String, String), TransportError> {
        unreachable!("mock transport has no credentials")
    }
}
struct ReflectTool {
    reader: ConversationReader,
    actor: ActorRef,
    observed: Arc<Mutex<Vec<exomonad_actor::ConversationTurn>>>,
    read: Arc<Notify>,
}
#[async_trait]
impl Provider for ReflectTool {
    async fn call(&self, name: &str, _: Value) -> Result<Value, ProviderError> {
        assert_eq!(name, "reflect");
        *self.observed.lock().unwrap() = (self.reader)(self.actor, 2).await.unwrap();
        self.read.notify_one();
        Ok(json!("reflected"))
    }
    fn tools(&self) -> Vec<Value> {
        vec![]
    }
}
struct Stream {
    calls: AtomicUsize,
    read: Arc<Notify>,
    request: Arc<Mutex<Option<RequestId>>>,
}
#[async_trait]
impl ResponsesTransport for Stream {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        unreachable!("must stream")
    }
    async fn create_streaming_for_request(
        &self,
        request: &RequestId,
        _: ResponsesRequest,
        sink: mpsc::Sender<StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst);
        let items = if round == 1 {
            *self.request.lock().unwrap() = Some(request.clone());
            let item = Item(
                json!({"type":"function_call", "call_id":"reflect-call", "name":"reflect", "arguments":"{}"}),
            );
            sink.send(StreamEvent::ItemDone(item.clone()))
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), self.read.notified())
                .await
                .expect("Reflect tool must read while the provider response is gated");
            vec![item]
        } else {
            let item = Item(
                json!({"type":"message", "role":"assistant", "phase":"final_answer", "content":"done"}),
            );
            sink.send(StreamEvent::ItemDone(item.clone()))
                .await
                .unwrap();
            vec![item]
        };
        Ok(ResponsesTurn {
            response_id: if round == 0 {
                String::new()
            } else {
                format!("response-{round}")
            },
            items,
            usage: Default::default(),
        })
    }
}

#[tokio::test]
async fn reflection_includes_streamed_active_request_then_records_response_completion() {
    let store = Arc::new(Store::memory().unwrap());
    let actor = ActorRef::first(ActorId(1));
    let identity = HostIdentity {
        run: "reflect-test".into(),
        actor: AgentPath("/root".into()),
        incarnation: "first".into(),
    };
    let bound_identity = identity.clone();
    let reader = super::embedded_reflect::conversation_reader(
        store.clone(),
        Arc::new(move |requested| (requested == actor).then(|| bound_identity.clone())),
    );
    assert!((reader)(actor, 0).await.unwrap().is_empty());
    assert!(matches!(
        (reader)(ActorRef::first(ActorId(2)), 1).await,
        Err(exomonad_actor::ConversationUnavailable::Unbound)
    ));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let read = Arc::new(Notify::new());
    let request = Arc::new(Mutex::new(None));
    let dispatcher = Arc::new(ReflectTool {
        reader: reader.clone(),
        actor,
        observed: observed.clone(),
        read: read.clone(),
    });
    let surface = Arc::new(ToolSurface::new("reflect-test".into(), vec![json!({"type":"function", "name":"reflect", "description":"Read own history", "strict":true, "parameters":{"type":"object", "properties":{}, "required":[], "additionalProperties":false}})], dispatcher).unwrap());
    let conversation = Conversation::attach(
        store.clone(),
        Arc::new(Host {
            identity: identity.clone(),
            surface,
        }),
        None,
    )
    .unwrap();
    assert!((reader)(actor, 2).await.unwrap().is_empty());
    let engine = conversation
        .engine::<NoAuth, _>(
            Stream {
                calls: AtomicUsize::new(0),
                read,
                request: request.clone(),
            },
            Arc::new(JobScheduler::new(1).unwrap()),
            EngineConfig {
                instructions: "test".into(),
                tools: vec![],
                model: "mock".into(),
                effort: Effort::Low,
                session_id: "test".into(),
                agent: identity.actor.clone(),
            },
            std::num::NonZeroU64::new(100_000).unwrap(),
        )
        .unwrap();
    let (_cancel, cancellation) = watch::channel(false);
    let (_wake, incoming) = mpsc::unbounded_channel();
    let first = engine
        .run_embedded(
            None,
            vec![Item(
                json!({"type":"message","role":"user","content":"first"}),
            )],
            cancellation.clone(),
            incoming,
        )
        .await
        .unwrap();
    assert!(store
        .settle_embedded_round(
            &identity,
            None,
            &first.head_request,
            EmbeddedRoundOutcome::Completed
        )
        .unwrap());
    let settled = (reader)(actor, 2).await.unwrap();
    assert_eq!(settled.len(), 1);
    assert_eq!(
        settled[0].state,
        ConversationTurnState::Completed {
            provider_response_id: None
        }
    );
    assert_eq!(settled[0].completed_at, None);
    let (_wake, incoming) = mpsc::unbounded_channel();
    let second = engine
        .run_embedded(
            Some(first.head_request.clone()),
            vec![Item(
                json!({"type":"message","role":"user","content":"second"}),
            )],
            cancellation,
            incoming,
        )
        .await
        .unwrap();
    let active = observed.lock().unwrap().clone();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0], settled[0]);
    let active_request = request.lock().unwrap().clone().unwrap();
    assert_eq!(active[1].turn, active_request.0);
    assert_eq!(active[1].state, ConversationTurnState::InProgress);
    assert_eq!(active[1].completed_at, None);
    assert!(matches!(&active[1].items[0], TurnItem::Message { text, .. } if text == "second"));
    assert!(
        matches!(&active[1].items[1], TurnItem::ToolCall { call, .. } if call == "reflect-call")
    );
    assert!(!active[1]
        .items
        .iter()
        .any(|item| matches!(item, TurnItem::ToolResult { .. })));
    assert!(store
        .settle_embedded_round(
            &identity,
            Some(&first.head_request),
            &second.head_request,
            EmbeddedRoundOutcome::Completed
        )
        .unwrap());
    let completed = (reader)(actor, 3).await.unwrap();
    let reflected = completed
        .iter()
        .find(|turn| turn.turn == active_request.0)
        .unwrap();
    assert_eq!(
        reflected.state,
        ConversationTurnState::Completed {
            provider_response_id: Some("response-1".into())
        }
    );
    assert_eq!(reflected.completed_at, None);
    assert!(reflected
        .items
        .iter()
        .any(|item| matches!(item, TurnItem::ToolResult { call, .. } if call == "reflect-call")));
    assert_eq!(completed.last().unwrap().turn, second.head_request.0);
    assert_eq!(
        (reader)(actor, 1).await.unwrap(),
        completed[completed.len() - 1..]
    );
    let mut stale = identity.clone();
    stale.incarnation = "stale".into();
    let stale_reader =
        super::embedded_reflect::conversation_reader(store, Arc::new(move |_| Some(stale.clone())));
    assert!(matches!(
        (stale_reader)(actor, 1).await,
        Err(exomonad_actor::ConversationUnavailable::Unreadable(_))
    ));
}
