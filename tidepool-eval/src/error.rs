//! Evaluation errors and value type descriptions.

use crate::value::ThunkId;
use tidepool_repr::{JoinId, PrimOpKind, VarId};

/// Name of a primitive literal's runtime type, for error reporting.
///
/// A closed set (replaces bare `&'static str` tags) so a `ValueKind::Literal`
/// can only carry one of GHC's unboxed literal type names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LitTypeName {
    /// `Int#`
    Int,
    /// `Word#`
    Word,
    /// `Double#`
    Double,
    /// `Char#`
    Char,
    /// `String` (unpacked `Addr#`/`[Char]`)
    Str,
}

impl std::fmt::Display for LitTypeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            LitTypeName::Int => "Int#",
            LitTypeName::Word => "Word#",
            LitTypeName::Double => "Double#",
            LitTypeName::Char => "Char#",
            LitTypeName::Str => "String",
        };
        f.write_str(s)
    }
}

/// Describes the kind of a Value for error reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueKind {
    /// Primitive literal value
    Literal(LitTypeName),
    /// Saturated data constructor
    Constructor,
    /// Function closure
    Closure,
    /// Lazy thunk
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

/// Which two counts an [`EvalError::ArityMismatch`] is comparing.
///
/// Replaces a bare `&'static str` context tag with a closed set (only the tags
/// the interpreter actually raises).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArityContext {
    /// Function / join-point arguments.
    Arguments,
    /// Case-alternative binders vs. constructor fields.
    CaseBinders,
}

impl std::fmt::Display for ArityContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ArityContext::Arguments => "arguments",
            ArityContext::CaseBinders => "case binders",
        };
        f.write_str(s)
    }
}

/// Low-byte payload of an error-sentinel `VarId` (encoded in Haskell
/// `Translate.hs`, decoded here). Mirrors the style of
/// [`tidepool_repr::VarKind`]: an exhaustive enum with an explicit decoder in
/// place of bare numeric matches.
///
/// The numeric mapping is a wire contract with the Haskell encoder — keep
/// [`SentinelKind::from_u8`] identical to it, including the lossy fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentinelKind {
    /// `0` — division by zero.
    DivByZero,
    /// `1` — arithmetic overflow.
    Overflow,
    /// `2` — Haskell `error "…"`.
    UserError,
    /// `3` — Haskell `undefined`.
    Undefined,
}

impl SentinelKind {
    /// Decode the sentinel's low byte. Unknown payloads collapse to
    /// [`SentinelKind::UserError`] — matching the pre-enum lossy catch-all.
    #[must_use]
    pub fn from_u8(byte: u8) -> Self {
        match byte {
            0 => SentinelKind::DivByZero,
            1 => SentinelKind::Overflow,
            2 => SentinelKind::UserError,
            3 => SentinelKind::Undefined,
            _ => SentinelKind::UserError,
        }
    }
}

/// Errors that can occur during interpretation.
///
/// Includes runtime type errors, unbound variables, and arity mismatches.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EvalError {
    /// Variable not found in environment
    #[error("unbound variable: v_{}", .0 .0)]
    UnboundVar(VarId),
    /// Arity mismatch (wrong number of arguments or fields)
    #[error("arity mismatch: expected {expected} {context}, got {got}")]
    ArityMismatch {
        context: ArityContext,
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
                context: ArityContext::Arguments,
                expected: 2,
                got: 1,
            },
            EvalError::TypeMismatch {
                expected: "Int#",
                got: ValueKind::Literal(LitTypeName::Char),
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
