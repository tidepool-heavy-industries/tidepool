//! Error types for effect handling.

use tidepool_bridge::BridgeError;
use tidepool_eval::error::EvalError;

/// Errors that can occur during effect handling.
#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    /// Evaluation error from the core-eval machine.
    #[error("Eval error: {0}")]
    Eval(#[from] EvalError),
    /// Bridge error during value conversion.
    #[error("Bridge error: {0}")]
    Bridge(#[from] BridgeError),
    /// No handler was found for an effect tag.
    #[error("Unhandled effect at tag {tag}")]
    UnhandledEffect {
        /// The unhandled effect tag.
        tag: u64,
    },
    /// A required constructor was not found in the DataConTable.
    #[error("{name} constructor not found in DataConTable")]
    MissingConstructor {
        /// Name of the missing constructor.
        name: &'static str,
    },
    /// A constructor had the wrong number of fields.
    #[error("{constructor} expects {expected} fields, got {got}")]
    FieldCountMismatch {
        /// Name of the constructor.
        constructor: &'static str,
        /// Expected field count.
        expected: usize,
        /// Actual field count.
        got: usize,
    },
    /// Encountered an unexpected value shape during dispatch.
    #[error("expected {context}, got {got}")]
    UnexpectedValue {
        /// Context describing the expected value.
        context: &'static str,
        /// Actual value encountered.
        got: String,
    },
    /// An effect handler encountered a runtime error.
    #[error("handler error: {0}")]
    Handler(String),
}
