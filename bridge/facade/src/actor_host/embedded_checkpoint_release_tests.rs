//! Releasing a checkpoint prevents new children without revoking admitted custody.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use super::*;
use harness::model::AgentPath;

#[tokio::test]
async fn released_checkpoint_keeps_an_admitted_childs_hosted_context() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        config.research_policy = exomonad_actor::ResearchPolicy {
            maximum_depth: 1,
            maximum_active_children: Some(2),
            default_depth: 1,
        };
    })
    .await
    .expect("production checkpoint host starts");
    let mut pending = std::collections::VecDeque::new();
    let root = AgentPath("/root".into());
    next_hosted_script_round(&mut requests, &mut pending, &root)
        .await
        .call(
            "checkpoint-issuer-setup",
            include_str!("checkpoint_issuer_setup.hs"),
        );
    let next_root = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    next_root.assert_committed("checkpoint-issuer-setup");
    let issuer = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(installation) = host
                .context
                .observer
                .installations()
                .into_iter()
                .find(|installation| installation.actor.identity() != host.context.actor.identity())
            {
                return installation;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("actual issuer attaches through production readiness");
    let issuer_id = issuer.actor.identity();
    let issuer_path = AgentPath(format!(
        "/root/a{}_i{}",
        issuer_id.id.0, issuer_id.incarnation.0
    ));
    next_hosted_script_round(&mut requests, &mut pending, &issuer_path)
        .await
        .call(
            "checkpoint-issuer-capture",
            include_str!("checkpoint_issuer_capture.hs"),
        );
    let issuer_done = next_hosted_script_round(&mut requests, &mut pending, &issuer_path).await;
    issuer_done.assert_committed("checkpoint-issuer-capture");
    issuer_done.finish();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let succeeded = host
                .context
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.actor == issuer_id)
                .and_then(|node| node.provider_turn)
                .is_some_and(|turn| turn.state == exomonad_model::ProviderTurnState::Succeeded);
            if succeeded {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("issuer succeeds through its real provider turn before retirement");
    next_root.call(
        "checkpoint-issuer-cleanup",
        "planCleanupFor producer >>= executeCleanup >>= display . cleanupReceiptComplete",
    );
    let root_after_cleanup = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    root_after_cleanup.assert_value("checkpoint-issuer-cleanup", "True");
    let issuer_cleanup = issuer
        .actor
        .terminal()
        .cleanup()
        .expect("actual issuer cleanup evidence");
    assert_eq!(issuer_cleanup.actor(), issuer_id);
    assert!(issuer_cleanup.is_confirmed(), "{issuer_cleanup:?}");
    root_after_cleanup.call(
        "checkpoint-observer-admission",
        include_str!("checkpoint_deferred_branch.hs"),
    );
    let root_after_admission = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    root_after_admission.assert_committed("checkpoint-observer-admission");
    let observer = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(installation) =
                host.context
                    .observer
                    .installations()
                    .into_iter()
                    .find(|installation| {
                        installation.checkpoint && installation.context_parent == Some(issuer_id)
                    })
            {
                return installation;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("retired issuer's genuine checkpoint admits an observer");
    let observer_id = observer.actor.identity();
    let observer_path = AgentPath(format!(
        "/root/a{}_i{}",
        observer_id.id.0, observer_id.incarnation.0
    ));
    // Hold the observer's first real provider request while the root releases
    // the token and probes refusal. The observer already owns its exact custody.
    let observer_round =
        next_hosted_script_round(&mut requests, &mut pending, &observer_path).await;
    root_after_admission.call(
        "checkpoint-release-twice",
        include_str!("checkpoint_release_twice.hs"),
    );
    let root_after_release = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    root_after_release.assert_value("checkpoint-release-twice", "True");
    root_after_release.call(
        "checkpoint-released-refusal",
        include_str!("checkpoint_released_refusal.hs"),
    );
    let root_after_refusal = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    root_after_refusal.assert_value("checkpoint-released-refusal", "True");
    assert_eq!(
        host.context
            .observer
            .installations()
            .iter()
            .filter(|installation| installation.checkpoint)
            .count(),
        1
    );
    observer_round.call(
        "checkpoint-inherited-read",
        "display (x == 41 && getX == 42)",
    );
    let observer_after_read =
        next_hosted_script_round(&mut requests, &mut pending, &observer_path).await;
    observer_after_read.assert_value("checkpoint-inherited-read", "True");
    observer_after_read.finish();
    root_after_refusal.call(
        "checkpoint-observer-cleanup",
        "planCleanupFor observer >>= executeCleanup >>= display . cleanupReceiptComplete",
    );
    let root_done = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    root_done.assert_value("checkpoint-observer-cleanup", "True");
    let observer_cleanup = observer
        .actor
        .terminal()
        .cleanup()
        .expect("actual observer cleanup evidence");
    assert_eq!(observer_cleanup.actor(), observer_id);
    assert!(observer_cleanup.is_confirmed(), "{observer_cleanup:?}");
    root_done.finish();
    host.stop()
        .await
        .expect("production checkpoint host acknowledges final cleanup");
}
