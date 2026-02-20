use tidepool_repr::DataConId;
use std::error::Error;
use std::fmt;

/// Errors that can occur when bridging between Rust types and Core Values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    /// The `DataConId` was not found in the `DataConTable`.
    UnknownDataCon(DataConId),
    /// The `DataConId` was found, but it has an unexpected name.
    UnknownDataConName(String),
    /// The number of fields in a constructor does not match the expected arity.
    ArityMismatch {
        /// The constructor identifier.
        con: DataConId,
        /// The expected number of fields.
        expected: usize,
        /// The actual number of fields received.
        got: usize,
    },
    /// The value has an unexpected type (e.g., expected a Literal, got a Con).
    TypeMismatch {
        /// A description of the expected type.
        expected: String,
        /// A description of the actual type received.
        got: String,
    },
    /// The type is not supported by the bridge.
    UnsupportedType(String),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BridgeError::UnknownDataCon(id) => write!(f, "Unknown DataConId: {:?}", id),
            BridgeError::UnknownDataConName(name) => write!(f, "Unknown DataCon name: {}", name),
            BridgeError::ArityMismatch { con, expected, got } => {
                write!(
                    f,
                    "Arity mismatch for DataCon {:?}: expected {}, got {}",
                    con, expected, got
                )
            }
            BridgeError::TypeMismatch { expected, got } => {
                write!(f, "Type mismatch: expected {}, got {}", expected, got)
            }
            BridgeError::UnsupportedType(ty) => write!(f, "Unsupported type: {}", ty),
        }
    }
}

impl Error for BridgeError {}
