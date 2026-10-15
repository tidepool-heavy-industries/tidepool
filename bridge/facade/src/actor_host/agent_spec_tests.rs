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
#![allow(
    clippy::disallowed_methods,
    reason = "test: launches real tmux/process fixtures directly, not through the production launcher"
)]

use std::path::Path;

use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_structured_tool};
use super::*;

/// One tool, whose description and schemas are fixed and whose body is the
/// only thing a test edits. Reloading a body-only edit is the whole point; the
/// declaration must not move with it.
fn tools_module(description: &str, answer: &str) -> String {
    format!(
        r#"{{-# LANGUAGE FlexibleContexts #-}}
{{-# LANGUAGE DeriveAnyClass #-}}
{{-# LANGUAGE DeriveGeneric #-}}
{{-# LANGUAGE OverloadedStrings #-}}
{{-# LANGUAGE TypeOperators #-}}
module Project.Tools (SpecTools (..), Probe (..), probeBody, tools, agentSpec) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract

newtype Probe = Probe {{ topic :: Text }}
  deriving (Generic, FromJSON, JsonSchema)

data SpecTools effects mode = SpecTools
  {{ probe :: mode :- Call Probe Text
  , notebook :: HaskellTools effects mode
  , reloadSpec :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  }}
  deriving (Generic)

-- | The tool's implementation as ordinary source, so a slot can exercise the
-- same function the tool dispatches to without going through the boundary.
probeBody :: Probe -> Eff effects Text
probeBody _ = pure "{answer}"

tools :: (KnownToolEffects effects, AsyncEffects effects) => SpecTools effects (AsServerT (Eff effects))
tools = SpecTools
  {{ probe = presentWith id $ tool "{description}" probeBody
  , notebook = haskellTools
  , reloadSpec = standardReload
  }}

agentSpec :: (KnownToolEffects effects, AsyncEffects effects) => AgentSpec (SpecTools effects) effects
agentSpec = defaultSpec {{ specTools = tools }}
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

agentSpec :: (KnownToolEffects effects, AsyncEffects effects) => AgentSpec (Tools.SpecTools effects) effects
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

/// Like `spec_module`, but the slot reacts to the `haskell` tool name instead
/// of `probe` — the name an authored cell's finished receipt reaches the slot
/// under, since a cell is not a tool call of its own.
fn spec_module_for_haskell_cell(slot: &str) -> String {
    format!(
        r#"{{-# LANGUAGE OverloadedStrings #-}}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Agent.Contract
import qualified Project.Tools as Tools

agentSpec :: (KnownToolEffects effects, AsyncEffects effects) => AgentSpec (Tools.SpecTools effects) effects
agentSpec = defaultSpec
  {{ specTools = Tools.tools
  , afterTool = Just noted
  }}

noted :: ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call /= T.pack "haskell" = pure NoAnnotation
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

/// A slot that runs the tool's own implementation. Its own tool use must not
/// bring it back round on itself.
const REENTERS: &str = "Annotated . (T.pack \"the slot ran the tool body and got: \" <>) <$> Tools.probeBody (Tools.Probe (T.pack \"again\"))";

fn write_spec_config(authored: &Path, modules: &[&str], spec: Option<&str>) {
    crate::exomonad::write_fixture_project_config(authored, "gpt-6-sol", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = modules.iter().map(|module| (*module).into()).collect();
        project.haskell.spec = spec.map(str::to_owned);
    });
}

fn write_workspace(workspace: &Path, description: &str, answer: &str) {
    let authored = workspace.join(".exomonad");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    write_spec_config(
        &authored,
        &["Project.Tools"],
        Some("Project.Tools.agentSpec"),
    );
    std::fs::write(
        authored.join("Project/Tools.hs"),
        tools_module(description, answer),
    )
    .unwrap();
}

const DESCRIPTION: &str = "Answer one fixed question about a topic.";

async fn start(description: &str, answer: &str) -> TestCampaign {
    TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, description, answer);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
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
        |admission| admission,
        move |config| {
            write_workspace(&config.workspace, DESCRIPTION, &answer);
            let authored = config.workspace.join(".exomonad");
            write_spec_config(
                &authored,
                &["Project.Tools", "AgentSpec"],
                Some("AgentSpec.agentSpec"),
            );
            std::fs::write(authored.join("AgentSpec.hs"), spec_module(&slot)).unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await
}

/// A root whose spec is named by rule two, carrying a slot that reacts to the
/// `haskell` tool name — the name an authored cell's receipt reaches it under.
async fn start_with_cell_slot(slot: &str) -> TestCampaign {
    let slot = slot.to_owned();
    TestCampaign::start_with_config(
        |admission| admission,
        move |config| {
            write_workspace(&config.workspace, DESCRIPTION, "keptwhole");
            let authored = config.workspace.join(".exomonad");
            write_spec_config(
                &authored,
                &["Project.Tools", "AgentSpec"],
                Some("AgentSpec.agentSpec"),
            );
            std::fs::write(
                authored.join("AgentSpec.hs"),
                spec_module_for_haskell_cell(&slot),
            )
            .unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await
}

async fn probe(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> String {
    let result =
        dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": "anything"})).await;
    result.to_string()
}

/// Ask this actor to rebuild its own spec, and read the receipt.
async fn reload(policy: &dyn exomonad_actor::ResidentToolEndpoint) -> serde_json::Value {
    dispatch_structured_tool(policy, "reload_spec", serde_json::json!({})).await
}

fn assert_reload_outcome(receipt: &serde_json::Value, tag: &str) {
    assert_eq!(receipt["status"], "committed", "{receipt}");
    assert_eq!(
        receipt["items"][0]["value"]["Right"]["tag"], tag,
        "{receipt}"
    );
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
        campaign
            .run_scenario(|campaign| {
                Box::pin(async move {
                    report(
                        campaign,
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
                        campaign,
                        "specUnchangedReload",
                        Some(round),
                        None,
                        before,
                        started,
                    );
                    assert_reload_outcome(&receipt, "ReloadUnchanged");

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
                        campaign,
                        "specEditedReload",
                        Some(round),
                        None,
                        before,
                        started,
                    );
                    assert_reload_outcome(&receipt, "ReloadPublished");
                    let changed = probe(policy).await;
                    assert!(changed.contains("measurement-two"), "{changed}");
                    assert!(
                        changed.contains("asked about this topic twice before"),
                        "{changed}"
                    );
                })
            })
            .await;
    }
}

/// The whole point: an edited body, the same declared surface, and the NEXT
/// call runs the new code — inside one session, with no new incarnation.
#[tokio::test]
async fn a_rebuilt_record_with_the_same_surface_swaps_and_later_calls_run_new_code() {
    let campaign = start(DESCRIPTION, "one").await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
                assert_reload_outcome(&receipt, "ReloadPublished");

                let after = probe(policy).await;
                assert!(after.contains("two"), "{after}");
            })
        })
        .await;
}

#[tokio::test]
async fn removing_only_the_slot_keeps_transitive_tool_implementation_linkable() {
    let campaign = TestCampaign::start_with_conversation(
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            // The authored spec shadows a pinned shared package on a second
            // source root. The shared tool record still uses Project.Shell
            // after the root spec drops its Watchdog import.
            let package = super::test_campaign::declared_workspace_package();
            let shared = package.join(".exomonad");
            fn copy_haskell_sources(source: &Path, destination: &Path) {
                std::fs::create_dir_all(destination).unwrap();
                for entry in std::fs::read_dir(source).unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    let target = destination.join(entry.file_name());
                    if entry.file_type().unwrap().is_dir() {
                        copy_haskell_sources(&path, &target);
                    } else if path.extension().is_some_and(|extension| extension == "hs") {
                        std::fs::copy(path, target).unwrap();
                    }
                }
            }
            for directory in ["Project", "Jev", "Exomonad/Contrib"] {
                copy_haskell_sources(
                    &shared.join(directory),
                    &authored.join("workspace").join(directory),
                );
            }
            crate::exomonad::write_fixture_project_config(&authored, "gpt-6-sol", |project| {
                project.haskell.source_roots = vec![".".into(), "workspace".into()];
                project.haskell.modules = vec![
                    "Exomonad.Contrib.Types".into(),
                    "Exomonad.Contrib.Actors".into(),
                    "Project.Work".into(),
                    "Exomonad.Contrib.Routing".into(),
                    "Project.Observe".into(),
                    "Project.Shell".into(),
                    "Project.Sift".into(),
                    "Project.Lookup".into(),
                    "Project.Reflex".into(),
                    "Project.Evidence".into(),
                    "Project.Investigate".into(),
                    "Exomonad.Contrib.Merge".into(),
                    "Project.ReviewPolicy".into(),
                    "Project.Search".into(),
                    "Project.History".into(),
                    "Project.Service".into(),
                    "Project.Repository".into(),
                    "Exomonad.Contrib.CheckResults".into(),
                    "Exomonad.Contrib.PrepareContinue".into(),
                    "Exomonad.Contrib.RetainedEvidence".into(),
                    "Project.AssumptionWatch".into(),
                    "Project.ParallelInvestigate".into(),
                    "Project.SlowCommandWatch".into(),
                    "Project.Interview".into(),
                    "Exomonad.Contrib.CheckPlan".into(),
                    "Project.WorkflowExamples".into(),
                    "Exomonad.Contrib.ReviewFlow".into(),
                    "Project.BaselineIncorporation".into(),
                    "Project.WorkflowReminders".into(),
                    "Project.WorkflowReminderExamples".into(),
                ];
                project.haskell.spec = Some("AgentSpec.agentSpec".into());
                project
                    .haskell
                    .flake_sources
                    .insert("jev-dsl".into(), vec!["core".into()]);
            });
            std::fs::write(
                authored.join("AgentSpec.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/fixtures/spec_reload_with_slot.hs",
                ),
            )
            .unwrap();
            for name in ["flake.nix", "flake.lock"] {
                std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
            }
            commit(&config.workspace, "capture reload fixture inputs");
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
        Some(Arc::new(|_, _| {
            Box::pin(async { Err(exomonad_actor::ConversationUnavailable::Unbound) })
        })),
    )
    .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let workspace = campaign._repository.path().to_path_buf();
                let policy = campaign.root_installation.policy.clone();
                let policy = policy.as_ref();

                assert!(policy.tools().iter().any(|tool| tool.name() == "bash"));

                std::fs::write(
                    workspace.join(".exomonad/AgentSpec.hs"),
                    tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/fixtures/spec_reload_without_slot.hs",
                    ),
                )
                .unwrap();
                let receipt = reload(policy).await;
                assert_reload_outcome(&receipt, "ReloadPublished");
                assert!(policy.tools().iter().any(|tool| tool.name() == "bash"));
            })
        })
        .await;
}

#[tokio::test]
async fn coherent_reload_installs_handlers_and_preserves_an_issued_request() {
    let log = super::call_timing_tests::CapturedLog(Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt().json().with_writer(log.clone())
        .with_env_filter("warn,exomonad_actor::toolset_preparation=info,exomonad_actor::call_timing=info,tidepool_extract_cmd::endpoint=info").finish();
    // The actor and blocking compiler tasks outlive the dispatch future. Each
    // native test has its own process, so observe their shared global writer.
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber in isolated test");
    let campaign = start(DESCRIPTION, "one").await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.as_ref();
                let old_request = policy.snapshot_for_request().expect("installed old spec");
                assert!(probe(old_request.as_ref()).await.contains("one"));
                std::fs::write(
                    campaign
                        ._repository
                        .path()
                        .join(".exomonad/Project/Tools.hs"),
                    tools_module(DESCRIPTION, "two"),
                )
                .unwrap();
                log.0.lock().unwrap().clear();
                let source_reload = reload(policy).await;
                assert_reload_outcome(&source_reload, "ReloadPublished");
                let captured = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
                let rows = captured
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .collect::<Vec<_>>();
                let waits = rows
                    .iter()
                    .filter(|row| row["fields"]["message"] == "toolset preparation wait")
                    .collect::<Vec<_>>();
                assert_eq!(waits.len(), 1, "{captured}");
                let preparation = &waits[0]["fields"]["preparation"];
                assert_eq!(waits[0]["fields"]["disposition"], "new");
                assert_eq!(waits[0]["fields"]["outcome"], "succeeded");
                for phase in ["service", "producer"] {
                    let producers = rows
                        .iter()
                        .filter(|row| {
                            row["fields"]["message"] == "toolset preparation producer"
                                && row["fields"]["phase"] == phase
                                && &row["fields"]["preparation"] == preparation
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(producers.len(), 1, "{captured}");
                    assert_eq!(producers[0]["fields"]["outcome"], "succeeded");
                }
                let summary = rows
                    .iter()
                    .find(|row| row["fields"]["message"] == "call timing")
                    .expect("reload call timing");
                assert_eq!(summary["fields"]["preparation_wait_count"], 1);
                assert_eq!(
                    summary["fields"]["execution"],
                    waits[0]["fields"]["execution"]
                );
                let admissions = rows
                    .iter()
                    .filter(|row| {
                        row["fields"]["phase"] == "compiler_transaction_admission"
                            && row["spans"].as_array().is_some_and(|spans| {
                                spans.iter().any(|span| {
                                    span["name"] == "toolset_preparation"
                                        && &span["preparation"] == preparation
                                })
                            })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(admissions.len(), 1, "{captured}");
                for field in ["producer", "endpoint"] {
                    let identity = admissions[0]["fields"][field]
                        .as_str()
                        .expect("the admitted compiler transaction retains its exact identity");
                    assert_eq!(identity.len(), 64, "{captured}");
                    assert!(
                        identity.bytes().all(|byte| byte.is_ascii_hexdigit()),
                        "{captured}"
                    );
                }
                assert!(
                    probe(policy).await.contains("two"),
                    "coherent reload installs the new handler"
                );
                assert!(
                    probe(old_request.as_ref()).await.contains("one"),
                    "accepted old call keeps its original handler"
                );
            })
        })
        .await;
}

/// Surface changes require explicit consent. A compatible-only reload retains
/// the old source and handlers and reports the changed declaration.
#[tokio::test]
async fn a_changed_description_is_refused_with_the_difference_and_the_old_record_answers() {
    let campaign = start(DESCRIPTION, "one").await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let workspace = campaign._repository.path().to_path_buf();
                let policy = campaign.root_installation.policy.clone();
                let policy = policy.as_ref();

                assert!(probe(policy).await.contains("one"));

                let source_layer = crate::exomonad::source::SourceLayer::new(
                    &campaign.config.run_directory.path(),
                );
                let before_source = source_layer.read_active().unwrap();

                std::fs::write(
                    workspace.join(".exomonad/Project/Tools.hs"),
                    tools_module("Answer one fixed question, and explain it.", "two"),
                )
                .unwrap();
                let receipt = reload(policy).await;
                assert_reload_outcome(&receipt, "ReloadRefused");
                let refusal = &receipt["items"][0]["value"]["Right"]["contents"][2];
                assert_eq!(refusal["refusalCategory"], "SurfaceIncompatible");
                assert!(refusal["refusalSurface"]["surfaceChanged"].as_array().unwrap().contains(&serde_json::json!("probe")));

                // The previously installed record is still the one serving calls.
                let after = probe(policy).await;
                assert!(after.contains("one"), "{after}");

                assert_eq!(
                    source_layer.read_active().unwrap(),
                    before_source,
                    "refusal preserves the published source as well as handlers"
                );
                let cell = dispatch_haskell_script(
                    policy,
                    "answer <- Project.Tools.probeBody (Project.Tools.Probe \"cell\")\nif answer == \"one\" then pure () else error \"the retained Project.Tools source changed\"",
                )
                .await;
                assert_eq!(cell["status"], "committed", "{cell}");
            })
        })
        .await;
}

/// A spec that does not typecheck fails its own reload. The edited file stays
/// on disk exactly as the model wrote it — repairing it is the next thing the
/// model does — and the previous spec is still serving calls.
#[tokio::test]
async fn a_spec_that_does_not_typecheck_leaves_the_old_one_active_and_the_file_on_disk() {
    let campaign = start(DESCRIPTION, "one").await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let workspace = campaign._repository.path().to_path_buf();
                let policy = campaign.root_installation.policy.clone();
                let policy = policy.as_ref();

                assert!(probe(policy).await.contains("one"));

                let source_layer = crate::exomonad::source::SourceLayer::new(
                    &campaign.config.run_directory.path(),
                );
                let before_source = source_layer.read_active().unwrap();

                let broken = tools_module(DESCRIPTION, "one")
                    .replace("pure \"one\"", "pure undefinedByThisSpecReload");
                assert!(broken.contains("undefinedByThisSpecReload"), "{broken}");
                std::fs::write(workspace.join(".exomonad/Project/Tools.hs"), &broken).unwrap();
                let receipt = reload(policy).await;
                assert_reload_outcome(&receipt, "ReloadRejected");
                assert!(receipt["items"][0]["value"]["Right"]["contents"][2].as_str().unwrap().contains("undefinedByThisSpecReload"));

                let after = probe(policy).await;
                assert!(after.contains("one"), "{after}");
                assert_eq!(
                    std::fs::read_to_string(workspace.join(".exomonad/Project/Tools.hs")).unwrap(),
                    broken,
                    "the edited file is exactly as it was written"
                );

                assert_eq!(
                    source_layer.read_active().unwrap(),
                    before_source,
                    "refusal preserves the published source as well as handlers"
                );
                let cell = dispatch_haskell_script(
                    policy,
                    "answer <- Project.Tools.probeBody (Project.Tools.Probe \"cell\")\nif answer == \"one\" then pure () else error \"the retained Project.Tools source changed\"",
                )
                .await;
                assert_eq!(cell["status"], "committed", "{cell}");
            })
        })
        .await;
}

#[tokio::test]
async fn an_installer_execution_failure_keeps_source_cells_and_old_handlers() {
    let campaign = start(DESCRIPTION, "one").await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.as_ref();
                let source_layer = crate::exomonad::source::SourceLayer::new(
                    &campaign.config.run_directory.path(),
                );
                let before_source = source_layer.read_active().unwrap();
                let broken = tools_module(DESCRIPTION, "two").replace(
                    "agentSpec = defaultSpec { specTools = tools }",
                    "agentSpec = error \"installer refused\"",
                );
                assert!(broken.contains("installer refused"));
                std::fs::write(
                    campaign
                        ._repository
                        .path()
                        .join(".exomonad/Project/Tools.hs"),
                    &broken,
                )
                .unwrap();
                let receipt = reload(policy).await;
                assert_eq!(receipt["status"], "committed", "{receipt}");
                assert_eq!(receipt["items"][0]["value"]["Left"]["tag"], "ReloadUnreadable", "{receipt}");
                assert!(receipt["items"][0]["value"]["Left"]["detail"].as_str().unwrap().contains("installer refused"), "{receipt}");
                assert_eq!(source_layer.read_active().unwrap(), before_source);
                assert!(probe(policy).await.contains("one"));
                let cell = dispatch_haskell_script(
                    policy,
                    "answer <- Project.Tools.probeBody (Project.Tools.Probe \"cell\")\nif answer == \"one\" then pure () else error \"the retained Project.Tools source changed\"",
                )
                .await;
                assert_eq!(cell["status"], "committed", "{cell}");
                assert_eq!(
                    std::fs::read_to_string(
                        campaign
                            ._repository
                            .path()
                            .join(".exomonad/Project/Tools.hs")
                    )
                    .unwrap(),
                    broken
                );
            })
        })
        .await;
}

/// A workspace with no conventional or configured spec installs the empty
/// default, even when it imports a module exporting tools.
#[tokio::test]
async fn a_workspace_without_a_spec_installs_the_empty_default() {
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            write_workspace(&config.workspace, DESCRIPTION, "one");
            crate::exomonad::write_fixture_project_config(
                &config.workspace.join(".exomonad"),
                "gpt-6-sol",
                |project| {
                    project.haskell.source_roots = vec![".".into()];
                    project.haskell.modules = vec!["Project.Tools".into()];
                },
            );
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let policy = policy.as_ref();

                let status = status(policy).await;
                assert!(status.contains("built-in default"), "{status}");
                assert!(status.contains("slots=[]"), "{status}");
                assert!(!policy.tools().iter().any(|tool| tool.name() == "probe"));
                assert!(!policy.tools().iter().any(|tool| tool.name() == "bash"));

                // The Haskell workbench is untouched by any of this.
                let cell = dispatch_haskell_script(policy, "1 + 1 :: Int").await;
                assert_eq!(cell["items"][0]["output"], "2", "{cell}");
            })
        })
        .await;
}

/// A child checkout can contain `AgentSpec.hs`, but the installed spec comes
/// from the run's current graph for both root and child.
#[tokio::test]
async fn the_run_spec_module_is_installed_for_a_child_with_a_checkout() {
    let campaign = TestCampaign::start_with_config(
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
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();

                let mut launch = {
                    let root = root.clone();
                    tokio::spawn(async move {
                        super::test_campaign::dispatch_haskell_script_response(
                            root.as_ref(),
                            CODING_CHILD,
                        )
                        .await
                    })
                };
                let child = next_child(campaign, &mut launch).await;
                let response = launch
                    .await
                    .expect("child launch task joins")
                    .expect("the exact child launch operation succeeds")
                    .into_json()
                    .unwrap();
                assert_eq!(response["status"], "committed", "{response}");

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
                assert!(
                    child.toolset_acquisition().is_none(),
                    "the child installs its explicit live spec"
                );
                assert_eq!(
                    child
                        .policy
                        .tools()
                        .iter()
                        .map(|tool| tool.name())
                        .collect::<Vec<_>>(),
                    ["status", "probe", "haskell", "haskell_sync", "reload_spec"],
                );
                // One compile, two products: the declared surface, and the slot the same
                // spec filled, retained beside the tools it sits with.
                assert!(child_status.contains("slots=[afterTool]"), "{child_status}");

                // The root resolves the run module; the child retains the explicitly supplied spec.
                let root_status = status(root.as_ref()).await;
                assert!(root_status.contains("run module"), "{root_status}");

                // The run spec serves the child's calls and after-tool slot.
                let answered = probe(child.policy.as_ref()).await;
                assert!(answered.contains("one"), "{answered}");
                assert!(
                    answered.contains("asked about this topic twice before"),
                    "{answered}"
                );
            })
        })
        .await;
}

/// A child editing historical checkout tooling cannot reload the run spec.
#[tokio::test]
async fn a_child_checkout_spec_edit_cannot_replace_the_run_spec() {
    let campaign = TestCampaign::start_with_config(
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
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let root = campaign.root_installation.policy.clone();

                let mut launch = {
                    let root = root.clone();
                    tokio::spawn(async move {
                        super::test_campaign::dispatch_haskell_script_response(
                            root.as_ref(),
                            CODING_CHILD,
                        )
                        .await
                    })
                };
                let child = next_child(campaign, &mut launch).await;
                let response = launch
                    .await
                    .expect("child launch task joins")
                    .expect("the exact child launch operation succeeds")
                    .into_json()
                    .unwrap();
                assert_eq!(response["status"], "committed", "{response}");

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
                assert_eq!(receipt["status"], "committed", "{receipt}");
                assert_eq!(
                    receipt["items"][0]["value"]["Left"]["tag"], "ReloadUnavailable",
                    "{receipt}"
                );

                assert!(probe(child.policy.as_ref()).await.contains("one"));
                assert!(probe(root.as_ref()).await.contains("one"));
            })
        })
        .await;
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
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
            })
        })
        .await;
}

/// A pruned view says it is a selection, and the whole result stays
/// addressable: the handle the slot was shown is an ordinary binding a later
/// cell evaluates.
#[tokio::test]
async fn a_pruned_result_says_it_is_a_selection_and_its_handle_answers_the_whole() {
    let campaign = start_with_slot("keptwhole", PRUNES).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let policy = campaign.root_installation.policy.clone();
                let cell = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(
                            policy.as_ref(),
                            "display (toolResult1 == \"keptwhole\")",
                        ),
                    )
                    .await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&cell),
                    "True",
                    "{cell}"
                );
            })
        })
        .await;
}

/// Abstention is silent. The model never asked for a judgement on this result,
/// so the original is delivered exactly as the tool produced it and the reason
/// lives where a model can go and look for it.
#[tokio::test]
async fn an_abstention_delivers_the_original_and_appears_only_in_the_receipt() {
    let campaign = start_with_slot("keptwhole", ABSTAINS).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
            })
        })
        .await;
}

#[tokio::test]
async fn no_annotation_preserves_the_original_successful_receipt() {
    let campaign = start_with_slot("keptwhole", "pure NoAnnotation").await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let result = dispatch_structured_tool(
                    campaign.root_installation.policy.as_ref(),
                    "probe",
                    serde_json::json!({"topic": "anything"}),
                )
                .await;
                assert_eq!(result["status"], "committed", "{result}");
                assert_eq!(result["items"][0]["output"], "keptwhole", "{result}");
                assert_eq!(result["items"][0]["value"], "keptwhole", "{result}");
            })
        })
        .await;
}

/// A failed presentation never changes the successful tool receipt. Its
/// diagnostic remains available through the actor's invocation log.
#[tokio::test]
async fn a_slot_that_fails_preserves_tool_success_and_logs_only() {
    let campaign = start_with_slot("keptwhole", FAILS).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.as_ref();
                for ordinal in 1..=2 {
                    let result = probe(policy).await;
                    let response: serde_json::Value = serde_json::from_str(&result).unwrap();
                    assert_eq!(response["status"], "committed", "{response}");
                    assert_eq!(response["items"][0]["output"], "keptwhole", "{response}");
                    let status = status(policy).await;
                    assert!(
                        status.contains(&format!("after-tool#{ordinal}")),
                        "{status}"
                    );
                    assert!(status.contains("failed:"), "{status}");
                }
                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let policy = campaign.root_installation.policy.clone();
                let cell = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(policy.as_ref(), "display (1 + 1 :: Int)"),
                    )
                    .await;
                assert_eq!(
                    super::test_campaign::committed_display_text(&cell),
                    "2",
                    "{cell}"
                );
            })
        })
        .await;
}

/// The root uses the run graph, and its spec is still the first
/// `AgentSpec.hs` its cells would resolve: no configuration key names it.
#[tokio::test]
async fn the_root_finds_its_spec_by_convention_with_no_key_naming_it() {
    use super::hosted_test_context::HostedTestRuntime;
    use super::test_campaign::{
        hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    };

    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        write_workspace(&config.workspace, DESCRIPTION, "keptwhole");
        // No configuration key or module import selects the conventional spec.
        write_spec_config(
            &config.workspace.join(".exomonad"),
            &["Project.Tools"],
            None,
        );
        std::fs::write(
            config.workspace.join(".exomonad/AgentSpec.hs"),
            spec_module(ANNOTATES),
        )
        .unwrap();
        super::test_campaign::commit_workspace(&config.workspace);
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(
                &config.workspace,
                &config.run_directory.path(),
            )
            .unwrap(),
        );
    })
    .await
    .expect("the conventional spec installs through production root readiness");
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Exercise the conventional agent spec and its after-tool slot.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            next_hosted_script_round(&mut requests, &mut pending, &root)
                .await
                .function(
                    "conventional-probe",
                    "probe",
                    serde_json::json!({"topic": "anything"}),
                );
            let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let result = round.settled_text("conventional-probe");
            assert!(result.contains("keptwhole"), "{result}");
            assert!(result.contains("twice before"), "{result}");
            round.function(
                "conventional-status",
                "status",
                serde_json::json!({"view": "detailed"}),
            );
            let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let status = round.settled_text("conventional-status");
            assert!(status.contains("rule=run module"), "{status}");
            round.finish();
        })
    })
    .await;
}

/// The real provider sees a synchronous final result only after presentation.
#[tokio::test]
async fn provider_inference_waits_for_after_tool_presentation() {
    provider_delivery_after_presentation(false).await;
}

/// Asynchronous acknowledgement is independent of presentation; the eventual
/// completed output carries the hook result, exactly once.
#[tokio::test]
async fn async_acknowledgement_precedes_hook_and_final_delivery_waits() {
    provider_delivery_after_presentation(true).await;
}

async fn provider_delivery_after_presentation(asynchronous: bool) {
    use super::hosted_test_context::HostedTestRuntime;
    use super::test_campaign::{
        hosted_script_provider, hosted_test_settings, next_hosted_script_round,
    };
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured_with_command_resources(
        &settings,
        &provider,
        exomonad_node::command_resources::CommandResourcePolicy {
            general_bytes: 2 * exomonad_node::command_resources::GIB,
            protected_bytes: 0,
            swap_max_bytes: 0,
            ..Default::default()
        },
        |config| {
            super::test_campaign::configure_command_path(config);
            write_workspace(&config.workspace, DESCRIPTION, "keptwhole");
            write_spec_config(
                &config.workspace.join(".exomonad"),
                &["Project.Tools", "AgentSpec"],
                Some("AgentSpec.agentSpec"),
            );
            std::fs::write(
                config.workspace.join(".exomonad/AgentSpec.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/after_tool_delivery_spec.hs",
                ),
            )
            .unwrap();
            super::test_campaign::commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
    )
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                host.context.config.workspace.join("after-tool-release"),
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .unwrap();
            host.input("Exercise awaited after-tool presentation.")
                .await
                .unwrap();
            let root = harness::model::AgentPath("/root".into());
            let mut pending = std::collections::VecDeque::new();
            let first = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let operation = first.operation("delivery-probe");
            if asynchronous {
                first.async_call("delivery-probe", "display (\"keptwhole\" :: Text)");
            } else {
                first.function(
                    "delivery-probe",
                    "probe",
                    serde_json::json!({"topic": "anything"}),
                );
            }
            let acknowledgement = if asynchronous {
                let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
                assert!(
                    host.runtime
                        .store()
                        .replay_tool_output_operation(&operation)
                        .unwrap()
                        .is_none(),
                    "acknowledgement does not settle the original operation before its hook"
                );
                Some(round)
            } else {
                None
            };
            host.while_operation_succeeds(
                &operation,
                tokio::time::timeout(Duration::from_secs(120), async {
                    while !host
                        .context
                        .config
                        .workspace
                        .join("after-tool-ready")
                        .exists()
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }),
            )
            .await
            .expect("actual hook command remains live")
            .expect("hook command reaches delivery gate");
            assert!(
                matches!(
                    requests.try_recv(),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                ),
                "no final-result inference before hook settlement"
            );
            let release = host.context.config.workspace.join("after-tool-release");
            tokio::task::spawn_blocking(move || std::fs::write(release, "release\n"))
                .await
                .unwrap()
                .unwrap();
            let scheduler = host.runtime.scheduler();
            let completed = host
                .while_host_running(scheduler.wait(&operation))
                .await
                .expect("the host remains live through the original operation")
                .expect("the original operation remains owned");
            assert!(
                matches!(&completed, harness::turn::JobOutput::Completed(Ok(_))),
                "the exact operation must succeed after its hook: {completed:?}"
            );
            if let Some(round) = acknowledgement {
                // The original operation is already complete. One event-based
                // yield lets the Engine publish its retained output; no timed polling.
                round.function("delivery-yield", "yield", serde_json::json!({"until":null}));
            }
            let round = host
                .while_operation_succeeds(
                    &operation,
                    next_hosted_script_round(&mut requests, &mut pending, &root),
                )
                .await
                .unwrap();
            let store = host.runtime.store();
            let outcome = store
                .replay_tool_output_operation(&operation)
                .unwrap()
                .expect("the actual next inference follows durable output settlement");
            assert_eq!(outcome.terminal, harness::store::TerminalOutcome::Success);
            let retained = store
                .replay_output_operation(&operation)
                .unwrap()
                .expect("the original operation retains its model output");
            assert_eq!(
                round
                    .request
                    .input
                    .iter()
                    .filter(|item| *item == &retained)
                    .count(),
                1,
                "the next request contains the exact original output once"
            );
            let response: serde_json::Value =
                serde_json::from_str(retained.0["output"].as_str().unwrap()).unwrap();
            let presented = response["items"][0]["output"].as_str().unwrap();
            assert!(presented.contains("keptwhole"), "{response}");
            assert_eq!(
                presented.matches("presentation-finished").count(),
                1,
                "{response}"
            );
            round.function(
                "delivery-status",
                "status",
                serde_json::json!({"view":"detailed"}),
            );
            let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
            let status = round.settled_text("delivery-status");
            assert_eq!(status.matches("after-tool#").count(), 1, "{status}");
            round.finish();
        })
    })
    .await;
}

/// The status control bypasses authored presentation. An ordinary cell reaches
/// the slot under its authored tool name, even when the slot returns no annotation.
#[tokio::test]
async fn status_bypasses_presentation_and_cells_reach_the_slot_unmatched() {
    let campaign = start_with_slot("keptwhole", ANNOTATES).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let policy = policy.as_ref();

                let status_before_cell = status(policy).await;
                assert!(
                    !status_before_cell.contains("[after-tool]"),
                    "{status_before_cell}"
                );

                let store =
                    super::display_output::open_run_store(campaign.session_root.path()).unwrap();
                let cell = campaign
                    .drive_actor_output(
                        &store,
                        dispatch_haskell_script(policy, "display (1 + 1 :: Int)"),
                    )
                    .await;
                assert_eq!(cell["status"], "committed", "{cell}");
                let displays = cell["items"][0]["operations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|operation| operation.get("display"))
                    .collect::<Vec<_>>();
                assert_eq!(displays.len(), 1, "{cell}");
                assert_eq!(displays[0]["text"], "2", "{cell}");

                // The cell's receipt still reached the slot — as `haskell`, which this
                // spec's `noted` does not match — so the log carries a silent invocation.
                let status_after_cell = status(policy).await;
                assert!(
                    status_after_cell.contains("after-tool#1 haskell"),
                    "{status_after_cell}"
                );
                assert!(status_after_cell.contains("silent"), "{status_after_cell}");

                // And the slot is installed and working, so none of that was vacuous.
                assert!(probe(policy).await.contains("twice before"));
            })
        })
        .await;
}

/// A haskell cell's finished receipt reaches the after-tool slot exactly once,
/// under the tool name `haskell` — the same slot, budget, and abstention
/// semantics a hosted tool call gets, not a second hook path.
#[tokio::test]
async fn a_haskell_cells_receipt_reaches_the_after_tool_slot_as_haskell() {
    let campaign = start_with_cell_slot(ANNOTATES).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let policy = policy.as_ref();

                let cell = dispatch_haskell_script(policy, "1 + 1 :: Int").await;
                let output = cell["items"][0]["output"].as_str().unwrap_or_default();
                assert!(output.contains('2'), "{cell}");
                assert!(output.contains("[after-tool]"), "{cell}");
                assert!(
                    output.contains("asked about this topic twice before"),
                    "{cell}"
                );

                let status = status(policy).await;
                assert!(status.contains("after-tool#1 haskell"), "{status}");
                assert!(status.contains("annotated"), "{status}");
            })
        })
        .await;
}

/// Abstention on a cell is silent, exactly as it is for a hosted tool call:
/// the receipt is delivered untouched and the reason is recorded only in the
/// actor's own inspectable log.
#[tokio::test]
async fn an_abstained_cell_receipt_is_delivered_untouched() {
    let campaign = start_with_cell_slot(ABSTAINS).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let policy = campaign.root_installation.policy.clone();
                let policy = policy.as_ref();

                let cell = dispatch_haskell_script(policy, "1 + 1 :: Int").await;
                assert_eq!(cell["items"][0]["output"], "2", "{cell}");

                let status = status(policy).await;
                assert!(status.contains("after-tool#1 haskell"), "{status}");
                assert!(
                    status.contains("abstained: the result is already minimal"),
                    "{status}"
                );
            })
        })
        .await;
}

/// A slot's own tool use never triggers a slot. This one runs the very
/// function the tool dispatches to, and one call still records exactly one
/// invocation.
#[tokio::test]
async fn a_slots_own_tool_use_does_not_bring_it_back_round_on_itself() {
    let campaign = start_with_slot("keptwhole", REENTERS).await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
            })
        })
        .await;
}

// ---------------------------------------------------------------------------
// Shared with the source-reload campaign: a child needs a committed package in
// its checkout to have a layer of its own.
// ---------------------------------------------------------------------------

const CODING_CHILD: &str = include_str!("fixtures/agent_spec_coding_child.hs");

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

async fn next_child(
    campaign: &mut TestCampaign,
    launch: &mut tokio::task::JoinHandle<
        Result<exomonad_actor::ResidentToolResponse, exomonad_actor::ResidentToolError>,
    >,
) -> exomonad_actor::LocalResidentInstallation {
    let child = tokio::select! {
        biased;
        child = campaign
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
        ) => child,
        result = launch => panic!("the exact child launch settled before admission: {result:?}"),
    };
    campaign.authority.install_grant(
        child.actor.identity().into(),
        ActorWorktreeGrant::Bound {
            enumerate: false,
            allocate: true,
        },
    );
    campaign.acknowledge_native_spawn(&child);
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
module Project.Tools (MyTools (..), Probe (..), tools, agentSpec) where

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
  , probe = presentWith id $ tool "Answer one fixed question about a topic." (\_ -> pure "one")
  }

agentSpec :: Member Cmd.Commands effects => AgentSpec MyTools effects
agentSpec = defaultSpec { specTools = tools }
"#;

/// A campaign whose configured spec installs the nesting record.
async fn start_nested() -> TestCampaign {
    TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            write_spec_config(
                &authored,
                &["Project.Tools"],
                Some("Project.Tools.agentSpec"),
            );
            std::fs::write(authored.join("Project/Tools.hs"), NESTED_TOOLS_MODULE).unwrap();
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
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

    let campaign = start_nested().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
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
                    tokio::spawn(policy.dispatch_json_boxed(ToolInvocation {
                        name: "bash".into(),
                        arguments: ToolArguments::Structured(serde_json::json!({"cmd":script})),
                        context: Some(ToolInvocationContext::external(
                            "nested-thread".into(),
                            "nested-turn".into(),
                            "nested-bash".into(),
                            Some("nested-bash".into()),
                            None,
                        )),
                    }))
                };
                let backend = super::command_test_support::TestCommands::completed("nested-ok");
                super::command_jobs_tests::backend_request(campaign)
                    .await
                    .supply(Ok(backend.clone()));
                let receipt = dispatch.await.unwrap().unwrap();
                assert_eq!(receipt["status"], "committed", "{receipt}");
                let output = receipt["items"][0]["output"].as_str().unwrap();
                assert!(output.contains("nested-ok"), "{receipt}");
                assert_eq!(backend.executions(), 1, "{receipt}");
            })
        })
        .await;
}
