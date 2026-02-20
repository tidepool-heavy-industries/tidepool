use tidepool_bridge::BridgeError;
use tidepool_eval::error::EvalError;

#[derive(Debug)]
pub enum EffectError {
    Eval(EvalError),
    Bridge(BridgeError),
    UnhandledEffect {
        tag: u64,
    },
    /// A required constructor was not found in the DataConTable.
    MissingConstructor {
        name: &'static str,
    },
    /// A constructor had the wrong number of fields.
    FieldCountMismatch {
        constructor: &'static str,
        expected: usize,
        got: usize,
    },
    /// Encountered an unexpected value shape during dispatch.
    UnexpectedValue {
        context: &'static str,
        got: String,
    },
    /// An effect handler encountered a runtime error.
    Handler(String),
}

impl From<EvalError> for EffectError {
    fn from(e: EvalError) -> Self {
        EffectError::Eval(e)
    }
}

impl From<BridgeError> for EffectError {
    fn from(e: BridgeError) -> Self {
        EffectError::Bridge(e)
    }
}

impl std::fmt::Display for EffectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EffectError::Eval(e) => write!(f, "Eval error: {}", e),
            EffectError::Bridge(e) => write!(f, "Bridge error: {}", e),
            EffectError::UnhandledEffect { tag } => write!(f, "Unhandled effect at tag {}", tag),
            EffectError::MissingConstructor { name } => {
                write!(f, "{} constructor not found in DataConTable", name)
            }
            EffectError::FieldCountMismatch {
                constructor,
                expected,
                got,
            } => {
                write!(f, "{} expects {} fields, got {}", constructor, expected, got)
            }
            EffectError::UnexpectedValue { context, got } => {
                write!(f, "expected {}, got {}", context, got)
            }
            EffectError::Handler(msg) => write!(f, "handler error: {}", msg),
        }
    }
}

impl std::error::Error for EffectError {}