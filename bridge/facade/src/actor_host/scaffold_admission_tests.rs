use super::command_test_support::{backend_request, TestCommands};
use super::test_campaign::{
    commit_workspace, committed_display_text, dispatch_haskell_script, dispatch_structured_tool,
    TestCampaign,
};
use exomonad_tool::{ActorEffectKey, HostedTool, ToolImplementation, ToolScheduling};
use futures_util::FutureExt;

use super::hosted_test_context::HostedTestRuntime;
use super::test_campaign::{
    hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use std::collections::VecDeque;

pub(super) fn scaffold(config: &mut super::ActorHostConfig) {
    crate::exomonad::new(crate::exomonad::NewOptions {
        path: Some(config.workspace.clone()),
        lock: Box::new(crate::exomonad::NixLock),
    })
    .expect("the shipped project must scaffold successfully");
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .expect("the freshly scaffolded project's inputs must freeze"),
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the matched native prepared root entry and workspace bundle"]
async fn prepared_scaffolded_agent_spec_lookup_and_context_fork_execute_originals() {
    assert!(std::env::var_os("TIDEPOOL_PREPARED_ROOT_ENTRY").is_some());
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 2);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_prepared_configured(&settings, &provider, |config| {
        scaffold(config);
        let embedded = config
            .embedded
            .clone()
            .expect("the hosted test owns its transport settings");
        crate::exomonad::edit_fixture_project_config(
            &config.workspace.join(".exomonad"),
            |project| {
                project.launch.embedded = Some(embedded);
            },
        );
        commit_workspace(&config.workspace);
        config.jev = Some(std::sync::Arc::new(super::test_campaign::FixtureJev));
    })
    .await
    .expect("the shipped workspace must prepare and select its completed deployment");
    host.run_scenario(|host| {
        Box::pin(async move {
            host.assert_fresh_prepared_workspace_original().await;
            host.input("Inspect and fork the prepared default workspace.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = VecDeque::new();
            next_hosted_script_round(&mut requests, &mut pending, &root)
                .await
                .call(
                    "prepared-default-binding",
                    "let trialSeed = 41 :: Int\ndisplay trialSeed",
                );
            let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            round.assert_value("prepared-default-binding", "41");
            round.function(
                "prepared-default-lookup",
                "lookup",
                serde_json::json!({"queries": ["checkpoint", "spawnSubagent"]}),
            );
            let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let lookup = round.settled_output("prepared-default-lookup");
            assert_eq!(lookup["status"], "committed", "{lookup}");
            let output = lookup["items"][0]["output"].as_str().unwrap();
            for name in ["checkpoint", "spawnSubagent"] {
                let block = output
                    .split("\n\n")
                    .find(|block| block.starts_with(&format!("{name}\n")))
                    .unwrap_or_else(|| panic!("missing lookup result for {name}: {lookup}"));
                assert!(
                    block
                        .split_once("::")
                        .is_some_and(|(_, signature)| !signature.trim().is_empty()),
                    "{lookup}"
                );
            }
            round.call(
                "prepared-default-child",
                &tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/scaffold_prepared_child.hs",
                ),
            );
            let child_round =
                tokio::time::timeout(std::time::Duration::from_secs(300), requests.recv())
                    .await
                    .expect("the prepared default spec must admit its child")
                    .expect("the child's native provider request");
            if child_round.origin().actor() == &root {
                child_round.assert_value("prepared-default-child", "42");
                panic!("the typed reply must come from the forked child");
            }
            let child = child_round.origin().actor().clone();
            child_round.call("prepared-default-reply", "respond (trialSeed + 1 :: Int)");
            let child_round = next_hosted_script_round(&mut requests, &mut pending, &child).await;
            assert_eq!(
                child_round.settled_output("prepared-default-reply")["status"],
                "replied"
            );
            child_round.finish();
            let completed = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            completed.assert_value("prepared-default-child", "42");
            completed.finish();
        })
    })
    .await;
}

/// Provider requests execute the package and pin delivered by `exomonad new`.
#[tokio::test(flavor = "multi_thread")]
async fn freshly_scaffolded_agent_spec_installs_notebook_and_workspace_tools() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, scaffold)
        .await
        .unwrap();
    host.input("Exercise the scaffolded tool and retained shell contracts.")
        .await
        .unwrap();
    let mut pending = VecDeque::new();
    let root = harness::model::AgentPath("/root".into());
    let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    round.call("scaffold-reflect", "import qualified Tidepool.Effects.Core as Core\nreflected <- Core.reflect 1\ndisplay (case reflected of { Right _ -> True; _ -> False })");
    let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    round.assert_value("scaffold-reflect", "True");
    round.call(
        "scaffold-facts",
        &tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/command_tool_facts_gate.hs",
        ),
    );
    let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    round.assert_value("scaffold-facts", "(True,True,True,True,True)");
    let stdout = format!("{}stdout-tail\n", "λ".repeat(800));
    let stderr = format!("{}stderr-tail\n", "μ".repeat(800));
    // The marker independently checks that recovering the exact emitted pointer
    // reads the retained process output without executing the shell again.
    round.function("scaffold-shell", "bash", serde_json::json!({
        "cmd": format!("printf x >> scaffold-shell-executions; printf '%s' '{}'; printf '%s' '{}' >&2", stdout, stderr),
        "workdir": null, "environment": null, "memory_mib": null, "tty": null,
        "stdin": null, "yield_time_ms": 30000, "max_output_bytes": 2048,
        "intent": "recover the retained streams without rerunning",
    }));
    let mut round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    let response = round.settled_output("scaffold-shell");
    assert_eq!(response["status"], "committed", "{response}");
    let output = response["items"][0]["output"].as_str().unwrap();
    let snapshot = retained_snapshot(output, stdout.len(), stderr.len()).to_owned();
    for (section, tail) in [(1, "stdout-tail"), (2, "stderr-tail")] {
        let source = format!("{snapshot}\nrecovered <- Project.Shell.section snap (Project.Shell.SectionId {section})\ndisplay (show recovered)");
        let call_id = format!("scaffold-recover-{section}");
        round.call(&call_id, &source);
        round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
        let response = round.settled_output(&call_id);
        let recovered = committed_display_text(&response);
        assert!(
            recovered.starts_with("Right ") && recovered.contains(tail),
            "{response}"
        );
    }
    assert_eq!(
        std::fs::read(
            host.context
                .config
                .workspace
                .join("scaffold-shell-executions")
        )
        .unwrap(),
        b"x"
    );
    let tools = host
        .context
        .observer
        .installation(host.context.actor.identity())
        .await
        .tools;
    round.finish();
    host.stop().await.unwrap();

    for (name, schedule) in [
        ("haskell", ToolScheduling::Async),
        ("haskell_sync", ToolScheduling::BeforeNextInference),
    ] {
        let matching: Vec<_> = tools.iter().filter(|tool| tool.name() == name).collect();
        assert_eq!(matching.len(), 1, "expected one {name}: {tools:?}");
        let tool = matching[0];
        assert!(matches!(tool, HostedTool::Custom(_)), "{tool:?}");
        assert_eq!(tool.implementation(), ToolImplementation::HaskellCell);
        assert_eq!(tool.scheduling(), schedule);
        for effect in [
            ActorEffectKey::Commands,
            ActorEffectKey::Lookup,
            ActorEffectKey::Replies,
            ActorEffectKey::Forks,
            ActorEffectKey::Watches,
            ActorEffectKey::Console,
        ] {
            assert!(
                tool.effect_keys().contains(&effect.into()),
                "{name} lacks {effect:?}"
            );
        }
    }
    // These authored record members distinguish the shipped workspace from
    // the built-in notebook fallback; descriptions and schema layout may evolve.
    for name in ["bash", "lookup", "submit_review"] {
        let matching: Vec<_> = tools.iter().filter(|tool| tool.name() == name).collect();
        assert_eq!(matching.len(), 1, "expected one {name}: {tools:?}");
        assert_eq!(
            matching[0].implementation(),
            ToolImplementation::ResidentHandler
        );
    }
}

/// A command-owner fault fixture checks a truncated retained slice. It does not
/// claim a provider attachment or production host shutdown.
#[tokio::test(flavor = "multi_thread")]
async fn pinned_shell_component_refuses_expired_retained_output_without_rerun() {
    let campaign = TestCampaign::start_with_config(|admission| admission, scaffold).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let checked = std::panic::AssertUnwindSafe(assert_pinned_shell_recovery(campaign))
                    .catch_unwind()
                    .await;
                campaign.observe_shutdown().await.unwrap();
                if let Err(panic) = checked {
                    std::panic::resume_unwind(panic);
                }
            })
        })
        .await;
}

fn retained_snapshot(output: &str, stdout_bytes: usize, stderr_bytes: usize) -> &str {
    assert!(output.len() <= 2048, "complete UTF-8 envelope: {output}");
    assert!(output.contains('λ') && output.contains('μ'), "{output}");
    assert!(
        !output.contains('\u{fffd}'),
        "UTF-8 decoding loss: {output}"
    );
    assert!(
        output.contains("stdout-tail") && output.contains("stderr-tail"),
        "{output}"
    );
    assert!(
        output.contains("omitted stdout bytes ") && output.contains("omitted stderr bytes "),
        "{output}"
    );
    let pointer = output.lines().last().unwrap();
    let snapshot = pointer
        .strip_prefix("Recover without rerunning: ")
        .and_then(|value| value.strip_suffix('.'))
        .unwrap();
    assert!(
        snapshot.starts_with("let snap = Project.Shell.outputSnapshotFor \""),
        "{pointer}"
    );
    assert!(
        snapshot.ends_with(&format!(" {stdout_bytes} {stderr_bytes}")),
        "{pointer}"
    );
    assert!(!output.contains("{{job_binding}}"), "{output}");
    snapshot
}

async fn assert_pinned_shell_recovery(campaign: &mut TestCampaign) {
    let stdout = format!("{}stdout-tail\n", "λ".repeat(800));
    let stderr = format!("{}stderr-tail\n", "μ".repeat(800));
    let commands = TestCommands::completed_streams(&stdout, &stderr);
    let policy = std::sync::Arc::clone(&campaign.root_installation.policy);
    let invoked = dispatch_structured_tool(
        policy.as_ref(),
        "bash",
        serde_json::json!({
                "cmd": "printf retained-output",
                "workdir": null,
                "environment": null,
                "memory_mib": null,
                "tty": null,
                "stdin": null,
                "yield_time_ms": 30000,
                "max_output_bytes": 2048,
                "intent": "recover the retained streams without rerunning"
        }),
    );
    tokio::pin!(invoked);
    let request = tokio::select! {
        request = backend_request(campaign) => request,
        result = &mut invoked => panic!("pinned bash completed before requesting its command backend: {result:?}"),
    };
    request.supply(Ok(commands.clone()));
    let response = invoked.await;
    assert_eq!(response["status"], "committed", "{response}");
    let output = response["items"][0]["output"].as_str().unwrap();
    let snapshot = retained_snapshot(output, stdout.len(), stderr.len());

    // Evaluate the exact pointer emitted by the pinned source, rather than
    // reconstructing a Job or asking the command backend to execute again.
    let policy = std::sync::Arc::clone(&campaign.root_installation.policy);
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    for (section, tail) in [(1, "stdout-tail"), (2, "stderr-tail")] {
        let source = format!(
            "{snapshot}\nrecovered <- Project.Shell.section snap (Project.Shell.SectionId {section})\ndisplay (show recovered)"
        );
        let response = campaign
            .drive_actor_output(&store, dispatch_haskell_script(policy.as_ref(), &source))
            .await;
        let recovered = committed_display_text(&response);
        assert!(recovered.starts_with("Right "), "{response}");
        assert!(recovered.contains(tail), "{response}");
    }
    assert_eq!(commands.executions(), 1, "snapshot recovery must not rerun");

    commands.shorten_slice_read(3);
    let source = format!(
        "import qualified Tidepool.Command as Cmd\n{snapshot}\n{}",
        tidepool_testing::fixture_source("bridge/facade/src/actor_host/scaffold_expired_output.hs")
    );
    let expired = campaign
        .drive_actor_output(&store, dispatch_haskell_script(policy.as_ref(), &source))
        .await;
    assert_eq!(committed_display_text(&expired), "True", "{expired}");
    assert_eq!(commands.executions(), 1, "refused recovery must not rerun");
}
