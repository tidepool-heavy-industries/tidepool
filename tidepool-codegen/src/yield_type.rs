/// Result of a single evaluation step.
#[derive(Debug, PartialEq, Eq)]
pub enum Yield {
    /// Pure result — evaluation complete.
    Done(*mut u8),
    /// Effect request — stash continuation, dispatch to handler.
    /// Fields: (union_tag: u64, request: *mut u8, continuation: *mut u8)
    Request {
        tag: u64,
        request: *mut u8,
        continuation: *mut u8,
    },
    /// Evaluation error.
    Error(YieldError),
}

impl std::fmt::Display for Yield {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Yield::Done(ptr) => write!(f, "Done({:p})", ptr),
            Yield::Request {
                tag,
                request,
                continuation,
            } => {
                write!(
                    f,
                    "Request(tag={}, req={:p}, cont={:p})",
                    tag, request, continuation
                )
            }
            Yield::Error(e) => write!(f, "Error({})", e),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, thiserror::Error)]
pub enum YieldError {
    /// Result HeapObject had unexpected tag byte.
    #[error("unexpected heap tag: {0}")]
    UnexpectedTag(u8),
    /// Result was Con but con_tag was neither Val nor E.
    #[error("unexpected constructor tag: {0}")]
    UnexpectedConTag(u64),
    /// Val constructor had wrong number of fields.
    #[error("Val constructor has {0} fields, expected >= 1")]
    BadValFields(u16),
    /// E constructor had wrong number of fields.
    #[error("E constructor has {0} fields, expected 2")]
    BadEFields(u16),
    /// Union constructor had wrong number of fields.
    #[error("Union constructor has {0} fields, expected 2")]
    BadUnionFields(u16),
    /// Null pointer encountered.
    #[error("null pointer in effect result")]
    NullPointer,
    /// Division by zero in JIT code.
    #[error("division by zero")]
    DivisionByZero,
    /// Arithmetic overflow in JIT code.
    #[error("arithmetic overflow")]
    Overflow,
    /// Haskell `error` called in JIT code.
    #[error("Haskell error called")]
    UserError,
    /// Haskell `error` called with a specific message.
    #[error("Haskell error: {0}")]
    UserErrorMsg(String),
    /// Haskell `undefined` forced in JIT code.
    #[error("Haskell undefined forced")]
    Undefined,
    /// GHC type metadata forced (should be dead code).
    #[error("forced type metadata (should be dead code)")]
    TypeMetadata,
    /// Unresolved external variable forced.
    #[error("unresolved variable VarId({0:#x}) [tag='{tag}', key={key}]", tag=(*.0 >> 56) as u8 as char, key=(*.0 & ((1u64 << 56) - 1)))]
    UnresolvedVar(u64),
    /// Application of null function pointer.
    #[error("application of null function pointer")]
    NullFunPtr,
    /// Application of non-closure heap object.
    #[error("application of non-closure (tag={0})")]
    BadFunPtrTag(u8),
    /// Heap overflow after GC.
    #[error("heap overflow (nursery exhausted after GC)")]
    HeapOverflow,
    /// Call depth exceeded (likely infinite list or unbounded recursion).
    #[error("stack overflow (likely infinite list or unbounded recursion — use zipWithIndex/imap/enumFromTo instead of [0..])")]
    StackOverflow,
    /// Fatal signal during JIT execution (SIGILL, SIGSEGV, SIGBUS, SIGTRAP).
    #[error("{}", format_yield_signal(*.0))]
    Signal(i32),
    /// Blackhole detected (infinite loop: thunk forced itself).
    #[error("blackhole detected (infinite loop: thunk forced itself)")]
    BlackHole,
    /// Thunk encountered with an invalid evaluation state.
    #[error("thunk has invalid evaluation state: {0}")]
    BadThunkState(u8),
}

fn format_yield_signal(sig: i32) -> String {
    let ctx = crate::host_fns::get_exec_context();
    #[cfg(unix)]
    {
        let name = match sig {
            libc::SIGILL => "SIGILL (illegal instruction — likely exhausted case branch)",
            libc::SIGSEGV => "SIGSEGV (segmentation fault — likely invalid memory access)",
            libc::SIGBUS => "SIGBUS (bus error)",
            libc::SIGTRAP => "SIGTRAP (trap — likely Cranelift trap instruction)",
            libc::SIGFPE => {
                "SIGFPE (arithmetic exception — likely division by zero or overflow)"
            }
            _ => {
                if !ctx.is_empty() {
                    return format!("JIT signal: signal {} (unknown, context: {})", sig, ctx);
                } else {
                    return format!("JIT signal: signal {} (unknown)", sig);
                }
            }
        };
        if !ctx.is_empty() {
            format!("JIT signal: {} (context: {})", name, ctx)
        } else {
            format!("JIT signal: {}", name)
        }
    }
    #[cfg(not(unix))]
    if !ctx.is_empty() {
        format!("JIT signal: signal {} (context: {})", sig, ctx)
    } else {
        format!("JIT signal: signal {}", sig)
    }
}

impl From<crate::host_fns::RuntimeError> for YieldError {
    fn from(err: crate::host_fns::RuntimeError) -> Self {
        use crate::host_fns::RuntimeError;
        match err {
            RuntimeError::DivisionByZero => YieldError::DivisionByZero,
            RuntimeError::Overflow => YieldError::Overflow,
            RuntimeError::UserError => YieldError::UserError,
            RuntimeError::UserErrorMsg(msg) => YieldError::UserErrorMsg(msg),
            RuntimeError::Undefined => YieldError::Undefined,
            RuntimeError::TypeMetadata => YieldError::TypeMetadata,
            RuntimeError::UnresolvedVar(id) => YieldError::UnresolvedVar(id),
            RuntimeError::NullFunPtr => YieldError::NullFunPtr,
            RuntimeError::BadFunPtrTag(tag) => YieldError::BadFunPtrTag(tag),
            RuntimeError::HeapOverflow => YieldError::HeapOverflow,
            RuntimeError::StackOverflow => YieldError::StackOverflow,
            RuntimeError::BlackHole => YieldError::BlackHole,
            RuntimeError::BadThunkState(state) => YieldError::BadThunkState(state),
        }
    }
}
