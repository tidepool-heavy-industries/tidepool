#[path = "../src/prepared_control.rs"]
mod prepared_control;

use prepared_control::{CallStatus, ControlError};

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
