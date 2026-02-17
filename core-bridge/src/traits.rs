use core_repr::DataConTable;
use core_eval::Value;
use crate::error::BridgeError;

/// Convert a Core Value (from evaluation) to a Rust type.
pub trait FromCore: Sized {
    /// Convert a Value to this type using the provided DataConTable for lookups.
    fn from_value(value: &Value, table: &DataConTable) -> Result<Self, BridgeError>;
}

/// Convert a Rust type to a Core Value (for interpolation into CoreExpr or evaluation).
pub trait ToCore {
    /// Convert this type to a Value using the provided DataConTable for lookups.
    fn to_value(&self, table: &DataConTable) -> Value;
}
