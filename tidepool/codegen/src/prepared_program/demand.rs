//! Exact source-owner demand for independently compiled recursive groups.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use tidepool_repr::execution_schema::{
    CachedHomeOwner, CertifiedGroup, ImportOwner, ModuleVersion, SymbolIdentity,
};

use super::{CompileError, CompiledProgram, ImageRegistry};

/// A source binder names one implementation at one compiler-assigned module
/// version. Textual identity alone is insufficient when a module changes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceBinder {
    pub version: ModuleVersion,
    pub binder: SymbolIdentity,
}

#[derive(Debug, thiserror::Error)]
pub enum DemandError {
    #[error("source module {unit}:{module} at {version:?} has conflicting product owners")]
    ConflictingOwner {
        unit: String,
        module: String,
        version: ModuleVersion,
    },
    #[error("source module {unit}:{module} repeats original group ordinal {ordinal}")]
    DuplicateGroup {
        unit: String,
        module: String,
        ordinal: u32,
    },
    #[error("source binder {0:?} is supplied by more than one original group")]
    DuplicateBinder(SourceBinder),
    #[error("no certified implementation for source binder {0:?}")]
    MissingSource(SourceBinder),
    #[error(transparent)]
    Compile(#[from] CompileError),
}

/// A validated inventory over original group arenas. All exact source edges
/// are checked during sealing, before any image is compiled or installed.
pub struct GroupInventory<'a> {
    groups: &'a [CertifiedGroup],
    binders: BTreeMap<SourceBinder, usize>,
}

pub struct SealedDemand<'a> {
    groups: &'a [CertifiedGroup],
    /// Closure order is stable in the supplied inventory's original order.
    indices: Vec<usize>,
}

pub struct DemandedImage<'a> {
    group: &'a CertifiedGroup,
    image: Arc<CompiledProgram>,
}

impl<'a> DemandedImage<'a> {
    pub fn group(&self) -> &'a CertifiedGroup {
        self.group
    }

    pub fn image(&self) -> &Arc<CompiledProgram> {
        &self.image
    }
}

impl<'a> GroupInventory<'a> {
    pub fn new(groups: &'a [CertifiedGroup]) -> Result<Self, DemandError> {
        let mut owners: BTreeMap<(String, String, ModuleVersion), &CachedHomeOwner> =
            BTreeMap::new();
        let mut ordinals = BTreeSet::new();
        let mut binders = BTreeMap::new();
        for (index, group) in groups.iter().enumerate() {
            let owner = group.owner();
            let module = (
                owner.unit.clone(),
                owner.module.clone(),
                owner.module_version.clone(),
            );
            if let Some(existing) = owners.insert(module.clone(), owner) {
                if existing != owner {
                    return Err(DemandError::ConflictingOwner {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                        version: owner.module_version.clone(),
                    });
                }
            }
            if !ordinals.insert((module, group.original_ordinal())) {
                return Err(DemandError::DuplicateGroup {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                    ordinal: group.original_ordinal(),
                });
            }
            for binder in group.binders() {
                let key = SourceBinder {
                    version: owner.module_version.clone(),
                    binder: binder.clone(),
                };
                if binders.insert(key.clone(), index).is_some() {
                    return Err(DemandError::DuplicateBinder(key));
                }
            }
        }
        Ok(Self { groups, binders })
    }

    /// Close imports transitively, including cycles. A source edge without an
    /// exact implementation fails before the returned batch can be consumed.
    pub fn seal(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
    ) -> Result<SealedDemand<'a>, DemandError> {
        let mut pending = VecDeque::new();
        for root in roots {
            pending.push_back(root);
        }
        let mut reachable = BTreeSet::new();
        while let Some(binder) = pending.pop_front() {
            let &index = self
                .binders
                .get(&binder)
                .ok_or(DemandError::MissingSource(binder))?;
            if !reachable.insert(index) {
                continue;
            }
            for origin in self.groups[index].imports() {
                if let ImportOwner::Source { version, binder } = origin {
                    pending.push_back(SourceBinder {
                        version: version.clone(),
                        binder: binder.clone(),
                    });
                }
            }
        }
        Ok(SealedDemand {
            groups: self.groups,
            indices: reachable.into_iter().collect(),
        })
    }
}

impl<'a> SealedDemand<'a> {
    pub fn groups(&self) -> impl ExactSizeIterator<Item = &'a CertifiedGroup> + '_ {
        self.indices.iter().map(|&index| &self.groups[index])
    }

    /// Compile only demanded images. The registry owns weak references and
    /// coalesces concurrent compiles of the same certified original group.
    pub fn compile(&self, registry: &ImageRegistry) -> Result<Vec<DemandedImage<'a>>, DemandError> {
        self.groups()
            .map(|group| {
                let image = registry.get_or_compile_group(group, || {
                    CompiledProgram::compile_certified_group(group).map(Arc::new)
                })?;
                Ok(DemandedImage { group, image })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::execution_schema::{testing, GlobalDecl, RuntimeRep};

    fn group(name: &str, ordinal: u32, imports: &[&str]) -> CertifiedGroup {
        let mut wire = testing::wire_program();
        wire.bindings[0] = match wire.bindings[0].clone() {
            tidepool_repr::execution_schema::Group::NonRecursive(mut top) => {
                top.identity = testing::identity("Fixture", name);
                tidepool_repr::execution_schema::Group::NonRecursive(top)
            }
            _ => unreachable!(),
        };
        let origins = imports
            .iter()
            .map(|name| {
                let binder = testing::identity("Fixture", name);
                wire.globals.push(GlobalDecl {
                    identity: binder.clone(),
                    rep: RuntimeRep::LiftedRef,
                    entry_signature: None,
                    required_evaluated: false,
                    required_generation: None,
                });
                ImportOwner::Source {
                    version: ModuleVersion([1; 32]),
                    binder,
                }
            })
            .collect();
        CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, ordinal).unwrap(),
            origins,
        )
        .unwrap()
    }

    fn source(name: &str) -> SourceBinder {
        SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", name),
        }
    }

    #[test]
    fn recursive_source_demand_compiles_only_reachable_original_groups() {
        let groups = [
            group("a", 4, &["b"]),
            group("b", 9, &["a"]),
            group("unused", 12, &[]),
        ];
        let inventory = GroupInventory::new(&groups).unwrap();
        let demand = inventory.seal([source("a")]).unwrap();
        assert_eq!(
            demand
                .groups()
                .map(CertifiedGroup::original_ordinal)
                .collect::<Vec<_>>(),
            vec![4, 9]
        );
        let registry = ImageRegistry::new();
        let first = demand.compile(&registry).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(registry.misses(), 2);
        let second = demand.compile(&registry).unwrap();
        assert!(first
            .iter()
            .zip(&second)
            .all(|(a, b)| Arc::ptr_eq(&a.image, &b.image)));
        assert_eq!(registry.hits(), 2);
    }

    #[test]
    fn missing_exact_source_refuses_batch_before_native_compile() {
        let groups = [group("a", 4, &["missing"])];
        let inventory = GroupInventory::new(&groups).unwrap();
        assert!(matches!(
            inventory.seal([source("a")]),
            Err(DemandError::MissingSource(missing)) if missing == source("missing")
        ));
    }
}
