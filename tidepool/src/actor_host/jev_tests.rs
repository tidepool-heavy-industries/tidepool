use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_structured_tool};
use super::*;
use tidepool_actor::{JevBackend, JevCallFailure};

/// Answers every request with one choice answer and records the requests.
struct FakeJev {
    requests: Mutex<Vec<serde_json::Value>>,
    answer: Result<String, JevCallFailure>,
}

impl JevBackend for FakeJev {
    fn ask(
        &self,
        request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, JevCallFailure>> {
        self.requests
            .lock()
            .push(serde_json::from_str(&request).expect("request is JSON"));
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

/// The Jev surface is pinned source, not Tidepool library: a run reaches it
/// through a workspace whose `flake.nix` names the jev-dsl revision and whose
/// own `Jev/Operators.hs` fixes that library's JSON type to Tidepool's. These
/// tests select the package this repository ships, so what they compile is
/// what a project gets — including the pin.
pub(super) fn pinned_jev_workspace(config: &mut ActorHostConfig) {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/shoal-workspace")
        .canonicalize()
        .expect("the Shoal workspace package this repository ships");
    let authored = config.workspace.join(".shoal");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("config.toml"),
        format!(
            "[defaults]\nmodel = 'test-model'\n\n[haskell]\nsource_roots = ['{}']\n\n[haskell.flake_sources]\njev-dsl = ['core']\n",
            package.join(".shoal").display()
        ),
    )
    .unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    super::test_campaign::commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve the pinned Haskell source"),
    );
}

async fn campaign_with(backend: Arc<FakeJev>) -> TestCampaign {
    TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = Some(backend);
            pinned_jev_workspace(config);
        },
    )
    .await
}

/// No `LANGUAGE` pragma: `OverloadedLabels` is in the cell dialect
/// (`session::dialect::EVAL_PRAGMAS`), so `#not_here` needs no ceremony.
const CELL: &str = r#"answer <- J.ask1 (J.rawState (String "retry loop in fetch; timeout branch at line 12"))
  (J.choice "Which line begins the retry-timeout branch?"
     (J.alt #not_here "The branch is not in this file" (0 :: Int)
        J..| J.many #line (\(k, _, _) -> k) (\(_, w, _) -> w)
               [("line-4", "if attempts > 3", 4), ("line-12", "if elapsed > timeout", 12)]))
either (const 0) (\a -> J.handle a (#not_here id J..| #line (\_ (_, _, n) -> n))) answer"#;

#[tokio::test]
async fn jev_choice_round_trips_through_the_host_backend() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Ok(serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {"value": {
                "type": "choice",
                "choice": "line-12",
                "probabilities": {"not_here": 0.02, "line-4": 0.08, "line-12": 0.9},
                "confidence": 0.9
            }},
            "usage": {}
        })
        .to_string()),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(campaign.root_installation.policy.as_ref(), CELL).await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "12", "{result}");
    let requests = backend.requests.lock();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["model"], "jev-latest", "{request}");
    assert_eq!(request["questions"]["value"]["type"], "choice", "{request}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn jev_call_failure_is_a_typed_left() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Err(JevCallFailure::Unconfigured),
    });
    let campaign = campaign_with(backend).await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        &CELL.replace(
            "either (const 0) (\\a -> J.handle a (#not_here id J..| #line (\\_ (_, _, n) -> n))) answer",
            "either (const \"failed\") (const \"answered\") answer :: Text",
        ),
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "failed", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Offers and packets bound in one statement are retained for later ones,
/// and the packet operators read unqualified.
#[tokio::test]
async fn retained_packet_bindings_reach_a_later_statement() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Ok(serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {
                "place": {"type": "choice", "choice": "line_12",
                          "probabilities": {"line_4": 0.1, "line_12": 0.9}, "confidence": 0.9},
                "enough": {"type": "noul", "noul": 0.8}
            },
            "usage": {}
        })
        .to_string()),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        // No `LANGUAGE` pragma. This is the load-bearing case: the packet
        // needs `OverloadedLabels` and `(J.answers r).place` needs
        // `OverloadedRecordDot`, and the latter is deliberately absent from
        // `DECL_TEMPLATE_SOURCE`, the parse-only template GHC uses to pick a
        // cell item's shape. If template selection ever starts needing it,
        // this test is where that shows up.
        r#"let offers = J.alt #line_4 "if attempts > 3" (4 :: Int) J..| J.alt #line_12 "if elapsed > timeout" 12
let packet = #place := J.choice "Which line begins the retry-timeout branch?" offers :& #enough := J.noul "Is the branch visible?"
answer <- J.ask (J.rawState (String "retry loop in fetch")) packet
either (const 0) (\r -> J.handle (J.answers r).place (#line_4 id J..| #line_12 id)) answer"#,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "12", "{result}");
    assert_eq!(backend.requests.lock().len(), 1);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A Jev-dense cell judges every file of a bound preview list in one packet,
/// and an unconfigured endpoint reaches the cell as an ordinary `Left`. The
/// per-row battery flattens to dotted wire keys, one per row, beside the
/// top-level cell.
#[tokio::test]
async fn a_per_row_battery_sends_one_request_with_one_question_per_row() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Err(JevCallFailure::Unconfigured),
    });
    let campaign = campaign_with(Arc::clone(&backend)).await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        r##"let previews = [("README.md", "# jev-dsl\ntyped packets"), ("LICENSE", "MIT")] :: [(Text, Text)]
answer <- J.ask (J.rawState (String "choosing what to read next"))
  ( #enough := J.noul "Is the listing enough to choose from?"
 :& #worth_reading := J.each fst (\(_, body) -> J.noul ("Worth reading in full? " <> body)) previews )
either (T.pack . show) (const "answered") answer :: Text"##,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let output = result["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(output.contains("no Jev endpoint is configured"), "{result}");
    let requests = backend.requests.lock();
    assert_eq!(requests.len(), 1);
    let questions = &requests[0]["questions"];
    assert!(
        questions.get("enough").is_some(),
        "enough missing: {questions}"
    );
    let per_file = questions
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with("worth_reading."))
        .count();
    assert_eq!(per_file, 2, "one question per previewed file: {questions}");
    drop(requests);
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// Haskell cell -> host effect -> live TypeSafe API -> typed answer. Opt-in:
/// `TYPESAFE_API_KEY` must be set; run with `--ignored`.
#[tokio::test]
#[ignore = "calls the live TypeSafe API; needs TYPESAFE_API_KEY"]
async fn live_jev_from_a_haskell_cell() {
    assert!(
        std::env::var("TYPESAFE_API_KEY").is_ok_and(|key| !key.is_empty()),
        "TYPESAFE_API_KEY is not set"
    );
    let campaign = TestCampaign::start_with_config(
        tidepool_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = None;
            pinned_jev_workspace(config);
        },
    )
    .await;
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        r#"answer <- J.ask1 (J.rawState (String "A cat is sitting on a warm windowsill in the sun."))
  (J.choice "Where is the cat?"
     (J.alt #windowsill "On a windowsill" (1 :: Int)
        J..| J.alt #roof "On a roof" 2
        J..| J.alt #bed "In a bed" 3))
either (const 0) (\a -> J.handle a (#windowsill id J..| #roof id J..| #bed id)) answer"#,
    )
    .await;
    assert_eq!(result["status"], "committed", "{result}");
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["output"], "1", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

// ---------------------------------------------------------------------------
// A pre-flight for the live demo: the agent's OWN hosted tool body, and its
// after-tool slot, each asking Jev from inside the retained compiled agent
// spec, rather than from a notebook cell. A cell reaches the facade as `J`
// through the workbench; a module under a source root does not and must
// import it itself.
// ---------------------------------------------------------------------------

/// `Project.Tools`: one tool whose body asks Jev a yes/no question about its
/// own `topic` argument and answers with text naming the branch the scripted
/// answer took. `Member Jev effects` is worked out from `Jev.Operators.ask1`'s
/// own `Member Jev effs` constraint, and carried on both `probeBody` and
/// `tools` so the constraint reaches whatever concrete row the spec compiles
/// against.
const AGENT_SPEC_JEV_TOOLS_MODULE: &str = r#"{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.Tools (SpecTools (..), Probe (..), probeBody, tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Aeson.Value (Value (String))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev)
import qualified Jev.Operators as J

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

newtype SpecTools mode = SpecTools { probe :: mode :- Call Probe Text }
  deriving (Generic)

-- | The tool's implementation as ordinary source: one Jev yes/no question
-- about its own argument, answered with text that names the branch the
-- scripted answer took.
probeBody :: Member Jev effects => Probe -> Eff effects Text
probeBody request = do
  answer <-
    J.ask1
      (J.rawState (String (topic request)))
      ( J.choice
          "Does this topic warrant investigation?"
          ( J.alt #yes "The topic clearly warrants investigation" ()
              J..| J.alt #no "The topic does not warrant investigation" ()
          )
      )
  pure $ case answer of
    Left _ -> "tool-body: jev unavailable for " <> topic request
    Right a ->
      J.handle
        a
        ( #yes (\_ -> "tool-body branch: yes, investigate " <> topic request)
            J..| #no (\_ -> "tool-body branch: no, skip " <> topic request)
        )

tools :: Member Jev effects => SpecTools (AsServerT (Eff effects))
tools =
  SpecTools
    { probe = tool "Answer one fixed question about a topic, judged by Jev." probeBody }
"#;

/// `AgentSpec`: the same tools record, with an after-tool slot that asks Jev
/// a second, independent yes/no question about `toolResultOutput result` (not
/// about the tool's argument) and returns `Annotated` text naming the branch
/// ITS answer took.
const AGENT_SPEC_JEV_SPEC_MODULE: &str = r#"{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Tidepool.Aeson.Value (Value (String))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev)
import qualified Jev.Operators as J
import qualified Project.Tools as Tools

agentSpec :: Member Jev effects => AgentSpec Tools.SpecTools effects
agentSpec =
  defaultSpec
    { specTools = Tools.tools
    , afterTool = Just noted
    }

-- | Shown every finished @probe@ call and what it answered. Asks Jev its OWN
-- question about the tool's output, independently of what the tool body
-- asked about the argument.
noted :: Member Jev effects => ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call /= T.pack "probe" = pure NoAnnotation
  | otherwise = do
      answer <-
        J.ask1
          (J.rawState (String (toolResultOutput result)))
          ( J.choice
              "Does this tool output look complete?"
              ( J.alt #yes "The output looks complete" ()
                  J..| J.alt #no "The output looks incomplete" ()
              )
          )
      pure $ case answer of
        Left _ -> Annotated (T.pack "after-tool branch: jev unavailable")
        Right a ->
          Annotated
            ( J.handle
                a
                ( #yes (\_ -> T.pack "after-tool branch: complete")
                    J..| #no (\_ -> T.pack "after-tool branch: incomplete")
                )
            )
"#;

/// A workspace that combines `pinned_jev_workspace`'s pinned Jev facade with
/// an authored `Project/Tools.hs` + `AgentSpec.hs`: two source roots at once
/// (this workspace's own `.shoal`, and the pinned package's), so a module
/// under either root can `import qualified Jev.Operators as J` itself.
fn pinned_jev_agent_spec_workspace(config: &mut ActorHostConfig) {
    // Everything `pinned_jev_workspace` does, inlined rather than called: that
    // helper ends by calling `FrozenWorkspace::load`, which memoizes its
    // result at `run_root/workspace/selection.json` and would otherwise hand
    // a SECOND call here the stale pre-authored selection instead of
    // re-reading the config and files written below. One workspace, one load.
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/shoal-workspace")
        .canonicalize()
        .expect("the Shoal workspace package this repository ships");
    let authored = config.workspace.join(".shoal");
    std::fs::create_dir_all(authored.join("Project")).unwrap();
    std::fs::write(
        authored.join("config.toml"),
        format!(
            "[defaults]\nmodel = 'test-model'\n\n\
             [haskell]\nsource_roots = ['.', '{}']\n\
             modules = ['Project.Tools', 'AgentSpec']\n\
             tools = 'Project.Tools.tools'\n\
             spec = 'AgentSpec.agentSpec'\n\n\
             [haskell.flake_sources]\njev-dsl = ['core']\n",
            package.join(".shoal").display()
        ),
    )
    .unwrap();
    std::fs::write(
        authored.join("Project/Tools.hs"),
        AGENT_SPEC_JEV_TOOLS_MODULE,
    )
    .unwrap();
    std::fs::write(authored.join("AgentSpec.hs"), AGENT_SPEC_JEV_SPEC_MODULE).unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    // `nix flake archive` reads only tracked files, and a child worktree
    // admission refuses a dirty source repository.
    super::test_campaign::commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::shoal::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve the combined pinned + authored Haskell source"),
    );
}

async fn probe_topic(
    policy: &dyn tidepool_actor::ResidentToolEndpoint,
    topic: &str,
) -> String {
    dispatch_structured_tool(policy, "probe", serde_json::json!({"topic": topic}))
        .await
        .to_string()
}

async fn detailed_status(policy: &dyn tidepool_actor::ResidentToolEndpoint) -> String {
    dispatch_structured_tool(policy, "status", serde_json::json!({"view": "detailed"}))
        .await
        .to_string()
}

/// The pre-flight the live demo depends on: nobody has yet proven that both
/// an agent's own hosted tool body AND its after-tool slot can ask Jev from
/// inside the retained compiled agent spec (as opposed to a notebook cell,
/// which is the only path every other Jev test exercises). One scripted
/// answer serves both requests, because both questions offer the same
/// `#yes`/`#no` alternatives; what is under test is that each call site can
/// reach the Jev effect on its own, not that they read different content.
#[tokio::test]
async fn a_tool_body_and_a_slot_can_both_ask_jev() {
    let backend = Arc::new(FakeJev {
        requests: Mutex::new(Vec::new()),
        answer: Ok(serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {"value": {
                "type": "choice",
                "choice": "yes",
                "probabilities": {"yes": 0.9, "no": 0.1},
                "confidence": 0.9
            }},
            "usage": {}
        })
        .to_string()),
    });

    let campaign = tokio::time::timeout(
        Duration::from_secs(120),
        TestCampaign::start_with_config(
            tidepool_actor::ResearchPolicy::default(),
            |admission| admission,
            |config| {
                config.jev = Some(Arc::clone(&backend) as tidepool_actor::JevBackendHandle);
                pinned_jev_agent_spec_workspace(config);
            },
        ),
    )
    .await
    .expect("campaign did not start within 120s");
    let policy = campaign.root_installation.policy.clone();
    let policy = policy.as_ref();

    let result = tokio::time::timeout(
        Duration::from_secs(120),
        probe_topic(policy, "the retry loop"),
    )
    .await
    .expect("probe did not answer within 120s");

    // (1) The tool body's own Jev branch, in the tool's own output.
    assert!(
        result.contains("tool-body branch: yes, investigate the retry loop"),
        "{result}"
    );
    // (2) The after-tool slot's own Jev branch, delivered as derived context
    // beside — not instead of — the tool's output.
    assert!(result.contains("[after-tool]"), "{result}");
    assert!(
        result.contains("Derived context, not part of the tool's output"),
        "{result}"
    );
    assert!(result.contains("after-tool branch: complete"), "{result}");

    // (3) The fake backend saw two independent Jev requests: one from the
    // tool body, one from the slot.
    assert_eq!(
        backend.requests.lock().len(),
        2,
        "one request from the tool body and one from the after-tool slot"
    );

    // (4) `status` shows exactly one after-tool row, annotated.
    let status = tokio::time::timeout(Duration::from_secs(120), detailed_status(policy))
        .await
        .expect("status did not answer within 120s");
    assert_eq!(
        status.matches("after-tool#").count(),
        1,
        "one call, one invocation: {status}"
    );
    assert!(status.contains("after-tool#1"), "{status}");
    assert!(status.contains("annotated"), "{status}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
