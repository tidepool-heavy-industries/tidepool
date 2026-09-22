use crate::error::BridgeError;
use crate::HaskellValue;
use tidepool_repr::execution_schema::RuntimeRep;
use tidepool_repr::{DataConId, DataConTable, Literal};

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
    /// Convert a HaskellValue to this type using the provided DataConTable for lookups.
    ///
    /// # Errors
    ///
    /// Returns `BridgeError::TypeMismatch` if the value's variant doesn't match the expected type.
    /// Returns `BridgeError::UnknownDataCon` when the outer constructor does not
    /// match this type, or `BridgeError::UnknownDataConName` when required metadata
    /// is missing. Once a constructor matches, derived decoders wrap nested failures
    /// in `BridgeError::FieldDecode` so dispatch cannot mistake them for outer misses.
    /// Returns `BridgeError::ArityMismatch` if a constructor has the wrong number of fields.
    fn from_value(value: &HaskellValue, table: &DataConTable) -> Result<Self, BridgeError>;
}

/// Encode a Rust type as a Haskell runtime value.
///
/// This trait is used to encode Rust values for the Haskell runtime.
pub trait ToHaskell: sealed::ToHaskellSealed {
    /// Emit this value structurally. Implementations must emit exactly one
    /// complete root; containers emit their children between `begin`/`end`.
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError>;

    /// Collect this value for snapshot and compatibility consumers.
    fn to_value(&self, table: &DataConTable) -> Result<HaskellValue, BridgeError> {
        let mut collector = HaskellValueCollector::default();
        self.visit(table, &mut collector)?;
        collector.finish()
    }
}

/// Structural sink for Rust-to-Haskell conversion.
pub trait HaskellVisitor {
    fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError>;
    fn end_constructor(&mut self) -> Result<(), BridgeError>;
    fn literal(&mut self, literal: Literal) -> Result<(), BridgeError>;
    fn byte_array(&mut self, bytes: Vec<u8>) -> Result<(), BridgeError>;

    /// The representation expected for the next constructor field, when the
    /// sink constructs directly against authenticated descriptors. Structural
    /// encoders can use this to select an unboxed worker field instead of
    /// manufacturing a boxed source-level wrapper.
    fn expected_field_rep(&self) -> Option<RuntimeRep> {
        None
    }
}

#[derive(Default)]
struct HaskellValueCollector {
    stack: Vec<(DataConId, usize, Vec<HaskellValue>)>,
    root: Option<HaskellValue>,
}

impl HaskellValueCollector {
    fn push(&mut self, value: HaskellValue) -> Result<(), BridgeError> {
        if let Some((_, expected, fields)) = self.stack.last_mut() {
            if fields.len() >= *expected {
                return Err(BridgeError::TypeMismatch {
                    expected: format!("constructor with {expected} fields"),
                    got: "too many visitor fields".into(),
                });
            }
            fields.push(value);
        } else if self.root.replace(value).is_some() {
            return Err(BridgeError::TypeMismatch {
                expected: "one visitor root".into(),
                got: "multiple visitor roots".into(),
            });
        }
        Ok(())
    }

    fn finish(self) -> Result<HaskellValue, BridgeError> {
        if !self.stack.is_empty() {
            return Err(BridgeError::TypeMismatch {
                expected: "closed visitor constructors".into(),
                got: "unfinished constructor".into(),
            });
        }
        self.root.ok_or_else(|| BridgeError::TypeMismatch {
            expected: "one visitor root".into(),
            got: "no visitor root".into(),
        })
    }
}

impl HaskellVisitor for HaskellValueCollector {
    fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError> {
        self.stack.push((id, fields, Vec::with_capacity(fields)));
        Ok(())
    }

    fn end_constructor(&mut self) -> Result<(), BridgeError> {
        let (id, expected, fields) = self.stack.pop().ok_or_else(|| BridgeError::TypeMismatch {
            expected: "open visitor constructor".into(),
            got: "constructor end without begin".into(),
        })?;
        if fields.len() != expected {
            return Err(BridgeError::ArityMismatch {
                con: id,
                expected,
                got: fields.len(),
            });
        }
        self.push(HaskellValue::Con(id, fields))
    }

    fn literal(&mut self, literal: Literal) -> Result<(), BridgeError> {
        self.push(HaskellValue::Lit(literal))
    }

    fn byte_array(&mut self, bytes: Vec<u8>) -> Result<(), BridgeError> {
        self.push(HaskellValue::ByteArray(std::sync::Arc::new(
            std::sync::Mutex::new(bytes),
        )))
    }
}
