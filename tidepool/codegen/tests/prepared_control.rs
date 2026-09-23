#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
#[path = "../src/prepared_control.rs"]
mod prepared_control;

use prepared_control::{CallStatus, ControlError, PreparedSafepoint};

#[test]
fn raw_statuses_have_stable_wire_values() {
    assert_eq!(CallStatus::from_raw(0), Ok(CallStatus::Success));
    assert_eq!(CallStatus::from_raw(1), Ok(CallStatus::LanguageFailure));
    assert_eq!(CallStatus::from_raw(2), Ok(CallStatus::IntegrityFailure));
    assert_eq!(CallStatus::from_raw(3), Ok(CallStatus::Cancelled));
}

#[test]
fn unknown_raw_status_is_typed() {
    assert_eq!(
        CallStatus::from_raw(99),
        Err(ControlError::UnknownStatus(99))
    );
}

#[test]
fn raw_safepoints_have_stable_wire_values() {
    for safepoint in [
        PreparedSafepoint::Allocation,
        PreparedSafepoint::FunctionEntry,
        PreparedSafepoint::Backedge,
        PreparedSafepoint::ThunkEntry,
        PreparedSafepoint::ThunkCommit,
    ] {
        assert_eq!(
            PreparedSafepoint::from_raw(safepoint as u32),
            Some(safepoint)
        );
    }
    assert_eq!(PreparedSafepoint::from_raw(5), None);
}
