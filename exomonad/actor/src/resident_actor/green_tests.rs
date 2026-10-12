use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Released(Arc<AtomicUsize>);
impl Drop for Released {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn pending(
    thread: i64,
    dropped: Arc<AtomicUsize>,
) -> (Pending<()>, tokio::sync::oneshot::Sender<()>) {
    let (send, receive) = tokio::sync::oneshot::channel();
    let lease = Released(dropped);
    (
        Pending {
            thread,
            future: maybe_done(Box::pin(async move {
                let _lease = lease;
                receive.await.expect("controlled readiness");
                Completion {
                    thread,
                    scopes: Vec::new(),
                    result: None,
                    attachment: None,
                    terminal: Vec::new(),
                }
            })),
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
    let mut green = GreenInvocation::<()> {
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
    let mut green = GreenInvocation::<()> {
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
    let mut green = GreenInvocation::<()> {
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
    let mut green = GreenInvocation::<()> {
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

#[tokio::test]
async fn opaque_adapter_attachment_preserves_infrastructure_failure() {
    let mut green = GreenInvocation::<u64>::default();
    green.enqueue_frontier(
        Vec::new(),
        Box::pin(async {
            (
                Err(ResidentActorWorkbenchError::ActorProtocol(
                    "controlled infrastructure failure".into(),
                )),
                73,
            )
        }),
    );
    let completion = green.next().await.unwrap();
    assert_eq!(completion.attachment, Some(73));
    assert!(matches!(
        completion.result,
        Some(Err(ResidentActorWorkbenchError::ActorProtocol(_)))
    ));
    assert!(completion.terminal.is_empty());
    assert!(green.threads.is_empty());
}

#[tokio::test]
async fn parked_frontier_progress_releases_fifo_checkout_before_native_advance() {
    use tidepool_runtime::session::registry::{CheckoutRequest, SessionRegistry};
    let session = tidepool_repr::SessionId(991);
    let machines = Arc::new(SessionRegistry::<(), u8>::new());
    assert!(machines.insert_idle(session, Box::new(())).is_none());
    let (machine, held) = machines.checkout_run(session).unwrap().into_parts();
    let pending_machines = machines.clone();
    let acquired = Arc::new(AtomicUsize::new(0));
    let observed = acquired.clone();
    let queued = Pending {
        thread: 1,
        future: maybe_done(Box::pin(async move {
            let (machine, receipt) = pending_machines
                .checkout_queued(session, CheckoutRequest::Run)
                .await
                .unwrap()
                .into_parts();
            observed.fetch_add(1, Ordering::SeqCst);
            pending_machines.settle_suspended(receipt, machine, Vec::new());
            Completion {
                thread: 1,
                scopes: Vec::new(),
                result: None,
                attachment: None,
                terminal: Vec::new(),
            }
        })),
    };
    let (ready, complete) = pending(2, Arc::new(AtomicUsize::new(0)));
    complete.send(()).unwrap();
    let mut green = GreenInvocation::<()> {
        pending: vec![queued, ready],
        threads: BTreeMap::from([(1, thread(0))]),
        ..Default::default()
    };
    assert!(
        green.active_work().is_none(),
        "parent main can retain an unjoined child frontier"
    );
    assert_eq!(green.next().await.unwrap().thread, 2);
    assert_eq!(acquired.load(Ordering::SeqCst), 0);
    machines.settle_suspended(held, machine, Vec::new());
    let native = async {
        let (machine, receipt) = machines
            .checkout_queued(session, CheckoutRequest::Run)
            .await
            .unwrap()
            .into_parts();
        assert_eq!(acquired.load(Ordering::SeqCst), 1);
        machines.settle_suspended(receipt, machine, Vec::new());
    };
    tokio::pin!(native);
    assert!(matches!(futures_util::poll!(&mut native), Poll::Pending));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut execution = InvocationExecution::from_green(green);
        execution.drive(native).await;
        green = execution.green.take().unwrap();
        assert_eq!(green.pending.len(), 1);
        assert_eq!(green.next().await.unwrap().thread, 1);
        assert!(green.pending.is_empty());
    })
    .await
    .expect("co-polling releases the original queued checkout");
}

#[tokio::test]
async fn cancelling_queued_frontier_releases_fifo_position_without_running_it() {
    use tidepool_runtime::session::registry::{CheckoutRequest, SessionRegistry};
    for descendant_only in [false, true] {
        let session = tidepool_repr::SessionId(992);
        let machines = Arc::new(SessionRegistry::<(), u8>::new());
        assert!(machines.insert_idle(session, Box::new(())).is_none());
        let (machine, held) = machines.checkout_run(session).unwrap().into_parts();
        let queued_machines = machines.clone();
        let acquired = Arc::new(AtomicUsize::new(0));
        let observed = acquired.clone();
        let mut green = GreenInvocation::<()> {
            threads: [(1, thread(0))].into(),
            pending: vec![Pending {
                thread: 1,
                future: maybe_done(Box::pin(async move {
                    let (machine, receipt) = queued_machines
                        .checkout_queued(session, CheckoutRequest::Run)
                        .await
                        .unwrap()
                        .into_parts();
                    observed.fetch_add(1, Ordering::SeqCst);
                    queued_machines.settle_suspended(receipt, machine, Vec::new());
                    Completion {
                        thread: 1,
                        scopes: Vec::new(),
                        result: None,
                        attachment: None,
                        terminal: Vec::new(),
                    }
                })),
            }],
            ..Default::default()
        };
        {
            let mut frontier = Box::pin(green.next());
            assert!(matches!(futures_util::poll!(&mut frontier), Poll::Pending));
        }
        let native = machines.checkout_queued(session, CheckoutRequest::Run);
        tokio::pin!(native);
        assert!(matches!(futures_util::poll!(&mut native), Poll::Pending));
        if descendant_only {
            assert_eq!(green.remove_descendant_frontiers(1), [1]);
        } else {
            green.cancel_parent();
        }
        machines.settle_suspended(held, machine, Vec::new());
        let checkout = tokio::time::timeout(std::time::Duration::from_secs(2), native)
            .await
            .expect("cancelled waiter releases original FIFO position")
            .unwrap();
        assert_eq!(acquired.load(Ordering::SeqCst), 0);
        let (machine, receipt) = checkout.into_parts();
        machines.settle_suspended(receipt, machine, Vec::new());
        assert!(green.pending.is_empty());
    }
}

#[tokio::test]
async fn buffered_frontier_completion_retains_exact_attachment_until_selection_or_cancel() {
    for cancelled in [false, true] {
        let dropped = Arc::new(AtomicUsize::new(0));
        let lease = Released(dropped.clone());
        let mut green = GreenInvocation::<Released>::default();
        green.enqueue_frontier(
            Vec::new(),
            Box::pin(async move { (Err(protocol("original frontier failure")), lease) }),
        );
        for _ in 0..3 {
            let mut execution = InvocationExecution::from_green(green);
            execution.drive(std::future::ready(())).await;
            green = execution.green.take().unwrap();
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert_eq!(green.active, 0);
        assert_eq!(green.pending.len(), 1);
        if cancelled {
            green.cancel_parent();
        } else {
            let completion = green.next().await.unwrap();
            assert!(completion.attachment.is_some());
            assert!(matches!(
                completion.result,
                Some(Err(ResidentActorWorkbenchError::ActorProtocol(_)))
            ));
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            drop(completion);
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(green.pending.is_empty());
    }
}

#[test]
fn generated_fifo_histories_preserve_cancelled_and_buffered_frontier_ownership() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    use tidepool_runtime::session::registry::{CheckoutRequest, SessionRegistry};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for partition in 0_u8..8 {
        let cancel_parent = partition & 1 != 0;
        let drop_driver = partition & 2 != 0;
        let cancel_buffered = partition & 4 != 0;
        let mut seed = [0x6d; 32];
        seed[0] = partition;
        let mut config = Config::with_cases(8);
        config.failure_persistence = None;
        let mut runner =
            TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &seed));
        let strategy = (
            1_usize..7,
            proptest::collection::vec(any::<bool>(), 6),
            0_usize..4,
        );
        eprintln!("FIFO ownership partition={partition} requested_cases=8 seed={seed:02x?}");
        runner
            .run(&strategy, |(count, cancelled, buffer_polls)| {
                runtime.block_on(async {
                    let session = tidepool_repr::SessionId(994);
                    let machines = Arc::new(SessionRegistry::<(), u8>::new());
                    assert!(machines.insert_idle(session, Box::new(())).is_none());
                    let (machine, held) = machines.checkout_run(session).unwrap().into_parts();
                    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
                    let dropped = Arc::new(AtomicUsize::new(0));
                    let mut green = GreenInvocation::<Released>::default();
                    for index in 0..count {
                        let id = index as i64 + 1;
                        green.threads.insert(id, thread(0));
                        let queue = machines.clone();
                        let observed = observed.clone();
                        let lease = Released(dropped.clone());
                        green.pending.push(Pending {
                            thread: id,
                            future: maybe_done(Box::pin(async move {
                                let (machine, receipt) = queue
                                    .checkout_queued(session, CheckoutRequest::Run)
                                    .await
                                    .unwrap()
                                    .into_parts();
                                observed.lock().unwrap().push(id);
                                queue.settle_suspended(receipt, machine, Vec::new());
                                Completion {
                                    thread: id,
                                    scopes: Vec::new(),
                                    result: None,
                                    attachment: Some(lease),
                                    terminal: Vec::new(),
                                }
                            })),
                        });
                    }
                    assert!(matches!(
                        futures_util::poll!(Box::pin(green.next())),
                        Poll::Pending
                    ));
                    let mut execution = InvocationExecution::from_green(green);
                    let native = machines.checkout_queued(session, CheckoutRequest::Run);
                    tokio::pin!(native);
                    assert!(matches!(futures_util::poll!(&mut native), Poll::Pending));
                    if drop_driver {
                        let mut driving = Box::pin(execution.drive(&mut native));
                        assert!(matches!(futures_util::poll!(&mut driving), Poll::Pending));
                        drop(driving);
                    }
                    if cancel_parent {
                        execution.cancel_parent();
                    } else {
                        for (index, cancelled) in cancelled.iter().take(count).enumerate() {
                            if *cancelled {
                                execution
                                    .green
                                    .as_mut()
                                    .unwrap()
                                    .remove_descendant_frontiers(index as i64 + 1);
                            }
                        }
                    }
                    let expected: Vec<_> = (0..count)
                        .filter(|index| !cancel_parent && !cancelled[*index])
                        .map(|index| index as i64 + 1)
                        .collect();
                    machines.settle_suspended(held, machine, Vec::new());
                    let checkout = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        execution.drive(&mut native),
                    )
                    .await
                    .expect("original queued survivors precede native advance")
                    .unwrap();
                    let (machine, receipt) = checkout.into_parts();
                    machines.settle_suspended(receipt, machine, Vec::new());
                    assert_eq!(*observed.lock().unwrap(), expected);
                    assert_eq!(dropped.load(Ordering::SeqCst), count - expected.len());
                    for _ in 0..buffer_polls {
                        execution.drive(std::future::ready(())).await;
                    }
                    assert_eq!(
                        dropped.load(Ordering::SeqCst),
                        count - expected.len(),
                        "buffered completion keeps its original attachment"
                    );
                    if cancel_buffered {
                        execution.cancel_parent();
                    } else {
                        for id in &expected {
                            let completion = execution.next().await.unwrap();
                            assert_eq!(completion.thread, *id);
                            assert!(completion.attachment.is_some());
                            drop(completion);
                        }
                    }
                    assert_eq!(
                        dropped.load(Ordering::SeqCst),
                        count,
                        "selection or cancellation releases each original attachment once"
                    );
                });
                Ok(())
            })
            .unwrap();
        eprintln!("FIFO ownership partition={partition} completed_cases=8");
    }
}
