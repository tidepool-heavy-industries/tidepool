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
        }
    }
}

impl std::error::Error for YieldError {}
