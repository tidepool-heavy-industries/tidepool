//! Checkpoint capture receipts and retained children through production hosting.

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    actor_output_origin, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
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
    host.run_scenario(|host| {
        Box::pin(async move {
            let origin =
                actor_output_origin(host.runtime.run_identity(), host.context.actor.identity());
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
                            "exomonad/host/src/actor_host/embedded_checkpoint_scope_setup.hs"
                        )
                    ),
                );
            let settled = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            settled.assert_authored_value(
                &host.runtime.store(),
                &origin,
                "checkpoint-scope-setup",
                "True",
            );
            settled.finish();
        })
    })
    .await;
}

#[tokio::test]
async fn published_embedded_checkpoint_survives_capture_cell_failure() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 3);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start(&settings, &provider)
        .await
        .expect("production checkpoint host starts");
    host.run_scenario(|host| {
        Box::pin(async move {
            let origin =
                actor_output_origin(host.runtime.run_identity(), host.context.actor.identity());
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
                            "exomonad/host/src/actor_host/embedded_checkpoint_scope_setup.hs"
                        )
                    ),
                );
            let after_setup = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            after_setup.assert_authored_value(
                &host.runtime.store(),
                &origin,
                "checkpoint-scope-setup",
                "True",
            );
            let expected_operation = after_setup.operation("checkpoint-parent-capture-failure");
            after_setup.call(
                "checkpoint-parent-capture-failure",
                &tidepool_testing::fixture_source(
                    "exomonad/host/src/actor_host/embedded_checkpoint_capture_and_children.hs",
                ),
            );
            let after_failure = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            after_failure.assert_failure(
                "checkpoint-parent-capture-failure",
                "expected checkpoint capture execution failure",
            );
            let failed = after_failure.settled_output("checkpoint-parent-capture-failure");
            let failure = &failed["failure"];
            assert_eq!(failure["class"], "runtime", "{failed}");
            assert_eq!(failure["phase"], "run", "{failed}");
            assert_eq!(
                failure["originalOperation"],
                serde_json::to_value(expected_operation).unwrap(),
                "{failed}",
            );
            use tidepool_runtime::session::{
                WorkbenchExecutionId, WorkbenchFailureLayer, WorkbenchItemStatus,
                WorkbenchNotPublishedReason, WorkbenchOperationDisposition, WorkbenchOperationId,
                WorkbenchOperationReceipt, WorkbenchPublicationOutcome,
            };
            assert_eq!(
                failure["publication"],
                serde_json::to_value(WorkbenchPublicationOutcome::NotPublished {
                    reason: WorkbenchNotPublishedReason::Failed,
                })
                .unwrap(),
                "{failed}",
            );
            let [item] = failure["items"].as_array().unwrap().as_slice() else {
                panic!("one failed input unit must retain its receipts: {failed}");
            };
            assert_eq!(item["index"], 0, "{failed}");
            assert_eq!(
                item["status"],
                serde_json::to_value(WorkbenchItemStatus::Rejected).unwrap(),
                "{failed}",
            );
            assert_eq!(
                item["failureLayer"],
                serde_json::to_value(WorkbenchFailureLayer::Effect).unwrap(),
                "{failed}",
            );
            let operations = item["operations"].as_array().unwrap();
            let [capture, _] = operations.as_slice() else {
                panic!("exact capture and cast receipts required: {failed}");
            };
            let execution: WorkbenchExecutionId =
                serde_json::from_value(capture["id"]["execution"].clone()).unwrap();
            let expected_receipts = ["capture context checkpoint", "cast"]
                .into_iter()
                .enumerate()
                .map(|(effect_ordinal, effect)| WorkbenchOperationReceipt {
                    display: None,
                    display_publication: None,
                    id: WorkbenchOperationId {
                        execution: execution.clone(),
                        input_unit_index: 0,
                        effect_ordinal,
                    },
                    effect: effect.to_owned(),
                    disposition: WorkbenchOperationDisposition::Committed,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                item["operations"],
                serde_json::to_value(expected_receipts).unwrap(),
                "{failed}",
            );
            after_failure.call(
                "checkpoint-parent-admit-stored-children",
                &format!(
                    "{}\ndisplay True",
                    tidepool_testing::fixture_source(
                        "exomonad/host/src/actor_host/embedded_checkpoint_admit_stored_children.hs"
                    )
                ),
            );
            let root_after_admission =
                next_hosted_script_round(&mut requests, &mut pending, &root).await;
            root_after_admission.assert_authored_value(
                &host.runtime.store(),
                &origin,
                "checkpoint-parent-admit-stored-children",
                "True",
            );
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
                settled.assert_authored_value(
                    &host.runtime.store(),
                    &actor_output_origin(host.runtime.run_identity(), *id),
                    &format!("checkpoint-child-{}-first", id.id.0),
                    "(41,42)",
                );
                settled.finish();
            }
            for (id, _) in &paths {
                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        if host
                            .context
                            .forest
                            .inspect_host_graph()
                            .await
                            .unwrap()
                            .into_iter()
                            .find(|node| node.actor == *id)
                            .and_then(|node| node.provider_turn)
                            .is_some_and(|turn| {
                                turn.state == exomonad_model::ProviderTurnState::Succeeded
                            })
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
                settled.assert_authored_value(
                    &host.runtime.store(),
                    &actor_output_origin(host.runtime.run_identity(), *id),
                    &format!("checkpoint-child-{}-later", id.id.0),
                    "(41,42)",
                );
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
        })
    })
    .await;
}
