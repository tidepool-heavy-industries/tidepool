//! A result too large to materialize is still a binding.
//!
//! The observation budget (`RunOptions::observation_budget`, 100_000) bounds
//! turning a heap value into a host `Value`. It charges one unit per value
//! node AND per copied payload byte (`prepared_program/observe.rs`'s
//! `ObservationBudget::charge_bytes`), so it is really a ~100 KB ceiling.
//!
//! It used to reject the whole cell unit. That is the wrong layer: the
//! binding a cell installs is the retained handle, which
//! `run_entry_retained` produces without consulting any budget, and a bind's
//! receipt renders binder names rather than the value. So an exhausted
//! budget discarded a sound binding — along with every effect already
//! committed to produce it.
//!
//! A bare expression — a cell with no `x <-` of its own — is displayed, so
//! unlike a bind it really does need a host `Value`. It still must not be
//! rejected for size: the cell is bound under its automatic `observationN`
//! name and shown through the ordinary `cellDisplay` paging, with the view
//! marked as a selection that names the binding holding the rest.
//!
//! The tests below are drawn from cells that failed in a live session on
//! 2026-09-17; the originals are preserved under
//! `plans/jev-lab/observation-limit/`. One lost 45 committed operations
//! (10 Jev calls, 35 command jobs). The other was `reflect 3`, which cannot
//! bind at all when three turns of conversation exceed 100 KB — as they
//! routinely do.

use super::command_jobs_tests::{backend_request, TestCommands};
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
    let mut campaign = TestCampaign::start().await;
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
        request = backend_request(&mut campaign) => request.supply(Ok(backend.clone())),
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
    let used = dispatch_haskell_script(policy.as_ref(), "T.length transcript").await;
    assert_eq!(used["status"], "committed", "{used}");
    assert_eq!(
        used["items"][0]["output"],
        OVERSIZED_BYTES.to_string(),
        "the retained binding must be the complete value, not a truncation: {used}"
    );

    // And the committed effect was not replayed to get it back.
    assert_eq!(
        backend.executions(),
        1,
        "recovering the value must not re-run the command that produced it"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn an_oversized_bare_expression_is_bound_and_shown_as_a_selection() {
    let mut campaign = TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();
    // The same committed-effect shape as the bind case above: one command job
    // whose output is past the budget. The difference is the cell that DISPLAYS
    // it — a bare expression, with no binder the author wrote.
    const CELL: &str = "transcript <- do\n  \
        job <- Cmd.quiet (Cmd.run (Cmd.argv [\"cat\", \"sources.txt\"]))\n  \
        pure (either (const \"\") id (Cmd.stdout job))";
    let mut running =
        tokio::spawn(async move { dispatch_haskell_script(policy.as_ref(), CELL).await });
    let backend = TestCommands::completed(&"x".repeat(OVERSIZED_BYTES));
    tokio::select! {
        request = backend_request(&mut campaign) => request.supply(Ok(backend.clone())),
        result = &mut running => panic!("the cell ended before requesting a command backend: {result:?}"),
    }
    assert_eq!(running.await.unwrap()["status"], "committed");

    let policy = campaign.root_installation.policy.clone();
    let shown = dispatch_haskell_script(policy.as_ref(), "transcript").await;
    assert_eq!(
        shown["status"], "committed",
        "an oversized bare expression must be shown, not rejected: {shown}"
    );
    let item = &shown["items"][0];
    let output = item["output"].as_str().unwrap();
    assert!(!output.contains("Display failed"), "{shown}");
    // It is bound under the automatic name a bare expression always gets.
    let name = item["installedBindings"][0]
        .as_str()
        .unwrap_or_else(|| panic!("a bare expression must leave a binding: {shown}"))
        .to_owned();
    // What the model sees says it is a selection and names the binding that
    // holds the rest — a bounded view must never read as the whole value.
    assert!(
        output.ends_with(&format!(
            "\n[selection of {name} (); display continues: cellDisplay.more]"
        )),
        "{shown}"
    );
    assert!(
        output.len() < OVERSIZED_BYTES,
        "the shown part must be bounded: {} characters",
        output.len()
    );

    // The rest of the display continues through the existing paging. This is
    // the next cell on purpose: every displayed cell republishes `cellDisplay`.
    let more = dispatch_haskell_script(policy.as_ref(), "cellDisplay.more").await;
    assert_eq!(more["status"], "committed", "{more}");
    assert!(
        more["items"][0]["output"]
            .as_str()
            .unwrap()
            .starts_with('x'),
        "the continuation must carry the rest of the value: {more}"
    );

    // The binding is a real one, and it is the expression the marker named: a
    // later cell uses it and gets the WHOLE value, not the shown selection.
    let used = dispatch_haskell_script(policy.as_ref(), &format!("T.length ({name} ())")).await;
    assert_eq!(
        used["items"][0]["output"],
        OVERSIZED_BYTES.to_string(),
        "the binding behind a selection must hold the complete value: {used}"
    );

    // None of that re-ran the command whose output is being shown.
    assert_eq!(
        backend.executions(),
        1,
        "displaying a committed result must not replay the effect that produced it"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn reflect_binds_history_larger_than_the_observation_budget() {
    let turns: Vec<_> = (0..3).map(bulky_turn).collect();
    let reader: exomonad_actor::ConversationReader = Arc::new(move |_actor, count| {
        let turns = turns.clone();
        Box::pin(async move { Ok(turns.into_iter().take(count).collect()) })
    });
    let campaign = TestCampaign::start_with_conversation(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |_| {},
        Some(reader),
    )
    .await;
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
    let used =
        dispatch_haskell_script(policy.as_ref(), "either (const 0) length editorialContext").await;
    assert_eq!(used["status"], "committed", "{used}");
    assert_eq!(
        used["items"][0]["output"], "3",
        "all three requested turns must survive the bind: {used}"
    );

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
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
    let mut campaign = TestCampaign::start_with_conversation(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |_| {},
        Some(reader),
    )
    .await;
    let policy = campaign.root_installation.policy.clone();
    let bash = {
        let policy = policy.clone();
        tokio::spawn(async move {
            policy
                .dispatch_boxed(exomonad_tool::ToolInvocation {
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
    backend_request(&mut campaign)
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
        .dispatch_boxed(exomonad_tool::ToolInvocation {
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
        .dispatch_boxed(exomonad_tool::ToolInvocation {
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

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

/// A `focus`ed bash call whose command output is large enough that the Jev
/// request built to SCORE it -- not the command's own retained job binding --
/// exceeds the observation budget while it sits parked for dispatch.
///
/// Live defect (wave 4, run 535e56ca, actor 5): a bash call with
/// `max_output_bytes` and `focus` set printed ~30 KB, and the whole unit was
/// reported failed with "observation budget 100000 exhausted" even though
/// the command's own effect had already committed -- the receipt read
/// "effects committed; observing the result failed". The site was
/// `PreparedEngine::park_suspension` (`tidepool_runtime::session::prepared`):
/// it observes a newly suspended request under `BudgetPolicy::Complete` to
/// classify and dispatch it (here, the section-scoring `Jev` request the
/// focused-bash template builds around the command's own output), and an
/// exhausted budget there propagated as a hard failure.
///
/// Every other observation after a commit (`finish_prepared`'s `SettlePlan`
/// arms, `observe_display_metadata`) already tolerated this by degrading to
/// `BudgetPolicy::Bounded` -- a cut, sentinel-marked walk. That degradation
/// is wrong HERE: this observation feeds a typed decode
/// (`handlers.dispatch` reconstructs a concrete request type, e.g.
/// `JevAskWith`, from the value field by field), and a `Bounded` cut can
/// land on any field, swapping in the oversize sentinel in place of an
/// `Int` or a map entry just as readily as a `Text` -- decode then rejects
/// it with a confusing type mismatch instead of a clean, budget-attributed
/// failure (confirmed while developing this fix: a `Bounded` retry here
/// turned the clean budget error into `expected LitInt or I#, got
/// Con(DataConId(...))`). The fix instead re-observes under `Complete` with
/// no display-sized ceiling: the request already exists in full on the
/// heap, so the real constraint on rematerializing it is memory, not the
/// 100_000-unit display budget.
#[tokio::test]
async fn a_focused_bash_calls_jev_request_exceeding_the_budget_still_commits() {
    let backend = Arc::new(SectionScoreJev {
        requests: Mutex::new(Vec::new()),
    });
    let mut campaign = TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.jev = Some(Arc::clone(&backend) as exomonad_actor::JevBackendHandle);
            selected_shell_workspace(config);
        },
    )
    .await;
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
        request = backend_request(&mut campaign) => request,
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

    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
