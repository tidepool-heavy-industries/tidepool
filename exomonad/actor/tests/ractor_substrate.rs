use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use ractor::{Actor, ActorProcessingErr, ActorRef as RactorRef, RpcReplyPort, SupervisionEvent};
use tidepool_actor::MailboxValue;
use tidepool_runtime::session::RootCustody;
use tokio::sync::{mpsc, oneshot};

fn assert_send_static<T: Send + 'static>() {}

#[test]
fn live_actor_payloads_fit_ractors_local_message_boundary() {
    assert_send_static::<MailboxValue>();
    assert_send_static::<RootCustody>();
}

struct ReadinessProbe;

impl Actor for ReadinessProbe {
    type Msg = ();
    type State = ();
    type Arguments = oneshot::Receiver<()>;

    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        ready: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        ready.await.map_err(Into::into)
    }
}

#[tokio::test]
async fn spawn_does_not_publish_before_pre_start_finishes() {
    let (ready_tx, ready_rx) = oneshot::channel();
    let spawn = tokio::spawn(Actor::spawn(None, ReadinessProbe, ready_rx));

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!spawn.is_finished());

    ready_tx.send(()).expect("readiness receiver must be live");
    let (actor, handle) = spawn
        .await
        .expect("spawn task must not panic")
        .expect("actor must publish after readiness");
    actor.stop(None);
    handle.await.expect("actor task must stop cleanly");
}

enum SupervisorMessage {
    Ping(RpcReplyPort<()>),
}

struct SupervisorProbe {
    events: mpsc::UnboundedSender<&'static str>,
}

impl Actor for SupervisorProbe {
    type Msg = SupervisorMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        (): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Ping(reply) => reply.send(()).map_err(Into::into),
        }
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: RactorRef<Self::Msg>,
        event: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let label = match event {
            SupervisionEvent::ActorStarted(_) => "started",
            SupervisionEvent::ActorTerminated(_, _, _) => "terminated",
            SupervisionEvent::ActorFailed(_, _) => "failed",
            SupervisionEvent::ProcessGroupChanged(_) => return Ok(()),
        };
        let _ = self.events.send(label);
        Ok(())
    }
}

enum ChildMessage {
    Panic,
}

struct ChildProbe;

impl Actor for ChildProbe {
    type Msg = ChildMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        (): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ChildMessage::Panic => panic!("supervision probe"),
        }
    }
}

#[tokio::test]
async fn child_failure_notifies_without_killing_an_overriding_supervisor() {
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (supervisor, supervisor_handle) =
        Actor::spawn(None, SupervisorProbe { events: events_tx }, ())
            .await
            .expect("supervisor must start");
    let (child, child_handle) = supervisor
        .spawn_linked(None, ChildProbe, ())
        .await
        .expect("child must start");

    assert_eq!(events_rx.recv().await, Some("started"));
    ractor::cast!(child, ChildMessage::Panic).expect("panic request must be accepted");
    child_handle.await.expect("ractor must contain child panic");
    assert_eq!(events_rx.recv().await, Some("failed"));

    ractor::call!(supervisor, SupervisorMessage::Ping)
        .expect("surviving supervisor must accept a call");
    supervisor.stop(None);
    supervisor_handle
        .await
        .expect("supervisor must stop cleanly");
}

#[derive(Debug)]
struct DropWitness(Arc<AtomicUsize>);

impl Drop for DropWitness {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

enum CustodyMessage {
    Park {
        value: DropWitness,
        entered: oneshot::Sender<()>,
    },
}

struct CustodyProbe;

impl Actor for CustodyProbe {
    type Msg = CustodyMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        (): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CustodyMessage::Park { value, entered } => {
                entered.send(()).map_err(|_| "probe receiver dropped")?;
                let _value = value;
                std::future::pending().await
            }
        }
    }
}

#[tokio::test]
async fn kill_drops_an_in_flight_local_payload_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (actor, handle) = Actor::spawn(None, CustodyProbe, ())
        .await
        .expect("custody actor must start");
    let (entered_tx, entered_rx) = oneshot::channel();
    ractor::cast!(
        actor,
        CustodyMessage::Park {
            value: DropWitness(Arc::clone(&drops)),
            entered: entered_tx,
        }
    )
    .expect("custody message must be accepted");
    entered_rx.await.expect("handler must own the payload");
    let (queued_tx, _queued_rx) = oneshot::channel();
    ractor::cast!(
        actor,
        CustodyMessage::Park {
            value: DropWitness(Arc::clone(&drops)),
            entered: queued_tx,
        }
    )
    .expect("queued custody message must be accepted");

    actor
        .kill_and_wait(Some(Duration::from_secs(1)))
        .await
        .expect("kill must interrupt the parked handler");
    handle.await.expect("actor task must finish");
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

enum ReplyMessage {
    ReplyAfterRelease {
        request: DropWitness,
        response: DropWitness,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
        done: oneshot::Sender<bool>,
        reply: RpcReplyPort<DropWitness>,
    },
}

struct ReplyProbe;

impl Actor for ReplyProbe {
    type Msg = ReplyMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        (): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ReplyMessage::ReplyAfterRelease {
                request,
                response,
                entered,
                release,
                done,
                reply,
            } => {
                entered.send(()).map_err(|_| "probe receiver dropped")?;
                release.await.map_err(|_| "probe release dropped")?;
                let receiver_was_dropped = reply.send(response).is_err();
                drop(request);
                let _ = done.send(receiver_was_dropped);
                Ok(())
            }
        }
    }
}

#[tokio::test]
async fn abandoned_rpc_drops_request_and_reply_custody_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (actor, handle) = Actor::spawn(None, ReplyProbe, ())
        .await
        .expect("reply actor must start");
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (done_tx, done_rx) = oneshot::channel();
    let call_actor = actor.clone();
    let call_drops = Arc::clone(&drops);
    let call = tokio::spawn(async move {
        call_actor
            .call(
                |reply| ReplyMessage::ReplyAfterRelease {
                    request: DropWitness(Arc::clone(&call_drops)),
                    response: DropWitness(call_drops),
                    entered: entered_tx,
                    release: release_rx,
                    done: done_tx,
                    reply,
                },
                None,
            )
            .await
    });
    entered_rx.await.expect("handler must own both values");
    call.abort();
    assert!(call
        .await
        .expect_err("call task must be cancelled")
        .is_cancelled());
    release_tx.send(()).expect("handler must still be running");
    assert!(done_rx.await.expect("handler must report reply settlement"));

    actor.stop(None);
    handle.await.expect("reply actor must stop cleanly");
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn forced_tree_shutdown_must_be_explicit() {
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let (parent, parent_handle) = Actor::spawn(None, SupervisorProbe { events: events_tx }, ())
        .await
        .expect("parent must start");
    let (child, child_handle) = parent
        .spawn_linked(None, ChildProbe, ())
        .await
        .expect("child must start");

    parent.stop_children(Some("owner exited".into()));
    parent
        .kill_and_wait(Some(Duration::from_secs(1)))
        .await
        .expect("parent kill must finish");
    parent_handle.await.expect("parent task must finish");
    child_handle.await.expect("child task must finish");
    assert_eq!(child.get_status(), ractor::ActorStatus::Stopped);
}
