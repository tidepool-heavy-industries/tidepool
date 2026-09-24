//! Focused execution of the published examples through the real resident tool.

use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_lookup, dispatch_status};
use super::*;
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

fn example(document: &str) -> &str {
    examples(document).next().unwrap()
}

fn examples(document: &str) -> impl Iterator<Item = &str> {
    document
        .split("```haskell\n")
        .skip(1)
        .map(|block| block.split_once("```").unwrap().0)
}

/// The shown part of a paged cell display, with its trailing selection marker
/// removed. The marker names the binding the rest stays in, so its exact text
/// varies with the cell's generation; what every paged display must say is
/// that the view is a selection and how to continue it.
fn selection_page<'a>(output: &'a str, context: &str) -> &'a str {
    let (page, marker) = output
        .split_once("\n[selection of ")
        .unwrap_or_else(|| panic!("a paged display must mark its selection: {context}"));
    assert!(
        marker.ends_with("; display continues: cellDisplay.more]"),
        "{context}"
    );
    page
}

async fn committed(
    policy: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let result = dispatch_haskell_script(policy, source).await;
    assert_eq!(result["status"], "committed", "{result:?}");
    result
}

#[tokio::test]
async fn colon_commands_and_ghci_groups_are_rejected_as_haskell_cells() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    for source in [":status", ":{\ncolonOnly = 1\n:}"] {
        let result = dispatch_haskell_script(policy, source).await;
        assert_eq!(result["status"], "rejected", "source={source}: {result}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_pages_large_text_and_exhausts_continuation() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    let first = committed(policy, include_str!("notebook_display_large_text.hs")).await;
    let output = first["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap();
    let page = selection_page(output, "{first}");
    assert_eq!(page.len(), 8192, "{first}");
    assert!(page.bytes().all(|byte| byte == b'x'), "{first}");
    assert!(!output.contains("Display failed"), "{first}");

    let second = committed(policy, "cellDisplay.more").await;
    let remainder = second["items"][0]["output"].as_str().unwrap();
    assert_eq!(remainder.len(), 1808, "{second}");
    assert!(remainder.bytes().all(|byte| byte == b'x'), "{second}");
    let exhausted = committed(policy, "TidepoolInspection.pageHasMore cellDisplay").await;
    assert_eq!(exhausted["items"][0]["output"], "False", "{exhausted}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_explains_resource_control_rejections() {
    let campaign = TestCampaign::start().await;
    let result = committed(
        campaign.root_installation.policy.as_ref(),
        include_str!("notebook_actor_scoped_handle.hs"),
    )
    .await;
    assert!(
        result["items"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("resource control guidance rendered"),
        "{result}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_compiles_one_expression_and_one_display_bundle() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    // Put startup work behind the counter.  A displayed expression still has
    // its whole-cell check and expression compilation, followed by exactly one
    // generated page/metadata/alias bundle.
    committed(policy, "0 :: Int").await;
    tidepool_extract_cmd::reset_extract_spawn_count();

    let shown = committed(policy, "sum [1 .. 10 :: Int]").await;
    assert_eq!(shown["items"][0]["output"], "55", "{shown}");
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        3,
        "one cell check, one expression, and one display bundle: {shown}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_failure_keeps_the_value_but_not_the_new_alias() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    let initial = committed(policy, "\"previous display\" :: Text").await;
    assert_eq!(
        initial["items"][0]["output"], "previous display",
        "{initial}"
    );

    let failed = committed(
        policy,
        "import Tidepool.Inspection (Display (..))\n\
         data FailingDisplay = FailingDisplay\n\
         instance Display FailingDisplay where displayTree _ = error \"display bottom\"\n\
         broken <- pure FailingDisplay\n\
         broken",
    )
    .await;
    let failure = failed["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap();
    assert!(failure.contains("Display failed"), "{failed}");
    assert!(failure.contains("Value remains bound"), "{failed}");

    let old_alias = committed(policy, "cellDisplay.text").await;
    assert_eq!(
        old_alias["items"][0]["output"], "previous display",
        "a failed page must not publish its new alias: {old_alias}"
    );
    let recovered = committed(
        policy,
        "case broken of FailingDisplay -> \"still bound\" :: Text",
    )
    .await;
    assert_eq!(
        recovered["items"][0]["output"], "still bound",
        "{recovered}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_shares_one_allowance_between_console_and_result() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    let first = committed(policy, include_str!("notebook_display_console_budget.hs")).await;
    assert_eq!(first["items"][0]["kind"], "declaration", "{first}");
    let output = first["items"][1]["output"].as_str().unwrap();
    assert!(!output.contains("Display failed"), "{first}");
    let visible = selection_page(output, "{first}");
    let printed = visible.bytes().filter(|byte| *byte == b'p').count();
    let shown = visible.bytes().filter(|byte| *byte == b'v').count();
    assert_eq!(printed, 6000, "{first}");
    assert!(shown > 0 && printed + shown <= 8192, "{first}");

    let continued = committed(policy, "cellDisplay.more").await;
    let suffix = continued["items"][0]["output"].as_str().unwrap();
    assert!(!suffix.contains('p'), "print ran again: {continued}");
    assert_eq!(
        suffix.bytes().filter(|byte| *byte == b'v').count(),
        6000 - shown,
        "{continued}"
    );
    assert!(suffix.bytes().all(|byte| byte == b'v'), "{continued}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_keeps_previous_cell_display_lexical_and_publishes_prefix() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    let initial = committed(policy, "\"old page\" :: Text").await;
    assert_eq!(initial["items"][0]["output"], "old page", "{initial}");

    let same_cell = committed(
        policy,
        include_str!("notebook_display_previous_cell_display.hs"),
    )
    .await;
    assert_eq!(same_cell["items"][0]["output"], "new page", "{same_cell}");
    assert_eq!(same_cell["items"][1]["output"], "old page", "{same_cell}");

    let prefix =
        dispatch_haskell_script(policy, include_str!("notebook_display_prefix_failure.hs")).await;
    assert_eq!(prefix["status"], "rejected", "{prefix}");
    assert_eq!(prefix["items"][0]["status"], "committed", "{prefix}");
    assert_eq!(prefix["items"][0]["output"], "prefix page", "{prefix}");
    assert_eq!(prefix["items"][1]["status"], "rejected", "{prefix}");
    assert!(
        prefix["items"][1]["output"]
            .as_str()
            .is_some_and(|output| output.contains("failed suffix")),
        "{prefix}"
    );
    let after_prefix = committed(policy, "cellDisplay.text").await;
    assert_eq!(
        after_prefix["items"][0]["output"], "prefix page",
        "{after_prefix}"
    );

    let invalid = dispatch_haskell_script(policy, "absentDisplayName").await;
    assert_eq!(invalid["status"], "rejected", "{invalid}");
    let after_rejection = committed(policy, "cellDisplay.text").await;
    assert_eq!(
        after_rejection["items"][0]["output"], "prefix page",
        "{after_rejection}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_uses_generated_generic_and_explicit_custom_renderers() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    let generic = committed(policy, include_str!("notebook_display_generic.hs")).await;
    let output = generic["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap();
    assert!(output.contains("NotebookPlain"), "{generic}");
    assert!(output.contains('3'), "{generic}");
    assert!(output.contains("<function>"), "{generic}");
    assert!(!output.contains("Display failed"), "{generic}");

    let custom = committed(policy, include_str!("notebook_display_custom.hs")).await;
    assert_eq!(
        custom["items"].as_array().unwrap().last().unwrap()["output"],
        "custom-display-wins",
        "{custom}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_page_capture_survives_observation_window() {
    let campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.as_ref();
    let first = committed(policy, include_str!("notebook_display_large_text.hs")).await;
    assert!(
        first["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("; display continues: cellDisplay.more]"),
        "{first}"
    );
    committed(policy, "savedPage <- pure cellDisplay").await;
    let window = committed(policy, include_str!("notebook_display_retention.hs")).await;
    let items = window["items"].as_array().unwrap();
    assert_eq!(items.len(), 9, "{window}");
    for (index, item) in items.iter().enumerate() {
        assert_eq!(item["output"], (index + 1).to_string(), "{window}");
    }
    let resumed = committed(policy, "savedPage.more").await;
    let remainder = resumed["items"][0]["output"].as_str().unwrap();
    assert_eq!(remainder.len(), 1808, "{resumed}");
    assert!(remainder.bytes().all(|byte| byte == b'x'), "{resumed}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_display_cell_display_is_child_local_but_parent_capture_remains_callable() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let initial = committed(root.as_ref(), "\"parent page\" :: Text").await;
    assert_eq!(initial["items"][0]["output"], "parent page", "{initial}");
    committed(
        root.as_ref(),
        "capturedPageText <- pure (\\() -> cellDisplay.text)",
    )
    .await;
    committed(
        root.as_ref(),
        include_str!("notebook_display_child_unfold.hs"),
    )
    .await;
    let child = campaign
        .next_deployment(
            "child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    let _binding = open_test_fork(&campaign, &child);
    campaign
        .next_deployment(
            "child session readiness",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    committed(
        child.policy.as_ref(),
        "initialPageText () = cellDisplay.text\nsetProbe = Set.size (Set.fromList [1 :: Int, 2])",
    )
    .await;
    let local = committed(child.policy.as_ref(), "initialPageText ()").await;
    assert_eq!(local["items"][0]["output"], "", "{local}");
    let set = committed(child.policy.as_ref(), "setProbe").await;
    assert_eq!(set["items"][0]["output"], "2", "{set}");
    let capture = committed(child.policy.as_ref(), "capturedPageText ()").await;
    assert_eq!(capture["items"][0]["output"], "parent page", "{capture}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_relocates_same_cell_types_and_rejects_before_installation() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();

    let setup = committed(root.as_ref(), include_str!("notebook_nominal_setup.hs")).await;
    assert_eq!(
        setup["summary"], "3 declarations, 2 statements, 1 expression",
        "{setup:?}"
    );
    let items = setup["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{setup:?}");
    for (item, kind, start_line) in [
        (&items[0], "declaration", 1),
        (&items[1], "statement", 7),
        (&items[2], "statement", 9),
        (&items[3], "expression", 11),
    ] {
        assert_eq!(item["kind"], kind, "{setup:?}");
        assert_eq!(item["span"]["startLine"], start_line, "{setup:?}");
        assert_eq!(item["span"]["startColumn"], 1, "{setup:?}");
    }
    let declaration_sources = items[0]["sourceItems"].as_array().unwrap();
    assert_eq!(declaration_sources.len(), 3, "{setup:?}");
    assert_eq!(
        declaration_sources
            .iter()
            .map(|item| item["ordinal"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "{setup:?}"
    );
    assert_eq!(
        declaration_sources
            .iter()
            .map(|item| item["span"]["startLine"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 4],
        "{setup:?}"
    );
    assert!(
        declaration_sources
            .iter()
            .all(|item| item["kind"] == "declaration"),
        "{setup:?}"
    );
    assert_eq!(items[1]["sourceItems"][0]["ordinal"], 3, "{setup:?}");
    assert_eq!(items[3]["status"], "committed", "{setup:?}");
    assert!(
        items[3]["output"]
            .as_str()
            .is_some_and(|output| output.contains("Nothing")),
        "{setup:?}"
    );

    let rejected =
        dispatch_haskell_script(root.as_ref(), include_str!("notebook_nominal_rejected.hs")).await;
    assert_eq!(rejected["status"], "rejected", "{rejected:?}");
    assert_eq!(
        rejected["items"].as_array().unwrap().len(),
        3,
        "{rejected:?}"
    );
    assert_eq!(rejected["items"][0]["status"], "notRun", "{rejected:?}");
    assert_eq!(rejected["items"][1]["status"], "notRun", "{rejected:?}");
    assert_eq!(rejected["items"][2]["status"], "rejected", "{rejected:?}");
    assert_eq!(rejected["items"][2]["span"]["startLine"], 6, "{rejected:?}");

    let missing_declaration = dispatch_haskell_script(
        root.as_ref(),
        include_str!("notebook_nominal_scope_probe.hs"),
    )
    .await;
    assert_eq!(
        missing_declaration["status"], "rejected",
        "{missing_declaration:?}"
    );
    assert!(
        missing_declaration.to_string().contains("MustNotCommit")
            && missing_declaration.to_string().contains("not in scope"),
        "{missing_declaration:?}"
    );

    let missing_binding = dispatch_haskell_script(root.as_ref(), "willNotRun").await;
    assert_eq!(missing_binding["status"], "rejected", "{missing_binding:?}");
    assert!(
        missing_binding.to_string().contains("willNotRun")
            && missing_binding.to_string().contains("not in scope"),
        "{missing_binding:?}"
    );

    let prefix_failure =
        dispatch_haskell_script(root.as_ref(), include_str!("notebook_prefix_failure.hs")).await;
    assert_eq!(prefix_failure["status"], "rejected", "{prefix_failure:?}");
    assert_eq!(
        prefix_failure["summary"], "0 declarations, 4 statements, 0 expressions",
        "{prefix_failure:?}"
    );
    let items = prefix_failure["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{prefix_failure:?}");
    assert_eq!(items[0]["status"], "committed", "{prefix_failure:?}");
    assert_eq!(items[1]["status"], "committed", "{prefix_failure:?}");
    assert_eq!(items[2]["status"], "rejected", "{prefix_failure:?}");
    assert_eq!(items[3]["status"], "notRun", "{prefix_failure:?}");
    assert!(items.iter().all(|item| item["kind"] == "statement"));

    let recovered = committed(root.as_ref(), "prefixValue\n").await;
    assert_eq!(recovered["items"][0]["output"], "41", "{recovered:?}");
    let missing_tail = dispatch_haskell_script(root.as_ref(), "tailValue\n").await;
    assert_eq!(missing_tail["status"], "rejected", "{missing_tail:?}");
    assert!(
        missing_tail.to_string().contains("tailValue")
            && missing_tail.to_string().contains("not in scope"),
        "{missing_tail:?}"
    );

    committed(root.as_ref(), "shadowed <- pure (1 :: Int)\n").await;
    let shadowed = committed(root.as_ref(), "shadowed <- pure (2 :: Int)\nshadowed\n").await;
    assert_eq!(shadowed["items"][1]["output"], "2", "{shadowed:?}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_prologue_applies_to_check_stage_and_execution() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let result = committed(root.as_ref(), include_str!("notebook_prologue.hs")).await;
    assert!(
        result["items"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("True"),
        "{result}"
    );
    let later = committed(
        root.as_ref(),
        "later <- pure (Imported.reverse [answer, 10])\nlater == [10, 7]\n",
    )
    .await;
    assert!(later.to_string().contains("True"), "{later}");
    let disabled = dispatch_haskell_script(
        root.as_ref(),
        "notInstalled <- pure (let ?offset = 5 in implicitTotal 2)\nnotInstalled\n",
    )
    .await;
    assert_eq!(
        disabled["status"], "rejected",
        "cell flags must not leak: {disabled}"
    );
    let imported = committed(
        root.as_ref(),
        "import qualified Data.Maybe as ImportedMaybe\n",
    )
    .await;
    assert_eq!(imported["items"][0]["kind"], "declaration", "{imported}");
    committed(
        root.as_ref(),
        "optional <- pure (ImportedMaybe.fromMaybe answer Nothing)\noptional\n",
    )
    .await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_rejection_retains_its_source_plan() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let rejected =
        dispatch_haskell_script(root.as_ref(), include_str!("notebook_multiple_errors.hs")).await;
    assert_eq!(rejected["status"], "rejected", "{rejected:?}");
    let items = rejected["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{rejected:?}");
    assert_eq!(items[0]["status"], "rejected", "{rejected:?}");
    assert!(
        items[0]["output"].as_str().unwrap().contains("<cell>:2:"),
        "{rejected:?}"
    );
    assert_eq!(items[1]["status"], "notRun", "{rejected:?}");
    assert_eq!(items[2]["status"], "rejected", "{rejected:?}");
    assert!(
        items[2]["output"].as_str().unwrap().contains("<cell>:4:"),
        "{rejected:?}"
    );
    assert_eq!(items[3]["status"], "notRun", "{rejected:?}");
    assert!(
        items.iter().all(|item| item["kind"].is_string()
            && item["span"].is_object()
            && item["installedBindings"]
                .as_array()
                .is_none_or(Vec::is_empty)),
        "{rejected:?}"
    );
    let missing = dispatch_haskell_script(root.as_ref(), "willNotRun").await;
    assert_eq!(missing["status"], "rejected", "{missing:?}");
    assert!(missing.to_string().contains("not in scope"), "{missing:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_preserves_old_types() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(root.as_ref(), include_str!("notebook_identity_original.hs")).await;
    let shadowed = committed(root.as_ref(), include_str!("notebook_identity_shadowed.hs")).await;
    let output = shadowed["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap();
    assert!(
        output.contains("OldVersion 1") && output.contains("NewVersion True"),
        "{shadowed:?}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_retains_observations_needed_by_its_suffix() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let initial = committed(root.as_ref(), "(42 :: Int)\n").await;
    let saved = initial["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    let source = include_str!("notebook_lease_suffix.hs").replace("__SAVED__", saved);
    let result = committed(root.as_ref(), &source).await;
    assert_eq!(
        result["items"].as_array().unwrap().last().unwrap()["output"],
        "42",
        "{result:?}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_infers_response_results() {
    let campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let response = committed(root.as_ref(), include_str!("notebook_identity_response.hs")).await;
    let output = response["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap();
    assert!(output.contains("Pending"), "{response:?}");
    committed(root.as_ref(), "later\npollResponse pending\n").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn notebook_cell_reply_marks_its_tail_not_run() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        "worker <- startAgent (readonlyAgent \"notebook-reply-worker\")\n",
    )
    .await;
    committed(
        root.as_ref(),
        "response <- request @Text worker (assignment [label|notebook-reply|] (\"ready\" :: Text))\n",
    )
    .await;
    let child = campaign
        .next_deployment(
            "notebook reply child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "notebook reply request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.message.contains("notebook-reply") =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;

    let reply = dispatch_haskell_script(
        child.policy.as_ref(),
        include_str!("notebook_reply_tail.hs"),
    )
    .await;
    assert_eq!(reply["status"], "replied", "{reply:?}");
    assert_eq!(
        reply["summary"], "0 declarations, 1 statement, 1 expression",
        "{reply:?}"
    );
    let items = reply["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{reply:?}");
    assert_eq!(items[0]["status"], "committed", "{reply:?}");
    assert_eq!(items[0]["kind"], "expression", "{reply:?}");
    assert_eq!(items[0]["terminalTransfer"], "replyAccepted", "{reply:?}");
    assert_eq!(items[1]["status"], "notRun", "{reply:?}");
    assert_eq!(items[1]["kind"], "statement", "{reply:?}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

fn open_test_fork(
    campaign: &TestCampaign,
    child: &exomonad_actor::LocalResidentInstallation,
) -> Arc<dyn exomonad_actor::ForkWorkspaceCustody> {
    campaign.authority.install_grant(
        child.actor.identity().into(),
        worktree_grant(child.effective_role.role()),
    );
    let [worktree_id] = child.launch_worktrees.as_slice() else {
        panic!("child must have one worktree")
    };
    let worktree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(worktree_id))
        .unwrap()
        .unwrap();
    let principal = WorktreePrincipal::exact_actor(
        &runtime_namespace(campaign.session_root.path()),
        child.actor.identity().id.0,
        child.actor.identity().incarnation.0,
    );
    assert_eq!(
        campaign
            .bindings
            .lock()
            .current(worktree.id())
            .unwrap()
            .agent(),
        &principal
    );
    let binding = child
        .worktree_custody
        .clone()
        .expect("bootstrap installed custody");
    child.fork_gate.as_ref().unwrap().mark_ready().unwrap();
    binding
}

#[tokio::test]
async fn published_unfold_watch_and_request_examples_execute() {
    execute_examples(false, None, 1).await;
}

#[tokio::test]
async fn invalid_label_literals_fail_before_actor_side_effects() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let me = committed(root.as_ref(), "inspectFull (agentIdentity me)").await;
    let identity = campaign.root_installation.actor.identity();
    assert_eq!(
        me["items"][0]["output"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect::<String>(),
        format!("({},{})", identity.id.0, identity.incarnation.0)
    );

    let invalid_watch = dispatch_haskell_script(
        root.as_ref(),
        "badWatch <- watch (\"Bad Label\" :: WatchLabel) (pure ())",
    )
    .await;
    assert_eq!(invalid_watch["status"], "rejected", "{invalid_watch}");
    assert!(
        invalid_watch
            .to_string()
            .contains("InvalidWatchLabel \\\"Bad Label\\\""),
        "{invalid_watch}"
    );
    for (source, expected) in [
        (
            "emptyWatch <- watch (\"\" :: WatchLabel) (pure ())",
            "EmptyWatchLabel",
        ),
        (
            concat!(
                "longWatch <- watch ",
                "(\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" :: WatchLabel) ",
                "(pure ())",
            ),
            "WatchLabelTooLong",
        ),
    ] {
        let rejected = dispatch_haskell_script(root.as_ref(), source).await;
        assert_eq!(rejected["status"], "rejected", "{rejected}");
        assert!(rejected.to_string().contains(expected), "{rejected}");
    }

    let invalid_unfold = dispatch_haskell_script(
        root.as_ref(),
        concat!(
            "badWorkers <- unfold (batch \"literal-errors\" \"branches\") $ ",
            "(,) <$> child (researching @Text projectHead (assignment [label|valid|] ())) ",
            "<*> child (researching @Text projectHead (assignment [label|Bad Label|] ()))",
        ),
    )
    .await;
    assert_eq!(invalid_unfold["status"], "rejected", "{invalid_unfold}");
    assert!(
        invalid_unfold
            .to_string()
            .contains("InvalidKebabName \\\"Bad Label\\\""),
        "{invalid_unfold}"
    );
    campaign.assert_no_deployment(
        "an invalid later branch launched an earlier child",
        |event| matches!(event, LocalResidentDeployment::PolicyInstalled(_)),
    );

    committed(
        root.as_ref(),
        concat!(
            "worker <- unfold (batch \"literal-errors\" \"request\") ",
            "(child (researching @Text projectHead (assignment [label|target|] ())))",
        ),
    )
    .await;
    let (_worker_installation, _worker_custody) = next_project_worker(&mut campaign).await;
    committed(root.as_ref(), "before <- listAgents").await;
    committed(
        root.as_ref(),
        concat!(
            "let requestCount target rows = sum [length (rosterCurrentRequests row) | ",
            "row <- rows, (rosterActorId row, rosterActorIncarnation row) == target]",
        ),
    )
    .await;
    committed(
        root.as_ref(),
        "let beforeRequests = requestCount (agentIdentity (responseActor worker)) before",
    )
    .await;
    let rendered = committed(root.as_ref(), "inspectFull worker").await;
    let rendered = rendered["items"][0]["output"].as_str().unwrap();
    assert!(rendered.contains("AgentRef ("), "{rendered}");
    assert!(rendered.contains("actor = AgentRef ("), "{rendered}");
    assert!(rendered.contains("path = ActorPath"), "{rendered}");
    assert!(rendered.contains("Response { request ="), "{rendered}");
    let invalid_request = dispatch_haskell_script(
        root.as_ref(),
        "badResponse <- request @Text (responseActor worker) (assignment [label|Bad Label|] ())",
    )
    .await;
    assert_eq!(invalid_request["status"], "rejected", "{invalid_request}");
    assert!(
        invalid_request
            .to_string()
            .contains("InvalidKebabName \\\"Bad Label\\\""),
        "{invalid_request}"
    );
    committed(root.as_ref(), "after <- listAgents").await;
    let unchanged = committed(
        root.as_ref(),
        "requestCount (agentIdentity (responseActor worker)) after == beforeRequests",
    )
    .await;
    assert_eq!(unchanged["items"][0]["output"], "True", "{unchanged}");

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn shared_api_guide_example_handles_success_and_unavailable() {
    // The guide's command/judgment example reads `J`, which a run gets from the
    // Jev library its workspace pins, so this campaign selects that workspace.
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        super::jev_tests::pinned_jev_workspace,
    )
    .await;
    let root = campaign.root_installation.policy.clone();
    let guide = include_str!("../../../../exomonad/prompts/api-guide.md");
    let mut guide_examples = examples(guide);
    committed(root.as_ref(), guide_examples.next().unwrap()).await;
    let child = campaign
        .next_deployment(
            "guide example child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    let _binding = open_test_fork(&campaign, &child);
    campaign
        .next_deployment(
            "guide example session readiness",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation } => {
                    assert!(activation.message.contains("Remove the stale path"));
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let pending = committed(root.as_ref(), "state <- pollWatch ready\ninspectFull state").await;
    let rendered = pending["items"][1]["output"].as_str().unwrap();
    // A pending watch observation is itself the registered wake: its
    // `PendingProgress` carries the dependency's lifecycle/provider evidence
    // and `pendingWatched = True`, so `inspectFull` shows there is nothing a
    // re-poll would add.
    assert!(rendered.contains("WatchPending"), "{rendered}");
    assert!(rendered.contains("PendingProgress"), "{rendered}");
    assert!(rendered.contains("pendingActorState"), "{rendered}");
    assert!(rendered.contains("pendingProviderHealth"), "{rendered}");
    assert!(rendered.contains("pendingWatched = True"), "{rendered}");
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let success = committed(root.as_ref(), guide_examples.next().unwrap()).await;
    // Define the command/Jev composition through the production workbench. Do not
    // call a live provider: this check proves the published surface compiles.
    committed(root.as_ref(), guide_examples.next().unwrap()).await;
    assert!(guide_examples.next().is_none(), "untested guide example");
    // Exercise changed evidence examples without live commands or model calls.
    // The host requests a backend for each command, including later calls in a cell.
    let command_examples = [
        "judgeChanges \"inspect changed documentation\"",
        examples(include_str!(
            "../../../../.exomonad/workspace/skills/exomonad-workbench/SKILL.md"
        ))
        .nth(2)
        .unwrap(),
        examples(include_str!(
            "../../../../.exomonad/workspace/skills/exomonad-workbench/SKILL.md"
        ))
        .nth(4)
        .unwrap(),
        example(include_str!(
            "../../../../.exomonad/workspace/skills/exomonad-jev/SKILL.md"
        )),
    ];
    for (index, source) in command_examples.into_iter().enumerate() {
        let mut running = tokio::spawn({
            let root = root.clone();
            async move { dispatch_haskell_script(root.as_ref(), source).await }
        });
        let result = loop {
            tokio::select! {
                result = &mut running => break result.unwrap(),
                request = super::command_jobs_tests::backend_request(&mut campaign) => {
                    request.supply(Ok(super::command_jobs_tests::TestCommands::completed("README.md")));
                }
            }
        };
        assert_eq!(result["status"], "committed", "{source}: {result}");
        if index == 0 {
            assert!(result.to_string().contains("Jev unavailable:"), "{result}");
        }
    }

    committed(
        root.as_ref(),
        example(include_str!(
            "../../../../.exomonad/workspace/skills/exomonad-jev/references/recent-changes.md"
        )),
    )
    .await;
    let layout =
        dispatch_haskell_script(root.as_ref(), include_str!("notebook_let_layout.hs")).await;
    assert_eq!(layout["status"], "rejected", "{layout}");
    assert!(layout.to_string().contains("parse error"), "{layout}");
    let repaired = committed(
        root.as_ref(),
        include_str!("notebook_let_layout_repaired.hs"),
    )
    .await;
    assert_eq!(
        repaired["items"].as_array().unwrap().last().unwrap()["output"],
        "42",
        "{repaired}"
    );
    assert_eq!(
        success["items"][1]["output"],
        "WatchReady (Right \"Remove the stale path and report the focused check.\")"
    );

    committed(
        root.as_ref(),
        include_str!("shared_api_guide_unavailable.hs"),
    )
    .await;
    campaign.await_watch_ready().await;
    let unavailable = committed(
        root.as_ref(),
        "state <- pollWatch retainedFailureReady\ninspectFull (fmap (either (const True) (const False) . settledValue) state)",
    )
    .await;
    assert_eq!(unavailable["items"][1]["output"], "WatchReady True");
    let outer_unavailable = committed(
        root.as_ref(),
        "state <- pollWatch outerFailureReady\ninspectFull (guideIsUnavailable state)",
    )
    .await;
    assert_eq!(outer_unavailable["items"][1]["output"], "True");

    // The guide's companion `doc reflect` example, on this same campaign so it
    // needs no compile of its own. This root has no conversation reader, which
    // is the unbound case the example is written to survive: it continues with
    // no history rather than being handed somebody else's.
    let reflect = committed(
        root.as_ref(),
        example(include_str!("../../../../exomonad/prompts/docs/reflect.md")),
    )
    .await;
    assert_eq!(reflect["items"][2]["output"], "[]", "{reflect}");
    assert_eq!(
        reflect["items"][1]["operations"][0]["effect"], "reflect",
        "{reflect}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn watch_documentation_request_options_reports_progress_then_settles() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        "lead <- startAgent (readonlyAgent \"documented-progress-lead\")",
    )
    .await;
    let snippets: Vec<_> = examples(include_str!("../../../../exomonad/prompts/docs/watch.md"))
        .filter(|snippet| snippet.contains("let progressOptions = assignment"))
        .collect();
    assert_eq!(snippets.len(), 1, "one complete documented progress setup");
    committed(root.as_ref(), snippets[0]).await;
    let child = campaign
        .next_deployment(
            "documented progress child policy installation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "documented progress request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { activation } => {
                    assert!(activation
                        .message
                        .contains("Publish cumulative findings; then return your final report."));
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    // Exercise the real resident actor path, without a model/provider execution claim.
    committed(child.policy.as_ref(), "reportProgress [\"finding\"]").await;
    campaign.await_watch_ready().await;
    let progress = committed(
        root.as_ref(),
        include_str!("watch_documentation_progress.hs"),
    )
    .await;
    assert_eq!(progress["items"][2]["output"], "True", "{progress}");
    assert_eq!(progress["items"][3]["output"], "True", "{progress}");
    let reply =
        dispatch_haskell_script(child.policy.as_ref(), "respond (\"final report\" :: Text)").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let settled = committed(
        root.as_ref(),
        include_str!("watch_documentation_settled.hs"),
    )
    .await;
    assert_eq!(settled["items"][2]["output"], "True", "{settled}");
    assert_eq!(settled["items"][3]["output"], "True", "{settled}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn activation_presents_prose_and_preserves_exact_inputs() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    let mut child = None;
    committed(root.as_ref(), "data Report = Report Int deriving Show\nworker <- startAgent (readonlyAgent \"activation-preview-worker\")").await;
    committed(
        root.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/activation_preview_setup.hs"),
    )
    .await;
    for (label, input, expected, reply) in [
        (
            "preview-text",
            "textPreview",
            "first line\nλ second line",
            "respond (Report 1)",
        ),
        (
            "preview-long-text",
            "longTextPreview",
            "FINAL-ACCEPTANCE-CONDITION",
            "respond (Report 1)",
        ),
        (
            "preview-oversized-text",
            "oversizedTextPreview",
            "expand with `inspectFull sessionInput`",
            "respond (Report 1)",
        ),
        (
            "preview-opaque",
            "opaquePreview",
            "<opaque value>",
            "respond (Report (sessionInput 16))",
        ),
        (
            "preview-effect",
            "effectPreview",
            "<opaque value>",
            "respond (Report 1)",
        ),
        (
            "preview-failure",
            "brokenPreview",
            "rendering unavailable",
            "case sessionInput of BrokenPreview n -> respond (Report n)",
        ),
    ] {
        committed(root.as_ref(), &format!("let previewLabel = [label|{label}|]\npreviewResponse <- request @Report worker (assignment previewLabel {input})")).await;
        let activation = campaign
            .next_deployment(
                "preview activation",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(installation) => {
                        child = Some((*installation).clone());
                        Err(LocalResidentDeployment::PolicyInstalled(installation))
                    }
                    LocalResidentDeployment::SessionReady { activation }
                        if activation.message.contains(label) =>
                    {
                        Ok(activation)
                    }
                    other => Err(other),
                },
            )
            .await;
        assert!(activation
            .message
            .contains("`reportProgress` is unavailable"));
        if label == "preview-text" {
            let unavailable =
                dispatch_lookup(child.as_ref().unwrap().policy.as_ref(), &["reportProgress"]).await;
            assert!(
                unavailable.to_string().contains("no match"),
                "{unavailable}"
            );
        }
        assert!(
            activation.message.contains(expected),
            "{}",
            activation.message
        );
        assert!(
            activation.message.contains("data Report"),
            "{}",
            activation.message
        );
        if label == "preview-long-text" {
            assert!(
                !activation.message.contains("omitted"),
                "{}",
                activation.message
            );
            let observation =
                committed(child.as_ref().unwrap().policy.as_ref(), "sessionInput").await;
            assert!(!observation["items"][0]["output"]
                .as_str()
                .unwrap()
                .contains("FINAL-ACCEPTANCE-CONDITION"));
        }
        if label == "preview-oversized-text" {
            assert!(activation.message.len() < 17 * 1024);
            assert!(!activation.message.contains("RETAINED-ASSIGNMENT-TAIL"));
            let expanded = committed(
                child.as_ref().unwrap().policy.as_ref(),
                "inspectFull sessionInput",
            )
            .await;
            assert!(
                expanded["items"][0]["output"]
                    .as_str()
                    .unwrap()
                    .contains("RETAINED-ASSIGNMENT-TAIL"),
                "{expanded}"
            );
        }
        let result = dispatch_haskell_script(child.as_ref().unwrap().policy.as_ref(), reply).await;
        assert_eq!(result["status"], "replied", "{result:?}");
    }
    committed(root.as_ref(), "stopAgent worker").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn rich_response_survives_resident_computation() {
    execute_examples(true, None, 1).await;
}

#[tokio::test]
async fn quiet_observation_retains_exact_results_without_repeating_effects() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(root.as_ref(), include_str!("quiet_observation_setup.hs")).await;
    let child = campaign
        .next_deployment(
            "quiet observation child policy installation",
            Duration::from_secs(60),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(installation) => Ok(installation),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "quiet observation session readiness",
            Duration::from_secs(60),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let reply = dispatch_haskell_script(child.policy.as_ref(), "respond delivery").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    campaign.await_watch_ready().await;
    let first = committed(root.as_ref(), "pollWatch ready").await;
    let saved = first["items"][0]["installedBindings"][0].as_str().unwrap();
    let output = first["items"][0]["output"].as_str().unwrap();
    assert!(output.starts_with("WatchReady"), "{first}");
    assert!(!output.contains("candidate-9828"), "{first}");
    assert!(!output.contains("Display failed"), "{first}");
    let expanded = committed(root.as_ref(), "cellDisplay.more").await;
    assert!(
        expanded["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("candidate-9828"),
        "{expanded}"
    );
    committed(root.as_ref(), &format!("let retained = {saved} ()")).await;
    committed(root.as_ref(), &format!("declaredEvidence = {saved} ()")).await;
    let second = committed(root.as_ref(), "pollWatch ready").await;
    assert_ne!(
        first["items"][0]["installedBindings"],
        second["items"][0]["installedBindings"]
    );
    let expiring = second["items"][0]["installedBindings"][0].as_str().unwrap();
    let failed_declaration = dispatch_haskell_script(
        root.as_ref(),
        "brokenDeclaration = missingObservationDependency :: Int",
    )
    .await;
    assert_eq!(
        failed_declaration["status"], "rejected",
        "{failed_declaration}"
    );
    for _ in 0..9 {
        committed(root.as_ref(), "pollWatch ready").await;
    }
    let expired = dispatch_haskell_script(root.as_ref(), &format!("{expiring} ()")).await;
    assert_eq!(expired["status"], "rejected", "{expired}");
    for name in ["retained", "declaredEvidence"] {
        let source = include_str!("quiet_observation_exact_probe.hs").replace("__NAME__", name);
        let exact = committed(root.as_ref(), &source).await;
        assert_eq!(exact["items"][0]["output"], "True", "{exact}");
    }
    let before = campaign
        .forest
        .inspect_graph(campaign.actor.identity())
        .unwrap()
        .len();
    let spawned = committed(root.as_ref(), "startAgent (readonlyAgent \"observe-once\")").await;
    let spawned_name = spawned["items"][0]["installedBindings"][0]
        .as_str()
        .unwrap();
    let inspect = format!("inspectFull (agentIdentity ({spawned_name} ()))");
    let one = committed(root.as_ref(), &inspect).await;
    let two = committed(root.as_ref(), &inspect).await;
    assert_eq!(one["items"][0]["output"], two["items"][0]["output"]);
    assert_eq!(
        campaign
            .forest
            .inspect_graph(campaign.actor.identity())
            .unwrap()
            .len(),
        before + 1
    );
    committed(root.as_ref(), &format!("stopAgent ({spawned_name} ())")).await;
    committed(root.as_ref(), "stopAgent worker").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn queued_unfold_survives_later_rejection() {
    execute_examples(false, Some("missingBindingAfterSuccessfulUnfold"), 1).await;
}

#[tokio::test]
async fn reattachment_preserves_completed_unacknowledged_forks() {
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    committed(
        root.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/documentation_setup.hs"),
    )
    .await;
    let boundary = tidepool_runtime::session::WorkbenchForkBoundary {
        thread_id: "actor-host-recovery".into(),
        call_id: "minimal-unfold".into(),
    };
    let result = root
        .dispatch_boxed(ToolInvocation {
            context: Some(ToolInvocationContext {
                context_call_id: Some(boundary.call_id.clone()),
                thread_id: boundary.thread_id.clone(),
                turn_id: "minimal-turn".into(),
                call_id: "minimal-inner-call".into(),
                namespace: Some("haskell".into()),
            }),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(
                example(include_str!("../../../../exomonad/prompts/docs/unfold.md")).into(),
            ),
        })
        .await
        .unwrap();
    assert_eq!(result["status"], "committed", "{result:?}");
    campaign.assert_no_deployment("no child before reattachment", |event| {
        matches!(event, LocalResidentDeployment::PolicyInstalled(_))
    });
    root.reattach_boxed().await.unwrap();
    assert!(matches!(
        root.reconcile_workbench_boxed(boundary.clone())
            .await
            .unwrap(),
        exomonad_actor::WorkbenchBoundaryReconciliation::Recovered { .. }
    ));
    root.complete_boxed(boundary.clone()).await.unwrap();
    root.complete_boxed(boundary.clone()).await.unwrap();

    let mut children = Vec::new();
    while children.len() < 2 {
        let child = campaign
            .next_deployment(
                "recovered fork startup",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(child) => Ok(child),
                    other => Err(other),
                },
            )
            .await;
        assert_eq!(child.fork_boundary.as_ref(), Some(&boundary));
        children.push(child);
    }
    for child in &children {
        let inherited = committed(child.policy.as_ref(), "sessionInput").await;
        assert!(inherited["items"][0]["output"].as_str().is_some());
    }
    campaign.assert_no_deployment(
        "exactly two children belong to the recovered unfold",
        |event| matches!(event, LocalResidentDeployment::PolicyInstalled(_)),
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn multiple_unfolds_are_admitted_before_completion() {
    execute_examples(false, None, 2).await;
}

async fn execute_examples(rich_response: bool, suffix: Option<&str>, groups: usize) {
    let mut campaign = TestCampaign::start().await;
    let root = Arc::clone(&campaign.root_installation.policy);
    for block in include_str!("../../../../exomonad/prompts/docs/workbench.md")
        .split("```haskell\n")
        .skip(1)
    {
        committed(root.as_ref(), block.split_once("```").unwrap().0).await;
    }
    committed(
        root.as_ref(),
        if rich_response {
            include_str!("../actor_host_fixtures/generic_actor/rich_response_setup.hs")
        } else {
            include_str!("../actor_host_fixtures/generic_actor/documentation_setup.hs")
        },
    )
    .await;
    let review_type = dispatch_lookup(root.as_ref(), &["Review"]).await;
    assert_eq!(review_type["status"], "committed", "{review_type:?}");
    let extra_group = if groups == 2 {
        include_str!("../actor_host_fixtures/generic_actor/second_queued_unfold.hs")
    } else {
        ""
    };
    let call_id = "documentation-unfold".to_owned();
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        root.dispatch_boxed(ToolInvocation {
            context: Some(ToolInvocationContext {
                context_call_id: Some(call_id.clone()),
                thread_id: "actor-host-vertical".into(),
                turn_id: call_id.clone(),
                call_id: call_id.clone(),
                namespace: Some("haskell".into()),
            }),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(format!(
                "{}\n{}\n{}\n{}",
                example(include_str!("../../../../exomonad/prompts/docs/unfold.md")),
                example(include_str!("../../../../exomonad/prompts/docs/watch.md")),
                extra_group,
                suffix.unwrap_or("")
            )),
        }),
    )
    .await
    .expect("unfold must return without provider startup")
    .unwrap();
    assert_eq!(
        result["status"],
        if suffix.is_some() {
            "rejected"
        } else {
            "committed"
        },
        "{result:?}"
    );
    campaign.assert_no_deployment("child started before tool completion", |event| {
        matches!(event, LocalResidentDeployment::PolicyInstalled(_))
    });
    let completion = tidepool_runtime::session::WorkbenchForkBoundary {
        thread_id: "actor-host-vertical".into(),
        call_id,
    };
    let expected_children = groups * 2;
    root.complete_boxed(completion.clone()).await.unwrap();
    root.complete_boxed(completion).await.unwrap();
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    let mut fork_boundary = None;
    let mut activation_messages = Vec::new();
    enum Collected {
        Child(Box<exomonad_actor::LocalResidentInstallation>),
        Activation(exomonad_actor::ResidentActivation),
    }
    while children.len() < expected_children {
        let collected = campaign
            .next_deployment(
                "child deployment",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(child) => Ok(Collected::Child(child)),
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        panic!("{actor:?} retired: {terminal:?}")
                    }
                    LocalResidentDeployment::SessionReady { activation } => {
                        Ok(Collected::Activation(activation))
                    }
                    other => Err(other),
                },
            )
            .await;
        match collected {
            Collected::Child(child) => {
                let expected_effort = child
                    .label
                    .ends_with("/consumer-tests")
                    .then_some(exomonad_actor::ForkEffort::Medium);
                assert_eq!(
                    child.fork_effort, expected_effort,
                    "the consumer explicitly requests Medium; the domain leaves selection to the host default"
                );
                let boundary = child.fork_boundary.as_ref().expect("hosted fork boundary");
                assert_eq!(boundary.thread_id, "actor-host-vertical");
                assert!(!boundary.call_id.is_empty());
                if let Some(expected) = &fork_boundary {
                    assert_eq!(
                        boundary, expected,
                        "siblings inherit the same hosted invocation"
                    );
                } else {
                    fork_boundary = Some(boundary.clone());
                }
                bindings.push(open_test_fork(&campaign, &child));
                children.push(child);
            }
            Collected::Activation(activation) => activation_messages.push(activation),
        }
    }
    let mut presented = std::collections::HashSet::new();
    while presented.len() < children.len() {
        let activation =
            if let Some(activation) = activation_messages.pop() {
                activation
            } else {
                campaign
                    .next_deployment("request activation", Duration::from_secs(120), |event| {
                        match event {
                            LocalResidentDeployment::SessionReady { activation } => Ok(activation),
                            other => Err(other),
                        }
                    })
                    .await
            };
        let Some(child) = children
            .iter()
            .find(|child| child.actor.identity() == activation.id.actor())
        else {
            continue;
        };
        assert!(
            !activation.message.contains("rendering unavailable"),
            "{}",
            activation.message
        );
        if !rich_response && child.label.ends_with("/domain") {
            assert!(
                activation.message.contains("sessionInput :: Int`):\n\n7"),
                "{}",
                activation.message
            );
            assert!(
                activation.message.contains("data Report"),
                "{}",
                activation.message
            );
        }
        presented.insert(activation.id.actor());
    }
    committed(
        root.as_ref(),
        "let sharedAfterUnfold = (\"later parent value\" :: Text)",
    )
    .await;
    for child in &children {
        let inherited = committed(child.policy.as_ref(), "sharedAfterUnfold").await;
        assert_eq!(
            inherited["items"][0]["output"].as_str().unwrap().trim(),
            "ready"
        );
    }
    committed(
        root.as_ref(),
        include_str!("../actor_host_fixtures/generic_actor/response_computation.hs"),
    )
    .await;
    for child in &children {
        let source = if rich_response {
            include_str!("../actor_host_fixtures/generic_actor/rich_response_reply.hs")
        } else if child.label.ends_with("/domain") {
            "respond (Report sessionInput)"
        } else {
            "respond sessionInput"
        };
        let result = dispatch_haskell_script(child.policy.as_ref(), source).await;
        assert_eq!(result["status"], "replied", "{result:?}");
    }
    campaign.await_watch_ready().await;
    let first = committed(root.as_ref(), "pollWatch joined").await;
    let second = committed(root.as_ref(), "pollWatch joined").await;
    for observation in [&first, &second] {
        assert!(observation["items"][0]["output"]
            .as_str()
            .unwrap()
            .starts_with("WatchReady"));
    }
    let saved = first["items"][0]["installedBindings"][0].as_str().unwrap();
    let full = committed(root.as_ref(), &format!("inspectFull ({saved} ())")).await;
    assert!(full["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("ReplyAvailable"));
    committed(
        root.as_ref(),
        "import Tidepool.Actors.Observe (actorContext)\ncontext <- actorContext\ncontextFirstUsage context\ncontextLatestUsage context",
    )
    .await;
    committed(
        root.as_ref(),
        "let worker = responseActor (fst workers)\nlet task = 9 :: Int",
    )
    .await;
    let domain = children
        .iter()
        .find(|child| child.label.ends_with("/domain"))
        .unwrap();
    for document in [
        include_str!("../../../../exomonad/prompts/docs/request.md"),
        include_str!("../../../../exomonad/prompts/docs/deadline.md"),
    ] {
        if rich_response {
            break;
        }
        committed(root.as_ref(), example(document)).await;
        committed(
            root.as_ref(),
            "let readyLabel = \"followup-result\" :: WatchLabel\nready <- watch readyLabel (awaitResponse response)",
        )
        .await;
        let reply =
            dispatch_haskell_script(domain.policy.as_ref(), "respond (Report sessionInput)").await;
        assert_eq!(reply["status"], "replied", "{reply:?}");
        campaign.await_watch_ready().await;
        let result = committed(root.as_ref(), "pollResponse response").await;
        assert!(result["items"][0]["output"]
            .as_str()
            .unwrap()
            .starts_with("ResponseReady"));
        let saved = result["items"][0]["installedBindings"][0].as_str().unwrap();
        let full = committed(root.as_ref(), &format!("inspectFull ({saved} ())")).await;
        assert!(
            full["items"][0]["output"]
                .as_str()
                .unwrap()
                .contains("Report 9"),
            "{full}"
        );
    }
    for child in children {
        child
            .actor
            .shutdown(ActorTerminal {
                kind: if child.label.ends_with("/consumer-tests") {
                    ActorExitKind::Failed
                } else {
                    ActorExitKind::Cancelled
                },
                summary: "documentation scenario terminal evidence".into(),
            })
            .await
            .unwrap();
    }
    let status = dispatch_status(root.as_ref(), "summary").await;
    let status = status["items"][0]["output"].as_str().unwrap();
    assert!(
        status.contains("terminal:Failed")
            && status.contains("documentation scenario terminal evidence"),
        "{status}"
    );
    campaign
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "documentation scenario complete".into(),
        })
        .await
        .unwrap();
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn model_selection_is_independent_of_inherited_and_selected_context() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(root.as_ref(), include_str!("fixtures/model_context.hs")).await;
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    {
        enum Arrival {
            Child(Box<exomonad_actor::LocalResidentInstallation>),
            Ready,
        }
        let mut ready = 0;
        while ready != 2 {
            let arrival = campaign
                .next_deployment(
                    "model-context child admission",
                    Duration::from_secs(120),
                    |event| match event {
                        LocalResidentDeployment::PolicyInstalled(child) => {
                            Ok(Arrival::Child(child))
                        }
                        LocalResidentDeployment::SessionReady { .. } => Ok(Arrival::Ready),
                        other => Err(other),
                    },
                )
                .await;
            match arrival {
                Arrival::Child(child) => {
                    bindings.push(open_test_fork(&campaign, &child));
                    children.push(child);
                }
                Arrival::Ready => ready += 1,
            }
        }
    }
    for child in &children {
        assert_eq!(child.model.as_deref(), Some("gpt-6-sol"));
        assert_eq!(child.supervisor_parent, Some(campaign.actor.identity()));
        if child.label.ends_with("/exact") {
            assert_eq!(child.context_parent, Some(campaign.actor.identity()));
            let inherited = committed(child.policy.as_ref(), "inspectFull parentOnly").await;
            assert_eq!(inherited["items"][0]["output"], "41");
        } else {
            assert_eq!(child.context_parent, None);
            assert_eq!(child.fork_effort, Some(exomonad_actor::ForkEffort::Medium));
            let missing = dispatch_lookup(child.policy.as_ref(), &["parentOnly"]).await;
            assert!(missing.to_string().contains("no match"), "{missing}");
        }
        let reply = dispatch_haskell_script(child.policy.as_ref(), "respond sessionInput").await;
        assert_eq!(reply["status"], "replied", "{reply}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn routes_forward_without_model_relay_and_retain_callback_failure() {
    let mut campaign = TestCampaign::start().await;
    let root = campaign.root_installation.policy.clone();
    committed(root.as_ref(), include_str!("fixtures/route.hs")).await;
    let mut children = Vec::new();
    let mut bindings = Vec::new();
    {
        enum Arrival {
            Child(Box<exomonad_actor::LocalResidentInstallation>),
            Ready,
        }
        let mut ready = 0;
        while ready != 2 {
            let arrival = campaign
                .next_deployment("route child admission", Duration::from_secs(120), |event| {
                    match event {
                        LocalResidentDeployment::PolicyInstalled(child) => {
                            Ok(Arrival::Child(child))
                        }
                        LocalResidentDeployment::SessionReady { .. } => Ok(Arrival::Ready),
                        other => Err(other),
                    }
                })
                .await;
            match arrival {
                Arrival::Child(child) => {
                    bindings.push(open_test_fork(&campaign, &child));
                    children.push(child);
                }
                Arrival::Ready => ready += 1,
            }
        }
    }
    let consumer = children
        .iter()
        .find(|child| child.label.ends_with("/consumer"))
        .unwrap();
    let producer = children
        .iter()
        .find(|child| child.label.ends_with("/producer"))
        .unwrap();
    dispatch_haskell_script(consumer.policy.as_ref(), "respond sessionInput").await;
    dispatch_haskell_script(producer.policy.as_ref(), "respond sessionInput").await;
    let reviewer = {
        enum ReviewArrival {
            Child(Box<exomonad_actor::LocalResidentInstallation>),
            Ready { message: String },
            RouteFailure { detail: String },
        }
        let owner = campaign.actor.identity();
        let mut reviewer = None;
        let mut forwarded = false;
        let mut review_ready = false;
        let mut failure_notified = false;
        loop {
            let arrival = campaign
                .next_deployment(
                    "reviewer admission/activation",
                    Duration::from_secs(120),
                    |event| match event {
                        LocalResidentDeployment::PolicyInstalled(child) => {
                            Ok(ReviewArrival::Child(child))
                        }
                        LocalResidentDeployment::SessionReady { activation } => {
                            Ok(ReviewArrival::Ready {
                                message: activation.message,
                            })
                        }
                        LocalResidentDeployment::WatchChanged { notification }
                            if notification.owner == owner =>
                        {
                            let exomonad_actor::WatchTransition::RouteFailed { detail } =
                                notification.transition
                            else {
                                panic!("successful route woke a model: {notification:?}");
                            };
                            Ok(ReviewArrival::RouteFailure { detail })
                        }
                        other => Err(other),
                    },
                )
                .await;
            match arrival {
                ReviewArrival::Child(child) => {
                    assert_eq!(child.context_parent, None);
                    assert_eq!(child.model.as_deref(), Some("gpt-6-sol"));
                    bindings.push(open_test_fork(&campaign, &child));
                    reviewer = Some(child);
                }
                ReviewArrival::Ready { message } => {
                    if message.contains("review-candidate") {
                        forwarded = true;
                    } else {
                        review_ready = true;
                    }
                }
                ReviewArrival::RouteFailure { detail } => {
                    assert!(detail.contains("deliberate route failure"), "{detail}");
                    assert!(!failure_notified, "route failure notified twice");
                    failure_notified = true;
                }
            }
            if forwarded && review_ready && failure_notified {
                if let Some(reviewer) = reviewer.take() {
                    break reviewer;
                }
            }
        }
    };
    let review = dispatch_haskell_script(reviewer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(review["status"], "replied", "{review}");
    let state = committed(root.as_ref(), "pollRoute forwarding\npollRoute broken").await;
    assert_eq!(state["items"][0]["output"], "RouteCompleted");
    assert!(
        state["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("deliberate route failure"),
        "{state}"
    );
    let recovered = committed(root.as_ref(), "recovered <- listRoutes\ninspectFull (length recovered)\nstates <- traverse pollRoute recovered\ninspectFull states").await;
    assert_eq!(recovered["items"][1]["output"], "3", "{recovered}");
    assert!(
        recovered["items"][3]["output"]
            .as_str()
            .unwrap()
            .contains("deliberate route failure"),
        "{recovered}"
    );
    let foreign = committed(
        consumer.policy.as_ref(),
        "owned <- listRoutes\ninspectFull (length owned)",
    )
    .await;
    assert_eq!(foreign["items"][1]["output"], "0", "{foreign}");
    let reply = dispatch_haskell_script(consumer.policy.as_ref(), "respond sessionInput").await;
    assert_eq!(reply["status"], "replied", "{reply}");
    committed(root.as_ref(), "stopAgent (responseActor producer)").await;
    committed(root.as_ref(), "unavailable <- requestWith @Text (responseActor producer) (assignment forwardedLabel (\"lost target\" :: Text))\nhandled <- route (awaitSettled unavailable) (\\settled -> case settled of { ReplyUnavailable _ -> pure (); ReplyAvailable _ -> error \"unexpected success\" })").await;
    let handled = committed(
        root.as_ref(),
        "pollRoute handled\nforgetRoute forwarding\nforgetRoute broken",
    )
    .await;
    assert_eq!(handled["items"][0]["output"], "RouteCompleted", "{handled}");
    assert_eq!(handled["items"][1]["output"], "WatchForgotten", "{handled}");
    assert_eq!(handled["items"][2]["output"], "WatchForgotten", "{handled}");
    let forgotten = committed(root.as_ref(), "pollRoute broken").await;
    assert_eq!(
        forgotten["items"][0]["output"], "RouteRejected ReplyStale",
        "{forgotten}"
    );
    let retained = committed(
        root.as_ref(),
        "retained <- listRoutes\ninspectFull (length retained)",
    )
    .await;
    assert_eq!(retained["items"][1]["output"], "2", "{retained}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn configured_modules_are_available_to_resident_declarations_from_frozen_sources() {
    let campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(), |admission| admission, |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(authored.join("Project")).unwrap();
            std::fs::write(authored.join("config.toml"), "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Types', 'Project.Work']\n").unwrap();
            std::fs::write(authored.join("Project/Types.hs"), include_str!("fixtures/project/Types.hs")).unwrap();
            std::fs::write(authored.join("Project/Work.hs"), include_str!("fixtures/project/Work.hs")).unwrap();
            config.workspace_inputs = Some(crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root).unwrap());
            std::fs::write(authored.join("Project/Work.hs"), "invalid edited source").unwrap();
        },
    ).await;
    let policy = campaign.root_installation.policy.as_ref();
    let result = committed(policy, "saved <- pure candidate\ninspectFull saved").await;
    assert_eq!(result["items"][1]["output"], "Preparation 7");
    let declaration = committed(policy, "readDelivery :: Delivery -> Int\nreadDelivery (Preparation n) = n\nreadDelivery (Complete n) = n\ninspectFull (readDelivery candidate)").await;
    assert_eq!(declaration["items"][1]["output"], "7");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn work_actor_consumes_later_progress_without_rearming() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/progress-route-producer.hs"),
    )
    .await;
    let (producer, _producer_binding) = next_project_worker(&mut campaign).await;
    committed(
        root.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/progress-route.hs"),
    )
    .await;
    committed(
        producer.policy.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/progress-route-questions.hs"),
    )
    .await;
    for (questions, expected, effects) in [
        ("[first]", "[[\"question-a\"]]", "1"),
        ("[first]", "[[\"question-a\"]]", "1"),
        ("[first,second]", "[[\"question-a\", \"question-b\"]]", "2"),
    ] {
        committed(
            producer.policy.as_ref(),
            &format!("import Tidepool.Agent.Reply (pollReply)\nreportProgress (WorkProgress [] {questions})\npollReply sessionReply"),
        )
        .await;
        let observed = committed(root.as_ref(), "view <- readWork forwarding\ninspectFull (map (map questionKey . workQuestions . sourceProgress) (collectedWork view))\nActor.call wakes (RoutingCount 0 id)").await;
        assert_eq!(observed["items"][1]["output"], expected, "{observed}");
        assert_eq!(observed["items"][2]["output"], effects, "{observed}");
    }
    let replied =
        dispatch_haskell_script(producer.policy.as_ref(), "respond (\"finished\" :: Text)").await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let closed = committed(root.as_ref(), "view <- readWork forwarding\ninspectFull (map Project.Routing.sourceStatus (collectedWork view))\nfinishWork forwarding").await;
    assert_eq!(closed["items"][1]["output"], "[WorkClosed]", "{closed}");
    assert!(
        closed["items"][2]["output"]
            .as_str()
            .unwrap()
            .starts_with("Completed"),
        "{closed}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

async fn workspace_campaign() -> TestCampaign {
    workspace_campaign_with(|_| {}).await
}

async fn workspace_campaign_with(configure: impl FnOnce(&Path)) -> TestCampaign {
    TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let authored = config.workspace.join(".exomonad");
            crate::exomonad::workspace::copy_authored(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exomonad/examples/workspace"),
                &config.workspace,
            )
            .unwrap();
            configure(&authored);
            super::test_campaign::commit_workspace(&config.workspace);
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

#[tokio::test]
async fn usage_comparisons_deduplicate_resumes_and_preserve_unknown_intervals() {
    let campaign = workspace_campaign().await;
    let result = committed(
        campaign.root_installation.policy.as_ref(),
        include_str!("fixtures/usage_comparisons.hs"),
    )
    .await;
    let output = result["items"].as_array().unwrap().last().unwrap()["output"]
        .as_str()
        .unwrap();
    assert!(output.contains("True"), "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn workspace_recipe_modules_and_snapshot_helpers_compile() {
    let campaign = workspace_campaign().await;
    let policy = campaign.root_installation.policy.as_ref();
    let result = committed(
        policy,
        "observed <- snapshot\ninspectFull (swarmUsage observed)",
    )
    .await;
    assert!(
        result["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("unknownActors = [("),
        "{result}"
    );
    let lookup = dispatch_lookup(
        policy,
        &[
            "implement",
            "reviewCandidate",
            "reviewAgain",
            "repair",
            "withDecision",
            "followWork",
        ],
    )
    .await;
    assert_eq!(lookup["status"], "committed", "{lookup}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

async fn next_project_worker(
    campaign: &mut TestCampaign,
) -> (
    exomonad_actor::LocalResidentInstallation,
    Arc<dyn exomonad_actor::ForkWorkspaceCustody>,
) {
    let (installation, custody, _) = next_project_activation(campaign).await;
    (installation, custody)
}

async fn next_project_activation(
    campaign: &mut TestCampaign,
) -> (
    exomonad_actor::LocalResidentInstallation,
    Arc<dyn exomonad_actor::ForkWorkspaceCustody>,
    exomonad_actor::ResidentActivation,
) {
    enum WorkerArrival {
        Child(Box<exomonad_actor::LocalResidentInstallation>),
        Ready(exomonad_actor::ResidentActivation),
    }
    let mut worker: Option<Box<exomonad_actor::LocalResidentInstallation>> = None;
    loop {
        let arrival = campaign
            .next_deployment(
                "worker admission/activation",
                Duration::from_secs(120),
                |event| match event {
                    LocalResidentDeployment::PolicyInstalled(child) => {
                        Ok(WorkerArrival::Child(child))
                    }
                    LocalResidentDeployment::SessionReady { activation }
                        if worker.as_ref().is_some_and(|child| {
                            child.actor.identity() == activation.id.actor()
                        }) =>
                    {
                        Ok(WorkerArrival::Ready(activation))
                    }
                    LocalResidentDeployment::WatchChanged { notification } => {
                        if let exomonad_actor::WatchTransition::RouteFailed { detail } =
                            &notification.transition
                        {
                            panic!("route failed while awaiting worker admission: {detail}");
                        }
                        Err(LocalResidentDeployment::WatchChanged { notification })
                    }
                    LocalResidentDeployment::Retired { actor, terminal } => {
                        panic!(
                            "actor {actor:?} retired while awaiting worker admission: {terminal:?}"
                        );
                    }
                    other => Err(other),
                },
            )
            .await;
        match arrival {
            WorkerArrival::Child(child) => worker = Some(child),
            WorkerArrival::Ready(activation) => {
                let child = worker.take().unwrap();
                let binding = open_test_fork(campaign, &child);
                return (*child, binding, activation);
            }
        }
    }
}

#[tokio::test]
async fn independent_admission_rejects_inheritance_and_supervised_escape() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    let fixture = include_str!("fixtures/independent_worker_setup.hs");
    let inherited = fixture
        .replace("withContext (selected id)", "withContext inherited")
        .replace("peer <- unfold", "peerAttempt <- attemptUnfold");
    committed(root.as_ref(), &inherited).await;
    let result = committed(
        root.as_ref(),
        "inspectFull (either show (const \"unexpected acceptance\") peerAttempt)",
    )
    .await;
    assert!(
        result.to_string().contains("requires a selected context"),
        "{result}"
    );
    committed(root.as_ref(), &fixture.replace("SwarmOwned", "ParentOwned")).await;
    let (worker, _binding) = next_project_worker(&mut campaign).await;
    assert_eq!(
        worker.supervisor_parent,
        Some(campaign.root_installation.actor.identity())
    );
    let attempted = fixture
        .replace("projectHead", "currentCheckout")
        .replace("peer <- unfold", "standaloneAttempt <- attemptUnfold");
    committed(worker.policy.as_ref(), &attempted).await;
    let result = committed(
        worker.policy.as_ref(),
        "inspectFull (either show (const \"unexpected acceptance\") standaloneAttempt)\npollReply sessionReply",
    )
    .await;
    assert!(
        result["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("only a top-level actor"),
        "{result}"
    );
    assert_eq!(result["items"][1]["output"], "ReplyOpen", "{result}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn independent_workers_retain_peer_requests_after_creator_retirement() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        include_str!("fixtures/independent_worker_setup.hs"),
    )
    .await;
    let (worker, _worker_binding) = next_project_worker(&mut campaign).await;
    assert!(worker.supervisor_parent.is_none());
    assert!(worker.context_parent.is_none());
    assert_eq!(
        worker.creator,
        Some(campaign.root_installation.actor.identity())
    );
    let result =
        dispatch_haskell_script(worker.policy.as_ref(), "respond (\"ready\" :: Text)").await;
    assert_eq!(result["status"], "replied", "{result}");
    committed(
        root.as_ref(),
        include_str!("fixtures/independent_peer_setup.hs"),
    )
    .await;
    let (observer, _observer_binding) = next_project_worker(&mut campaign).await;
    assert!(observer.supervisor_parent.is_none());
    assert_eq!(observer.creator, worker.creator);
    let unshared = committed(observer.policy.as_ref(), "let retainedPeer = sessionInput\nvisibleBefore <- snapshot\ninspectFull (length (snapshotActors visibleBefore))\nshareObservation retainedPeer retainedPeer").await;
    assert_eq!(unshared["items"][2]["output"], "1", "{unshared}");
    assert_eq!(
        unshared["items"][3]["output"], "ObservationUnauthorized",
        "{unshared}"
    );
    assert_eq!(
        campaign
            .forest
            .inspect_graph(observer.actor.identity())
            .unwrap()
            .len(),
        1
    );
    let status = dispatch_status(observer.policy.as_ref(), "lineage").await;
    assert!(
        !status["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains(&worker.label),
        "{status}"
    );
    let shared = committed(
        root.as_ref(),
        "shareObservation (responseActor peerObserver) (responseActor peer)",
    )
    .await;
    assert_eq!(
        shared["items"][0]["output"], "ObservationShared",
        "{shared}"
    );
    let observed = committed(observer.policy.as_ref(), "visibleAfter <- snapshot\ninspectFull (length (snapshotActors visibleAfter))\nstopAgent retainedPeer").await;
    assert_eq!(observed["items"][1]["output"], "2", "{observed}");
    assert_eq!(
        observed["items"][2]["output"], "StopUnauthorized",
        "{observed}"
    );
    assert_eq!(
        campaign
            .forest
            .inspect_graph(observer.actor.identity())
            .unwrap()
            .len(),
        2
    );
    let status = dispatch_status(observer.policy.as_ref(), "lineage").await;
    assert!(
        status["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains(&worker.label),
        "{status}"
    );
    let result =
        dispatch_haskell_script(observer.policy.as_ref(), "respond (\"ready\" :: Text)").await;
    assert_eq!(result["status"], "replied", "{result}");
    campaign
        .root_installation
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "planner finished".into(),
        })
        .await
        .unwrap();
    assert!(worker.actor.terminal().get().is_none());
    assert!(observer.actor.terminal().get().is_none());
    committed(observer.policy.as_ref(), "let followupLabel = [label|peer-followup|]\nfollowup <- request @Text retainedPeer (assignment followupLabel (\"after planner retirement\" :: Text))").await;
    let worker_actor = worker.actor.identity();
    campaign
        .next_deployment(
            "peer-followup request activation",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == worker_actor =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let result = dispatch_haskell_script(
        worker.policy.as_ref(),
        "respond (sessionInput <> \" accepted\" :: Text)",
    )
    .await;
    assert_eq!(result["status"], "replied", "{result}");
    let result = committed(
        observer.policy.as_ref(),
        "settled <- pollResponse followup\ninspectFull settled",
    )
    .await;
    assert!(
        result["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("after planner retirement accepted"),
        "{result}"
    );
    worker
        .actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "peer finished".into(),
        })
        .await
        .unwrap();
    let stale = committed(
        observer.policy.as_ref(),
        "shareObservation retainedPeer retainedPeer",
    )
    .await;
    assert_eq!(
        stale["items"][0]["output"], "ObservationRecipientUnavailable",
        "{stale}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    assert_eq!(
        worker.actor.terminal().get().unwrap().kind,
        ActorExitKind::Completed
    );
    assert_eq!(
        observer.actor.terminal().get().unwrap().kind,
        ActorExitKind::Cancelled
    );
    assert!(campaign
        .forest
        .new_workbench(
            "after-swarm-stop".into(),
            exomonad_actor::EffectiveRole::root()
        )
        .await
        .is_err());
}

#[tokio::test]
async fn project_review_retains_evidence_and_owns_direct_repair() {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    let source = campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        &format!("let sourceHead = GitOid \"{}\"", source.as_str()),
    )
    .await;
    committed(
        root.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_delivery_setup.hs"),
    )
    .await;
    let (implementer, _implementer_binding) = next_project_worker(&mut campaign).await;
    assert_eq!(
        implementer.instructions.as_deref(),
        Some(include_str!(
            "../../../../exomonad/examples/workspace/.exomonad/prompts/task.md"
        ))
    );
    let tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &implementer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let candidate = campaign
        ._repository
        .writer_at(tree.cwd())
        .commit_file("feature.txt", "candidate\n", "implement feature")
        .unwrap();
    let replied = dispatch_haskell_script(
        implementer.policy.as_ref(),
        &format!(
            "respond (Produced (Candidate (GitOid \"{}\") [\"focused candidate check\"] [\"open product gate\"]))",
            candidate.as_str()
        ),
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    committed(
        root.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_review_start.hs"),
    )
    .await;
    let (reviewer, _reviewer_binding) = next_project_worker(&mut campaign).await;
    let review_instructions =
        include_str!("../../../../exomonad/examples/workspace/.exomonad/prompts/review.md");
    assert_eq!(reviewer.instructions.as_deref(), Some(review_instructions));
    let launched = super::developer_instructions_selected(
        &reviewer.effective_role,
        &exomonad_agent::InteractiveLaunchMode::Fresh,
        None,
        reviewer.instructions.as_deref(),
    );
    assert!(launched.starts_with(review_instructions));
    assert!(launched.contains("Runtime policy ("));
    let evidence = committed(reviewer.policy.as_ref(), "inspectFull (reviewInput sessionInput)\nlet RetainedImplementer repairTarget = repairOwner sessionInput\ninspectFull (agentIdentity repairTarget)").await;
    assert!(
        evidence["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("focused candidate check"),
        "{evidence}"
    );
    assert!(
        evidence["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("open product gate"),
        "{evidence}"
    );
    assert_eq!(
        evidence["items"][2]["output"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect::<String>(),
        format!(
            "({},{})",
            implementer.actor.identity().id.0,
            implementer.actor.identity().incarnation.0
        )
    );
    committed(
        reviewer.policy.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_review_repair.hs"),
    )
    .await;
    let pending = committed(
        reviewer.policy.as_ref(),
        "import Tidepool.Agent.Reply (pollReply)\npollReply sessionReply",
    )
    .await;
    assert_eq!(pending["items"][1]["output"], "ReplyOpen", "{pending}");
    let implementer_actor = implementer.actor.identity();
    campaign
        .next_deployment(
            "repair request activation",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == implementer_actor =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let repair_packet = committed(
        implementer.policy.as_ref(),
        "inspectFull (taskSource (repairAssignment sessionInput), repairInput sessionInput, repairFindings sessionInput)",
    ).await;
    for expected in [
        source.as_str(),
        candidate.as_str(),
        "open product gate",
        "preserve the product gate",
    ] {
        assert!(
            repair_packet.to_string().contains(expected),
            "missing {expected} from typed repair packet: {repair_packet}"
        );
    }
    let repair = committed(implementer.policy.as_ref(), "inspectFull sessionInput").await;
    assert!(
        repair["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("preserve the product gate"),
        "{repair}"
    );
    let revised = campaign
        ._repository
        .writer_at(tree.cwd())
        .commit_file("feature.txt", "repaired\n", "repair feature")
        .unwrap();
    let replied = dispatch_haskell_script(
        implementer.policy.as_ref(),
        &format!(
            "respond (Produced (Candidate (GitOid \"{}\") [\"focused repair check\"] [\"open product gate\"]))",
            revised.as_str()
        ),
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    let result = committed(reviewer.policy.as_ref(), "state <- pollWatch repaired\ninspectFull (fmap (either (const False) (const True) . settledValue) state)").await;
    assert_eq!(result["items"][1]["output"], "WatchReady True", "{result}");
    committed(
        reviewer.policy.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_design_question.hs"),
    )
    .await;
    let (expert, _expert_binding) = next_project_worker(&mut campaign).await;
    assert_eq!(expert.model.as_deref(), Some("planner"));
    assert_eq!(expert.fork_effort, Some(exomonad_actor::ForkEffort::Medium));
    assert_eq!(expert.supervisor_parent, Some(reviewer.actor.identity()));
    assert_eq!(expert.context_parent, None);
    let question = committed(expert.policy.as_ref(), "inspectFull sessionInput").await;
    for expected in [
        revised.as_str(),
        "focused repair check",
        "feature review",
        "retain the gate",
    ] {
        assert!(
            question.to_string().contains(expected),
            "missing {expected}: {question}"
        );
    }
    let expert_tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &expert.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    let amendment = campaign
        ._repository
        .writer_at(expert_tree.cwd())
        .commit_file(
            "plans/feature.md",
            "Preparation retains the open product gate.\n",
            "clarify acceptance",
        )
        .unwrap();
    let answered = dispatch_haskell_script(
        expert.policy.as_ref(),
        &format!("respond (AmendPlan (PlanAmendment (GitOid \"{}\") (GitOid \"{}\") [\"plans/feature.md\"] \"retain the preparation gate\" [\"feature review\"] [\"boundary evidence\"]))", revised.as_str(), amendment.as_str()),
    ).await;
    assert_eq!(answered["status"], "replied", "{answered}");
    let decision = committed(reviewer.policy.as_ref(), "design <- pollWatch designReady\ninspectFull (fmap settledValue design)\npollReply sessionReply").await;
    assert!(
        decision["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("retain the preparation gate"),
        "{decision}"
    );
    assert_eq!(decision["items"][2]["output"], "ReplyOpen", "{decision}");
    committed(
        reviewer.policy.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_plan_incorporation.hs"),
    )
    .await;
    let implementer_actor = implementer.actor.identity();
    campaign
        .next_deployment(
            "incorporation request activation",
            Duration::from_secs(120),
            move |event| match event {
                LocalResidentDeployment::SessionReady { activation }
                    if activation.id.actor() == implementer_actor =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await;
    let offered = committed(
        implementer.policy.as_ref(),
        "inspectFull (incorporationAmendment sessionInput)",
    )
    .await;
    assert!(
        offered.to_string().contains(amendment.as_str()),
        "{offered}"
    );
    let git = exomonad_worktree::git::GitCli::new();
    git.run(tree.cwd(), &["merge", "--ff-only", amendment.as_str()])
        .unwrap();
    let incorporated_head = git.run(tree.cwd(), &["rev-parse", "HEAD"]).unwrap();
    assert_eq!(
        std::fs::read_to_string(tree.cwd().join("plans/feature.md")).unwrap(),
        "Preparation retains the open product gate.\n"
    );
    let incorporated = dispatch_haskell_script(implementer.policy.as_ref(),
        &format!("respond (Incorporated (incorporationAmendment sessionInput) (GitOid \"{}\") [\"read exact plan at resulting head\"])", incorporated_head.trimmed())).await;
    assert_eq!(incorporated["status"], "replied", "{incorporated}");
    let checked = committed(reviewer.policy.as_ref(), "incorporation <- pollWatch planReady\ninspectFull (fmap settledValue incorporation)\npollReply sessionReply").await;
    for expected in [
        "Incorporated",
        incorporated_head.trimmed(),
        "read exact plan at resulting head",
    ] {
        assert!(
            checked["items"][1]["output"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{checked}"
        );
    }
    assert_eq!(checked["items"][2]["output"], "ReplyOpen", "{checked}");
    let questions = committed(
        reviewer.policy.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_review_questions.hs"),
    )
    .await;
    assert_eq!(
        questions["items"].as_array().unwrap().last().unwrap()["output"],
        "ReplyOpen",
        "{questions}"
    );
    committed(
        root.as_ref(),
        &format!(
            "let incorporatedHead = GitOid \"{}\"",
            incorporated_head.trimmed()
        ),
    )
    .await;
    let pending = committed(
        root.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_decision_return.hs"),
    )
    .await;
    assert!(pending.to_string().contains("ResponsePending"), "{pending}");
    // Carries the producing actor's own progress, so this poll answers "is
    // it moving" without a second round trip.
    assert!(pending.to_string().contains("state="), "{pending}");
    let delivery = campaign
        .next_deployment(
            "decision request update",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::RequestUpdate { delivery } => Ok(delivery),
                LocalResidentDeployment::SessionReady { activation } => {
                    panic!("decision queued a new obligation: {:?}", activation.id)
                }
                other => Err(other),
            },
        )
        .await;
    let presentation = delivery.begin().unwrap();
    assert!(presentation
        .message()
        .contains("Preparation retains the boundary"));
    assert!(presentation.message().contains(incorporated_head.trimmed()));
    presentation.presented();
    let status = committed(root.as_ref(), "pollRequestUpdate clarification").await;
    assert_eq!(
        status["items"][0]["output"], "Right UpdatePresented",
        "{status}"
    );
    // Source incorporation remains distinct from presenting the accepted decision.
    let review_tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &reviewer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    git.run(
        review_tree.cwd(),
        &["merge", "--ff-only", incorporated_head.trimmed()],
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(review_tree.cwd().join("plans/feature.md")).unwrap(),
        "Preparation retains the open product gate.\n"
    );
    let propagated = committed(
        reviewer.policy.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/project_decision_consumer.hs"),
    )
    .await;
    assert!(
        propagated.to_string().contains("(True,True,True,True)"),
        "{propagated}"
    );
    assert_eq!(
        propagated["items"].as_array().unwrap().last().unwrap()["output"],
        "ReplyOpen",
        "{propagated}"
    );
    let (consumer, _consumer_binding, activation) = next_project_activation(&mut campaign).await;
    assert_eq!(consumer.context_parent, None);
    let selected = &activation.message;
    for expected in [
        incorporated_head.trimmed(),
        "Preparation retains the boundary",
        "read exact plan at resulting head",
        "Why:",
        "Preserve the product gate",
    ] {
        assert!(
            selected.contains(expected),
            "missing {expected} from fresh context: {selected}"
        );
    }
    let consumer_context = committed(
        consumer.policy.as_ref(),
        "inspectFull (taskContext sessionInput)",
    )
    .await;
    assert!(
        consumer_context
            .to_string()
            .contains(incorporated_head.trimmed()),
        "{consumer_context}"
    );
    let consumer_tree = campaign
        .worktrees
        .lookup(&exomonad_worktree::WorktreeId::from_raw(
            &consumer.launch_worktrees[0],
        ))
        .unwrap()
        .unwrap();
    assert_eq!(
        git.run(consumer_tree.cwd(), &["rev-parse", "HEAD"])
            .unwrap()
            .trimmed(),
        incorporated_head.trimmed()
    );
    let attention = committed(
        root.as_ref(),
        "remaining <- pollProgress reviewQuestions\ninspectFull remaining",
    )
    .await;
    assert!(
        attention.to_string().contains("product-gate"),
        "{attention}"
    );
    assert!(
        !attention["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains("questionKey = \"semantics\""),
        "answered question survived: {attention}"
    );
    let original = committed(
        root.as_ref(),
        "original <- pollResponse worker\ninspectFull original",
    )
    .await;
    assert!(
        original["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains(candidate.as_str()),
        "{original}"
    );
    assert!(
        !original["items"][1]["output"]
            .as_str()
            .unwrap()
            .contains(revised.as_str()),
        "repair changed the original response: {original}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn route_finishes_owned_request_without_a_model_relay() {
    route_reply_case(false).await;
}

#[tokio::test]
async fn route_reply_preserves_request_cancellation() {
    route_reply_case(true).await;
}

async fn route_reply_case(cancel: bool) {
    let mut campaign = workspace_campaign().await;
    campaign._repository.writer().stage(".exomonad").unwrap();
    let source = campaign
        ._repository
        .writer()
        .commit_empty("workspace program")
        .unwrap();
    let root = campaign.root_installation.policy.clone();
    committed(
        root.as_ref(),
        "let routeCampaign = \"route-reply\" :: CampaignLabel",
    )
    .await;
    committed(
        root.as_ref(),
        &format!("let sourceHead = GitOid \"{}\"", source.as_str()),
    )
    .await;
    committed(
        root.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/route-reply-setup.hs"),
    )
    .await;
    let (lead, _lead_binding) = next_project_worker(&mut campaign).await;
    committed(
        lead.policy.as_ref(),
        include_str!("../../../../.exomonad/workspace/checks/route-reply-worker.hs"),
    )
    .await;
    let (worker, _worker_binding) = next_project_worker(&mut campaign).await;
    if cancel {
        let result = committed(root.as_ref(), "cancelRequest lead").await;
        assert!(
            result.to_string().contains("CancellationRequested"),
            "{result}"
        );
    }
    let replied = dispatch_haskell_script(
        worker.policy.as_ref(),
        "respond (Candidate (GitOid \"exact-candidate\") [\"checked\"] [\"open gate\"])",
    )
    .await;
    assert_eq!(replied["status"], "replied", "{replied}");
    // Observe the requester on success: the lead never needs a relay turn.
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = if cancel {
                committed(lead.policy.as_ref(), "pollRoute forwarding").await
            } else {
                committed(
                    root.as_ref(),
                    "answer <- pollResponse lead\ninspectFull answer",
                )
                .await
            };
            let output = result.to_string();
            if output.contains("exact-candidate") || output.contains("RouteFailed") {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    if cancel {
        assert!(
            outcome.to_string().contains("CancellationRequested"),
            "{outcome}"
        );
        let pending = committed(
            lead.policy.as_ref(),
            "import Tidepool.Agent.Reply (pollReply)\npollReply sessionReply",
        )
        .await;
        assert!(
            pending.to_string().contains("ReplyCancellationRequested"),
            "{pending}"
        );
    } else {
        let response = outcome;
        let route = committed(lead.policy.as_ref(), "pollRoute forwarding").await;
        assert!(route.to_string().contains("RouteCompleted"), "{route}");
        assert!(
            response.to_string().contains("exact-candidate"),
            "{response}"
        );
        assert!(response.to_string().contains("open gate"), "{response}");
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn frozen_prompt_bytes_round_trip_through_haskell() {
    let campaign = workspace_campaign_with(|authored| {
        let config = authored.join("config.toml");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("literal = \"prompts/literal.md\"\n");
        std::fs::write(config, text).unwrap();
        std::fs::write(
            authored.join("prompts/literal.md"),
            "\u{1}f\0".to_owned() + "9\n\"\\\tλ\u{7f}",
        )
        .unwrap();
    })
    .await;
    let result = committed(
        campaign.root_installation.policy.as_ref(),
        "inspectFull (fmap (map fromEnum . T.unpack) (workspacePrompt \"literal\"))",
    )
    .await;
    // The list layout breaks a line after every comma (`treeParts`, for
    // line-based paging); this assertion cares about the exact byte values
    // round-tripping, not the display's line breaks, so it strips them
    // before comparing.
    let output = result["items"][0]["output"]
        .as_str()
        .unwrap_or_else(|| panic!("{result}"));
    assert_eq!(
        output.replace('\n', ""),
        "Just [1,102,0,57,10,34,92,9,955,127]",
        "{result}"
    );
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

fn recipe_workspace(checks: Option<&[&str]>) -> tempfile::TempDir {
    let repository = tempfile::tempdir().unwrap();
    // A candidate is a project, and a project is a Git tree: that is how `nix`
    // reads the `flake.nix` a package's pinned Haskell source is named in.
    let git = exomonad_worktree::GitCli::new();
    git.try_run(repository.path(), &["init", "--quiet"])
        .unwrap();
    git.try_run(
        repository.path(),
        &["config", "user.name", "Exomonad recipe check"],
    )
    .unwrap();
    git.try_run(
        repository.path(),
        &["config", "user.email", "recipe-check@localhost"],
    )
    .unwrap();
    crate::exomonad::workspace::copy_authored(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../exomonad/examples/workspace"),
        repository.path(),
    )
    .unwrap();
    if let Some(checks) = checks {
        let path = repository.path().join(".exomonad/config.toml");
        let mut config: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        config["haskell"]["checks"] = toml::Value::Array(
            checks
                .iter()
                .map(|entry| toml::Value::String((*entry).into()))
                .collect(),
        );
        std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    }
    super::test_campaign::commit_workspace(repository.path());
    repository
}

/// `exomonad check --workspace` resolves the installed spec's required
/// effects against every launchable child role's effect row and fails
/// closed, naming the role and the effect, instead of letting a spec a child
/// cannot satisfy compile clean and fail only once a child is admitted (the
/// `Journal` friction the preflight exists to catch before that fork).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_check_refuses_a_spec_no_child_role_can_satisfy() {
    let repository = recipe_workspace(None);
    let spec = repository.path().join(".exomonad/AgentSpec.hs");
    let original = std::fs::read_to_string(&spec).unwrap();
    let widened = original
        .replacen(
            "import Tidepool.Effects.Core (ActorContext, Commands, Jev, Lookup, Notifications, Reflect)",
            "import Tidepool.Effects.Core (ActorContext, Commands, Jev, Lookup, Notifications, Reflect)\nimport Tidepool.Effects (Journal)",
            1,
        )
        .replacen(
            "Member Reflect effects\n  ) =>",
            "Member Reflect effects, Member Journal effects\n  ) =>",
            1,
        );
    assert_ne!(
        widened, original,
        "fixture's AgentSpec.hs no longer matches either replaced string"
    );
    std::fs::write(&spec, widened).unwrap();
    super::test_campaign::commit_workspace(repository.path());
    let error = crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("role research lacks effect Journal required by AgentSpec.agentSpec"),
        "{message}"
    );
    assert!(
        message.contains("role coding lacks effect Journal required by AgentSpec.agentSpec"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn candidate_workspace_runs_its_own_model_free_recipes() {
    let repository = recipe_workspace(None);
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn recipe_checks_reject_a_candidate_only_defect_and_accept_its_repair() {
    let repository = recipe_workspace(Some(&["Project.CollaborationChecks.collaboration"]));
    let work = repository
        .path()
        .join(".exomonad/checks/project_decision_consumer.hs");
    let original = std::fs::read_to_string(&work).unwrap();
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
    let broken = original.replacen(
        "resolveQuestion acceptedDecision changedQuestions == changedQuestions",
        "resolveQuestion acceptedDecision changedQuestions /= changedQuestions",
        1,
    );
    assert_ne!(broken, original, "fixture no longer matches replaced text");
    std::fs::write(&work, broken).unwrap();
    // This candidate fixture is data, not a compiled module; it still compiles,
    // and the running selected worker must expose its defect.
    crate::exomonad::check(Some(repository.path().to_path_buf()), false)
        .await
        .unwrap();
    let error = crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("an old answer cannot clear a changed question or rewind the task source"),
        "{error}"
    );
    std::fs::write(&work, original).unwrap();
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

/// A recipe module that fails to COMPILE (not merely fails an assertion) must
/// still surface GHC's own diagnostic text — file:line:col, severity, and
/// message — not just `CompileError`'s one-line "N diagnostic(s)" summary.
/// Regression for the dev-friction bug where a lane had to reconnect to the
/// compile daemon by hand to see what GHC actually said.
#[tokio::test(flavor = "multi_thread")]
async fn recipe_check_compile_failure_reports_the_ghc_diagnostic_text() {
    let repository = recipe_workspace(Some(&["Project.CollaborationChecks.collaboration"]));
    let work = repository.path().join(".exomonad/Project/Checks.hs");
    let original = std::fs::read_to_string(&work).unwrap();
    let broken = original.replace(
        "readFile actor (checkSource name) >>= void . turn actor",
        "readFile actor (checkSource name) >>= void . undefinedRecipeIdentifierXyz",
    );
    assert_ne!(broken, original, "fixture no longer matches replaced text");
    std::fs::write(&work, broken).unwrap();
    super::test_campaign::commit_workspace(repository.path());
    let error = crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("Haskell compilation failed") || message.contains("compilation failed"),
        "{message}"
    );
    assert!(
        message.contains("undefinedRecipeIdentifierXyz") && message.contains("not in scope"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn candidate_routing_recipes_exercise_failure_and_attention() {
    let repository = recipe_workspace(Some(&["Project.RoutingChecks.routing"]));
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_handoff_recipe_integrates_later_final_heads_from_both_lanes() {
    let repository = recipe_workspace(Some(&["Project.RoutingChecks.twoLaneHandoff"]));
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn attention_actor_recipe_retains_independent_sources_through_closure() {
    let repository = recipe_workspace(Some(&["Project.RoutingChecks.independentSources"]));
    crate::exomonad::check(Some(repository.path().to_path_buf()), true)
        .await
        .unwrap();
}

#[tokio::test]
async fn work_router_queries_receipts_as_the_issuing_actor() {
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            let package = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .unwrap()
                .join("exomonad/examples/workspace");
            crate::exomonad::workspace::copy_authored(&package, &config.workspace).unwrap();
            super::test_campaign::commit_workspace(&config.workspace);
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
    committed(root.as_ref(), include_str!("work_notification.hs")).await;
    let source = campaign
        .next_deployment(
            "the progress source",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::PolicyInstalled(source) => Ok(source),
                other => Err(other),
            },
        )
        .await;
    campaign
        .next_deployment(
            "the source request activation",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::SessionReady { .. } => Ok(()),
                other => Err(other),
            },
        )
        .await;
    let publisher = source.policy.clone();
    let publication = tokio::spawn(async move {
        committed(publisher.as_ref(),
            "reportProgress (WorkProgress [] [Question \"decision\" (DesignQuestion \"plans/test.md\" (GitOid \"candidate\") \"choose the boundary\" [] [] [])])"
        ).await
    });
    let message = campaign
        .next_deployment(
            "the router's native message",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationSend(message) => Ok(message),
                other => Err(other),
            },
        )
        .await;
    let sender = message.owner();
    assert_ne!(sender, campaign.actor.identity());
    assert_eq!(message.target(), campaign.actor.identity());
    let directory = tempfile::tempdir().unwrap();
    let inbox = ActorInbox::open(
        directory.path().join("rows"),
        directory.path().join("cursor"),
    )
    .unwrap();
    let key = "work-router-inbox";
    admit_notification(&message, key.into(), &inbox);
    publication.await.unwrap();
    let wrong_owner = committed(root.as_ref(),
        "view <- readWork collector\nlet [receipt] = [r | Notice _ (Right r) <- workNotices view]\npollNotification receipt"
    ).await;
    assert!(
        wrong_owner.to_string().contains("NotificationUnauthorized"),
        "{wrong_owner}"
    );
    let policy = root.clone();
    let query = tokio::spawn(async move {
        committed(
            policy.as_ref(),
            "R.call (workNotification (R.client collector)) receipt",
        )
        .await
    });
    let poll = campaign
        .next_deployment(
            "a receipt query without another message",
            Duration::from_secs(120),
            |event| match event {
                LocalResidentDeployment::NotificationPoll(poll) => Ok(poll),
                other => Err(other),
            },
        )
        .await;
    assert_eq!(poll.owner(), sender);
    let observed = observe_notification_receipt(&poll, campaign.actor.identity(), key, &inbox);
    assert_eq!(observed, Ok(exomonad_actor::NotificationState::Accepted));
    poll.observed(observed);
    let result = query.await.unwrap();
    assert!(
        result.to_string().contains("NotificationAccepted"),
        "{result}"
    );
    let replaced = committed(root.as_ref(),
        "collector <- R.replace collector (workDefinition sources (notifyWork owner (workMessage id)))\nR.call (workNotification (R.client collector)) receipt"
    ).await;
    assert!(
        replaced.to_string().contains("NotificationUnauthorized"),
        "{replaced}"
    );
    let retained = committed(
        root.as_ref(),
        "inspectFull . length . workNotices <$> readWork collector",
    )
    .await;
    assert_eq!(retained["items"][0]["output"], "1", "{retained}");
    committed(root.as_ref(), "finishWork collector").await;
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
