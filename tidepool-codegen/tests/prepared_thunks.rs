#[path = "../src/prepared_thunks.rs"]
mod prepared_thunks;

use prepared_thunks::{PreparedThunk, ThunkEnter, ThunkStateView, ThunkUpdateError};
use tidepool_repr::execution_schema::UpdatePolicy;

fn token<T, E>(enter: ThunkEnter<T, E>) -> prepared_thunks::EvaluationToken {
    match enter {
        ThunkEnter::Evaluate(token) => token,
        _ => panic!("expected an evaluation token"),
    }
}

#[test]
fn memoized_thunk_preserves_first_failure_and_rejects_duplicate_update() {
    let mut thunk = PreparedThunk::<u64, &'static str>::new(UpdatePolicy::Memoize);
    let first = token(thunk.enter());
    assert_eq!(thunk.enter(), ThunkEnter::Blackhole);
    thunk.fail(first, "first cause").unwrap();
    assert_eq!(thunk.enter(), ThunkEnter::Failed("first cause"));
    assert_eq!(
        thunk.complete(first, 42),
        Err(ThunkUpdateError::StaleUpdate)
    );
    assert_eq!(thunk.state(), ThunkStateView::Failed);
}

#[test]
fn cancellation_clears_inflight_authority_without_publishing_payload() {
    let mut thunk = PreparedThunk::<u64, &'static str>::new(UpdatePolicy::Memoize);
    let cancelled = token(thunk.enter());
    thunk.cancel(cancelled).unwrap();
    assert_eq!(thunk.state(), ThunkStateView::Unevaluated);
    let retry = token(thunk.enter());
    assert_ne!(cancelled, retry);
    assert_eq!(
        thunk.complete(cancelled, 7),
        Err(ThunkUpdateError::StaleUpdate)
    );
    thunk.complete(retry, 42).unwrap();
    assert_eq!(thunk.enter(), ThunkEnter::Cached(42));
}

#[test]
fn single_entry_result_is_never_reentered_or_updated_twice() {
    let mut thunk = PreparedThunk::<u64, &'static str>::new(UpdatePolicy::SingleEntry);
    let only = token(thunk.enter());
    thunk.complete(only, 42).unwrap();
    assert_eq!(thunk.state(), ThunkStateView::Consumed);
    assert_eq!(thunk.enter(), ThunkEnter::SingleEntryReentered);
    assert_eq!(thunk.fail(only, "late"), Err(ThunkUpdateError::StaleUpdate));
}
