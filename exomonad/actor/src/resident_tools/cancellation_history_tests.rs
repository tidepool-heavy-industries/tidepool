use super::*;
use proptest::prelude::*;

#[derive(Default)]
struct Model {
    winner: Option<WorkbenchCancellationCause>,
    pending: Option<WorkbenchCancellationCause>,
    terminal: bool,
    publication: bool,
}
fn cause(code: u8) -> WorkbenchCancellationCause {
    match code {
        0 => WorkbenchCancellationCause::DeliveredInput,
        1 => WorkbenchCancellationCause::ProviderFailureCleanup,
        2 => WorkbenchCancellationCause::ExplicitCancellation,
        3 => WorkbenchCancellationCause::Retirement,
        4 => WorkbenchCancellationCause::Unknown,
        5 => WorkbenchCancellationCause::RoundFailureCleanup,
        _ => unreachable!("generated cause range"),
    }
}
fn reply() -> crate::KernelWorkbenchReply {
    Ok(WorkbenchResponse {
        status: tidepool_runtime::session::WorkbenchRunStatus::Rejected,
        summary: None,
        items: Vec::new(),
        next_index: 0,
        total: 1,
        publication: None,
    })
}

#[derive(Clone, Debug)]
enum Event {
    Request(u8),
    Settle,
    ClaimPublication,
    ContinuePublication,
    TerminalPublication,
    Observe,
}
fn events() -> impl Strategy<Value = Event> {
    prop_oneof![
        (0u8..6).prop_map(Event::Request),
        Just(Event::Settle),
        Just(Event::ClaimPublication),
        Just(Event::ContinuePublication),
        Just(Event::TerminalPublication),
        Just(Event::Observe),
    ]
}

proptest! {
    #[test]
    fn generated_cancellation_and_publication_histories_retain_first_winning_cause(
        history in prop::collection::vec(events(), 0..40),
    ) {
        let control = WorkbenchExecutionControl::untracked();
        let mut model = Model::default();
        let mut claim = None;
        for event in &history {
            match event {
                Event::Request(code) => {
                    let requested = cause(*code);
                    let expected = !model.terminal && !model.publication && model.winner.is_none();
                    let actual = control.request_cancellation_with_cause(requested);
                    prop_assert_eq!(actual, expected, "history={:?}", history);
                    if expected { model.winner = Some(requested); }
                    else if !model.terminal && model.publication && model.pending.is_none() {
                        model.pending = Some(requested);
                    }
                }
                Event::Settle => {
                    control.settle(reply());
                    model.terminal = true;
                }
                Event::ClaimPublication if !model.terminal => {
                    let expected = model.winner.is_none() && !model.publication;
                    let actual = control.publication_decision().claim_commit();
                    prop_assert_eq!(actual.is_some(), expected, "history={:?}", history);
                    if let Some(actual) = actual { claim = Some(actual); model.publication = true; }
                }
                Event::ContinuePublication => {
                    if let Some(actual) = claim.take() {
                        let pending = actual.published_continuing();
                        prop_assert_eq!(pending, model.pending.is_some());
                        control.complete_continuing_publication(pending);
                        model.publication = false;
                        if !model.terminal { model.winner = model.pending.take(); }
                    }
                }
                Event::TerminalPublication => {
                    if let Some(actual) = claim.take() {
                        prop_assert_eq!(actual.published(), model.pending.is_some());
                        model.publication = false;
                        model.terminal = true;
                    }
                }
                Event::ClaimPublication | Event::Observe => {}
            }
            prop_assert_eq!(control.cancellation_cause(), model.winner, "history={:?}", history);
        }
    }
}

#[tokio::test]
async fn cancellation_wait_observes_provider_failure_cause_before_returning() {
    let control = WorkbenchExecutionControl::untracked();
    let cancelling = control.clone();
    *control.cancellation_observed.lock() = Some(Box::new(move || {
        assert!(cancelling
            .request_cancellation_with_cause(WorkbenchCancellationCause::ProviderFailureCleanup,));
    }));
    let observed = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        control.wait_for_cancellation(),
    )
    .await
    .unwrap();
    assert_eq!(observed, WorkbenchCancellationCause::ProviderFailureCleanup);
    assert_eq!(
        control.interruption_detail("sleep"),
        "sleep interrupted by provider failure cleanup"
    );
}

#[test]
fn continuing_publication_promotion_retains_pending_cause_across_a_second_request() {
    let control = WorkbenchExecutionControl::untracked();
    let claim = control.publication_decision().claim_commit().unwrap();
    assert!(!control
        .request_cancellation_with_cause(WorkbenchCancellationCause::ProviderFailureCleanup,));
    let pending = claim.published_continuing();
    assert!(pending);
    assert_eq!(control.cancellation_cause(), None);
    assert!(!control.request_cancellation_with_cause(WorkbenchCancellationCause::Retirement,));
    control.complete_continuing_publication(pending);
    assert_eq!(
        control.cancellation_cause(),
        Some(WorkbenchCancellationCause::ProviderFailureCleanup)
    );
}

#[test]
fn terminal_reply_before_pending_promotion_does_not_issue_cancellation() {
    let control = WorkbenchExecutionControl::untracked();
    let claim = control.publication_decision().claim_commit().unwrap();
    assert!(!control
        .request_cancellation_with_cause(WorkbenchCancellationCause::ProviderFailureCleanup,));
    let pending = claim.published_continuing();
    control.settle(reply());
    control.complete_continuing_publication(pending);
    assert_eq!(control.cancellation_cause(), None);
}

#[test]
fn terminal_publication_without_cancellation_has_no_pending_cause() {
    // Minimal retained history [6, 8]: claim then terminal publication.
    let control = WorkbenchExecutionControl::untracked();
    let claim = control.publication_decision().claim_commit().unwrap();
    assert!(!claim.published());
    assert_eq!(control.cancellation_cause(), None);
    assert!(!control.request_cancellation_with_cause(WorkbenchCancellationCause::Retirement));
    assert_eq!(control.cancellation_cause(), None);
}
