use super::*;

#[test]
fn accepted_settlement_survives_lost_observer_and_reobserves_exact_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    let ticket = store.begin_native_transition().unwrap();
    let (reply, observer) = mpsc::sync_channel(1);
    drop(observer);
    store
        .handle
        .sender
        .send(Command::Settle {
            id: ticket.id,
            outcome: SettlementOutcome::Completed,
            reply: Reply::Blocking(reply),
        })
        .unwrap();
    assert_eq!(store.status(), NativeStoreStatus::Ready);
    let (reply, result) = mpsc::sync_channel(1);
    store
        .handle
        .sender
        .send(Command::Settle {
            id: ticket.id,
            outcome: SettlementOutcome::Completed,
            reply: Reply::Blocking(reply),
        })
        .unwrap();
    result.recv().unwrap().unwrap();
    drop(ticket);
    assert_eq!(store.status(), NativeStoreStatus::Ready);
}

#[test]
fn stale_abandonment_after_known_settlement_cannot_fence_new_operation() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    let ticket = store.begin_native_transition().unwrap();
    let (reply, observer) = mpsc::sync_channel(1);
    store
        .handle
        .sender
        .send(Command::Settle {
            id: ticket.id,
            outcome: SettlementOutcome::Completed,
            reply: Reply::Blocking(reply),
        })
        .unwrap();
    // Completion submission certifies the local transition already happened.
    // The observer's acknowledgement is buffered and does not issue that fact.
    assert_eq!(store.status(), NativeStoreStatus::Ready);
    let successor = store.begin_native_transition().unwrap();
    drop(observer);
    drop(ticket);
    assert_eq!(store.status(), NativeStoreStatus::Pending);
    successor.complete_blocking().unwrap();
    assert_eq!(store.status(), NativeStoreStatus::Ready);
}

#[tokio::test]
async fn lost_provider_grant_receiver_fences_the_actual_native_owner() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create_async(directory.path().join("run.sqlite"))
        .await
        .unwrap();
    let (reply, observer) = tokio::sync::oneshot::channel();
    store
        .handle
        .sender
        .send(Command::Begin {
            store: store.clone(),
            reply: Reply::Async(reply),
        })
        .unwrap();
    drop(observer);
    let failure = store.wait_recovery_required().await;
    assert_eq!(failure.kind, StoreErrorKind::RecoveryRequired);
    assert!(matches!(
        store.status_async().await,
        NativeStoreStatus::Fenced(_)
    ));
}

#[test]
fn private_storage_worker_panic_fences_and_settles_its_pending_request() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    store.inject_fault(StoreFault::WorkerPanic);
    assert!(matches!(
        store.reserve_ceiling(RecoveryDomainId::Root, StoreRevision(0), Generation(1)),
        StoreDisposition::RecoveryRequired(_)
    ));
    assert!(matches!(store.status(), NativeStoreStatus::Fenced(_)));
    assert!(store.begin_native_transition().is_err());
}

#[test]
fn conflicting_retained_settlement_is_refused_without_changing_known_completion() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    let ticket = store.begin_native_transition().unwrap();
    let id = ticket.id;
    ticket.complete_blocking().unwrap();
    let (reply, result) = mpsc::sync_channel(1);
    store
        .handle
        .sender
        .send(Command::Settle {
            id,
            outcome: SettlementOutcome::Refused,
            reply: Reply::Blocking(reply),
        })
        .unwrap();
    assert_eq!(
        result.recv().unwrap().unwrap_err().kind,
        StoreErrorKind::Integrity
    );
    assert_eq!(store.status(), NativeStoreStatus::Ready);
}

#[test]
fn stopped_owner_observation_does_not_fabricate_zero_work() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    store.handle.sender.send(Command::Shutdown).unwrap();
    assert_eq!(
        store.work().unwrap_err().kind,
        StoreErrorKind::RecoveryRequired
    );
}

#[tokio::test]
async fn provider_transition_retains_its_original_native_owner_until_settlement() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        RunRecoveryStore::create_async(directory.path().join("run.sqlite"))
            .await
            .unwrap(),
    );
    let observed = Arc::downgrade(&store);
    let attachment = Arc::new(());
    let observed_attachment = Arc::downgrade(&attachment);
    let ticket = store
        .begin_provider_transition()
        .await
        .unwrap()
        .retain_attachment(attachment, Arc::clone(&store));
    drop(store);
    assert!(observed.upgrade().is_some());
    assert!(observed_attachment.upgrade().is_some());
    ticket.complete().await.unwrap();
    assert!(observed.upgrade().is_none());
    assert!(observed_attachment.upgrade().is_none());
}
