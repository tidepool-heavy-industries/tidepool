//! FROZEN CONTRACT (wave-2 `09-askuser-form-gui.md`): the operator-input seam
//! shared by the `AskUser` effect decode (WS1), the driver's form servicer
//! (WS2), and the web GUI (WS3). Single source for the form-spec / submission
//! wire types and the [`OperatorGate`] the driver blocks on.
//!
//! The gate is **sync-blocking**, mirroring the existing between-loops stdin
//! gate ([`super::driver::SelfHarnessDriver::between_loops_gate`]) and the
//! driver's `block_in_place`/`block_on` turn-driving: `present_form` blocks the
//! driver thread until the operator submits; `await_continue` blocks until the
//! operator advances the loop. A web implementation (WS3) parks a channel; the
//! headless [`StdinGate`] reads a line.
#![allow(dead_code)] // scaffold: consumers (WS1/WS2/WS3) wire these as they land.

use serde::{Deserialize, Serialize};

/// A typed form the agent spawned via `askUser :: Form a -> M a`. Rendered by
/// the web GUI; its [`Submission`] decodes back to the agent's `a`. v1 field
/// kinds only: enum (1-of-N) / int / text / bool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormSpec {
    pub fields: Vec<Field>,
}

/// One field in a [`FormSpec`]. `key` is the stable submission key; `label` is
/// the operator-facing prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
}

/// The v1 typed primitives. `Enum` is a 1-of-N choice over labelled tags (the
/// `tag` is what the submission carries; the `label` is what the operator sees).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    Enum { options: Vec<EnumOption> },
    Int,
    Text,
    Bool,
}

/// One choice in an [`FieldKind::Enum`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnumOption {
    pub label: String,
    pub tag: String,
}

/// A flat operator submission: one scalar JSON value per field `key` (enum →
/// the chosen `tag` string, int → number, text → string, bool → bool). ONE
/// canonical shape — no `{values,prose}` coercion, no keyed/unkeyed duality.
pub type Submission = serde_json::Map<String, serde_json::Value>;

/// The seam the driver blocks on for operator input. Sync-blocking by design
/// (see module docs). A web GUI (WS3) implements this by parking a channel
/// resolved from an HTTP handler; [`StdinGate`] keeps headless runs working.
pub trait OperatorGate: Send + Sync {
    /// Present `spec` to the operator and BLOCK until they submit. The returned
    /// [`Submission`] is decoded against the form by the caller; a decode
    /// failure re-presents the form (the retry is Haskell-side in `askUser`).
    fn present_form(&self, spec: &FormSpec) -> Submission;

    /// BLOCK until the operator advances to the next loop iteration (the
    /// human button-click gate that replaces the stdin between-loops gate).
    fn await_continue(&self);
}

/// Headless default: `await_continue` reads a line from stdin (the current
/// between-loops behavior); `present_form` reads one JSON line as the flat
/// submission, so non-web/CLI drives and tests still work. WS2 wires this as
/// the default when no web gate is configured.
#[derive(Debug, Default)]
pub struct StdinGate;

impl OperatorGate for StdinGate {
    fn present_form(&self, _spec: &FormSpec) -> Submission {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return Submission::new();
        }
        serde_json::from_str(line.trim()).unwrap_or_default()
    }

    fn await_continue(&self) {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
}
