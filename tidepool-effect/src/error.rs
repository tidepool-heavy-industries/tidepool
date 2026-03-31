use tidepool_bridge::BridgeError;
use tidepool_eval::error::EvalError;

#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    #[error("Eval error: {0}")]
    Eval(#[from] EvalError),
    #[error("Bridge error: {0}")]
    Bridge(#[from] BridgeError),
    #[error("Unhandled effect at tag {tag}")]
    UnhandledEffect { tag: u64 },
    /// A required constructor was not found in the DataConTable.
    #[error("{name} constructor not found in DataConTable")]
    MissingConstructor { name: &'static str },
    /// A constructor had the wrong number of fields.
    #[error("{constructor} expects {expected} fields, got {got}")]
    FieldCountMismatch {
        constructor: &'static str,
        expected: usize,
        got: usize,
    },
    /// Encountered an unexpected value shape during dispatch.
    #[error("expected {context}, got {got}")]
    UnexpectedValue { context: &'static str, got: String },
    /// An effect handler encountered a runtime error.
    #[error("handler error: {0}")]
    Handler(String),
}
