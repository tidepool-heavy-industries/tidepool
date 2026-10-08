//! Checkpoint capture receipts and retained children through production hosting.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use super::*;
use harness::model::AgentPath;

#[tokio::test(flavor = "current_thread")]
async fn embedded_checkpoint_scope_setup_starts_its_haskell_actor() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 2);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start(&settings, &provider)
        .await
        .expect("production checkpoint host starts");
    host.input("Exercise checkpoint scope and retained children.")
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
                    "bridge/facade/src/actor_host/embedded_checkpoint_scope_setup.hs"
                )
            ),
        );
    let settled = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    settled.assert_value("checkpoint-scope-setup", "True");
    settled.finish();
    host.stop()
        .await
        .expect("production checkpoint host acknowledges cleanup");
}

#[tokio::test]
async fn published_embedded_checkpoint_survives_capture_cell_failure() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start(&settings, &provider)
        .await
        .expect("production checkpoint host starts");
    host.input("Exercise checkpoint scope and retained children.")
        .await
        .unwrap();
    let mut pending = std::collections::VecDeque::new();
    let root = AgentPath("/root".into());
    let actor = host.context.actor.identity();
    next_hosted_script_round(&mut requests, &mut pending, &root)
        .await
        .call(
            "checkpoint-scope-setup",
            &format!(
                "{}\ndisplay True",
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/embedded_checkpoint_scope_setup.hs"
                )
            ),
        );
    let after_setup = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    after_setup.assert_value("checkpoint-scope-setup", "True");
    after_setup.call(
        "checkpoint-parent-capture-failure",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/embedded_checkpoint_capture_and_children.hs",
        ),
    );
    let after_failure = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    for retained in [
        "expected checkpoint capture execution failure",
        "Committed (capture context checkpoint)",
        "Committed (cast)",
    ] {
        after_failure.assert_failure("checkpoint-parent-capture-failure", retained);
    }
    after_failure.call(
        "checkpoint-parent-admit-stored-children",
        &format!(
            "{}\ndisplay True",
            tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/embedded_checkpoint_admit_stored_children.hs"
            )
        ),
    );
    let root_after_admission = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    root_after_admission.assert_value("checkpoint-parent-admit-stored-children", "True");
    let children = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let children = host
                .context
                .observer
                .installations()
                .into_iter()
                .filter(|installation| {
                    installation.checkpoint && installation.context_parent == Some(actor)
                })
                .collect::<Vec<_>>();
            if children.len() == 2 {
                return children;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("production host attaches both stored-checkpoint children");
    let mut paths = Vec::new();
    for child in &children {
        let id = child.actor.identity();
        let path = AgentPath(format!("/root/a{}_i{}", id.id.0, id.incarnation.0));
        next_hosted_script_round(&mut requests, &mut pending, &path)
            .await
            .call(
                &format!("checkpoint-child-{}-first", id.id.0),
                "display (show (x, getX))",
            );
        paths.push((id, path));
    }
    for (id, path) in &paths {
        let settled = next_hosted_script_round(&mut requests, &mut pending, path).await;
        settled.assert_value(&format!("checkpoint-child-{}-first", id.id.0), "(41,42)");
        settled.finish();
    }
    for (id, _) in &paths {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if host
                    .context
                    .forest
                    .inspect_host_graph()
                    .into_iter()
                    .find(|node| node.actor == *id)
                    .and_then(|node| node.provider_turn)
                    .is_some_and(|turn| turn.state == exomonad_model::ProviderTurnState::Succeeded)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("child completes its first real provider turn");
        let binding = host
            .context
            .binding(*id)
            .expect("production attachment owns child binding");
        binding
            .conversation()
            .unwrap()
            .input(
                &format!("checkpoint-child-{}-later-input", id.id.0),
                "operator",
                "Read the captured Haskell context again.",
            )
            .await
            .unwrap();
    }
    for (id, path) in &paths {
        next_hosted_script_round(&mut requests, &mut pending, path)
            .await
            .call(
                &format!("checkpoint-child-{}-later", id.id.0),
                "display (show (x, getX))",
            );
    }
    for (id, path) in &paths {
        let settled = next_hosted_script_round(&mut requests, &mut pending, path).await;
        settled.assert_value(&format!("checkpoint-child-{}-later", id.id.0), "(41,42)");
        settled.finish();
    }
    assert_eq!(
        host.context
            .observer
            .installations()
            .iter()
            .filter(|installation| installation.checkpoint
                && installation.context_parent == Some(actor))
            .count(),
        2,
        "released token admits no third child"
    );
    for child in &children {
        assert!(
            child.actor.terminal().get().is_none(),
            "detached captured owner remains live after both reads"
        );
    }
    root_after_admission.finish();
    host.stop()
        .await
        .expect("production checkpoint host acknowledges all child cleanup");
}
