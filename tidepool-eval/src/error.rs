use crate::value::ThunkId;
use tidepool_repr::{JoinId, PrimOpKind, VarId};

/// Describes the kind of a Value for error reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueKind {
    Literal(&'static str), // "Int#", "Word#", "Double#", "Char#", "String"
    Constructor,
    Closure,
    Thunk,
    /// Fallback for complex values — stores Debug output
    Other(String),
}

impl std::fmt::Display for ValueKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValueKind::Literal(name) => write!(f, "{}", name),
            ValueKind::Constructor => write!(f, "constructor"),
            ValueKind::Closure => write!(f, "closure"),
            ValueKind::Thunk => write!(f, "thunk"),
            ValueKind::Other(s) => write!(f, "{}", s),
        }
    }
}

/// Evaluation error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EvalError {
    /// Variable not found in environment
    #[error("unbound variable: v_{}", .0 .0)]
    UnboundVar(VarId),
    /// Arity mismatch (wrong number of arguments or fields)
    #[error("arity mismatch: expected {expected} {context}, got {got}")]
    ArityMismatch {
        context: &'static str, // "arguments", "fields", "case binders"
        expected: usize,
        got: usize,
    },
    /// Type mismatch during evaluation
    #[error("type mismatch: expected {expected}, got {got}")]
    TypeMismatch {
        expected: &'static str,
        got: ValueKind,
    },
    /// No matching alternative in case expression
    #[error("no matching case alternative")]
    NoMatchingAlt,
    /// Infinite loop detected (thunk forced itself)
    #[error("infinite loop: thunk {} forced itself", .0 .0)]
    InfiniteLoop(ThunkId),
    /// Unsupported primop
    #[error("unsupported primop: {0:?}")]
    UnsupportedPrimOp(PrimOpKind),
    /// Heap exhausted
    #[error("heap exhausted")]
    HeapExhausted,
    /// Application of non-function value
    #[error("application of non-function value")]
    NotAFunction,
    /// Jump to unknown join point
    #[error("jump to unbound join point: j_{}", .0 .0)]
    UnboundJoin(JoinId),
    /// Haskell `error "..."` called
    #[error("Haskell error called")]
    UserError,
    /// Haskell `undefined` forced
    #[error("Haskell undefined forced")]
    Undefined,
    /// Recursion depth limit exceeded during deep_force
    #[error("recursion depth limit exceeded")]
    DepthLimit,
    /// Internal invariant violation (should never happen)
    #[error("internal error: {0}")]
    InternalError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let errs = vec![
            EvalError::UnboundVar(VarId(42)),
            EvalError::ArityMismatch {
                context: "arguments",
                expected: 2,
                got: 1,
            },
            EvalError::TypeMismatch {
                expected: "Int#",
                got: ValueKind::Literal("Char#"),
            },
            EvalError::NoMatchingAlt,
            EvalError::InfiniteLoop(ThunkId(0)),
            EvalError::UnsupportedPrimOp(PrimOpKind::IntAdd),
            EvalError::HeapExhausted,
            EvalError::NotAFunction,
            EvalError::UnboundJoin(JoinId(7)),
        ];

        for err in errs {
            let s = format!("{}", err);
            assert!(!s.is_empty(), "Display for {:?} should not be empty", err);
        }
    }
}
