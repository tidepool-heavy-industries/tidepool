//! Immutable literal imports retain their original certified producer. Package
//! literals additionally require the protected target's interface witness.

use super::{CompileError, CompiledProgram, DemandedImage, GlobalRefusalPhase, SourceBinder};
use std::collections::BTreeMap;
use std::sync::Arc;
use tidepool_repr::execution_schema::{
    CachedHomeOwner, CertifiedGroup, CertifiedGroupCode, DefinitionsView, GlobalId, Group, HeapRhs,
    ImportOwner, RuntimeRep, SymbolIdentity, ValueId,
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
}

/// Storage from an actual compiled original singleton Bytes group. Its private
/// provenance cannot be supplied by a caller with an arbitrary address or handle.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceLiteral {
    source: SourceLiteralOwner,
    storage: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceLiteralOwner {
    pub owner: CachedHomeOwner,
    pub original_ordinal: u32,
    pub binder: SourceBinder,
}

impl SourceLiteralOwner {
    pub(super) fn from_code(group: &CertifiedGroupCode) -> Option<(ValueId, Self)> {
        let definitions = group.definitions();
        let [Group::NonRecursive(top)] = definitions.bindings() else {
            return None;
        };
        let [binder] = group.binders() else {
            return None;
        };
        if binder != &top.identity
            || !matches!(top.binding.rhs, HeapRhs::Bytes(_))
            || !definitions.globals().is_empty()
        {
            return None;
        }
        Some((
            top.binding.id,
            Self {
                owner: group.owner().clone(),
                original_ordinal: group.original_ordinal(),
                binder: SourceBinder {
                    version: group.owner().module_version.clone(),
                    binder: binder.clone(),
                },
            },
        ))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum LiteralImport {
    Package(PackageLiteral),
    Source(SourceLiteral),
}

impl LiteralImport {
    pub(super) fn storage(&self) -> &Arc<[u8]> {
        match self {
            Self::Package(literal) => literal.storage(),
            Self::Source(literal) => &literal.storage,
        }
    }

    pub(super) fn logical_bytes(&self) -> &[u8] {
        let storage = self.storage();
        &storage[..storage.len() - 1]
    }

    pub(super) fn source(&self) -> Option<&SourceLiteralOwner> {
        match self {
            Self::Package(_) => None,
            Self::Source(literal) => Some(&literal.source),
        }
    }
}

/// Immutable storage selection facts, never machine-local binding authority.
#[derive(Clone, Debug)]
pub enum LiteralOwner {
    Source {
        version: tidepool_repr::execution_schema::ModuleVersion,
        binder: SymbolIdentity,
    },
    Package {
        unit: String,
        module: String,
        binder: SymbolIdentity,
        interface_digest: [u8; 32],
    },
}

impl LiteralOwner {
    fn from_import(owner: &ImportOwner) -> Option<Self> {
        match owner {
            ImportOwner::Source { version, binder } => Some(Self::Source {
                version: version.clone(),
                binder: binder.clone(),
            }),
            ImportOwner::Package {
                unit,
                module,
                binder,
                interface_digest,
            } => Some(Self::Package {
                unit: unit.clone(),
                module: module.clone(),
                binder: binder.clone(),
                interface_digest: *interface_digest,
            }),
            ImportOwner::Retained { .. } | ImportOwner::CodeExport { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub(super) struct GroupPackageLiterals(BTreeMap<GlobalId, LiteralImport>);

impl GroupPackageLiterals {
    pub(super) fn select(
        group: &CertifiedGroup,
        supplied: &BTreeMap<SymbolIdentity, PackageLiteral>,
        sources: &BTreeMap<SourceBinder, SourceLiteral>,
    ) -> Result<Self, CompileError> {
        Self::select_definitions(&group.definitions(), group.imports(), supplied, sources)
    }

    pub(super) fn select_definitions(
        definitions: &DefinitionsView<'_>,
        owners: &[ImportOwner],
        supplied: &BTreeMap<SymbolIdentity, PackageLiteral>,
        sources: &BTreeMap<SourceBinder, SourceLiteral>,
    ) -> Result<Self, CompileError> {
        let owners = owners
            .iter()
            .map(LiteralOwner::from_import)
            .collect::<Vec<_>>();
        Self::select_immutable(definitions, &owners, supplied, sources)
    }

    pub(super) fn select_immutable(
        definitions: &DefinitionsView<'_>,
        owners: &[Option<LiteralOwner>],
        supplied: &BTreeMap<SymbolIdentity, PackageLiteral>,
        sources: &BTreeMap<SourceBinder, SourceLiteral>,
    ) -> Result<Self, CompileError> {
        if definitions.globals().len() != owners.len() {
            return Err(CompileError::LiteralImportCount {
                globals: definitions.globals().len(),
                owners: owners.len(),
            });
        }
        let mut selected = BTreeMap::new();
        for (index, (declaration, owner)) in definitions.globals().iter().zip(owners).enumerate() {
            if declaration.rep != RuntimeRep::Address {
                continue;
            }
            let id = GlobalId(index as u32);
            let missing = || {
                super::unsupported_global(
                    definitions,
                    id,
                    GlobalRefusalPhase::NonReferenceRepresentation,
                )
            };
            if let Some(LiteralOwner::Source { version, binder }) = owner {
                let key = SourceBinder {
                    version: version.clone(),
                    binder: binder.clone(),
                };
                let literal = sources.get(&key).ok_or_else(missing)?;
                if declaration.entry_signature.is_some()
                    || !declaration.required_evaluated
                    || declaration.required_generation.is_some()
                    || binder != &declaration.identity
                    || literal.source.binder != key
                    || literal.source.owner.module_version != *version
                    || literal.source.owner.unit != binder.unit
                    || literal.source.owner.module != binder.module
                {
                    return Err(CompileError::SourceLiteralContract(Box::new(key)));
                }
                selected.insert(id, LiteralImport::Source(literal.clone()));
                continue;
            }
            let literal = supplied.get(&declaration.identity).ok_or_else(missing)?;
            let Some(LiteralOwner::Package {
                unit,
                module,
                binder,
                interface_digest,
            }) = owner
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
            selected.insert(id, LiteralImport::Package(literal.clone()));
        }
        Ok(Self(selected))
    }

    pub(super) fn get(&self, id: GlobalId) -> Option<&LiteralImport> {
        self.0.get(&id)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&GlobalId, &LiteralImport)> {
        self.0.iter()
    }
}

impl DemandedImage {
    /// Inventory only the original binder of an independent singleton Bytes
    /// group. Mixed groups need their live managed instance, never a recompile
    /// of its mutable CAFs to recover an address.
    pub fn source_literals(&self) -> BTreeMap<SourceBinder, SourceLiteral> {
        if self
            .image()
            .certified_source
            .as_ref()
            .is_none_or(|(owner, ordinal)| {
                owner != self.group().owner() || *ordinal != self.group().original_ordinal()
            })
        {
            return BTreeMap::new();
        }
        self.image().source_literals()
    }
}

impl CompiledProgram {
    /// Literal storage issued by compilation of a validated original Bytes group.
    pub fn source_literals(&self) -> BTreeMap<SourceBinder, SourceLiteral> {
        let image = self;
        let Some((value, source)) = image.source_literal_producer.as_ref() else {
            return BTreeMap::new();
        };
        let Some(export) = image.top_exports.get(value) else {
            return BTreeMap::new();
        };
        let Some(storage) = image.byte_tops.get(value) else {
            return BTreeMap::new();
        };
        if export.identity != source.binder.binder
            || export.rep != RuntimeRep::Address
            || export.entry_signature.is_some()
            || !export.evaluated
            || storage.last() != Some(&0)
        {
            return BTreeMap::new();
        }
        let key = source.binder.clone();
        BTreeMap::from([(
            key.clone(),
            SourceLiteral {
                source: source.clone(),
                storage: Arc::clone(storage),
            },
        )])
    }
}

impl CompiledProgram {
    /// The exact source binder authenticated for this immutable import slot.
    /// Address representation alone never exempts a managed source lease.
    pub fn authenticated_source_literal(&self, id: GlobalId) -> Option<&SourceBinder> {
        self.import_slots
            .get(id.0 as usize)?
            .literal_source
            .as_ref()
            .map(|source| &source.binder)
    }

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
