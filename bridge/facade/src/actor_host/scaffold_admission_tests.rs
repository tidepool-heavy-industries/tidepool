use super::test_campaign::{commit_workspace, TestCampaign};
use exomonad_tool::{ActorEffectKey, HostedTool, ToolImplementation, ToolScheduling};

/// Exercise the package and pinned submodule that `exomonad new` delivers,
/// rather than copying repository examples or installing a fallback spec.
#[tokio::test(flavor = "multi_thread")]
async fn freshly_scaffolded_agent_spec_installs_notebook_and_workspace_tools() {
    let campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            crate::exomonad::new(crate::exomonad::NewOptions {
                path: Some(config.workspace.clone()),
                lock: Box::new(crate::exomonad::NixLock),
            })
            .expect("the shipped project must scaffold successfully");
            commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
                .expect("the freshly scaffolded project's inputs must freeze"),
            );
        },
    )
    .await;

    // Release the real runtime even if one of the surface assertions fails.
    let tools = campaign.root_installation.policy.tools().to_vec();
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();

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
