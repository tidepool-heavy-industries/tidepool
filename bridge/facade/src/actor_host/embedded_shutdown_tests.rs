use super::*;

#[tokio::test]
async fn embedded_shutdown_reports_driver_failure_and_accepts_confirmed_cancellation() {
    use embedded_service::EmbeddedDriverError;
    let actor = ActorRef::first(exomonad_actor::ActorId(42));
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { (actor, (), Ok(())) });
    assert_eq!(
        drain_embedded_shutdown(&mut tasks, Duration::from_secs(1)).await,
        None
    );

    tasks.spawn(async move {
        (
            actor,
            (),
            Err(EmbeddedDriverError::Engine(
                harness::engine::EngineError::InvalidFunctionCall,
            )),
        )
    });
    let failure = drain_embedded_shutdown(&mut tasks, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(
        failure.contains("malformed Responses tool call item"),
        "{failure}"
    );
    assert!(failure.contains("42"), "{failure}");

    tasks.spawn(async move {
        (
            actor,
            (),
            Err(EmbeddedDriverError::Engine(
                harness::engine::EngineError::Cleanup {
                    primary: Box::new(harness::engine::EngineError::Cancelled {
                        head_request: None,
                    }),
                    cleanup: "claim settlement failed".into(),
                },
            )),
        )
    });
    let failure = drain_embedded_shutdown(&mut tasks, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(failure.contains("claim settlement failed"), "{failure}");
}

#[tokio::test(start_paused = true)]
async fn embedded_shutdown_timeout_does_not_wait_for_unconfirmed_abort() {
    let mut tasks: JoinSet<((), (), Result<(), embedded_service::EmbeddedDriverError>)> =
        JoinSet::new();
    tasks.spawn(async {
        (
            (),
            (),
            Err(embedded_service::EmbeddedDriverError::Host(
                "prior driver failure".into(),
            )),
        )
    });
    tasks.spawn(std::future::pending());
    let failure = drain_embedded_shutdown(&mut tasks, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(failure.contains("cleanup unconfirmed"), "{failure}");
    assert!(failure.contains("prior driver failure"), "{failure}");
    while let Some(result) = tasks.join_next().await {
        assert!(result.unwrap_err().is_cancelled());
    }
}

#[test]
fn embedded_resource_release_preserves_cleanup_uncertainty() {
    use embedded_service::EmbeddedDriverError;
    use harness::engine::EngineError;
    assert_eq!(
        embedded_resource_release(None),
        exomonad_actor::ResourceRelease::Released
    );
    let cleanup = EmbeddedDriverError::Engine(EngineError::Cleanup {
        primary: Box::new(EngineError::Cancelled { head_request: None }),
        cleanup: "claim settlement failed".into(),
    });
    assert!(
        matches!(embedded_resource_release(Some(&cleanup)), exomonad_actor::ResourceRelease::Retained(detail)
        if detail.contains("claim settlement failed"))
    );
}
