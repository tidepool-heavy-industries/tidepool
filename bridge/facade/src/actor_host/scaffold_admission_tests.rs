use super::command_test_support::{backend_request, TestCommands};
use super::test_campaign::{
    commit_workspace, committed_display_text, dispatch_haskell_script, dispatch_structured_tool,
    TestCampaign,
};
use exomonad_tool::{ActorEffectKey, HostedTool, ToolImplementation, ToolScheduling};
use futures_util::FutureExt;

/// Exercise the package and pinned submodule that `exomonad new` delivers,
/// rather than copying repository examples or installing a fallback spec.
#[tokio::test(flavor = "multi_thread")]
async fn freshly_scaffolded_agent_spec_installs_notebook_and_workspace_tools() {
    let mut campaign = TestCampaign::start_with_embedded_host_services(|config| {
        crate::exomonad::new(crate::exomonad::NewOptions {
            path: Some(config.workspace.clone()),
            lock: Box::new(crate::exomonad::NixLock),
        })
        .expect("the shipped project must scaffold successfully");
        commit_workspace(&config.workspace);
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                .expect("the freshly scaffolded project's inputs must freeze"),
        );
    })
    .await;

    // Reuse this actual pinned installation for command recovery; the embedded
    // selected-shell tests exercise a separate source owner.
    let checked = std::panic::AssertUnwindSafe(async {
        assert_pinned_shell_recovery(&mut campaign).await;
        let policy = std::sync::Arc::clone(&campaign.root_installation.policy);
        let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
        let reflected = campaign
            .drive_actor_output(
                &store,
                dispatch_haskell_script(
                    policy.as_ref(),
                    "import qualified Tidepool.Effects.Core as Core\nreflected <- Core.reflect 1\ndisplay (show (case reflected of { Left Core.ReflectUnbound -> True; _ -> False }))",
                ),
            )
            .await;
        assert_eq!(
            committed_display_text(&reflected),
            "True",
            "an installed reader must preserve absence of a bound application: {reflected}"
        );
        // No focus and no watchWith: these assertions do not ask Jev or launch
        // a provider turn, even when the production lazy client is configured.
        let facts = campaign
            .drive_actor_output(
                &store,
                dispatch_haskell_script(policy.as_ref(), include_str!("command_tool_facts_gate.hs")),
            )
            .await;
        assert_eq!(
            committed_display_text(&facts),
            "(True,True,True,True,True)",
            "the actual pinned Watchdog must judge typed facts independently of presentation: {facts}"
        );
    })
    .catch_unwind()
    .await;

    // Release the real runtime before checking the installed surface.
    let tools = campaign.root_installation.policy.tools().to_vec();
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    if let Err(panic) = checked {
        std::panic::resume_unwind(panic);
    }

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
    assert!(output.len() <= 2048, "complete UTF-8 envelope: {output}");
    assert!(output.contains('λ') && output.contains('μ'), "{output}");
    assert!(
        !output.contains('\u{fffd}'),
        "UTF-8 decoding loss: {output}"
    );
    assert!(
        output.contains("stdout-tail") && output.contains("stderr-tail"),
        "both retained stream tails: {output}"
    );
    assert!(
        output.contains("omitted stdout bytes ") && output.contains("omitted stderr bytes "),
        "both frozen stream omission ranges: {output}"
    );
    let pointer = output.lines().last().unwrap();
    let snapshot = pointer
        .strip_prefix("Recover without rerunning: ")
        .and_then(|value| value.strip_suffix('.'))
        .expect("the complete pinned recovery instruction must fit the envelope");
    assert!(
        snapshot.starts_with("let snap = Project.Shell.outputSnapshotFor \""),
        "the actual pin owns the session-id snapshot route: {pointer}"
    );
    assert!(
        snapshot.ends_with(&format!(" {} {}", stdout.len(), stderr.len())),
        "recovery must retain both actual frozen byte endpoints: {pointer}"
    );
    assert!(!output.contains("{{job_binding}}"), "{output}");

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
        "{snapshot}\nexpired <- Project.Shell.sectionPage snap (Project.Shell.SectionId 1)\ndisplay (show expired)"
    );
    let expired = campaign
        .drive_actor_output(&store, dispatch_haskell_script(policy.as_ref(), &source))
        .await;
    assert!(
        committed_display_text(&expired).starts_with("Left (SnapshotExpired Stdout "),
        "{expired}"
    );
    assert_eq!(commands.executions(), 1, "refused recovery must not rerun");
}
