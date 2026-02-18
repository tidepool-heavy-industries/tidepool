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
