//! A result too large to materialize is still a binding.
//!
//! The observation budget (`RunOptions::observation_budget`, 100_000) bounds
//! turning a heap value into a host `Value`. It charges one unit per value
//! node AND per copied payload byte (`prepared_program/observe.rs`'s
//! `ObservationBudget::charge_bytes`), so it is really a ~100 KB ceiling.
//!
//! Persistent bindings and bare observations retain the original heap handle.
//! Neither path materializes an authored value for presentation; `display`
//! publishes a bounded page separately through the durable output owner.

use super::command_jobs_tests::backend_request;
use super::command_test_support::TestCommands;
use super::jev_tests::{selected_shell_workspace, SectionScoreJev};
use super::test_campaign::TestCampaign;
use super::tests::{dispatch_haskell_script, dispatch_structured_tool};
use super::*;

/// Comfortably past the budget, and far enough past that no accounting
/// detail decides the outcome.
const OVERSIZED_BYTES: usize = 400_000;

/// One turn's worth of transcript. Three of these is ~600 KB, the shape
/// `reflect 3` actually sees in a session that has been reading files.
fn bulky_turn(index: usize) -> exomonad_actor::ConversationTurn {
    exomonad_actor::ConversationTurn {
        turn: format!("turn-{index}"),
        started_at: None,
        completed_at: None,
        items: vec![
            exomonad_actor::TurnItem::Message {
                role: exomonad_actor::ConversationRole::Assistant,
                text: format!("turn {index} reasoning"),
            },
            exomonad_actor::TurnItem::ToolResult {
                call: format!("call-{index}"),
                output: "e".repeat(OVERSIZED_BYTES / 2),
            },
        ],
    }
}

#[tokio::test]
async fn an_effectful_result_past_the_observation_budget_still_binds() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    // One unit: a committed command job whose output is the oversized
    // payload. This is the live cell's shape — read sources, keep them in the
    // bound value — with the reading reduced to a single command.
    const CELL: &str = "transcript <- do\n  \
        job <- Cmd.quiet (Cmd.run (Cmd.argv [\"cat\", \"sources.txt\"]))\n  \
        pure (either (const \"\") id (Cmd.stdout job))";
    let mut running =
        tokio::spawn(async move { dispatch_haskell_script(policy.as_ref(), CELL).await });
    let backend = TestCommands::completed(&"x".repeat(OVERSIZED_BYTES));
    tokio::select! {
        request = backend_request(campaign) => request.supply(Ok(backend.clone())),
        result = &mut running => panic!("the cell ended before requesting a command backend: {result:?}"),
    }
    let bound = running.await.unwrap();
    assert_eq!(
        bound["status"], "committed",
        "an oversized result must not reject the unit that committed effects to produce it: {bound}"
    );

    // The point of the binding: Haskell can still use it. Reading its length
    // forces the whole value the budget refused to materialize.
    let policy = campaign.root_installation.policy.clone();
    let used = dispatch_haskell_script(policy.as_ref(), "if T.length transcript == 400000 then pure () else error \"retained transcript was truncated\"").await;
    assert_eq!(used["status"], "committed", "{used}");

    // And the committed effect was not replayed to get it back.
    assert_eq!(
        backend.executions(),
        1,
        "recovering the value must not re-run the command that produced it"
    );
})).await;
}

#[tokio::test]
async fn an_oversized_bare_expression_retains_the_whole_value_without_presentation() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    // Capture a bare expression after its producing command has committed.
    const CELL: &str = "transcript <- do\n  \
        job <- Cmd.quiet (Cmd.run (Cmd.argv [\"cat\", \"sources.txt\"]))\n  \
        pure (either (const \"\") id (Cmd.stdout job))";
    let mut running =
        tokio::spawn(async move { dispatch_haskell_script(policy.as_ref(), CELL).await });
    let backend = TestCommands::completed(&"x".repeat(OVERSIZED_BYTES));
    tokio::select! {
        request = backend_request(campaign) => request.supply(Ok(backend.clone())),
        result = &mut running => panic!("the cell ended before requesting a command backend: {result:?}"),
    }
    assert_eq!(running.await.unwrap()["status"], "committed");

    let policy = campaign.root_installation.policy.clone();
    let shown = dispatch_haskell_script(policy.as_ref(), "transcript").await;
    assert_eq!(
        shown["status"], "committed",
        "an oversized bare expression must retain its value: {shown}"
    );
    let item = &shown["items"][0];
    let name = item["installedBindings"][0]
        .as_str()
        .unwrap_or_else(|| panic!("a bare expression must leave a binding: {shown}"))
        .to_owned();
    assert!(
        item["operations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|operation| operation.get("display").is_none()),
        "{shown}"
    );
    campaign.assert_no_deployment("retaining a bare value must not publish output", |event| {
        matches!(event, LocalResidentDeployment::DisplayPublished(_))
    });
    let used = dispatch_haskell_script(policy.as_ref(), &format!("if T.length ({name} ()) == 400000 then pure () else error \"bare value was truncated\"")).await;
    assert_eq!(used["status"], "committed", "{used}");

    // None of that re-ran the command whose output is being shown.
    assert_eq!(
        backend.executions(),
        1,
        "displaying a committed result must not replay the effect that produced it"
    );
})).await;
}

#[tokio::test]
async fn reflect_binds_history_larger_than_the_observation_budget() {
    let turns: Vec<_> = (0..3).map(bulky_turn).collect();
    let reader: exomonad_actor::ConversationReader = Arc::new(move |_actor, count| {
        let turns = turns.clone();
        Box::pin(async move { Ok(turns.into_iter().take(count).collect()) })
    });
    let campaign =
        TestCampaign::start_with_conversation(|admission| admission, |_| {}, Some(reader)).await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();

    let bound = dispatch_haskell_script(policy.as_ref(), "editorialContext <- reflect 3").await;
    assert_eq!(
        bound["status"], "committed",
        "`reflect 3` must bind its history rather than exhaust the budget: {bound}"
    );

    // Bound, and usable as the api-guide promises: "Bind it once and reuse
    // the value as the context argument for the questions that follow."
    // `reflect` answers `Either ReflectError [ConversationTurn]`; a `Left`
    // counts as zero turns so it fails the assertion below rather than this one.
    let used = dispatch_haskell_script(policy.as_ref(), "if either (const 0) length editorialContext == 3 then pure () else error \"reflect lost requested turns\"").await;
    assert_eq!(used["status"], "committed", "{used}");
})).await;
}

#[tokio::test]
async fn accepted_stdin_is_acknowledged_even_when_presentation_would_exhaust_observation() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let large_context = Arc::new(AtomicBool::new(false));
    let should_be_large = Arc::clone(&large_context);
    let turn = exomonad_actor::ConversationTurn {
        turn: "oversized-turn".into(),
        started_at: None,
        completed_at: None,
        items: vec![exomonad_actor::TurnItem::ToolResult {
            call: "large-result".into(),
            output: "e".repeat(OVERSIZED_BYTES),
        }],
    };
    let reader: exomonad_actor::ConversationReader = Arc::new(move |_actor, count| {
        let turns = if should_be_large.load(Ordering::Acquire) {
            vec![turn.clone()]
        } else {
            Vec::new()
        };
        Box::pin(async move { Ok(turns.into_iter().take(count).collect()) })
    });
    let campaign = TestCampaign::start_with_conversation(
        |admission| admission,
        super::test_campaign::configure_shell_workspace,
        Some(reader),
    )
    .await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = campaign.root_installation.policy.clone();
    let bash = {
        let policy = policy.clone();
        tokio::spawn(async move {
            policy
                .dispatch_json_boxed(exomonad_tool::ToolInvocation {
                    context: None,
                    name: "bash".into(),
                    arguments: exomonad_tool::ToolArguments::Structured(serde_json::json!({
                        "cmd":"cat", "stdin":true, "yield_time_ms":0
                    })),
                })
                .await
        })
    };
    let backend = TestCommands::new();
    backend_request(campaign)
        .await
        .supply(Ok(backend.clone()));
    let started = bash.await.unwrap().unwrap();
    assert_eq!(started["status"], "committed", "{started}");
    assert!(
        backend.output_budgets().is_empty(),
        "a command returned immediately as a session has no output to preview yet"
    );
    let session = started["items"][0]["output"]
        .as_str()
        .unwrap()
        .split("session_id: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();

    let preview = dispatch_haskell_script(
        policy.as_ref(),
        "import qualified Tidepool.Command as Cmd\nobserved <- Cmd.observeWith (Cmd.Observation 0 32768) job1 (\\_ -> pure \"\")",
    )
    .await;
    assert_eq!(preview["status"], "committed", "{preview}");
    assert_eq!(
        backend.output_budgets().first().copied(),
        Some(16 * 1024),
        "observeWith must materialize only a bounded initial page"
    );

    // Make the presenter's Reflect input exceed the host observation budget
    // only after the command has started; the accepted write itself must not
    // invoke that optional presenter.
    large_context.store(true, Ordering::Release);
    let input = policy
        .dispatch_json_boxed(exomonad_tool::ToolInvocation {
            context: None,
            name: "write_stdin".into(),
            arguments: exomonad_tool::ToolArguments::Structured(serde_json::json!({
                "session_id":session, "chars":"q", "yield_time_ms":0
            })),
        })
        .await
        .unwrap();
    assert_eq!(input["status"], "committed", "{input}");
    assert!(
        input["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("Input acknowledged by backend; child consumption is unknown."),
        "the host acknowledgment must survive presentation failure: {input}"
    );
    assert_eq!(backend.control_count(), 1, "input must be submitted once");

    // Empty input is still a polling observation and invokes the presenter.
    // Its oversized Reflect context is optional context, not grounds to
    // reinterpret the earlier accepted write as a failed call.
    backend.finish();
    let poll = policy
        .dispatch_json_boxed(exomonad_tool::ToolInvocation {
            context: None,
            name: "write_stdin".into(),
            arguments: exomonad_tool::ToolArguments::Structured(serde_json::json!({
                "session_id":session, "yield_time_ms":0
            })),
        })
        .await
        .unwrap();
    assert_eq!(poll["status"], "committed", "{poll}");
    assert!(
        backend
            .output_budgets()
            .iter()
            .all(|bytes| *bytes <= 16 * 1024),
        "no implicit observation may request an unbounded initial page"
    );
    assert_eq!(backend.control_count(), 1, "polling must not resend input");
})).await;
}

/// A focused command retains its output even when the typed Jev section-scoring
/// request exceeds the ordinary heap-observation budget. Request decoding must
/// preserve every field rather than replacing one with an observation sentinel.
#[tokio::test]
async fn a_focused_bash_calls_jev_request_exceeding_the_budget_still_commits() {
    let backend = Arc::new(SectionScoreJev {
        requests: Mutex::new(Vec::new()),
    });
    let campaign = TestCampaign::start_with_config(
        |admission| admission,
        |config| {
            config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
            selected_shell_workspace(config);
        },
    )
    .await;
    campaign.run_scenario(|campaign| Box::pin(async move {
    let policy = Arc::clone(&campaign.root_installation.policy);
    let invoked_policy = Arc::clone(&policy);
    let mut invoked = tokio::spawn(async move {
        dispatch_structured_tool(
            invoked_policy.as_ref(),
            "bash",
            serde_json::json!({
                "cmd": "cat sources.txt",
                "workdir": null,
                "environment": null,
                "memory_mib": null,
                "tty": null,
                "stdin": null,
                "yield_time_ms": 30000,
                "max_output_bytes": 30000,
                "intent": "retain the decisive diagnostic",
                "focus": "retain the decisive diagnostic"
            }),
        )
        .await
    });
    // The live defect's own command printed only ~30 KB, but the budget this
    // classification spends is dominated by NODE COUNT from `Sift.hs`'s
    // per-section JSON packet (one score question, with its own rubric, per
    // ~2 KB section) rather than raw bytes of one packed `Text` -- a 30 KB,
    // single-repeated-character, no-newline command output (~15 sections)
    // measured well under the budget in practice. `OVERSIZED_BYTES` (~200
    // sections) is sized to trip it deterministically regardless of that
    // per-section accounting detail.
    let command_output = "e".repeat(OVERSIZED_BYTES);
    let commands = TestCommands::completed_streams(&command_output, "");
    let request = tokio::select! {
        request = backend_request(campaign) => request,
        result = &mut invoked => panic!("bash completed before requesting its command backend: {result:?}"),
    };
    request.supply(Ok(commands.clone()));
    let response = invoked.await.unwrap();
    assert_eq!(
        response["status"], "committed",
        "a command whose effect already committed must not have its unit reported failed \
         because scoring its own output for `focus` ran past the observation budget: {response}"
    );
    assert_eq!(
        commands.executions(),
        1,
        "recovering from an oversized scoring request must not replay the command"
    );
})).await;
}
