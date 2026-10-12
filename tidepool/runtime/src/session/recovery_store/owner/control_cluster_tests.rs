use super::*;

#[test]
fn lost_settlement_reply_fences_the_real_native_owner() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    let ticket = store.begin_native_transition().unwrap();
    let (reply, observer) = mpsc::sync_channel(1);
    drop(observer);
    store.handle.sender.send(Command::Settle {
        id: ticket.id, reply: Reply::Blocking(reply),
    }).unwrap();
    assert!(matches!(store.status(), NativeStoreStatus::Fenced(_)));
    drop(ticket);
    assert!(store.begin_native_transition().is_err());
}

#[test]
fn unobserved_buffered_settlement_ack_does_not_authorize_ticket_abandonment() {
    let directory = tempfile::tempdir().unwrap();
    let store = RunRecoveryStore::create(&directory.path().join("run.sqlite")).unwrap();
    let ticket = store.begin_native_transition().unwrap();
    let (reply, observer) = mpsc::sync_channel(1);
    store.handle.sender.send(Command::Settle {
        id: ticket.id, reply: Reply::Blocking(reply),
    }).unwrap();
    // Status is an ordered owner round trip: the successful acknowledgement is
    // buffered, but the affine ticket has not observed it.
    assert_eq!(store.status(), NativeStoreStatus::Ready);
    drop(observer);
    drop(ticket);
    assert!(matches!(store.status(), NativeStoreStatus::Fenced(_)));
    assert!(store.begin_native_transition().is_err());
}
