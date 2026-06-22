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
    /// Fatal signal during JIT execution (SIGILL, SIGSEGV, SIGBUS, SIGTRAP).
    #[error("{}", format_yield_signal(*.0))]
    Signal(i32),
    /// A runtime error raised by host/JIT code via the RUNTIME_ERROR flag. Holds
    /// the shared error set ONCE — DivisionByZero, Overflow, UserError(Msg),
    /// Undefined, CaseTrap, BadPointer, TypeMetadata, UnresolvedVar, NullFunPtr,
    /// BadFunPtrTag, HeapOverflow, StackOverflow, BlackHole, BadThunkState,
    /// Cancelled — rather than re-declaring each variant + message (they were
    /// duplicated verbatim from `RuntimeError`). `#[from]` derives
    /// `From<RuntimeError>`; `transparent` forwards its Display unchanged.
    #[error(transparent)]
    Runtime(#[from] crate::host_fns::RuntimeError),
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
            libc::SIGFPE => "SIGFPE (arithmetic exception — likely division by zero or overflow)",
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
