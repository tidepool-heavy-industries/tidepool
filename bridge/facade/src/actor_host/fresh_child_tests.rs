use super::display_output;
use super::test_campaign::{committed_display_text, dispatch_haskell_script, TestCampaign};
use exomonad_actor::{ActorExitKind, ActorTerminal, LocalResidentDeployment};
use std::time::Duration;

/// Opt-in qualification of the dedicated-machine factory through a selected
/// typed launch. Default campaigns and the production host share a machine.
/// This campaign explicitly installs the root's compiled child bootstrap;
/// the child must retain its nominal inputs, allocate its own declarations,
/// and release its distinct machine on retirement.
#[tokio::test]
async fn opted_in_selected_context_child_owns_and_retires_its_machine() {
    let mut campaign = TestCampaign::start_with_child_sessions().await;
    let root = campaign.root_installation.policy.clone();
    let root_session = campaign
        .forest
        .actor_session(campaign.actor.identity())
        .expect("root actor has a session");

    let parent = dispatch_haskell_script(
        root.as_ref(),
        "data FreshParentInput = FreshParentInput Int deriving Show\ndata FreshParentReply = FreshParentReply Int deriving Show",
    ).await;
    assert_eq!(parent["status"], "committed", "{parent}");
    let parent_manifest_path = campaign.session_root.path().join("root-declarations.json");
    let parent_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&parent_manifest_path).unwrap()).unwrap();
    let parent_high_water = parent_manifest["high_water"].as_u64().unwrap();
    let root_for_setup = root.clone();
    let setup = tokio::spawn(async move {
        dispatch_haskell_script(
            root_for_setup.as_ref(),
            include_str!("fresh_selected_child_setup.hs"),
        )
        .await
    });
    let installation = campaign
        .next_deployment(
            "cross-session child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                other => Err(other),
            },
        )
        .await;
    let child_session = campaign
        .forest
        .actor_session(installation.actor.identity())
        .expect("child actor has a session");
    assert_ne!(
        child_session, root_session,
        "an opted-in eligible SelectedContext launch must own its own machine"
    );

    installation
        .fork_gate
        .as_ref()
        .expect("selected fork readiness owner")
        .mark_ready()
        .unwrap();
    let setup = setup.await.unwrap();
    assert_eq!(setup["status"], "committed", "{setup}");
    campaign
        .next_deployment(
            "fresh child typed activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == installation.actor.identity() =>
                {
                    Ok(activation)
                }
                LocalResidentDeployment::Retired { actor, terminal }
                    if actor == installation.actor.identity() =>
                {
                    panic!("fresh child retired before typed activation: {terminal:?}");
                }
                other => Err(other),
            },
        )
        .await;
    let store = display_output::open_run_store(campaign.session_root.path()).unwrap();
    let reply = campaign.drive_actor_output(&store, dispatch_haskell_script(
        installation.policy.as_ref(),
        "data FreshChildNominal = FreshChildNominal FreshParentInput\nfreshChildValue <- pure (FreshChildNominal sessionInput)\ndisplay (case freshChildValue of FreshChildNominal (FreshParentInput n) -> n + 1)",
    )).await;
    assert_eq!(committed_display_text(&reply), "42", "{reply}");
    let child_root = campaign
        .session_root
        .path()
        .join("haskell-session-children")
        .join(child_session.0.to_string());
    let child_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(child_root.join("declarations.json")).unwrap())
            .unwrap();
    assert_eq!(child_manifest["source_session"], child_session.0);
    let authored = child_manifest["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| {
            node["kind"] == "authored"
                && node["exports"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|export| export["identity"]["occurrence"] == "FreshChildNominal")
        })
        .unwrap();
    let generation = authored["id"].as_u64().unwrap();
    assert!(generation > parent_high_water, "{child_manifest}");
    let identity = authored["exports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|export| export["identity"]["occurrence"] == "FreshChildNominal")
        .unwrap();
    assert_eq!(
        identity["identity"]["module"],
        tidepool_repr::SessionModule::lib(tidepool_repr::Generation(generation)).module_name()
    );
    let projections: Vec<tidepool_toolchain::recovery_artifacts::RecoveryJoinRef> = child_manifest
        ["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|artifact| artifact["kind"] == "join")
        .map(|artifact| serde_json::from_value(artifact["reference"].clone()).unwrap())
        .collect();
    assert!(
        authored["lexical_roots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|root| {
                projections.iter().any(|projection| {
                    root["unit"] == projection.unit && root["module"] == projection.module
                })
            }),
        "the public lexical root must be an issued interface: {child_manifest}"
    );
    let native: tidepool_toolchain::recovery_artifacts::RecoveryArtifactRef =
        serde_json::from_value(
            child_manifest["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|artifact| {
                    artifact["kind"] == "home"
                        && artifact["reference"]["unit"] == identity["identity"]["unit"]
                        && artifact["reference"]["module"] == identity["identity"]["module"]
                })
                .unwrap()["reference"]
                .clone(),
        )
        .unwrap();
    tidepool_toolchain::recovery_artifacts::verify_materialized_ref(&child_root, &native).unwrap();
    use tidepool_toolchain::artifact_inventory::{ArtifactDescriptor, ArtifactId};
    let mut reached = std::collections::BTreeSet::new();
    let mut pending =
        std::collections::VecDeque::from([ArtifactDescriptor::from_recovery_product(&native).id]);
    let dependencies: Vec<(ArtifactId, ArtifactId)> = child_manifest["artifact_dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| {
            (
                serde_json::from_value(edge["source"].clone()).unwrap(),
                serde_json::from_value(edge["target"].clone()).unwrap(),
            )
        })
        .collect();
    while let Some(source) = pending.pop_front() {
        if reached.insert(source) {
            pending.extend(
                dependencies
                    .iter()
                    .filter_map(|(owner, dependency)| (*owner == source).then_some(*dependency)),
            );
        }
    }
    assert!(projections.iter().any(|projection| reached.contains(&ArtifactDescriptor::from_recovery_join(projection).id)),
        "original native declaration must retain its selected interface dependency: {child_manifest}");
    for projection in &projections {
        tidepool_toolchain::recovery_artifacts::verify_materialized_join(&child_root, projection)
            .unwrap();
    }
    let parent_after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&parent_manifest_path).unwrap()).unwrap();
    assert!(!parent_after["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["exports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|export| export["identity"]["occurrence"] == "FreshChildNominal")));

    installation
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "test teardown".into(),
        })
        .await
        .expect("child shuts down");
    campaign
        .next_deployment(
            "cross-session child retirement",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::Retired { actor, terminal }
                    if actor == installation.actor.identity() =>
                {
                    Ok(terminal)
                }
                other => Err(other),
            },
        )
        .await;
    assert_eq!(
        campaign.forest.session_state_of(child_session),
        tidepool_runtime::session::ResidentSessionState::Gone,
        "the dedicated child session must be released once its actor retires"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
