//! Releasing a checkpoint prevents new children without revoking admitted custody.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use super::*;
use harness::model::AgentPath;

async fn attached_actor_path(host: &HostedTestRuntime, actor: ActorRef) -> AgentPath {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let Some(binding) = host.context.binding(actor) {
                if let Some(conversation) = binding.conversation() {
                    assert!(binding.is_live());
                    assert_eq!(conversation.identity(), binding.identity());
                    assert_eq!(
                        conversation.identity().run,
                        runtime_namespace(&host.context.config.run_directory.path())
                    );
                    assert_eq!(
                        conversation.identity().incarnation,
                        actor.incarnation.0.to_string()
                    );
                    return conversation.identity().actor.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("exact admitted actor attaches its real hosted conversation")
}

async fn wait_for_succeeded_provider_turn(host: &HostedTestRuntime, actor: ActorRef) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let succeeded = host
                .context
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.actor == actor)
                .and_then(|node| node.provider_turn)
                .is_some_and(|turn| turn.state == exomonad_model::ProviderTurnState::Succeeded);
            if succeeded {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("actor succeeds through its real provider turn before retirement");
}

#[tokio::test]
async fn released_checkpoint_keeps_an_admitted_childs_hosted_context() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |_| {})
        .await
        .expect("production checkpoint host starts");
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Exercise issuer retirement and checkpoint release.")
                .await
                .unwrap();
            let mut pending = std::collections::VecDeque::new();
            let root = AgentPath("/root".into());
            next_hosted_script_round(&mut requests, &mut pending, &root)
                .await
                .call(
                    "checkpoint-scope-setup",
                    &format!(
                        "{}\ndisplay True",
                        tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/embedded_checkpoint_scope_setup.hs",
                        ),
                    ),
                );
            let after_scope_setup =
                next_hosted_script_round(&mut requests, &mut pending, &root).await;
            after_scope_setup.assert_value("checkpoint-scope-setup", "True");
            after_scope_setup.call(
                "checkpoint-issuer-setup",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/checkpoint_issuer_setup.hs",
                ),
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
                        .find(|installation| {
                            installation.actor.identity() != host.context.actor.identity()
                        })
                    {
                        return installation;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("actual issuer attaches through production readiness");
            let issuer_id = issuer.actor.identity();
            let issuer_path = attached_actor_path(&host, issuer_id).await;
            next_hosted_script_round(&mut requests, &mut pending, &issuer_path)
                .await
                .call(
                    "checkpoint-issuer-capture",
                    &tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/checkpoint_issuer_capture.hs",
                    ),
                );
            let issuer_done =
                next_hosted_script_round(&mut requests, &mut pending, &issuer_path).await;
            issuer_done.assert_committed("checkpoint-issuer-capture");
            issuer_done.finish();
            wait_for_succeeded_provider_turn(&host, issuer_id).await;
            // Provider completion leaves the typed assignment pending. Cleanup must
            // retain the issuer until that assignment has actually replied.
            next_root.call(
                "checkpoint-pending-cleanup-refusal",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/checkpoint_pending_cleanup_refusal.hs",
                ),
            );
            let root_after_pending =
                next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after_pending.assert_value("checkpoint-pending-cleanup-refusal", "True");
            assert!(issuer.actor.terminal().get().is_none());
            assert!(issuer.actor.terminal().cleanup().is_none());
            host.context
                .binding(issuer_id)
                .expect("production attachment retains the pending issuer")
                .conversation()
                .unwrap()
                .input(
                    "checkpoint-issuer-reply-input",
                    "operator",
                    "Reply to the original typed capture assignment.",
                )
                .await
                .unwrap();
            next_hosted_script_round(&mut requests, &mut pending, &issuer_path)
                .await
                .call("checkpoint-issuer-reply", "respond (\"captured\" :: Text)");
            let issuer_replied =
                next_hosted_script_round(&mut requests, &mut pending, &issuer_path).await;
            let reply = issuer_replied.settled_output("checkpoint-issuer-reply");
            assert_eq!(reply["status"], "replied", "{reply}");
            assert_eq!(reply["publication"]["status"], "published", "{reply}");
            assert_eq!(
                reply["items"][0]["terminalTransfer"], "replyAccepted",
                "{reply}"
            );
            issuer_replied.finish();
            wait_for_succeeded_provider_turn(&host, issuer_id).await;
            root_after_pending.call(
                "checkpoint-issuer-cleanup",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/checkpoint_issuer_cleanup.hs",
                ),
            );
            let root_after_cleanup =
                next_hosted_script_round(&mut requests, &mut pending, &root).await;
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
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/checkpoint_deferred_branch.hs",
                ),
            );
            let root_after_admission =
                next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after_admission.assert_committed("checkpoint-observer-admission");
            let observer = tokio::time::timeout(Duration::from_secs(120), async {
                loop {
                    if let Some(installation) = host
                        .context
                        .observer
                        .installations()
                        .into_iter()
                        .find(|installation| {
                            installation.checkpoint
                                && installation.context_parent == Some(issuer_id)
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
            let observer_path = attached_actor_path(&host, observer_id).await;
            assert_eq!(
                observer_path,
                AgentPath(format!(
                    "{}/a{}_i{}",
                    issuer_path.0, observer_id.id.0, observer_id.incarnation.0
                )),
                "the checkpoint child retains its issuer's hosted conversation ancestry"
            );
            // Hold the observer's first real provider request while the root releases
            // the token and probes refusal. The observer already owns its exact custody.
            let observer_round =
                next_hosted_script_round(&mut requests, &mut pending, &observer_path).await;
            root_after_admission.call(
                "checkpoint-release-twice",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/checkpoint_release_twice.hs",
                ),
            );
            let root_after_release =
                next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after_release.assert_value("checkpoint-release-twice", "True");
            root_after_release.call(
                "checkpoint-released-refusal",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/checkpoint_released_refusal.hs",
                ),
            );
            let root_after_refusal =
                next_hosted_script_round(&mut requests, &mut pending, &root).await;
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
            observer_after_read.call(
                "checkpoint-observer-reply",
                "respond (\"inspected\" :: Text)",
            );
            let observer_replied =
                next_hosted_script_round(&mut requests, &mut pending, &observer_path).await;
            let reply = observer_replied.settled_output("checkpoint-observer-reply");
            assert_eq!(reply["status"], "replied", "{reply}");
            assert_eq!(reply["publication"]["status"], "published", "{reply}");
            assert_eq!(
                reply["items"][0]["terminalTransfer"], "replyAccepted",
                "{reply}"
            );
            observer_replied.finish();
            wait_for_succeeded_provider_turn(&host, observer_id).await;
            root_after_refusal.call(
                "checkpoint-observer-cleanup",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/checkpoint_observer_cleanup.hs",
                ),
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
        })
    })
    .await;
}
