use super::*;

struct Executor;
impl Actor for Executor {
    type Msg = KernelMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(&self, _: RactorRef<Self::Msg>, (): ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(&self, _: RactorRef<Self::Msg>, _: Self::Msg, _: &mut ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

async fn opened() -> (LifecycleHandle, RactorRef<KernelMessage>) {
    let identity = ActorRef::first(crate::ActorId(9101));
    let owner = LifecycleHandle::start(identity, RetainedActorExit::new()).await.unwrap();
    let (executor, _) = Actor::spawn(None, Executor, ()).await.unwrap();
    owner.attach_executor(executor.clone()).await.unwrap();
    owner.executor_started().await.unwrap();
    (owner, executor)
}

#[tokio::test]
async fn close_preserves_accepted_lease_and_refuses_new_admission() {
    let (owner, executor) = opened().await;
    let admitted = owner.transaction().await.unwrap();
    owner.close().await.unwrap();
    assert_eq!(owner.observation().phase, LifecyclePhase::Closing);
    assert_eq!(owner.observation().transactions, 1);
    assert!(matches!(owner.transaction().await, Err(KernelCallFailure::MailboxClosed(_))));
    let mut drained = Box::pin(owner.wait_transactions());
    assert!(futures_util::poll!(drained.as_mut()).is_pending());
    drop(admitted);
    tokio::time::timeout(std::time::Duration::from_secs(5), drained).await.unwrap().unwrap();
    assert_eq!(owner.observation().phase, LifecyclePhase::Closing);
    executor.stop(None);
    owner.address.stop(None);
}

#[tokio::test]
async fn lost_admission_observer_releases_its_exact_lease_without_reopening() {
    let (owner, executor) = opened().await;
    let (reply, observer) = tokio::sync::oneshot::channel();
    owner.address.cast(LifecycleCommand::Transaction(reply.into())).unwrap();
    drop(observer);
    owner.close().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), owner.wait_transactions()).await.unwrap().unwrap();
    assert_eq!(owner.observation().transactions, 0);
    assert_eq!(owner.observation().phase, LifecyclePhase::Closing);
    assert!(matches!(owner.transaction().await, Err(KernelCallFailure::MailboxClosed(_))));
    executor.stop(None);
    owner.address.stop(None);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn admission_histories_obey_close_and_exact_lease_release(
        steps in proptest::collection::vec((0u8..4, 0usize..4), 1..48)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (owner, executor) = opened().await;
            let mut open = true;
            let mut held: [Option<ActorAdmissionLease>; 4] = std::array::from_fn(|_| None);
            for (operation, slot) in steps {
                match operation {
                    0 => {
                        let result = owner.transaction().await;
                        assert_eq!(result.is_ok(), open);
                        if let Ok(lease) = result { held[slot] = Some(lease); }
                    }
                    1 => { held[slot].take(); }
                    2 => { owner.close().await.unwrap(); open = false; }
                    _ => {
                        // The stale sequence was never issued. It cannot close,
                        // reopen, or release a live operation.
                        owner.address.cast(LifecycleCommand::NativeCompleted {
                            sequence: u64::MAX, outcome: Ok(()),
                        }).unwrap();
                    }
                }
                // A same-mailbox round trip orders prior lease drops and stale
                // completions without using a timing assumption.
                let barrier = owner.transaction().await;
                assert_eq!(barrier.is_ok(), open);
                let snapshot = owner.observation();
                assert_eq!(snapshot.phase, if open { LifecyclePhase::Open } else { LifecyclePhase::Closing });
                assert_eq!(snapshot.transactions, held.iter().filter(|lease| lease.is_some()).count() + usize::from(open));
                drop(barrier);
            }
            drop(held);
            owner.close().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), owner.wait_transactions()).await.unwrap().unwrap();
            executor.stop(None);
            owner.address.stop(None);
        });
    }
}
