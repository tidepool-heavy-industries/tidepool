use core_bridge::BridgeError;
use core_eval::error::EvalError;

#[derive(Debug)]
pub enum EffectError {
    Eval(EvalError),
    Bridge(BridgeError),
    UnhandledEffect { tag: u64 },
    BadContinuation(String),
    BadUnion(String),
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
            EffectError::BadContinuation(s) => write!(f, "Bad continuation: {}", s),
            EffectError::BadUnion(s) => write!(f, "Bad union: {}", s),
        }
    }
}

impl std::error::Error for EffectError {}
