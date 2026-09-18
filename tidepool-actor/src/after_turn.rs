//! The observational System 1 slot invoked after one completed provider turn.
//!
//! Unlike `after_tool`, its answer is never inserted into provider context.
//! Authored effects may publish their own notifications, but mere observation
//! is passive and remains inspectable through actor status.

use std::time::Duration;

/// `Tidepool.Agent.Contract.afterTurnEntry` is the other half of this pair.
pub(crate) const AFTER_TURN_ENTRY: i64 = 2;

/// The slot name `installSpec` publishes when the spec fills the field.
pub(crate) const AFTER_TURN_SLOT: &str = "afterTurn";

/// A turn review must not wedge later actor work or retirement.
const AFTER_TURN_WAIT: Duration = Duration::from_secs(300);

pub(crate) fn wait() -> Duration {
    std::env::var(crate::after_tool::AFTER_TOOL_WAIT_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .map_or(AFTER_TURN_WAIT, Duration::from_millis)
}
