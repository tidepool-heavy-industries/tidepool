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
use super::test_campaign::TestCampaign;
use super::tests::dispatch_haskell_script;
use super::*;

/// Comfortably past the budget, and far enough past that no accounting
/// detail decides the outcome.
const OVERSIZED_BYTES: usize = 400_000;

/// One turn's worth of transcript. Three of these is ~600 KB, the shape
/// `reflect 3` actually sees in a session that has been reading files.
fn bulky_turn(index: usize) -> tidepool_actor::ConversationTurn {
    tidepool_actor::ConversationTurn {
        turn: format!("turn-{index}"),
        started_at: None,
        completed_at: None,
        items: vec![
            tidepool_actor::TurnItem::Message {
                role: tidepool_actor::ConversationRole::Assistant,
                text: format!("turn {index} reasoning"),
            },
            tidepool_actor::TurnItem::ToolResult {
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
    let reader: tidepool_actor::ConversationReader = Arc::new(move |_actor, count| {
        let turns = turns.clone();
        Box::pin(async move { Ok(turns.into_iter().take(count).collect()) })
    });
    let campaign = TestCampaign::start_with_conversation(
        tidepool_actor::ResearchPolicy::default(),
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
