use crate::error::BridgeError;
use tidepool_eval::Value;
use tidepool_repr::DataConTable;

#[doc(hidden)]
pub struct FromCoreMarker;
#[doc(hidden)]
pub struct ToCoreMarker;

/// Private module for internal traits. Implementation of these traits is only
/// supported via the provided derive macros.
#[doc(hidden)]
pub mod __private {
    pub trait Sealed<T: ?Sized> {}
}

/// Convert a Core Value (from evaluation) to a Rust type.
///
/// This trait is used to extract native Rust values from evaluated Core expressions.
///
/// # Sealing
///
/// This trait is sealed and should only be implemented via `#[derive(FromCore)]`.
/// Manual implementations are unsupported and may break in future versions.
pub trait FromCore: Sized + __private::Sealed<FromCoreMarker> {
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
///
/// # Sealing
///
/// This trait is sealed and should only be implemented via `#[derive(ToCore)]`.
/// Manual implementations are unsupported and may break in future versions.
pub trait ToCore: __private::Sealed<ToCoreMarker> {
    /// Convert this type to a Value using the provided DataConTable for lookups.
    ///
    /// # Errors
    ///
    /// Returns `BridgeError::UnknownDataConName` if a required constructor is missing from the table.
    fn to_value(&self, table: &DataConTable) -> Result<Value, BridgeError>;
}
