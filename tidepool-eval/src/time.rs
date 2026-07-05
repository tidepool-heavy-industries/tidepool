//! The `ParseISO8601` primop: `parseISO8601 :: Text -> Either Text UTCTime`.
//!
//! Chrono does the spec-compliant RFC-3339/ISO-8601 parse; a parse failure is
//! TYPED (`Left msg`) rather than the old hand-rolled parser's silent
//! corruption. The SAME builder ([`parse_iso8601_str`]) backs the tree-walker
//! (`eval.rs`) and the JIT host fn (`runtime_parse_iso8601`), so the two agree
//! by construction — mirroring the `JsonDecode` rail in `json.rs`.

use std::cell::Cell;
use tidepool_repr::{DataConId, DataConTable, Literal};

use crate::value::Value;

/// The minimal constructor ids needed to build `Either Text UTCTime` from Rust:
/// `Left`/`Right` (`Either`), `I#` (boxing the epoch-millis `Int` — `UTCTime` is
/// a newtype over `Int`), and `Text`.
///
/// Deliberately a SUBSET of [`crate::json::JsonConIds`], resolved on its own so
/// `parseISO8601` works even when the aeson `Value` / `Data.Map` constructors
/// are not in scope (they need not be, just because a program parses a
/// timestamp).
#[derive(Debug, Clone, Copy)]
pub struct TimeConIds {
    pub left: DataConId,
    pub right: DataConId,
    pub i_hash: DataConId,
    pub text: DataConId,
}

impl TimeConIds {
    /// Resolve the ids from a table; `None` if `Either`/`I#`/`Text` are absent.
    pub fn from_table(table: &DataConTable) -> Option<Self> {
        Some(TimeConIds {
            left: table.get_by_name_arity("Left", 1)?,
            right: table.get_by_name_arity("Right", 1)?,
            i_hash: table.get_by_name_arity("I#", 1)?,
            text: table.get_by_name_arity("Text", 3)?,
        })
    }
}

/// Parse an RFC-3339/ISO-8601 timestamp into the eval `Value` for
/// `Either Text UTCTime`. `UTCTime` is a newtype over epoch-millisecond `Int`,
/// so `Right` wraps a boxed `I#`. Total: a parse failure is `Left <message>`.
pub fn parse_iso8601_str(input: &str, ids: &TimeConIds) -> Value {
    match chrono::DateTime::parse_from_rfc3339(input.trim()) {
        Ok(dt) => {
            let millis = Value::Con(
                ids.i_hash,
                vec![Value::Lit(Literal::LitInt(dt.timestamp_millis()))],
            );
            Value::Con(ids.right, vec![millis])
        }
        Err(e) => {
            let msg = crate::shapes::make_text(&format!("parseISO8601: {input:?}: {e}"), ids.text);
            Value::Con(ids.left, vec![msg])
        }
    }
}

thread_local! {
    /// The `Either`/`I#`/`Text` ids for the current eval, cached by
    /// `env_from_datacon_table` (the universal eval-setup chokepoint). Read by
    /// the `ParseISO8601` primop arm in `eval.rs`. `None` before any env has
    /// been built on this thread, or when the constructors aren't in scope.
    static TIME_CON_IDS: Cell<Option<TimeConIds>> = const { Cell::new(None) };
}

/// Cache the time constructor ids for this thread. `None` clears them.
pub fn set_time_con_ids(ids: Option<TimeConIds>) {
    TIME_CON_IDS.with(|c| c.set(ids));
}

/// The time constructor ids cached for this thread, if any.
pub fn time_con_ids() -> Option<TimeConIds> {
    TIME_CON_IDS.with(|c| c.get())
}
