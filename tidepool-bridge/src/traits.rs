use crate::error::BridgeError;
use tidepool_eval::Value;
use tidepool_repr::DataConTable;

/// Implementation detail for sealing traits.
#[doc(hidden)]
pub mod sealed {
    pub trait FromCoreSealed {}
    pub trait ToCoreSealed {}
}

/// Convert a Core Value (from evaluation) to a Rust type.
///
/// This trait is used to extract native Rust values from evaluated Core expressions.
/// Implementations should handle potential type mismatches and arity errors.
pub trait FromCore: Sized + sealed::FromCoreSealed {
    /// Convert a Value to this type using the provided DataConTable for lookups.
    ///
    /// # Errors
    ///
    /// Returns `BridgeError::TypeMismatch` if the value's variant doesn't match the expected type.
    /// Returns `BridgeError::UnknownDataCon` or `BridgeError::UnknownDataConName` if a required
    /// constructor is missing from the table.
    /// Returns `BridgeError::ArityMismatch` if a constructor has the wrong number of fields.
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError>;
}

/// Convert a Rust type to a Core Value (for interpolation into CoreExpr or evaluation).
///
/// This trait is used to inject Rust values into the Core evaluator.
pub trait ToCore: sealed::ToCoreSealed {
    /// Convert this type to a Value using the provided DataConTable for lookups.
    ///
    /// # Errors
    ///
    /// Returns `BridgeError::UnknownDataConName` if a required constructor is missing from the table.
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError>;
}
