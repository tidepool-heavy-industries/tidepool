use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Released(Arc<AtomicUsize>);
impl Drop for Released {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn pending(thread: i64, dropped: Arc<AtomicUsize>) -> (Pending, tokio::sync::oneshot::Sender<()>) {
    let (send, receive) = tokio::sync::oneshot::channel();
    let lease = Released(dropped);
    (
        Pending {
            thread,
            future: Box::pin(async move {
                let _lease = lease;
                receive.await.expect("controlled readiness");
                Completion {
                    thread,
                    scopes: Vec::new(),
                    result: None,
                    receipt: None,
                    terminal: Vec::new(),
                }
            }),
        },
        send,
    )
}

fn work() -> Arc<InvocationWork> {
    InvocationWork::new(
        ActorRef {
            id: crate::ActorId(45),
            incarnation: crate::Incarnation::FIRST,
        },
        RequestReservationOwner::Scope(99),
    )
}

fn thread(parent: i64) -> Thread {
    Thread {
        parent,
        work: work(),
        realm: RealmId::fresh(),
        status: ThreadStatus::Running,
        control: crate::WorkbenchExecutionControl::untracked(),
    }
}

#[tokio::test]
async fn independent_frontiers_settle_in_reverse_order_without_losing_sibling() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let (first, finish_first) = pending(1, dropped.clone());
    let (second, finish_second) = pending(2, dropped.clone());
    let mut green = GreenThreads {
        pending: vec![first, second],
        ..Default::default()
    };
    finish_second.send(()).unwrap();
    assert_eq!(green.next().await.unwrap().thread, 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(green.pending.len(), 1);
    finish_first.send(()).unwrap();
    assert_eq!(green.next().await.unwrap().thread, 1);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancelling_thread_discards_descendants_and_preserves_sibling_readiness() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let (first, _finish_first) = pending(1, dropped.clone());
    let (nested, _finish_nested) = pending(3, dropped.clone());
    let (sibling, finish_sibling) = pending(2, dropped.clone());
    let mut green = GreenThreads {
        threads: [(1, thread(0)), (2, thread(0)), (3, thread(1))].into(),
        pending: vec![first, nested, sibling],
        ..Default::default()
    };
    assert_eq!(green.remove_descendant_frontiers(1), [1, 3]);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    finish_sibling.send(()).unwrap();
    assert_eq!(green.next().await.unwrap().thread, 2);
    assert_eq!(dropped.load(Ordering::SeqCst), 3);
}

#[test]
fn parent_cancellation_fences_every_child_and_releases_all_effect_futures() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let (first, _finish_first) = pending(1, dropped.clone());
    let (second, _finish_second) = pending(2, dropped.clone());
    let mut green = GreenThreads {
        threads: [(1, thread(0)), (2, thread(0))].into(),
        pending: vec![first, second],
        ..Default::default()
    };
    green.cancel_parent();
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
    assert!(green.pending.is_empty());
    assert!(green
        .threads
        .values()
        .all(|thread| thread.work.with_admission(|| ()).is_err()));
}

#[test]
fn join_validates_all_handles_and_observes_terminal_input_order() {
    let mut green = GreenThreads {
        threads: [(1, thread(0)), (2, thread(0))].into(),
        ..Default::default()
    };
    assert_eq!(green.winner(&[1, 2]).unwrap(), None);
    green.threads.get_mut(&2).unwrap().status = ThreadStatus::Cancelled;
    assert_eq!(green.winner(&[1, 2]).unwrap(), Some(2));
    green.threads.get_mut(&1).unwrap().status = ThreadStatus::Settled;
    assert_eq!(green.winner(&[1, 2]).unwrap(), Some(1));
    assert_eq!(green.winner(&[2, 1]).unwrap(), Some(2));
    assert!(green.winner(&[1, 99]).is_err());
    assert!(green.winner(&[]).is_err());
}
