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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum YieldError {
    /// Result HeapObject had unexpected tag byte.
    UnexpectedTag(u8),
    /// Result was Con but con_tag was neither Val nor E.
    UnexpectedConTag(u64),
    /// Val constructor had wrong number of fields.
    BadValFields(u16),
    /// E constructor had wrong number of fields.
    BadEFields(u16),
    /// Union constructor had wrong number of fields.
    BadUnionFields(u16),
    /// Null pointer encountered.
    NullPointer,
    /// Division by zero in JIT code.
    DivisionByZero,
    /// Arithmetic overflow in JIT code.
    Overflow,
    /// Haskell `error` called in JIT code.
    UserError,
    /// Haskell `undefined` forced in JIT code.
    Undefined,
    /// GHC type metadata forced (should be dead code).
    TypeMetadata,
    /// Unresolved external variable forced.
    UnresolvedVar(u64),
    /// Application of null function pointer.
    NullFunPtr,
    /// Application of non-closure heap object.
    BadFunPtrTag(u8),
    /// Heap overflow after GC.
    HeapOverflow,
    /// Fatal signal during JIT execution (SIGILL, SIGSEGV, SIGBUS, SIGTRAP).
    Signal(i32),
}

impl std::fmt::Display for YieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YieldError::UnexpectedTag(tag) => write!(f, "unexpected heap tag: {}", tag),
            YieldError::UnexpectedConTag(tag) => write!(f, "unexpected constructor tag: {}", tag),
            YieldError::BadValFields(n) => {
                write!(f, "Val constructor has {} fields, expected >= 1", n)
            }
            YieldError::BadEFields(n) => write!(f, "E constructor has {} fields, expected 2", n),
            YieldError::BadUnionFields(n) => {
                write!(f, "Union constructor has {} fields, expected 2", n)
            }
            YieldError::NullPointer => write!(f, "null pointer in effect result"),
            YieldError::DivisionByZero => write!(f, "division by zero"),
            YieldError::Overflow => write!(f, "arithmetic overflow"),
            YieldError::UserError => write!(f, "Haskell error called"),
            YieldError::Undefined => write!(f, "Haskell undefined forced"),
            YieldError::TypeMetadata => write!(f, "forced type metadata (should be dead code)"),
            YieldError::UnresolvedVar(id) => {
                let tag_char = (*id >> 56) as u8 as char;
                let key = *id & ((1u64 << 56) - 1);
                write!(
                    f,
                    "unresolved variable VarId({:#x}) [tag='{}', key={}]",
                    id, tag_char, key
                )
            }
            YieldError::NullFunPtr => write!(f, "application of null function pointer"),
            YieldError::BadFunPtrTag(tag) => write!(f, "application of non-closure (tag={})", tag),
            YieldError::HeapOverflow => write!(f, "heap overflow (nursery exhausted after GC)"),
            YieldError::Signal(sig) => {
                #[cfg(unix)]
                {
                    let name = match *sig {
                        libc::SIGILL => "SIGILL (illegal instruction — likely exhausted case branch)",
                        libc::SIGSEGV => {
                            "SIGSEGV (segmentation fault — likely invalid memory access)"
                        }
                        libc::SIGBUS => "SIGBUS (bus error)",
                        libc::SIGTRAP => "SIGTRAP (trap — likely Cranelift trap instruction)",
                        _ => return write!(f, "JIT signal: signal {} (unknown)", sig),
                    };
                    write!(f, "JIT signal: {}", name)
                }
                #[cfg(not(unix))]
                write!(f, "JIT signal: signal {}", sig)
            }
        }
    }
}

impl std::error::Error for YieldError {}

impl From<crate::host_fns::RuntimeError> for YieldError {
    fn from(err: crate::host_fns::RuntimeError) -> Self {
        use crate::host_fns::RuntimeError;
        match err {
            RuntimeError::DivisionByZero => YieldError::DivisionByZero,
            RuntimeError::Overflow => YieldError::Overflow,
            RuntimeError::UserError => YieldError::UserError,
            RuntimeError::Undefined => YieldError::Undefined,
            RuntimeError::TypeMetadata => YieldError::TypeMetadata,
            RuntimeError::UnresolvedVar(id) => YieldError::UnresolvedVar(id),
            RuntimeError::NullFunPtr => YieldError::NullFunPtr,
            RuntimeError::BadFunPtrTag(tag) => YieldError::BadFunPtrTag(tag),
            RuntimeError::HeapOverflow => YieldError::HeapOverflow,
        }
    }
}
