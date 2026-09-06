//! Read-only, bounded derivation of Shoal run artifacts.
//! Missing evidence is not a negative observation or an acceptance verdict.
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "certainty", rename_all = "snake_case")]
pub enum Evidence<T> {
    Observed { value: T, source: String },
    Inferred { value: T, reason: String },
    Unknown { reason: String },
}
