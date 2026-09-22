//! Failures and limits for host-built managed values.

use tidepool_heap::external_storage::ExternalStorageValidationError;
use tidepool_repr::DataConId;

use crate::descriptor_bridge::DescriptorMarshalError;

/// Why incremental managed construction could not complete.
/// No variant publishes a result handle.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnswerBuildError {
    #[error("byte storage for the answer could not be allocated: {0:?}")]
    Storage(ExternalStorageValidationError),
    /// The program's own `ByteArray#` wrapper descriptor refused its single
    /// address field: an integrity failure, not an answer-shape one.
    #[error("byte-array wrapper: {0}")]
    Wrapper(DescriptorMarshalError),
    #[error("constructor {0:?} is not declared by any installed program")]
    UnknownConstructor(DataConId),
    #[error("constructor {host_id:?} takes {expected} fields, the answer supplies {actual}")]
    FieldCount {
        host_id: DataConId,
        expected: usize,
        actual: usize,
    },
    #[error("constructor {host_id:?} field {index}: {error}")]
    Field {
        host_id: DataConId,
        index: usize,
        error: DescriptorMarshalError,
    },
    #[error("one managed construction allocation needs {0} bytes, more than the nursery can hold")]
    TooLarge(usize),
    /// Generated code packs objects at word alignment; a descriptor asking
    /// for more cannot be laid out in the span the builder reserves.
    #[error("constructor {host_id:?} requires {required}-byte alignment, above the nursery's word alignment")]
    Alignment { host_id: DataConId, required: u32 },
    /// A borrowed handle field named a handle no longer live in this engine's
    /// ledger (already released, or minted under another engine).
    #[error("a managed construction field's borrowed handle is not live")]
    UnknownHandle,
    #[error("a managed construction node belongs to another builder")]
    ForeignNode,
}

/// Semantic nesting bound for a structurally decoded host response.
pub const MAX_ANSWER_DEPTH: usize = 4096;
