//! Materialized observation policy and markers for retained prepared values.

use tidepool_bridge::HaskellValue;
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
pub fn oversize_cut() -> HaskellValue {
    HaskellValue::Con(OVERSIZE_SENTINEL, Vec::new())
}

/// Appended to a byte/Text leaf a bounded walk could not finish copying.
pub const TRUNCATION_MARKER: &[u8] = b"...[oversize: observation budget exhausted]";

/// Truncate `bytes` to what `remaining` budget can still afford (reserving
/// room for [`TRUNCATION_MARKER`]) and append the marker.
///
/// [`oversize_cut`] swaps a whole subtree for a `Con` sentinel, which is fine
/// where the caller only ever inspects/re-displays an arbitrary
/// `HaskellValue`. It does NOT typecheck where a literal is structurally
/// required -- most concretely, `Text`'s own backing `ByteArray#` field: a
/// single string too large to finish copying is still exactly one leaf,
/// never a constructor, so cutting it must stay a (shorter) literal rather
/// than change shape. Truncation lands on a UTF-8 boundary so a `Text`
/// backing array stays valid UTF-8; an arbitrary byte string tolerates the
/// same cut trivially.
#[must_use]
pub fn truncate_oversize_bytes(bytes: &[u8], remaining: usize) -> Vec<u8> {
    let keep = remaining
        .saturating_sub(TRUNCATION_MARKER.len())
        .min(bytes.len());
    let keep = match std::str::from_utf8(&bytes[..keep]) {
        Ok(_) => keep,
        Err(error) => error.valid_up_to(),
    };
    let mut truncated = Vec::with_capacity(keep + TRUNCATION_MARKER.len());
    truncated.extend_from_slice(&bytes[..keep]);
    truncated.extend_from_slice(TRUNCATION_MARKER);
    truncated
}

#[must_use]
pub fn contains_oversize_sentinel(value: &HaskellValue) -> bool {
    let mut pending = vec![value];
    while let Some(node) = pending.pop() {
        if let HaskellValue::Con(id, fields) = node {
            if *id == OVERSIZE_SENTINEL {
                return true;
            }
            pending.extend(fields.iter());
        }
    }
    false
}

#[must_use]
pub fn contains_closure_sentinel(value: &HaskellValue) -> bool {
    match value {
        HaskellValue::Con(id, fields) => {
            *id == CLOSURE_SENTINEL || fields.iter().any(contains_closure_sentinel)
        }
        _ => false,
    }
}

#[must_use]
pub fn field_contains_closure_sentinel(value: &HaskellValue, index: usize) -> bool {
    matches!(value, HaskellValue::Con(_, fields) if fields.get(index).is_some_and(contains_closure_sentinel))
}
