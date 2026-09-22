//! `serde_json::Value` streaming through program-authenticated JSON roles.

use crate::error::BridgeError;
use crate::traits::{sealed::ToHaskellSealed, HaskellVisitor, ToHaskell};
use tidepool_repr::DataConTable;

impl ToHaskellSealed for serde_json::Value {}

impl ToHaskell for serde_json::Value {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let layout = table.json_layout().ok_or_else(|| {
            BridgeError::UnknownDataConName(
                "compiler-authenticated JSON layout is unavailable for this host mount".into(),
            )
        })?;
        crate::json_builder::visit_json(self, layout, visitor)
    }
}
