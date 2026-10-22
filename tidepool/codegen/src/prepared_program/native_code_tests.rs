//! Test composition of the public linked-code and preparation owners.

use super::{
    ImageRegistry, LiteralOwner, PackageLiteral, PreparedNativeCode, SourceBinder, SourceLiteral,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_repr::execution_schema::{InventoryOperation, SymbolIdentity};
use tidepool_repr::native_code::{
    NativeCodeBody, NativeCodeComponent, NativeCodePlan, NativeCodePlanImport,
};

pub(crate) fn prepare(
    body: NativeCodeBody,
    imports: Vec<NativeCodePlanImport>,
    owners: &[Option<LiteralOwner>],
    packages: &BTreeMap<SymbolIdentity, PackageLiteral>,
    sources: &BTreeMap<SourceBinder, SourceLiteral>,
    registry: &ImageRegistry,
) -> Arc<PreparedNativeCode> {
    let code = NativeCodeComponent::link(
        &[NativeCodePlan { body, imports }],
        &[0],
        &InventoryOperation::new(Default::default()),
    )
    .unwrap()
    .remove(0);
    Arc::new(PreparedNativeCode::prepare(code, owners, packages, sources, registry).unwrap())
}

pub(crate) fn prepare_closed(
    body: NativeCodeBody,
    registry: &ImageRegistry,
) -> Arc<PreparedNativeCode> {
    prepare(
        body,
        vec![],
        &[],
        &BTreeMap::new(),
        &BTreeMap::new(),
        registry,
    )
}
