use super::*;

#[tokio::test]
async fn reservation_issued_before_seal_cannot_authorize_later_reservation() {
    let (control, _task) = RunControlClient::start().await.unwrap();
    let reservation = control.reserve(crate::Incarnation(1)).await.unwrap();
    let issued = reservation.identity();
    let membership = control.seal().await.unwrap();
    assert!(membership.actors.is_empty());
    assert!(matches!(
        control.reserve(crate::Incarnation(1)).await,
        Err(DirectoryError::Closed)
    ));
    assert!(control.resolve(issued).await.unwrap().is_none());
    drop(reservation);
    assert!(control.seal().await.unwrap().actors.is_empty());
    control.owner.stop(None);
}

#[tokio::test]
async fn seal_overtaking_prepared_input_coordinator_preserves_cleanup_reservation() {
    let (control, task) = RunControlClient::start().await.unwrap();
    let (issued, reservation) = oneshot::channel();
    let (sealed, membership) = oneshot::channel();
    // Enqueue both commands before yielding: Seal precedes the supervised
    // starter's asynchronous completion on the run-control mailbox.
    control
        .owner
        .send_message(RunControlMessage::ReserveExistingInput {
            incarnation: crate::Incarnation(1),
            reply: issued,
        })
        .unwrap();
    control
        .owner
        .send_message(RunControlMessage::Seal { reply: sealed })
        .unwrap();
    assert!(membership.await.unwrap().actors.is_empty());
    let reservation = reservation.await.unwrap().unwrap();
    assert!(reservation.cleanup_only());
    assert!(control
        .resolve(reservation.identity())
        .await
        .unwrap()
        .is_none());
    assert!(
        matches!(
            control.settle_root_preparations(Duration::from_millis(20)).await,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ),
        "an outstanding prepared reservation still owns unsettled custody"
    );
    drop(reservation);
    assert!(matches!(
        control.settle_root_preparations(Duration::from_secs(2)).await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    assert!(matches!(
        control.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    control.finish_run().await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn finalizing_run_refuses_late_prepared_submission_without_running_producer() {
    let (control, task) = RunControlClient::start().await.unwrap();
    control.seal().await.unwrap();
    assert!(matches!(
        control.settle_root_preparations(Duration::from_secs(2)).await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    assert!(matches!(
        control.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    let grants = control
        .ask(|reply| RunControlMessage::FinishRun {
            members: Vec::new(),
            reply,
        })
        .await
        .unwrap()
        .unwrap();
    assert!(grants.is_empty());
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = started.clone();
    let producer: RootOperation = Box::new(move |_ticket| {
        Box::pin(async move {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
            (
                Err(std::io::Error::other("refused producer executed").into()),
                crate::CleanupComponentOutcome::Confirmed,
            )
        })
    });
    assert!(matches!(
        control.prepare_existing_input(producer).await,
        Err(DirectoryError::Closed)
    ));
    assert!(matches!(
        control.reserve_existing_input(crate::Incarnation(1)).await,
        Err(DirectoryError::Closed)
    ));
    assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
    control.finish_run().await.unwrap();
    task.await.unwrap();
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn reservation_and_seal_histories_preserve_issued_identity_and_cutoff(
        operations in proptest::collection::vec((0u8..3, 0usize..3), 1..24)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (control, _task) = RunControlClient::start().await.unwrap();
            let mut running = true;
            let mut issued = std::collections::HashSet::new();
            let mut held: [Option<ActorReservation>; 3] = std::array::from_fn(|_| None);
            for (operation, slot) in operations {
                match operation {
                    0 => {
                        let result = control.reserve(crate::Incarnation(1)).await;
                        assert_eq!(result.is_ok(), running);
                        if let Ok(reservation) = result {
                            assert!(issued.insert(reservation.identity()), "identity issued twice");
                            held[slot] = Some(reservation);
                        } else {
                            assert!(matches!(result, Err(DirectoryError::Closed)));
                        }
                    }
                    1 => { held[slot].take(); }
                    _ => {
                        assert!(control.seal().await.unwrap().actors.is_empty());
                        running = false;
                    }
                }
            }
            drop(held);
            control.seal().await.unwrap();
            control.owner.stop(None);
        });
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn preparation_observer_and_close_histories_retain_admitted_producers(
        operations in proptest::collection::vec((0u8..4, 0usize..3), 1..20)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let directory = Arc::new(tempfile::tempdir().unwrap());
            let (control, task) = RunControlClient::start().await.unwrap();
            let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut open = true;
            let mut admitted = 0;
            let mut observers: [Option<tokio::task::JoinHandle<()>>; 3] = std::array::from_fn(|_| None);
            let mut releases = Vec::new();
            let mut dropped_observers = Vec::new();
            // Every generated history admits a real producer before varying
            // cancellation, release and close ordering.
            for (operation, slot) in std::iter::once((0, 0)).chain(operations) {
                match operation {
                    0 => {
                        let (release, gate) = oneshot::channel();
                        let (started, start) = oneshot::channel();
                        let completed = completed.clone();
                        let directory = directory.clone();
                        let producer: RootOperation = Box::new(move |_ticket| Box::pin(async move {
                            let _ = started.send(());
                            let _ = gate.await;
                            // This real read has no resource to retire; its error
                            // is deliberately delivered through the production result path.
                            let error = std::fs::metadata(directory.path().join("absent-input")).unwrap_err();
                            completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            (Err(Box::new(error) as Box<dyn std::error::Error + Send + Sync>), crate::CleanupComponentOutcome::Confirmed)
                        }));
                        if open {
                            if let Some(observer) = observers[slot].take() { observer.abort(); dropped_observers.push(observer); }
                            let client = control.clone();
                            observers[slot] = Some(tokio::spawn(async move {
                                assert!(matches!(client.prepare_root(producer).await, Ok(Err(_))));
                            }));
                            tokio::time::timeout(Duration::from_secs(2), start).await.unwrap().unwrap();
                            releases.push(release);
                            admitted += 1;
                        } else {
                            assert!(matches!(control.prepare_root(producer).await, Err(DirectoryError::Closed)));
                        }
                    }
                    1 => { if let Some(observer) = observers[slot].take() { observer.abort(); dropped_observers.push(observer); } }
                    2 => { control.seal().await.unwrap(); open = false; }
                    _ => { if let Some(release) = releases.pop() { let _ = release.send(()); } }
                }
            }
            control.seal().await.unwrap();
            for release in releases { let _ = release.send(()); }
            for observer in observers.into_iter().flatten().chain(dropped_observers) {
                if let Err(error) = observer.await { assert!(error.is_cancelled(), "producer observer panicked: {error}"); }
            }
            assert!(matches!(control.settle_root_preparations(Duration::from_secs(2)).await, crate::CleanupComponentOutcome::Confirmed));
            assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), admitted);
            assert!(matches!(control.shutdown_run_resources().await, crate::CleanupComponentOutcome::Confirmed));
            control.finish_run().await.unwrap();
            task.await.unwrap();
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_seal_cancels_shared_physical_work_before_cleanup_confirmation() {
    let (control, task) = RunControlClient::start().await.unwrap();
    let (other_control, other_task) = RunControlClient::start().await.unwrap();
    let other_ticket = crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(
        other_control.preparation_client().producer(),
    )
    .register_work()
    .unwrap();
    let other_cancellation = other_ticket.cancellation().host_admission_cancellation();
    let client = control.preparation_client();
    let observer = crate::RetainedActorExit::new();
    let owner = crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(observer.clone())
        .shared_preparation(client.producer());
    let ticket = owner.register_work().unwrap();
    let cancellation = ticket.cancellation().host_admission_cancellation();
    let observed_cancellation = cancellation.clone();
    let (started, entered) = tokio::sync::oneshot::channel();
    let (cancelled, stopped) = tokio::sync::oneshot::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let physical = tokio::task::spawn_blocking(move || {
        ticket.run(|_| {
            started.send(()).unwrap();
            while cancellation.checkpoint().is_ok() {
                std::thread::yield_now();
            }
            cancelled.send(()).unwrap();
            finish.recv_timeout(Duration::from_secs(10)).unwrap();
        })
    });
    entered.await.unwrap();
    // One caller's retirement owns observation, never the producer stop edge.
    observer.request_shutdown(ActorTerminal::new(
        ActorExitKind::Cancelled,
        "waiter stopped",
    ));
    assert!(client.claim(|| ()).is_ok());
    observed_cancellation.checkpoint().unwrap();
    control.seal().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), stopped)
        .await
        .unwrap()
        .unwrap();
    other_cancellation.checkpoint().unwrap();
    assert!(client.claim(|| ()).is_err());
    assert!(
        crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(client.producer())
            .register_work()
            .is_err()
    );
    assert!(!control.shared_cleanup.observation().is_confirmed());
    assert!(matches!(
        control
            .settle_run_preparations(Duration::from_millis(10))
            .await,
        crate::CleanupComponentOutcome::Unconfirmed(_)
    ));
    assert_eq!(
        control.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    );
    assert!(
        control.finish_run().await.is_err(),
        "resource settlement cannot finalize pending shared compilation"
    );
    release.send(()).unwrap();
    physical.await.unwrap();
    assert_eq!(
        control
            .settle_run_preparations(Duration::from_secs(2))
            .await,
        crate::CleanupComponentOutcome::Confirmed
    );
    other_ticket.run(|_| ());
    other_control.seal().await.unwrap();
    assert_eq!(
        other_control
            .settle_run_preparations(Duration::from_secs(2))
            .await,
        crate::CleanupComponentOutcome::Confirmed
    );
    other_control.owner.stop(None);
    other_task.await.unwrap();
    control.owner.stop(None);
    task.await.unwrap();
}

#[tokio::test]
async fn observing_root_preparations_preserves_open_shared_admission() {
    let (control, task) = RunControlClient::start().await.unwrap();
    assert_eq!(
        control.settle_root_preparations(Duration::from_secs(1)).await,
        crate::CleanupComponentOutcome::Confirmed
    );
    assert!(control.preparation_client().claim(|| ()).is_ok());
    assert!(!control.shared_cleanup.observation().is_confirmed());
    control.seal().await.unwrap();
    assert_eq!(
        control
            .settle_run_preparations(Duration::from_secs(1))
            .await,
        crate::CleanupComponentOutcome::Confirmed
    );
    control.owner.stop(None);
    task.await.unwrap();
}
