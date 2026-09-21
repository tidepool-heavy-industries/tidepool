//! Materialized observation policy and markers for retained prepared values.

use tidepool_bridge::Value;
use tidepool_repr::DataConId;

/// Marker for an opaque closure retained behind a prepared value handle.
pub const CLOSURE_SENTINEL: DataConId = DataConId(u64::MAX);

/// Marker for a retained value whose materialization exceeded its display budget.
pub const OVERSIZE_SENTINEL: DataConId = DataConId(u64::MAX - 1);

/// How observation handles an exhausted materialization budget.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BudgetPolicy {
    #[default]
    Complete,
    Bounded,
}

impl BudgetPolicy {
    #[must_use]
    pub fn cuts(self) -> bool {
        matches!(self, Self::Bounded)
    }
}

#[must_use]
pub fn oversize_cut() -> Value {
    Value::Con(OVERSIZE_SENTINEL, Vec::new())
}

#[must_use]
pub fn contains_oversize_sentinel(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(node) = pending.pop() {
        if let Value::Con(id, fields) = node {
            if *id == OVERSIZE_SENTINEL {
                return true;
            }
            pending.extend(fields.iter());
        }
    }
    false
}

#[must_use]
pub fn contains_closure_sentinel(value: &Value) -> bool {
    match value {
        Value::Con(id, fields) => {
            *id == CLOSURE_SENTINEL || fields.iter().any(contains_closure_sentinel)
        }
        _ => false,
    }
}

#[must_use]
pub fn field_contains_closure_sentinel(value: &Value, index: usize) -> bool {
    matches!(value, Value::Con(_, fields) if fields.get(index).is_some_and(contains_closure_sentinel))
}
