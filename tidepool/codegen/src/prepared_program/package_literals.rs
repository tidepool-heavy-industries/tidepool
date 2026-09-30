//! Native-local literal bindings preserve the certified original group. Only
//! an actual compiled Bytes top can supply storage; the runtime supplies its
//! same-target protected package-interface witness before requesting a token.

use super::{CompileError, CompiledProgram, GlobalRefusalPhase};
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_repr::execution_schema::{
    CertifiedGroup, GlobalId, ImportOwner, RuntimeRep, SymbolIdentity,
};

#[cfg(test)]
mod tests;

/// Owned bytes and exact import contract, rather than an authorization token.
/// The runtime's protected target/package owner supplies the interface witness.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PackageLiteral {
    identity: SymbolIdentity,
    interface_digest: [u8; 32],
    // Includes GHC's implicit final NUL; embedded NUL bytes remain distinct.
    storage: Arc<[u8]>,
}

impl PackageLiteral {
    pub(super) fn storage(&self) -> &Arc<[u8]> {
        &self.storage
    }

    pub(super) fn logical_bytes(&self) -> &[u8] {
        &self.storage[..self.storage.len() - 1]
    }
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub(super) struct GroupPackageLiterals(BTreeMap<GlobalId, PackageLiteral>);

impl GroupPackageLiterals {
    pub(super) fn select(
        group: &CertifiedGroup,
        supplied: &BTreeMap<SymbolIdentity, PackageLiteral>,
    ) -> Result<Self, CompileError> {
        let mut selected = BTreeMap::new();
        for (index, (declaration, owner)) in group
            .definitions()
            .globals()
            .iter()
            .zip(group.imports())
            .enumerate()
        {
            if declaration.rep != RuntimeRep::Address {
                continue;
            }
            let id = GlobalId(index as u32);
            let missing = || {
                super::unsupported_global(
                    &group.definitions(),
                    id,
                    GlobalRefusalPhase::NonReferenceRepresentation,
                )
            };
            let literal = supplied.get(&declaration.identity).ok_or_else(missing)?;
            let ImportOwner::Package {
                unit,
                module,
                binder,
                interface_digest,
            } = owner
            else {
                return Err(missing().into());
            };
            if declaration.entry_signature.is_some()
                || !declaration.required_evaluated
                || declaration.required_generation.is_some()
                || binder != &declaration.identity
                || &binder.unit != unit
                || &binder.module != module
                || literal.identity != *binder
                || literal.interface_digest != *interface_digest
                || *interface_digest == [0; 32]
            {
                return Err(CompileError::PackageLiteralContract(Box::new(
                    declaration.identity.clone(),
                )));
            }
            selected.insert(id, literal.clone());
        }
        Ok(Self(selected))
    }

    pub(super) fn get(&self, id: GlobalId) -> Option<&PackageLiteral> {
        self.0.get(&id)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&GlobalId, &PackageLiteral)> {
        self.0.iter()
    }
}

impl CompiledProgram {
    /// Inventory actual immutable byte tops under caller-supplied interface
    /// facts. The runtime must first validate its protected same-target package
    /// certificate; this native storage capsule does not issue that authority.
    /// It does not issue machine handles or advertise managed code exports.
    pub fn package_literals(
        &self,
        digest_for: impl Fn(&str, &str) -> Option<[u8; 32]>,
    ) -> BTreeMap<SymbolIdentity, PackageLiteral> {
        self.byte_tops
            .iter()
            .filter_map(|(id, storage)| {
                let export = self.top_exports.get(id)?;
                let digest = digest_for(&export.identity.unit, &export.identity.module)?;
                if digest == [0; 32]
                    || export.identity.namespace != "value"
                    || export.rep != RuntimeRep::Address
                    || export.entry_signature.is_some()
                    || !export.evaluated
                    || storage.last() != Some(&0)
                {
                    return None;
                }
                let identity = export.identity.clone();
                Some((
                    identity.clone(),
                    PackageLiteral {
                        identity,
                        interface_digest: digest,
                        storage: Arc::clone(storage),
                    },
                ))
            })
            .collect()
    }
}
