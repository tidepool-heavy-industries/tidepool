//! One history through provider issuance, ordinary workspace edits and coherent reload.
use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    commit_workspace, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

struct UnexpectedJudgment(AtomicUsize);
impl exomonad_actor::JevBackend for UnexpectedJudgment {
    fn ask(
        &self,
        _packet: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(exomonad_actor::JevCallFailure::JevTransport(
                "this history requires no semantic judgment".into(),
            ))
        })
    }
}

/// Follow the exact published page receipt to its retained Store row. The
/// rendered response may omit page text when it provides an output reference.
fn assert_true_page(host: &HostedTestRuntime, result: &Value) {
    assert_eq!(result["status"], "committed", "{result}");
    let operations: Vec<_> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["operations"].as_array().into_iter().flatten())
        .filter(|operation| operation.get("display").is_some())
        .collect();
    assert_eq!(operations.len(), 1, "{result}");
    let operation = operations[0];
    let display = &operation["display"];
    let publication = &operation["displayPublication"];
    assert_eq!(publication["status"], "published", "{result}");
    assert_eq!(publication["publication"]["display"], display["identity"]);
    assert_eq!(publication["output"], display["output"]);
    let actor = host.context.actor.identity();
    let origin = harness::store::actor_output::ActorOutputOrigin {
        run: display["output"]["run"].as_str().unwrap().into(),
        native_actor: display["identity"][0].as_u64().unwrap(),
        incarnation: display["identity"][1].as_u64().unwrap(),
    };
    assert_eq!(origin.native_actor, actor.id.0);
    assert_eq!(origin.incarnation, actor.incarnation.0);
    let sequence = display["output"]["sequence"].as_i64().unwrap();
    let page = host
        .runtime
        .store()
        .actor_output_page(&origin, sequence - 1, 1)
        .unwrap();
    assert_eq!(page.outputs.len(), 1);
    let row = &page.outputs[0];
    assert_eq!(row.reference().sequence, sequence);
    assert_eq!(
        row.emission().id.display_slot,
        display["identity"][2].as_u64().unwrap()
    );
    assert_eq!(
        row.emission().id.page_ordinal,
        publication["publication"]["pageOrdinal"].as_u64().unwrap()
    );
    assert_eq!(
        row.emission().execution,
        harness::store::actor_output::ActorOutputExecution::Notebook {
            execution: operation["id"]["execution"].as_str().unwrap().into(),
            input_unit_index: operation["id"]["inputUnitIndex"].as_u64().unwrap(),
            effect_ordinal: operation["id"]["effectOrdinal"].as_u64().unwrap(),
        }
    );
    assert_eq!(row.emission().page.text, "True");
}

#[tokio::test(flavor = "multi_thread")]
async fn coherent_reload_replaces_the_notebook_and_prompts_without_rewriting_issued_work() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let judgments = Arc::new(UnexpectedJudgment(AtomicUsize::new(0)));
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        super::startup_persona_acceptance_tests::configure_persona(config, "AgentSpec.agentSpec");
        config.jev = Some(judgments.clone());
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("Project/Work.hs"),
            include_str!("fixtures/coherent_reload_work_a.hs"),
        )
        .unwrap();
        crate::exomonad::edit_fixture_project_config(&authored, |project| {
            project.haskell.modules.push("Project.Work".into());
        });
        let inputs = config.workspace.join("reload-inputs");
        std::fs::create_dir_all(&inputs).unwrap();
        for (name, body) in [
            (
                "work-a.txt",
                include_str!("fixtures/coherent_reload_work_a.hs"),
            ),
            (
                "work-b.txt",
                include_str!("fixtures/coherent_reload_work_b.hs"),
            ),
            (
                "work-invalid.txt",
                include_str!("fixtures/coherent_reload_work_invalid.hs"),
            ),
            (
                "prompt-b.txt",
                include_str!("fixtures/coherent_reload_prompt_b.md"),
            ),
        ] {
            std::fs::write(inputs.join(name), body).unwrap();
        }
        commit_workspace(&config.workspace);
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(
                &config.workspace,
                &config.run_directory.path(),
            )
            .unwrap(),
        );
    })
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            let root = harness::model::AgentPath("/root".into());
            let mut pending = VecDeque::new();
            let retained = host
                .context
                .forest
                .initial_context_observation(host.context.actor.identity())
                .await
                .unwrap();
            assert_eq!(
                retained.state,
                exomonad_actor::InitialContextState::Published
            );
            let context = retained.result.as_ref().unwrap().rendered.clone();
            let installation = host
                .context
                .observer
                .installation(host.context.actor.identity())
                .await;
            let old_request = installation
                .policy
                .snapshot_for_request()
                .expect("issued generation A");
            host.input("Keep our workshop conversation through this edit.")
                .await
                .unwrap();
            let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let session = round.request.session_id.clone();
            let instructions_a = round.request.instructions.clone();
            let operation = round.operation("bind-a");
            round.sync_call_named(
                "bind-a",
                "haskell",
                include_str!("fixtures/coherent_reload_bind_a.hs"),
            );
            round = host
                .while_operation_succeeds(
                    &operation,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            assert_true_page(host, &round.settled_output("bind-a"));
            let operation = round.operation("publish-b");
            round.sync_call_named(
                "publish-b",
                "haskell",
                include_str!("fixtures/coherent_reload_publish_b.hs"),
            );
            round = host
                .while_operation_succeeds(
                    &operation,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            assert_true_page(host, &round.settled_output("publish-b"));
            let instructions_b = serde_json::to_value(&round.request).unwrap()["instructions"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_ne!(instructions_b, instructions_a);
            assert!(instructions_b.contains(include_str!("fixtures/coherent_reload_prompt_b.md")));
            let late = super::tests::dispatch_haskell_script(
                old_request.as_ref(),
                include_str!("fixtures/coherent_reload_late_a.hs"),
            )
            .await;
            assert_eq!(
                late["status"], "committed",
                "issued A must remain usable: {late}"
            );
            assert_eq!(
                late["items"].as_array().unwrap().last().unwrap()["output"],
                "True",
                "{late}"
            );
            let current = host
                .context
                .observer
                .installation(host.context.actor.identity())
                .await;
            let missing =
                super::tests::dispatch_haskell_script(current.policy.as_ref(), "display aOnly")
                    .await;
            assert_eq!(
                missing["status"], "rejected",
                "fresh B must not inherit A names: {missing}"
            );
            for (id, source) in [
                ("use-b", include_str!("fixtures/coherent_reload_use_b.hs")),
                ("noop", include_str!("fixtures/coherent_reload_noop.hs")),
                ("reject", include_str!("fixtures/coherent_reload_reject.hs")),
                (
                    "after-rejection",
                    include_str!("fixtures/coherent_reload_after_rejection.hs"),
                ),
            ] {
                assert_eq!(round.request.session_id, session);
                assert_eq!(round.request.instructions, instructions_b);
                super::startup_context_acceptance_tests::assert_request_user_texts(
                    host,
                    host.context.actor.identity(),
                    &round.request,
                    &[
                        context.as_str(),
                        "Keep our workshop conversation through this edit.",
                    ],
                )
                .await;
                let operation = round.operation(id);
                round.sync_call_named(id, "haskell", source);
                round = host
                    .while_operation_succeeds(
                        &operation,
                        next_hosted_script_round(&mut requests, &mut pending, &root),
                    )
                    .await
                    .unwrap();
                assert_true_page(host, &round.settled_output(id));
            }
            assert_eq!(round.request.instructions, instructions_b);
            assert_eq!(
                host.context
                    .forest
                    .initial_context_observation(host.context.actor.identity())
                    .await
                    .unwrap(),
                retained
            );
            assert_eq!(
                judgments.0.load(Ordering::SeqCst),
                0,
                "startup and reload must not invoke Jev"
            );
            drop(old_request);
            round.finish();
        })
    })
    .await;
}
