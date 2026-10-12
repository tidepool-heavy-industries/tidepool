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
    drop(reservation);
    assert!(matches!(
        control.settle_preparations(Duration::from_secs(2)).await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    assert!(matches!(
        control.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    ));
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
            assert!(matches!(control.settle_preparations(Duration::from_secs(2)).await, crate::CleanupComponentOutcome::Confirmed));
            assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), admitted);
            assert!(matches!(control.shutdown_run_resources().await, crate::CleanupComponentOutcome::Confirmed));
            control.finish_run().await.unwrap();
            task.await.unwrap();
        });
    }
}
