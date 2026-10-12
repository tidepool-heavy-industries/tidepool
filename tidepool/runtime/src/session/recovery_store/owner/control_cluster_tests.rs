use super::*;

#[test]
fn accepted_settlement_survives_lost_observer_and_reobserves_exact_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    let ticket = store.begin_native_transition().unwrap();
    let (reply, observer) = mpsc::sync_channel(1);
    drop(observer);
    store.handle.sender.send(Command::Settle {
        id: ticket.id, reply: Reply::Blocking(reply),
    }).unwrap();
    assert_eq!(store.status(), NativeStoreStatus::Ready);
    let (reply, result) = mpsc::sync_channel(1);
    store.handle.sender.send(Command::Settle {
        id: ticket.id, reply: Reply::Blocking(reply),
    }).unwrap();
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
    store.handle.sender.send(Command::Settle {
        id: ticket.id, reply: Reply::Blocking(reply),
    }).unwrap();
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
