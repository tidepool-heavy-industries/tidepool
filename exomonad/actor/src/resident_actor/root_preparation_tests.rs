use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tidepool_runtime::session::{ModuleEnv, SessionLib};

#[tokio::test]
async fn killed_root_producer_retires_minted_native_placement_once() {
    let session_root = tempfile::tempdir().unwrap();
    let session_id = tidepool_runtime::session::fresh_session_id();
    let lib = SessionLib::open(
        session_id,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let (forest, _deployments) = ResidentForest::new(
        ActorWorkbenchSource::new("", Vec::new()),
        session_id,
        session,
        None,
        crate::Incarnation::FIRST,
    )
    .await
    .unwrap();
    let runner = forest.environment.runner.clone();
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = cleanup_calls.clone();
    let (published, minted) = tokio::sync::oneshot::channel();
    let directory = forest.directory.clone();
    let preparation = tokio::spawn(async move {
        directory
            .prepare_root(Box::new(move |ticket| {
                Box::pin(async move {
                    let cleanup_runner = runner.clone();
                    let publisher = ticket.placement_publisher(Arc::new(move |placement| {
                        let cleanup_runner = cleanup_runner.clone();
                        let observed_calls = observed_calls.clone();
                        Box::pin(async move {
                            observed_calls.fetch_add(1, Ordering::SeqCst);
                            match cleanup_runner
                                .retire_root_placement_wait(
                                    placement,
                                    Some(std::time::Duration::from_secs(5)),
                                )
                                .await
                            {
                                Ok(()) => crate::CleanupComponentOutcome::Confirmed,
                                Err(error) => {
                                    crate::CleanupComponentOutcome::Unconfirmed(error.to_string())
                                }
                            }
                        })
                    }));
                    let observer = publisher.observer_for_test();
                    let record = runner
                        .provision_root_scope_tracked(session_id, publisher.into_mint())
                        .await
                        .unwrap();
                    published
                        .send((record.placement(), observer))
                        .unwrap_or_else(|_| panic!("placement observer disappeared"));
                    std::future::pending::<(
                        crate::local_actor::run_control::RootOperationResult,
                        crate::CleanupComponentOutcome,
                    )>()
                    .await
                })
            }))
            .await
    });
    let (placement, publisher) = tokio::time::timeout(std::time::Duration::from_secs(5), minted)
        .await
        .unwrap()
        .unwrap();
    let scope_live = || {
        let machines = forest.environment.runner.machines_for_test();
        let mut checkout = machines.checkout_run(placement.session).unwrap();
        let live = checkout
            .machine()
            .compile_view_in(placement.lexical_scope)
            .is_some();
        let holes = checkout
            .machine()
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect();
        checkout.restore_suspended(holes);
        live
    };
    assert!(scope_live());
    publisher.kill_producer_for_test().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), preparation)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let settlement = forest
        .directory
        .settle_preparations(std::time::Duration::from_secs(5))
        .await;
    assert!(
        matches!(settlement, crate::CleanupComponentOutcome::Unconfirmed(_)),
        "a killed producer has no completion receipt: {settlement:?}"
    );
    assert!(
        !scope_live(),
        "the actual compiler-issued scope must be retired"
    );
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
    let _ = forest.shutdown().await;
    assert_eq!(
        cleanup_calls.load(Ordering::SeqCst),
        1,
        "retained settlement must prevent a second retirement"
    );
}

#[tokio::test]
async fn native_mint_after_producer_death_still_reaches_actual_cleanup() {
    let session_root = tempfile::tempdir().unwrap();
    let session_id = tidepool_runtime::session::fresh_session_id();
    let lib = SessionLib::open(
        session_id,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let (forest, _deployments) = ResidentForest::new(
        ActorWorkbenchSource::new("", Vec::new()),
        session_id,
        session,
        None,
        crate::Incarnation::FIRST,
    )
    .await
    .unwrap();
    let runner = forest.environment.runner.clone();
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = cleanup_calls.clone();
    let (published, publisher_observation) = tokio::sync::oneshot::channel();
    let (entered, before_mint) = tokio::sync::oneshot::channel();
    let (release, mint_release) = tokio::sync::oneshot::channel();
    let (cleaned, mut cleanups) = tokio::sync::mpsc::unbounded_channel();
    let directory = forest.directory.clone();
    let preparation = tokio::spawn(async move {
        directory
            .prepare_root(Box::new(move |ticket| {
                Box::pin(async move {
                    let cleanup_runner = runner.clone();
                    let publisher = ticket.placement_publisher(Arc::new(move |placement| {
                        let cleanup_runner = cleanup_runner.clone();
                        let observed_calls = observed_calls.clone();
                        let cleaned = cleaned.clone();
                        Box::pin(async move {
                            observed_calls.fetch_add(1, Ordering::SeqCst);
                            let result = cleanup_runner
                                .retire_root_placement_wait(
                                    placement,
                                    Some(std::time::Duration::from_secs(5)),
                                )
                                .await;
                            let _ = cleaned.send((placement, result.is_ok()));
                            match result {
                                Ok(()) => crate::CleanupComponentOutcome::Confirmed,
                                Err(error) => {
                                    crate::CleanupComponentOutcome::Unconfirmed(error.to_string())
                                }
                            }
                        })
                    }));
                    let _ = published.send(publisher.observer_for_test());
                    let _ = runner
                        .provision_root_scope_paused_for_test(
                            session_id,
                            publisher.into_mint(),
                            entered,
                            mint_release,
                        )
                        .await;
                    std::future::pending::<(
                        crate::local_actor::run_control::RootOperationResult,
                        crate::CleanupComponentOutcome,
                    )>()
                    .await
                })
            }))
            .await
    });
    let publisher = publisher_observation.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), before_mint)
        .await
        .unwrap()
        .unwrap();
    publisher.kill_producer_for_test().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), preparation)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let lost = forest
        .directory
        .settle_preparations(std::time::Duration::from_secs(5))
        .await;
    assert!(matches!(
        lost,
        crate::CleanupComponentOutcome::Unconfirmed(_)
    ));
    release.send(()).unwrap();
    let (placement, confirmed) =
        tokio::time::timeout(std::time::Duration::from_secs(5), cleanups.recv())
            .await
            .unwrap()
            .unwrap();
    assert!(
        confirmed,
        "the retained callback must perform actual native cleanup"
    );
    let machines = forest.environment.runner.machines_for_test();
    let mut checkout = machines.checkout_run(placement.session).unwrap();
    assert!(checkout
        .machine()
        .compile_view_in(placement.lexical_scope)
        .is_none());
    let holes = checkout
        .machine()
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    checkout.restore_suspended(holes);
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
    let _ = forest.shutdown().await;
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refused_control_notification_keeps_supervisor_owned_native_cleanup() {
    use crate::local_actor::run_control::{RootPlacementPublicationError, RunControlClient};
    let session_root = tempfile::tempdir().unwrap();
    let session_id = tidepool_runtime::session::fresh_session_id();
    let lib = SessionLib::open(
        session_id,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let session = ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let (forest, _deployments) = ResidentForest::new(
        ActorWorkbenchSource::new("", Vec::new()),
        session_id,
        session,
        None,
        crate::Incarnation::FIRST,
    )
    .await
    .unwrap();
    let (refused_control, refused_task) = RunControlClient::start().await.unwrap();
    refused_control.stop_owner_for_test().await;
    refused_task.await.unwrap();
    let runner = forest.environment.runner.clone();
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = cleanup_calls.clone();
    let (published, placement) = tokio::sync::oneshot::channel();
    let result = forest
        .directory
        .prepare_root(Box::new(move |ticket| {
            Box::pin(async move {
                let cleanup_runner = runner.clone();
                let publisher = ticket
                    .placement_publisher(Arc::new(move |placement| {
                        let runner = cleanup_runner.clone();
                        let observed_calls = observed_calls.clone();
                        Box::pin(async move {
                            observed_calls.fetch_add(1, Ordering::SeqCst);
                            match runner
                                .retire_root_placement_wait(
                                    placement,
                                    Some(std::time::Duration::from_secs(5)),
                                )
                                .await
                            {
                                Ok(()) => crate::CleanupComponentOutcome::Confirmed,
                                Err(error) => {
                                    crate::CleanupComponentOutcome::Unconfirmed(error.to_string())
                                }
                            }
                        })
                    }))
                    .with_control_for_test(&refused_control);
                let mint = publisher.into_mint();
                let placement = runner.provision_root_scope(session_id).await.unwrap();
                let error = match mint.publish(placement) {
                    Err(error @ RootPlacementPublicationError::ControlUnavailable(_)) => error,
                    Err(error) => {
                        panic!("supervisor handoff failed before control refusal: {error:?}")
                    }
                    Ok(_) => panic!("a stopped control endpoint accepted publication"),
                };
                let _ = published.send(placement);
                (
                    Err(Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
                    crate::CleanupComponentOutcome::Confirmed,
                )
            })
        }))
        .await
        .unwrap();
    let Err(error) = result else {
        panic!("control refusal returned an admitted resource")
    };
    assert!(error.downcast::<RootPlacementPublicationError>().is_ok());
    let placement = placement.await.unwrap();
    assert!(matches!(
        forest
            .directory
            .settle_preparations(std::time::Duration::from_secs(5))
            .await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    let machines = forest.environment.runner.machines_for_test();
    let mut checkout = machines.checkout_run(placement.session).unwrap();
    assert!(
        checkout
            .machine()
            .compile_view_in(placement.lexical_scope)
            .is_none(),
        "the supervisor must retire the actual native scope after the second send refuses"
    );
    let holes = checkout
        .machine()
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    checkout.restore_suspended(holes);
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
    let _ = forest.shutdown().await;
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
}
