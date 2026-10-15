use super::*;

async fn run_execution_identity_history(history: Vec<(Option<[u8; 16]>, bool, bool)>) {
    let mut fixture = behavior(false);
    fixture.behavior.owned_workbench = true;
    fixture.behavior.independent_workbench_admission = true;
    fixture.behavior.reconcile_retires_scope = true;
    let observed = Arc::new(Mutex::new(Vec::new()));
    fixture.behavior.observed_execution_ids = Some(observed.clone());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    fixture.behavior.workbench_gate = Some((entered.clone(), release.clone()));
    let (actor, task) = spawn_local_actor(None, fixture.behavior)
        .await
        .expect("actor");
    for (index, (supplied, cancel, drop_observer)) in history.into_iter().enumerate() {
        let control = crate::WorkbenchExecutionControl::untracked();
        let expected = supplied
            .map(tidepool_runtime::session::WorkbenchExecutionId::from_digest)
            .unwrap_or_else(|| control.execution_id(actor.identity()));
        let request = WorkbenchRequest::from_cell_input("pure ()");
        let request = match supplied {
            Some(_) => request.with_execution_id(expected.clone()),
            None => request,
        };
        let (reply, receive) = oneshot::channel();
        actor
            .submit_workbench(
                crate::ActorWorkbenchInvocation::unbound(request),
                Some(control.clone()),
                reply.into(),
            )
            .await
            .expect("submit public producer");
        entered.notified().await;
        assert_eq!(
            observed.lock()[index],
            Some(expected.clone()),
            "scheduler and execution must retain one issued identity"
        );
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::Execution {
            actor_id: actor.identity().id.0,
            incarnation: actor.identity().incarnation.0,
            execution_id: expected,
        };
        if cancel {
            control.request_cancellation();
        }
        let (reply, pending) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ReconcileWorkbenchBoundary {
                boundary: boundary.clone(),
                reply: reply.into(),
            })
            .unwrap();
        assert!(matches!(
            pending.await.unwrap(),
            crate::WorkbenchBoundaryReconciliation::Pending
        ));
        assert_eq!(
            fixture
                .calls
                .lock()
                .iter()
                .filter(|call| **call == "boundary-retired")
                .count(),
            index,
            "active execution must not enter scope retirement"
        );
        let receive = if drop_observer {
            drop(receive);
            None
        } else {
            Some(receive)
        };
        release.notify_one();
        if let Some(receive) = receive {
            assert!(receive.await.unwrap().is_ok());
        }
        // A mailbox probe waits behind completion, without assuming observer drop
        // stops the owned task. Retry Pending until the actual step settles.
        loop {
            let (reply, settled) = oneshot::channel();
            actor
                .address()
                .send_message(KernelMessage::ReconcileWorkbenchBoundary {
                    boundary: boundary.clone(),
                    reply: reply.into(),
                })
                .unwrap();
            match settled.await.unwrap() {
                crate::WorkbenchBoundaryReconciliation::Pending => tokio::task::yield_now().await,
                crate::WorkbenchBoundaryReconciliation::Settled => break,
                other => panic!("unexpected reconciliation: {other:?}"),
            }
        }
        assert_eq!(
            fixture
                .calls
                .lock()
                .iter()
                .filter(|call| **call == "boundary-retired")
                .count(),
            index + 1
        );
    }
    actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "identity history complete".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn public_no_id_execution_stays_pending_until_owner_settles() {
    tokio::time::timeout(
        Duration::from_secs(5),
        run_execution_identity_history(vec![(None, false, false)]),
    )
    .await
    .expect("history bounded");
}

#[test]
fn issued_execution_identity_survives_admission_and_reconciliation_histories() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let strategy = proptest::collection::vec(
        (
            proptest::option::of(any::<[u8; 16]>()),
            any::<bool>(),
            any::<bool>(),
        ),
        1..5,
    );
    let mut config = Config::with_cases(16);
    config.source_file = Some(file!());
    config.test_name =
        Some("issued_execution_identity_survives_admission_and_reconciliation_histories");
    TestRunner::new(config)
        .run(&strategy, |tail| {
            let mut history = vec![
                (None, false, false),
                (Some([61; 16]), false, false),
                (None, true, true),
            ];
            history.extend(tail);
            runtime.block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    run_execution_identity_history(history),
                )
                .await
                .unwrap()
            });
            Ok(())
        })
        .unwrap();
}
