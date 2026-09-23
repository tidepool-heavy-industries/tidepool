//! Reloading one actor's agent spec from inside a live session, and the
//! after-tool slot that spec fills.
//!
//! The comparison itself is checked in `exomonad_tool::surface`, discovery in
//! `exomonad_actor::agent_spec`, and the delivery rules in
//! `exomonad_actor::after_tool`. What is checked HERE is what only a live actor
//! can answer: a rebuilt record that declares the same surface swaps and a
//! later call runs the new code; one that declares a different surface is
//! refused and the previous record keeps answering; a spec found by convention
//! in an actor's own checkout is the one that gets installed; and a retained
//! slot, applied at the tool-result boundary in the actor's own resident
//! machine, annotates or prunes what the model is shown without ever rewriting
//! it.

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
module Project.Tools (SpecTools (..), Probe (..), probeBody, tools) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract

newtype Probe = Probe {{ topic :: Text }}
  deriving (Generic, FromJSON, JsonSchema)

newtype SpecTools mode = SpecTools {{ probe :: mode :- Call Probe Text }}
  deriving (Generic)

-- | The tool's implementation as ordinary source, so a slot can exercise the
-- same function the tool dispatches to without going through the boundary.
probeBody :: Probe -> Eff effects Text
probeBody _ = pure "{answer}"

tools :: SpecTools (AsServerT (Eff effects))
tools = SpecTools {{ probe = tool "{description}" probeBody }}
"#
    )
}

/// A spec module found by convention: the module `AgentSpec`, the value
/// `agentSpec`, and a record update over the default that fills one slot. Only
/// the slot's body differs between tests.
fn spec_module(slot: &str) -> String {
    format!(
        r#"{{-# LANGUAGE OverloadedStrings #-}}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools

agentSpec :: AgentSpec Tools.SpecTools effects
agentSpec = defaultSpec
  {{ specTools = Tools.tools
  , afterTool = Just noted
  }}

noted :: ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call /= T.pack "probe" = pure NoAnnotation
  | otherwise = {slot}
"#
    )
}

/// A spec module whose slot can suspend on the resident `Sleep` effect: the
/// same shape as `spec_module`, with a `Member Sleep effects` constraint and
/// the imports that constraint needs. Kept separate so the plain
/// `spec_module` every other test uses never carries an effect constraint it
/// doesn't need.
fn spec_module_with_sleep(slot: &str) -> String {
    format!(
        r#"{{-# LANGUAGE OverloadedStrings #-}}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Sleep, sleep)
import Tidepool.Duration (seconds)
import qualified Project.Tools as Tools

agentSpec :: Member Sleep effects => AgentSpec Tools.SpecTools effects
agentSpec = defaultSpec
  {{ specTools = Tools.tools
  , afterTool = Just noted
  }}

noted :: Member Sleep effects => ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call /= T.pack "probe" = pure NoAnnotation
  | otherwise = {slot}
"#
    )
}

/// Derived context beside the tool's own output.
const ANNOTATES: &str = "pure (Annotated (T.pack \"asked about this topic twice before\"))";

/// A selection, and the handle the whole of it stays addressable under.
const PRUNES: &str = "pure (Pruned (T.pack \"the line that mattered\") (toolResultHandle result))";

/// A deliberate non-decision. Silent to the model.
const ABSTAINS: &str = "pure (Abstained (T.pack \"the result is already minimal\"))";

/// A slot that is simply broken.
const FAILS: &str = "error \"the slot is broken\"";

/// Suspends on the resident `Sleep` effect for three seconds — long enough to
/// outlast a short after-tool wait while the slot is genuinely parked mid-
/// effect, not merely slow — and then annotates.
const SLEEPS_THEN_ANNOTATES: &str =
    "do { sleep (seconds 3); pure (Annotated (T.pack \"slept then annotated\")) }";

/// A slot that runs the tool's own implementation. Its own tool use must not
/// bring it back round on itself.
const REENTERS: &str =
    "Annotated . (T.pack \"the slot ran the tool body and got: \" <>) <$> Tools.probeBody (Tools.Probe (T.pack \"again\"))";

const CONFIG: &str = "[defaults]\nmodel = 'gpt-6-sol'\n\
                      [haskell]\nsource_roots = ['.']\nmodules = ['Project.Tools']\n\
                      tools = 'Project.Tools.tools'\n";

/// The same workspace with rule two answering: `[haskell] spec` names the
/// module, so the ROOT installs the spec and its slot without needing a
/// checkout of its own.
const SPEC_CONFIG: &str = "[defaults]\nmodel = 'gpt-6-sol'\n\
                           [haskell]\nsource_roots = ['.']\n\
                           modules = ['Project.Tools', 'AgentSpec']\n\
                           tools = 'Project.Tools.tools'\n\
                           spec = 'AgentSpec.agentSpec'\n";

fn write_workspace(workspace: &Path, description: &str, answer: &str) {
    let authored = workspace.join(".exomonad");
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
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, description, answer);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
                .unwrap(),
            );
        },
    )
    .await
}

/// A root whose spec is named by rule two, so it carries a slot with no
/// checkout of its own.
async fn start_with_slot(answer: &str, slot: &str) -> TestCampaign {
    let slot = slot.to_owned();
    let answer = answer.to_owned();
    TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        move |config| {
            write_workspace(&config.workspace, DESCRIPTION, &answer);
            let authored = config.workspace.join(".exomonad");
            std::fs::write(authored.join("config.toml"), SPEC_CONFIG).unwrap();
            std::fs::write(authored.join("AgentSpec.hs"), spec_module(&slot)).unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
                .unwrap(),
            );
        },
    )
    .await
}

/// A root whose spec is named by rule two, carrying a slot that can suspend
/// on the resident `Sleep` effect.
async fn start_with_sleeping_slot(answer: &str, slot: &str) -> TestCampaign {
    let slot = slot.to_owned();
    let answer = answer.to_owned();
    TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        move |config| {
            write_workspace(&config.workspace, DESCRIPTION, &answer);
            let authored = config.workspace.join(".exomonad");
            std::fs::write(authored.join("config.toml"), SPEC_CONFIG).unwrap();
            std::fs::write(authored.join("AgentSpec.hs"), spec_module_with_sleep(&slot)).unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
                .unwrap(),
            );
        },
    )
    .await
}

/// What the one hosted tool answers right now.
async fn probe(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    let result =
        dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": "anything"})).await;
    result.to_string()
}

/// Ask this actor to rebuild its own spec, and read the receipt.
async fn reload(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "reload_agent_spec", serde_json::json!({}))
        .await
        .to_string()
}

async fn status(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "status", serde_json::json!({"view": "detailed"}))
        .await
        .to_string()
}

/// Reports activation and reload costs through the same counters as notebook
/// cells. Run alone against a resident daemon; wall times are observations,
/// while tool and slot output prove each measured installation was usable.
#[tokio::test]
#[ignore = "reports actor-spec costs; requires a resident compiler daemon"]
async fn actor_spec_cost_measurement() {
    use super::cell_compile_cost_tests::report;
    use std::time::Instant;

    assert!(std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_some());
    for round in 0..2 {
        let before = tidepool_extract_cmd::extract_spawn_count();
        let started = Instant::now();
        let campaign = start_with_slot("measurement-one", ANNOTATES).await;
        report(
            &campaign,
            "specActivation",
            Some(round),
            None,
            before,
            started,
        );
        let policy = campaign.root_installation.policy.as_ref();
        let initial = probe(policy).await;
        assert!(initial.contains("measurement-one"), "{initial}");
        assert!(
            initial.contains("asked about this topic twice before"),
            "{initial}"
        );

        let before = tidepool_extract_cmd::extract_spawn_count();
        let started = Instant::now();
        let receipt = reload(policy).await;
        report(
            &campaign,
            "specUnchangedReload",
            Some(round),
            None,
            before,
            started,
        );
        assert!(receipt.contains("swapped"), "{receipt}");

        std::fs::write(
            campaign
                ._repository
                .path()
                .join(".exomonad/Project/Tools.hs"),
            tools_module(DESCRIPTION, "measurement-two"),
        )
        .unwrap();
        let before = tidepool_extract_cmd::extract_spawn_count();
        let started = Instant::now();
        let receipt = reload(policy).await;
        report(
            &campaign,
            "specEditedReload",
            Some(round),
            None,
            before,
            started,
        );
        assert!(receipt.contains("swapped"), "{receipt}");
        let changed = probe(policy).await;
        assert!(changed.contains("measurement-two"), "{changed}");
        assert!(
            changed.contains("asked about this topic twice before"),
            "{changed}"
        );
        campaign.forest.shutdown().await;
        campaign.hosted.await.unwrap();
    }
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
        workspace.join(".exomonad/Project/Tools.hs"),
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
        workspace.join(".exomonad/Project/Tools.hs"),
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

    let broken =
        tools_module(DESCRIPTION, "one").replace("pure \"one\"", "pure undefinedByThisSpecReload");
    assert!(broken.contains("undefinedByThisSpecReload"), "{broken}");
    std::fs::write(workspace.join(".exomonad/Project/Tools.hs"), &broken).unwrap();
    let receipt = reload(policy).await;
    assert!(receipt.contains("rejected"), "{receipt}");
    assert!(receipt.contains("undefinedByThisSpecReload"), "{receipt}");
    assert!(!receipt.contains("swapped"), "{receipt}");

    let after = probe(policy).await;
    assert!(after.contains("one"), "{after}");
    assert_eq!(
        std::fs::read_to_string(workspace.join(".exomonad/Project/Tools.hs")).unwrap(),
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
/// The root, which has no checkout of its own, finds the run's copy by the same
/// rule. Each reads its own roots first, so once the child edits its checkout
/// the two are running two different specs, which is the whole reason
/// discovery is per checkout.
#[tokio::test]
async fn a_checkout_spec_module_is_installed_ahead_of_the_workspace_key() {
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, DESCRIPTION, "one");
            std::fs::write(
                config.workspace.join(".exomonad/AgentSpec.hs"),
                spec_module(ANNOTATES),
            )
            .unwrap();
            commit(&config.workspace, "authored package with a spec");
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
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
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert!(
        checkout.cwd().join(".exomonad/AgentSpec.hs").exists(),
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

    // The root has no checkout of its own, and finds the run's copy of the
    // same module by the same rule: the first `AgentSpec.hs` in the roots its
    // cells resolve, which for the root are the run's.
    let root_status = status(root.as_ref()).await;
    assert!(root_status.contains("checkout module"), "{root_status}");

    // And the spec the child installed is the one serving its calls, slot and
    // all — attributing its annotation to the revision of the checkout it was
    // compiled from rather than to the run's.
    let answered = probe(child.policy.as_ref()).await;
    assert!(answered.contains("one"), "{answered}");
    assert!(
        answered.contains("asked about this topic twice before"),
        "{answered}"
    );
    assert!(!answered.contains("spec revision (run)"), "{answered}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A child editing its own checkout rebuilds its own spec, and the reload
/// reaches that checkout's layer alone: the root, which shares the run's layer
/// with every other actor, keeps answering with the record it installed.
#[tokio::test]
async fn a_child_reloads_its_own_spec_and_never_upgrades_anybody_else() {
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, DESCRIPTION, "one");
            std::fs::write(
                config.workspace.join(".exomonad/AgentSpec.hs"),
                spec_module(ANNOTATES),
            )
            .unwrap();
            commit(&config.workspace, "authored package with a spec");
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
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
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &child.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert!(probe(child.policy.as_ref()).await.contains("one"));
    assert!(probe(root.as_ref()).await.contains("one"));

    std::fs::write(
        checkout.cwd().join(".exomonad/Project/Tools.hs"),
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
// The after-tool slot, applied at the tool-result boundary.
// ---------------------------------------------------------------------------

/// Derived context reaches the model beside the tool's own output, said to be
/// derived and naming the revision the slot was compiled from. The tool's
/// answer is still there in full: the slot annotated, it did not rewrite.
#[tokio::test]
async fn an_annotation_reaches_the_model_as_derived_context_beside_the_output() {
    let campaign = start_with_slot("keptwhole", ANNOTATES).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = probe(policy).await;
    assert!(result.contains("keptwhole"), "{result}");
    assert!(
        result.contains("Derived context, not part of the tool's output"),
        "{result}"
    );
    assert!(
        result.contains("asked about this topic twice before"),
        "{result}"
    );
    assert!(result.contains("spec revision"), "{result}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A pruned view says it is a selection, and the whole result stays
/// addressable: the handle the slot was shown is an ordinary binding a later
/// cell evaluates.
#[tokio::test]
async fn a_pruned_result_says_it_is_a_selection_and_its_handle_answers_the_whole() {
    let campaign = start_with_slot("keptwhole", PRUNES).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = probe(policy).await;
    assert!(
        result.contains("A selection of this result, not the whole of it"),
        "{result}"
    );
    assert!(result.contains("the line that mattered"), "{result}");
    assert!(result.contains("toolResult1"), "{result}");

    // The handle is a binding in the same lexical scope the model's own cells
    // run in, so the whole result is one cell away.
    let cell = dispatch_haskell_script(policy, "inspectFull toolResult1").await;
    assert!(
        cell["items"][0]["output"]
            .as_str()
            .unwrap_or_default()
            .contains("keptwhole"),
        "{cell}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Abstention is silent. The model never asked for a judgement on this result,
/// so the original is delivered exactly as the tool produced it and the reason
/// lives where a model can go and look for it.
#[tokio::test]
async fn an_abstention_delivers_the_original_and_appears_only_in_the_receipt() {
    let campaign = start_with_slot("keptwhole", ABSTAINS).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = probe(policy).await;
    assert!(result.contains("keptwhole"), "{result}");
    assert!(!result.contains("[after-tool]"), "{result}");
    assert!(!result.contains("already minimal"), "{result}");

    let status = status(policy).await;
    assert!(status.contains("after-tool#1"), "{status}");
    assert!(
        status.contains("abstained: the result is already minimal"),
        "{status}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A slot that failed is not a slot that abstained: the original result is
/// preserved and one compact line warns, because a failure could change how
/// the result is read. The second failure earns a reference, not a second copy
/// of the diagnostic.
#[tokio::test]
async fn a_slot_that_fails_delivers_the_original_with_one_compact_line() {
    let campaign = start_with_slot("keptwhole", FAILS).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let first = probe(policy).await;
    assert!(first.contains("keptwhole"), "{first}");
    assert!(first.contains("the slot did not answer"), "{first}");
    assert!(first.contains("after-tool#1"), "{first}");
    assert_eq!(
        first.matches("[after-tool]").count(),
        1,
        "one compact line, not a diagnostic dump: {first}"
    );

    let second = probe(policy).await;
    assert!(second.contains("keptwhole"), "{second}");
    assert!(
        second.contains("same failure as after-tool#1"),
        "a repeated failure does not fill the conversation with copies: {second}"
    );
    assert!(!second.contains("the slot did not answer"), "{second}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// The result waits for the slot, and when the wait runs out the original is
/// delivered anyway. Nothing is rolled back and nothing is replayed.
#[tokio::test]
async fn a_slot_that_outruns_its_wait_delivers_the_original_result() {
    // The wait is five minutes, which is not a test. Shortening it to nothing
    // is the whole of what this knob is for.
    std::env::set_var(exomonad_actor::AFTER_TOOL_WAIT_ENV, "0");
    let campaign = start_with_slot("keptwhole", ANNOTATES).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = probe(policy).await;
    assert!(result.contains("keptwhole"), "{result}");
    assert!(result.contains("no answer within 0ms"), "{result}");
    assert!(
        !result.contains("asked about this topic twice before"),
        "{result}"
    );

    let status = status(policy).await;
    assert!(status.contains("timed out after 0ms"), "{status}");

    std::env::remove_var(exomonad_actor::AFTER_TOOL_WAIT_ENV);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A slot cut off while it is suspended on an effect, rather than at its first
/// await: the wait is 400ms and the slot sleeps three seconds on the resident
/// `Sleep` effect. Its suspended turn is aborted, so the machine keeps
/// answering at its usual pace, and the late slot answer reaches nobody.
#[tokio::test]
async fn a_slot_cut_off_mid_effect_leaves_the_machine_answering() {
    std::env::set_var(exomonad_actor::AFTER_TOOL_WAIT_ENV, "400");
    let campaign = start_with_sleeping_slot("keptwhole", SLEEPS_THEN_ANNOTATES).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    // The first authored cell pays for the cell template's cold compile. Pay
    // it here, so the timing below measures the cut-off and nothing else.
    let warm = std::time::Instant::now();
    let _ = dispatch_haskell_script(policy, "inspectFull (2 + 2 :: Int)").await;
    eprintln!("DUMP warm-up cell before any slot [{:?}]", warm.elapsed());
    let baseline = std::time::Instant::now();
    let _ = dispatch_haskell_script(policy, "inspectFull (3 + 3 :: Int)").await;
    let baseline = baseline.elapsed();
    eprintln!("DUMP warm baseline cell [{baseline:?}]");

    let t0 = std::time::Instant::now();
    let first = tokio::time::timeout(Duration::from_secs(60), probe(policy))
        .await
        .expect("first probe did not hang");
    eprintln!("DUMP first probe [{:?}]: {first}", t0.elapsed());
    assert!(first.contains("keptwhole"), "{first}");
    assert!(first.contains("no answer within 400ms"), "{first}");

    // Immediately: the slot future was just dropped mid-effect. Does the next
    // call on the same actor still work?
    let t1 = std::time::Instant::now();
    let second = tokio::time::timeout(Duration::from_secs(60), probe(policy))
        .await
        .expect("second probe did not hang");
    eprintln!(
        "DUMP second probe (immediately after the cut-off) [{:?}]: {second}",
        t1.elapsed()
    );

    // An authored cell, in the same resident workbench.
    let t2 = std::time::Instant::now();
    let cell = tokio::time::timeout(
        Duration::from_secs(60),
        dispatch_haskell_script(policy, "inspectFull (1 + 1 :: Int)"),
    )
    .await
    .expect("authored cell did not hang");
    eprintln!(
        "DUMP authored cell after cut-off [{:?}]: {cell}",
        t2.elapsed()
    );
    // A slot cut off while suspended on an effect must not hold the machine
    // against the next caller: the cell costs what a warm cell cost before.
    assert_eq!(cell["items"][0]["output"], "2", "{cell}");
    assert!(
        t2.elapsed() < baseline * 2 + Duration::from_secs(5),
        "a cell after two cut-off slots took {:?} against a warm baseline of {baseline:?}",
        t2.elapsed()
    );

    // Wait past when the original 3s sleep would have elapsed, then probe and
    // check status again.
    tokio::time::sleep(Duration::from_secs(4)).await;

    let t3 = std::time::Instant::now();
    let third = tokio::time::timeout(Duration::from_secs(60), probe(policy))
        .await
        .expect("third probe did not hang");
    eprintln!(
        "DUMP third probe (after the original sleep would have elapsed) [{:?}]: {third}",
        t3.elapsed()
    );

    let t4 = std::time::Instant::now();
    let status_after = tokio::time::timeout(Duration::from_secs(60), status(policy))
        .await
        .expect("status did not hang");
    eprintln!("DUMP status after [{:?}]: {status_after}", t4.elapsed());
    assert!(third.contains("keptwhole"), "{third}");
    assert!(!third.contains("slept then annotated"), "{third}");
    assert_eq!(
        status_after.matches("timed out after 400ms").count(),
        3,
        "{status_after}"
    );

    std::env::remove_var(exomonad_actor::AFTER_TOOL_WAIT_ENV);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// The root has no checkout of its own, and its spec is still the first
/// `AgentSpec.hs` its cells would resolve: no configuration key names it.
#[tokio::test]
async fn the_root_finds_its_spec_by_convention_with_no_key_naming_it() {
    let campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, DESCRIPTION, "keptwhole");
            // CONFIG names only the tools key, and lists no spec module.
            std::fs::write(
                config.workspace.join(".exomonad/AgentSpec.hs"),
                spec_module(ANNOTATES),
            )
            .unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
                .unwrap(),
            );
        },
    )
    .await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = probe(policy).await;
    assert!(result.contains("keptwhole"), "{result}");
    assert!(result.contains("twice before"), "{result}");

    let status = status(policy).await;
    assert!(status.contains("rule=checkout module"), "{status}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// The two tools a model repairs a broken slot with are never annotated, so a
/// slot can never block its own repair. Neither is an authored cell, which is
/// not a tool call at all.
#[tokio::test]
async fn the_repair_tools_and_authored_cells_are_never_annotated() {
    let campaign = start_with_slot("keptwhole", ANNOTATES).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let reloaded = reload(policy).await;
    assert!(reloaded.contains("swapped"), "{reloaded}");
    assert!(!reloaded.contains("[after-tool]"), "{reloaded}");

    let status = status(policy).await;
    assert!(!status.contains("[after-tool]"), "{status}");

    let cell = dispatch_haskell_script(policy, "inspectFull (1 + 1 :: Int)").await;
    assert_eq!(cell["items"][0]["output"], "2", "{cell}");

    // And the slot is installed and working, so none of that was vacuous.
    assert!(probe(policy).await.contains("twice before"));

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A slot's own tool use never triggers a slot. This one runs the very
/// function the tool dispatches to, and one call still records exactly one
/// invocation.
#[tokio::test]
async fn a_slots_own_tool_use_does_not_bring_it_back_round_on_itself() {
    let campaign = start_with_slot("keptwhole", REENTERS).await;
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = probe(policy).await;
    assert!(
        result.contains("the slot ran the tool body and got: keptwhole"),
        "{result}"
    );
    assert_eq!(
        result.matches("[after-tool]").count(),
        1,
        "one boundary, one annotation: {result}"
    );

    let status = status(policy).await;
    assert_eq!(
        status.matches("after-tool#").count(),
        1,
        "one call, one invocation: {status}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

// ---------------------------------------------------------------------------
// Shared with the source-reload campaign: a child needs a committed package in
// its checkout to have a layer of its own.
// ---------------------------------------------------------------------------

const CODING_CHILD: &str = "let campaign = \"agent-spec\" :: CampaignLabel\n\
     let group = \"checkout\" :: ForkGroupLabel\n\
     let leaf = [label|editor|]\n\
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

async fn next_child(campaign: &mut TestCampaign) -> exomonad_actor::LocalResidentInstallation {
    let child = campaign
        .next_deployment(
            "child admission",
            Duration::from_secs(180),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(child) => Ok(*child),
                LocalResidentDeployment::Retired { actor, terminal } => {
                    panic!("{actor:?} retired: {terminal:?}")
                }
                other => Err(other),
            },
        )
        .await;
    campaign.authority.install_grant(
        child.actor.identity().into(),
        worktree_grant(child.effective_role.role()),
    );
    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    child
}

// ---------------------------------------------------------------------------
// Nesting: a tools record that carries another tools record as a field.
// ---------------------------------------------------------------------------

/// A workspace tools record that nests `Tidepool.Command.Tools.ShellTools`
/// beside a tool of its own. The `shell` selector names no tool — the inner
/// record's fields are spliced in at that position — so this record declares
/// the whole shell surface first and then `probe`.
const NESTED_TOOLS_MODULE: &str = r#"{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.Tools (MyTools (..), Probe (..), tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

data MyTools mode = MyTools
  { shell :: Shell.ShellTools mode
  , probe :: mode :- Call Probe Text
  }
  deriving (Generic)

tools :: Member Cmd.Commands effects => MyTools (AsServerT (Eff effects))
tools = MyTools
  { shell = Shell.tools
  , probe = tool "Answer one fixed question about a topic." (\_ -> pure "one")
  }
"#;

/// A campaign whose workspace `[haskell] tools` key names the nesting record.
async fn start_nested() -> TestCampaign {
    TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::write(authored.join("config.toml"), CONFIG).unwrap();
            std::fs::write(authored.join("Project/Tools.hs"), NESTED_TOOLS_MODULE).unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_root,
                )
                .unwrap(),
            );
        },
    )
    .await
}

/// Naming an agent's own tools record replaces the shell record rather than
/// adding to it, so the only way to keep `bash` is to carry the shell record
/// inside your own. One nested field declares the whole shell surface at the
/// position it occupies, ahead of the record's own tool, and both halves
/// answer: the nested `bash` reaches the same compiled structured handler and the
/// same shared command owner it does when the shell record is installed
/// alone.
#[tokio::test]
async fn a_nested_shell_record_declares_its_tools_in_place_and_both_halves_answer() {
    use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

    let mut campaign = start_nested().await;
    let policy = campaign.root_installation.policy.clone();

    // (1) The declared surface: the inner record's tools, in the inner
    // record's own field order, at the position the `shell` field occupies —
    // and `shell` itself is not a tool.
    let declared: Vec<&str> = policy.tools().iter().map(|tool| tool.name()).collect();
    let index = |name: &str| {
        declared
            .iter()
            .position(|declared| *declared == name)
            .unwrap_or_else(|| panic!("{name} is not declared: {declared:?}"))
    };
    let bash = index("bash");
    let cancel = index("cancel_command");
    let probe_at = index("probe");
    assert!(bash < cancel, "{declared:?}");
    assert!(cancel < probe_at, "{declared:?}");
    assert!(!declared.contains(&"exec_command"), "{declared:?}");
    assert!(!declared.contains(&"shell"), "{declared:?}");

    // (2) The record's own tool answers.
    assert!(probe(policy.as_ref()).await.contains("one"));

    // (3) The nested structured tool answers, through the shell record's own handler
    // and the campaign's shared command owner. The command backend is the
    // test one every other hosted command test uses, so what is checked here
    // is that a nested `bash` reaches it and returns its output — not what a
    // real shell would print.
    let script = "echo nested-ok";
    let dispatch = {
        let policy = policy.clone();
        tokio::spawn(policy.dispatch_boxed(ToolInvocation {
            name: "bash".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"cmd":script})),
            context: Some(ToolInvocationContext {
                context_call_id: Some("nested-bash".into()),
                thread_id: "nested-thread".into(),
                turn_id: "nested-turn".into(),
                call_id: "nested-bash".into(),
                namespace: None,
            }),
        }))
    };
    let backend = super::command_jobs_tests::TestCommands::completed("nested-ok");
    super::command_jobs_tests::backend_request(&mut campaign)
        .await
        .supply(Ok(backend.clone()));
    let receipt = dispatch.await.unwrap().unwrap();
    assert_eq!(receipt["status"], "committed", "{receipt}");
    let output = receipt["items"][0]["output"].as_str().unwrap();
    assert!(output.contains("nested-ok"), "{receipt}");
    assert_eq!(backend.executions(), 1, "{receipt}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
