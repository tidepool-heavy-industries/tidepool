//! Reloading one actor's agent spec from inside a live session.
//!
//! The comparison itself is checked in `tidepool_tool::surface`, and discovery
//! in `tidepool_actor::agent_spec`. What is checked HERE is what only a live
//! actor can answer: a rebuilt record that declares the same surface swaps and
//! a later call runs the new code; one that declares a different surface is
//! refused and the previous record keeps answering; and a spec found by
//! convention in an actor's own checkout is the one that gets installed.

use std::path::Path;

use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_structured_tool};
use super::*;

/// One tool, whose description and schemas are fixed and whose body is the
/// only thing a test edits. Reloading a body-only edit is the whole point; the
/// declaration must not move with it.
fn tools_module(description: &str, answer: &str) -> String {
    format!(
        r#"{{-# LANGUAGE DeriveAnyClass #-}}
{{-# LANGUAGE DeriveGeneric #-}}
{{-# LANGUAGE OverloadedStrings #-}}
{{-# LANGUAGE TypeOperators #-}}
module Project.Tools (SpecTools (..), Probe (..), tools) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract

newtype Probe = Probe {{ topic :: Text }}
  deriving (Generic, FromJSON, JsonSchema)

newtype SpecTools mode = SpecTools {{ probe :: mode :- Call Probe Text }}
  deriving (Generic)

tools :: SpecTools (AsServerT (Eff effects))
tools = SpecTools {{ probe = tool "{description}" (\_ -> pure "{answer}") }}
"#
    )
}

/// A spec module found by convention: the module `AgentSpec`, the value
/// `agentSpec`, and a record update over the default that fills one slot.
const SPEC_MODULE: &str = r#"{-# LANGUAGE OverloadedStrings #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools

agentSpec :: AgentSpec Tools.SpecTools effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just noted
  }

noted :: ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call == "probe" = pure (Annotated (toolResultHandle result))
  | otherwise = pure NoAnnotation
"#;

const CONFIG: &str = "[defaults]\nmodel = 'gpt-5.6-sol'\n\
                      [haskell]\nsource_roots = ['.']\nmodules = ['Project.Tools']\n\
                      tools = 'Project.Tools.tools'\n";

fn write_workspace(workspace: &Path, description: &str, answer: &str) {
    let authored = workspace.join(".shoal");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(authored.join("config.toml"), CONFIG).unwrap();
    std::fs::write(
        authored.join("Project/Tools.hs"),
        tools_module(description, answer),
    )
    .unwrap();
}

const DESCRIPTION: &str = "Answer one fixed question about a topic.";

async fn start(description: &str, answer: &str) -> TestCampaign {
    TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, description, answer);
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await
}

/// What the one hosted tool answers right now.
async fn probe(policy: &dyn tidepool_actor::ResidentToolEndpoint) -> String {
    let result =
        dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": "anything"})).await;
    result.to_string()
}

/// Ask this actor to rebuild its own spec, and read the receipt.
async fn reload(policy: &dyn tidepool_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "reload_agent_spec", serde_json::json!({}))
        .await
        .to_string()
}

async fn status(policy: &dyn tidepool_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "status", serde_json::json!({"view": "detailed"}))
        .await
        .to_string()
}

/// The whole point: an edited body, the same declared surface, and the NEXT
/// call runs the new code — inside one session, with no new incarnation.
#[tokio::test]
async fn a_rebuilt_record_with_the_same_surface_swaps_and_later_calls_run_new_code() {
    let campaign = start(DESCRIPTION, "one").await;
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    assert!(probe(policy).await.contains("one"));

    // Only the handler body moves. The description and both schemas are
    // spelled identically, so the declared surface cannot have changed.
    std::fs::write(
        workspace.join(".shoal/Project/Tools.hs"),
        tools_module(DESCRIPTION, "two"),
    )
    .unwrap();
    let receipt = reload(policy).await;
    assert!(receipt.contains("swapped"), "{receipt}");
    assert!(receipt.contains("install=2"), "{receipt}");

    let after = probe(policy).await;
    assert!(after.contains("two"), "{after}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A changed description is a change a model would see, and the tool bridge
/// registers a list once and serves it read-only. So the reload is refused, the
/// difference is returned by name, and the record already installed keeps
/// answering with what it always answered.
#[tokio::test]
async fn a_changed_description_is_refused_with_the_difference_and_the_old_record_answers() {
    let campaign = start(DESCRIPTION, "one").await;
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    assert!(probe(policy).await.contains("one"));

    std::fs::write(
        workspace.join(".shoal/Project/Tools.hs"),
        tools_module("Answer one fixed question, and explain it.", "two"),
    )
    .unwrap();
    let receipt = reload(policy).await;
    assert!(receipt.contains("refused"), "{receipt}");
    assert!(receipt.contains("probe: description changed"), "{receipt}");
    assert!(!receipt.contains("swapped"), "{receipt}");

    // The previously installed record is still the one serving calls.
    let after = probe(policy).await;
    assert!(after.contains("one"), "{after}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A spec that does not typecheck fails its own reload. The edited file stays
/// on disk exactly as the model wrote it — repairing it is the next thing the
/// model does — and the previous spec is still serving calls.
#[tokio::test]
async fn a_spec_that_does_not_typecheck_leaves_the_old_one_active_and_the_file_on_disk() {
    let campaign = start(DESCRIPTION, "one").await;
    let workspace = campaign._repository.path().to_path_buf();
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    assert!(probe(policy).await.contains("one"));

    let broken = tools_module(DESCRIPTION, "one").replace(
        "(\\_ -> pure \"one\")",
        "(\\_ -> pure undefinedByThisSpecReload)",
    );
    std::fs::write(workspace.join(".shoal/Project/Tools.hs"), &broken).unwrap();
    let receipt = reload(policy).await;
    assert!(receipt.contains("rejected"), "{receipt}");
    assert!(receipt.contains("undefinedByThisSpecReload"), "{receipt}");
    assert!(!receipt.contains("swapped"), "{receipt}");

    let after = probe(policy).await;
    assert!(after.contains("one"), "{after}");
    assert_eq!(
        std::fs::read_to_string(workspace.join(".shoal/Project/Tools.hs")).unwrap(),
        broken,
        "the edited file is exactly as it was written"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Discovery is implicit, so it is reported. A workspace with no `AgentSpec.hs`
/// anywhere resolves by the existing `[haskell] tools` key, and says so — which
/// is also the check that such a workspace behaves exactly as it did before
/// specs existed.
#[tokio::test]
async fn a_workspace_without_a_spec_file_reports_the_tools_key_and_behaves_as_before() {
    let campaign = start(DESCRIPTION, "one").await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let status = status(policy).await;
    assert!(status.contains("workspace tools key"), "{status}");
    assert!(status.contains("Project.Tools.tools"), "{status}");
    assert!(status.contains("slots=[]"), "{status}");
    assert!(probe(policy).await.contains("one"));

    // The Haskell workbench is untouched by any of this.
    let cell = dispatch_haskell_script(policy, "inspectFull (1 + 1 :: Int)").await;
    assert_eq!(cell["items"][0]["output"], "2", "{cell}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// An actor whose own checkout carries `AgentSpec.hs` installs THAT, ahead of
/// both configured keys, and says which rule matched and which file it read.
/// The root, which has no checkout of its own, keeps resolving by the
/// workspace's key — so the two actors in one run are running two different
/// specs, which is the whole reason discovery is per checkout.
#[tokio::test]
async fn a_checkout_spec_module_is_installed_ahead_of_the_workspace_key() {
    let mut campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, DESCRIPTION, "one");
            std::fs::write(config.workspace.join(".shoal/AgentSpec.hs"), SPEC_MODULE).unwrap();
            commit(&config.workspace, "authored package with a spec");
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await;
    let root = campaign.root_installation.policy.clone();

    let launch = {
        let root = root.clone();
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), CODING_CHILD).await })
    };
    let child = next_child(&mut campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");

    let checkout = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert!(
        checkout.cwd().join(".shoal/AgentSpec.hs").exists(),
        "the checkout carries the spec module"
    );

    let child_status = status(child.policy.as_ref()).await;
    assert!(child_status.contains("checkout module"), "{child_status}");
    assert!(
        child_status.contains("AgentSpec.agentSpec"),
        "{child_status}"
    );
    assert!(child_status.contains("AgentSpec.hs"), "{child_status}");
    // One compile, two products: the declared surface, and the slot the same
    // spec filled, retained beside the tools it sits with.
    assert!(child_status.contains("slots=[afterTool]"), "{child_status}");

    // The root has no checkout of its own, so rule one cannot answer for it.
    let root_status = status(root.as_ref()).await;
    assert!(root_status.contains("workspace tools key"), "{root_status}");

    // And the spec the child installed is the one serving its calls.
    assert!(probe(child.policy.as_ref()).await.contains("one"));

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A child editing its own checkout rebuilds its own spec, and the reload
/// reaches that checkout's layer alone: the root, which shares the run's layer
/// with every other actor, keeps answering with the record it installed.
#[tokio::test]
async fn a_child_reloads_its_own_spec_and_never_upgrades_anybody_else() {
    let mut campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, DESCRIPTION, "one");
            std::fs::write(config.workspace.join(".shoal/AgentSpec.hs"), SPEC_MODULE).unwrap();
            commit(&config.workspace, "authored package with a spec");
            config.workspace_inputs = Some(
                crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
                    .unwrap(),
            );
        },
    )
    .await;
    let root = campaign.root_installation.policy.clone();

    let launch = {
        let root = root.clone();
        tokio::spawn(async move { dispatch_haskell_script(root.as_ref(), CODING_CHILD).await })
    };
    let child = next_child(&mut campaign).await;
    assert_eq!(launch.await.unwrap()["status"], "committed");

    let checkout = campaign
        .worktrees
        .lookup(&tidepool_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert!(probe(child.policy.as_ref()).await.contains("one"));
    assert!(probe(root.as_ref()).await.contains("one"));

    std::fs::write(
        checkout.cwd().join(".shoal/Project/Tools.hs"),
        tools_module(DESCRIPTION, "two"),
    )
    .unwrap();
    let receipt = reload(child.policy.as_ref()).await;
    assert!(receipt.contains("swapped"), "{receipt}");
    assert!(receipt.contains("checkout module"), "{receipt}");

    assert!(probe(child.policy.as_ref()).await.contains("two"));
    // Scoped to the actor that asked: the root never saw this reload.
    assert!(probe(root.as_ref()).await.contains("one"));

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

// ---------------------------------------------------------------------------
// Shared with the source-reload campaign: a child needs a committed package in
// its checkout to have a layer of its own.
// ---------------------------------------------------------------------------

const CODING_CHILD: &str = "let campaign = \"agent-spec\" :: CampaignLabel\n\
     let group = \"checkout\" :: ForkGroupLabel\n\
     let leaf = \"editor\" :: Label\n\
     worker <- unfold (batch campaign group) (child (coding @Text projectHead (assignment leaf ())))\n";

fn commit(workspace: &Path, message: &str) {
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=Agent Spec Test",
        "-c",
        "user.email=agent-spec@example.invalid",
        "commit",
        "-m",
        message,
    ]);
}

async fn next_child(campaign: &mut TestCampaign) -> tidepool_actor::LocalResidentInstallation {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            match campaign.deployments.recv().await.unwrap() {
                LocalResidentDeployment::PolicyInstalled(child) => {
                    campaign.authority.install_grant(
                        child.actor.identity().into(),
                        worktree_grant(child.effective_role.role()),
                    );
                    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
                    return *child;
                }
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("{actor:?} retired: {terminal:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("child admission")
}
