use thiserror::Error;
use tidepool_repr::DataConId;

/// Errors that can occur when bridging between Rust types and Core Values.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    /// The `DataConId` was not found in the `DataConTable`.
    #[error("Unknown DataConId: {0:?}")]
    UnknownDataCon(DataConId),
    /// The `DataConId` was found, but it has an unexpected name.
    #[error("Unknown DataCon name: {0}")]
    UnknownDataConName(String),
    /// The number of fields in a constructor does not match the expected arity.
    #[error("Arity mismatch for DataCon {con:?}: expected {expected}, got {got}")]
    ArityMismatch {
        /// The constructor identifier.
        con: DataConId,
        /// The expected number of fields.
        expected: usize,
        /// The actual number of fields received.
        got: usize,
    },
    /// The value has an unexpected type (e.g., expected a Literal, got a Con).
    #[error("Type mismatch: expected {expected}, got {got}")]
    TypeMismatch {
        /// A description of the expected type.
        expected: String,
        /// A description of the actual type received.
        got: String,
    },
    /// The type is not supported by the bridge.
    #[error("Unsupported type: {0}")]
    UnsupportedType(String),
    /// Internal invariant violation (should never happen).
    #[error("Internal error: {0}")]
    InternalError(String),
}
