use crate::error::BridgeError;
use crate::Value;
use tidepool_repr::DataConTable;

/// Implementation detail for sealing traits.
#[doc(hidden)]
pub mod sealed {
    pub trait FromHaskellSealed {}
    pub trait ToHaskellSealed {}
}

/// Decode an evaluated Haskell value into a Rust type.
///
/// This trait is used to extract native Rust values from evaluated Haskell data.
/// Implementations should handle potential type mismatches and arity errors.
pub trait FromHaskell: Sized + sealed::FromHaskellSealed {
    /// Convert a Value to this type using the provided DataConTable for lookups.
    ///
    /// # Errors
    ///
    /// Returns `BridgeError::TypeMismatch` if the value's variant doesn't match the expected type.
    /// Returns `BridgeError::UnknownDataCon` when the outer constructor does not
    /// match this type, or `BridgeError::UnknownDataConName` when required metadata
    /// is missing. Once a constructor matches, derived decoders wrap nested failures
    /// in `BridgeError::FieldDecode` so dispatch cannot mistake them for outer misses.
    /// Returns `BridgeError::ArityMismatch` if a constructor has the wrong number of fields.
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError>;
}

/// Encode a Rust type as a Haskell runtime value.
///
/// This trait is used to encode Rust values for the Haskell runtime.
pub trait ToHaskell: sealed::ToHaskellSealed {
    /// Convert this type to a Value using the provided DataConTable for lookups.
    ///
    /// # Errors
    ///
    /// Returns `BridgeError::UnknownDataConName` if a required constructor is missing from the table.
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError>;
}
