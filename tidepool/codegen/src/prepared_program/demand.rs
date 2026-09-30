//! Exact source-owner demand for independently compiled recursive groups.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use tidepool_repr::execution_schema::{
    CachedHomeOwner, CertifiedGroup, DefinitionsView, Group, ImportOwner, ModuleVersion,
    ProjectedGroup, Signature, SymbolIdentity, ValueId,
};

use super::{CompileError, CompiledProgram, GroupInstanceId, ImageRegistry, PreparedHandle};

/// A source binder names one implementation at one compiler-assigned module
/// version. Textual identity alone is insufficient when a module changes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceBinder {
    pub version: ModuleVersion,
    pub binder: SymbolIdentity,
}

/// Machine-owned root for one materialized source binder in one original
/// group installation. Cloning this descriptor does not mint another root:
/// scope custody shares one handle and releases it exactly once when the last
/// lexical/capture owner closes.
#[derive(Clone, Debug)]
pub struct SourceInstanceLease {
    instance: GroupInstanceId,
    owner: CachedHomeOwner,
    original_ordinal: u32,
    binder: SourceBinder,
    value: ValueId,
    handle: PreparedHandle,
    entry_signature: Option<Signature>,
}

impl SourceInstanceLease {
    pub(crate) fn new(
        instance: GroupInstanceId,
        owner: CachedHomeOwner,
        original_ordinal: u32,
        binder: SourceBinder,
        value: ValueId,
        handle: PreparedHandle,
        entry_signature: Option<Signature>,
    ) -> Self {
        Self {
            instance,
            owner,
            original_ordinal,
            binder,
            value,
            handle,
            entry_signature,
        }
    }

    #[must_use]
    pub fn instance(&self) -> GroupInstanceId {
        self.instance
    }

    #[must_use]
    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }

    #[must_use]
    pub fn original_ordinal(&self) -> u32 {
        self.original_ordinal
    }

    #[must_use]
    pub fn binder(&self) -> &SourceBinder {
        &self.binder
    }

    #[must_use]
    pub fn value(&self) -> ValueId {
        self.value
    }

    #[must_use]
    pub fn handle(&self) -> PreparedHandle {
        self.handle
    }

    #[must_use]
    pub fn entry_signature(&self) -> Option<&Signature> {
        self.entry_signature.as_ref()
    }
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
    #[error("scoped source instance does not match certified group for {0:?}")]
    InvalidInheritedInstance(SourceBinder),
    #[error("source group has invalid original binder inventory for {0:?}")]
    InvalidGroup(SourceBinder),
    #[error(transparent)]
    Compile(#[from] CompileError),
}

/// Source-only outline of one worker-certified original group. Retained and
/// package imports are intentionally absent: they are resolved only if demand
/// reaches a group that needs a new native instance.
pub struct SourceGroupOutline {
    owner: CachedHomeOwner,
    original_ordinal: u32,
    tops: BTreeMap<SymbolIdentity, ValueId>,
    imports: Vec<SourceBinder>,
}

impl SourceGroupOutline {
    pub fn from_projected(
        owner: CachedHomeOwner,
        group: &ProjectedGroup,
        source_imports: Vec<SourceBinder>,
    ) -> Result<Self, DemandError> {
        Self::from_definitions(
            owner,
            group.original_ordinal(),
            group.binders(),
            group.definitions(),
            source_imports,
        )
    }

    fn from_certified(group: &CertifiedGroup) -> Result<Self, DemandError> {
        let imports = group
            .imports()
            .iter()
            .filter_map(|origin| match origin {
                ImportOwner::Source { version, binder } => Some(SourceBinder {
                    version: version.clone(),
                    binder: binder.clone(),
                }),
                ImportOwner::Retained { .. }
                | ImportOwner::CodeExport { .. }
                | ImportOwner::Package { .. } => None,
            })
            .collect();
        Self::from_definitions(
            group.owner().clone(),
            group.original_ordinal(),
            group.binders(),
            group.definitions(),
            imports,
        )
    }

    fn from_definitions(
        owner: CachedHomeOwner,
        original_ordinal: u32,
        binders: &[SymbolIdentity],
        definitions: DefinitionsView<'_>,
        imports: Vec<SourceBinder>,
    ) -> Result<Self, DemandError> {
        let all_tops: BTreeMap<_, _> = definitions
            .bindings()
            .iter()
            .flat_map(|binding| match binding {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops.as_slice(),
            })
            .map(|top| (&top.identity, top.binding.id))
            .collect();
        let mut tops = BTreeMap::new();
        for binder in binders {
            let key = SourceBinder {
                version: owner.module_version.clone(),
                binder: binder.clone(),
            };
            let Some(&value) = all_tops.get(binder) else {
                return Err(DemandError::InvalidGroup(key));
            };
            if binder.unit != owner.unit
                || binder.module != owner.module
                || tops.insert(binder.clone(), value).is_some()
            {
                return Err(DemandError::InvalidGroup(key));
            }
        }
        for import in &imports {
            if !definitions.globals().iter().any(|global| {
                global.identity == import.binder && global.required_generation.is_none()
            }) {
                return Err(DemandError::InvalidGroup(import.clone()));
            }
        }
        Ok(Self {
            owner,
            original_ordinal,
            tops,
            imports,
        })
    }
}

/// The one exact source-closure policy used before and after retained-import
/// resolution. `PendingGroupInventory` uses it without requiring unused
/// groups' retained values to remain live.
struct DemandIndex {
    groups: Vec<SourceGroupOutline>,
    binders: BTreeMap<SourceBinder, usize>,
}

pub struct PendingGroupInventory {
    index: DemandIndex,
}

pub struct PendingSealedDemand {
    indices: Vec<usize>,
    inherited: Vec<InheritedSourceDemand>,
}

impl PendingSealedDemand {
    pub fn new_group_indices(&self) -> &[usize] {
        &self.indices
    }

    pub fn inherited_demands(&self) -> &[InheritedSourceDemand] {
        &self.inherited
    }

    pub fn into_parts(self) -> (Vec<usize>, Vec<InheritedSourceDemand>) {
        (self.indices, self.inherited)
    }
}

impl PendingGroupInventory {
    pub fn new(groups: Vec<SourceGroupOutline>) -> Result<Self, DemandError> {
        Ok(Self {
            index: DemandIndex::new(groups)?,
        })
    }

    pub fn seal_with_inherited(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
        existing: &BTreeMap<SourceBinder, SourceInstanceLease>,
        anchors: &HashMap<(CachedHomeOwner, u32), SourceInstanceLease>,
    ) -> Result<PendingSealedDemand, DemandError> {
        self.index.seal(roots, existing, anchors)
    }
}

/// A validated inventory over original group arenas. All exact source edges
/// are checked during sealing, before any image is compiled or installed.
pub struct GroupInventory<'a> {
    groups: &'a [CertifiedGroup],
    index: DemandIndex,
}

pub struct SealedDemand<'a> {
    groups: &'a [CertifiedGroup],
    /// Closure order is stable in the supplied inventory's original order.
    indices: Vec<usize>,
    inherited: Vec<InheritedSourceDemand>,
}

/// A new binder requested from a previously installed group in the same
/// lexical instance domain. The anchor is a live scoped root for that exact
/// installation; no second group image or CAF instance is compiled.
#[derive(Clone)]
pub struct InheritedSourceDemand {
    owner: CachedHomeOwner,
    original_ordinal: u32,
    binder: SourceBinder,
    value: ValueId,
    anchor: SourceInstanceLease,
}

impl InheritedSourceDemand {
    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }
    pub fn original_ordinal(&self) -> u32 {
        self.original_ordinal
    }
    pub fn binder(&self) -> &SourceBinder {
        &self.binder
    }
    pub fn value(&self) -> ValueId {
        self.value
    }
    pub fn anchor(&self) -> &SourceInstanceLease {
        &self.anchor
    }
}

pub struct DemandedImage {
    group: CertifiedGroup,
    image: Arc<CompiledProgram>,
}

impl DemandedImage {
    /// Compile one already selected original group outside the machine
    /// checkout. The batch installer still checks the complete closed set
    /// before publishing any native program.
    pub fn compile(group: CertifiedGroup, registry: &ImageRegistry) -> Result<Self, DemandError> {
        let image = registry.get_or_compile_group(&group, || {
            CompiledProgram::compile_certified_group(&group).map(Arc::new)
        })?;
        Ok(Self { group, image })
    }

    pub fn group(&self) -> &CertifiedGroup {
        &self.group
    }

    pub fn image(&self) -> &Arc<CompiledProgram> {
        &self.image
    }
}

impl<'a> GroupInventory<'a> {
    pub fn new(groups: &'a [CertifiedGroup]) -> Result<Self, DemandError> {
        let outlines = groups
            .iter()
            .map(SourceGroupOutline::from_certified)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            groups,
            index: DemandIndex::new(outlines)?,
        })
    }

    /// Close imports transitively, including cycles. A source edge without an
    /// exact implementation fails before the returned batch can be consumed.
    pub fn seal(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
    ) -> Result<SealedDemand<'a>, DemandError> {
        self.seal_with_inherited(roots, &BTreeMap::new(), &HashMap::new())
    }

    /// Stop closure at exact live lexical instances. A later demand for an
    /// unrooted sibling binder reuses that same group installation; it is
    /// recorded separately for native root acquisition under checkout.
    pub fn seal_with_inherited(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
        existing: &BTreeMap<SourceBinder, SourceInstanceLease>,
        anchors: &HashMap<(CachedHomeOwner, u32), SourceInstanceLease>,
    ) -> Result<SealedDemand<'a>, DemandError> {
        let selected = self.index.seal(roots, existing, anchors)?;
        Ok(SealedDemand {
            groups: self.groups,
            indices: selected.indices,
            inherited: selected.inherited,
        })
    }
}

impl DemandIndex {
    fn new(groups: Vec<SourceGroupOutline>) -> Result<Self, DemandError> {
        let mut owners: BTreeMap<(String, String, ModuleVersion), &CachedHomeOwner> =
            BTreeMap::new();
        let mut ordinals = BTreeSet::new();
        let mut binders = BTreeMap::new();
        for (index, group) in groups.iter().enumerate() {
            let owner = &group.owner;
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
            if !ordinals.insert((module, group.original_ordinal)) {
                return Err(DemandError::DuplicateGroup {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                    ordinal: group.original_ordinal,
                });
            }
            for binder in group.tops.keys() {
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

    fn seal(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
        existing: &BTreeMap<SourceBinder, SourceInstanceLease>,
        anchors: &HashMap<(CachedHomeOwner, u32), SourceInstanceLease>,
    ) -> Result<PendingSealedDemand, DemandError> {
        let mut pending = VecDeque::new();
        for root in roots {
            pending.push_back(root);
        }
        let mut reachable = BTreeSet::new();
        let mut inherited = BTreeMap::new();
        while let Some(binder) = pending.pop_front() {
            let &index = self
                .binders
                .get(&binder)
                .ok_or_else(|| DemandError::MissingSource(binder.clone()))?;
            let group = &self.groups[index];
            if let Some(lease) = existing.get(&binder) {
                if lease.binder() != &binder
                    || lease.owner() != &group.owner
                    || lease.original_ordinal() != group.original_ordinal
                {
                    return Err(DemandError::InvalidInheritedInstance(binder));
                }
                continue;
            }
            if let Some(anchor) = anchors.get(&(group.owner.clone(), group.original_ordinal)) {
                if anchor.owner() != &group.owner
                    || anchor.original_ordinal() != group.original_ordinal
                    || anchor.binder().version != group.owner.module_version
                    || !group.tops.contains_key(&anchor.binder().binder)
                {
                    return Err(DemandError::InvalidInheritedInstance(binder));
                }
                inherited
                    .entry(binder.clone())
                    .or_insert_with(|| InheritedSourceDemand {
                        owner: group.owner.clone(),
                        original_ordinal: group.original_ordinal,
                        value: group.tops[&binder.binder],
                        binder,
                        anchor: anchor.clone(),
                    });
                continue;
            }
            if !reachable.insert(index) {
                continue;
            }
            pending.extend(group.imports.iter().cloned());
        }
        Ok(PendingSealedDemand {
            indices: reachable.into_iter().collect(),
            inherited: inherited.into_values().collect(),
        })
    }
}

impl<'a> SealedDemand<'a> {
    pub fn groups(&self) -> impl ExactSizeIterator<Item = &'a CertifiedGroup> + '_ {
        self.indices.iter().map(|&index| &self.groups[index])
    }

    pub fn inherited_demands(&self) -> &[InheritedSourceDemand] {
        &self.inherited
    }

    /// Compile only demanded images. The registry owns weak references and
    /// coalesces concurrent compiles of the same certified original group.
    pub fn compile(&self, registry: &ImageRegistry) -> Result<Vec<DemandedImage>, DemandError> {
        self.groups()
            .map(|group| DemandedImage::compile(group.clone(), registry))
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

    #[test]
    fn pending_source_closure_skips_unused_unresolvable_groups() {
        let groups = [
            group("a", 4, &["b"]),
            group("b", 9, &["a"]),
            group("unused", 12, &["missing"]),
        ];
        let outlines = groups
            .iter()
            .map(SourceGroupOutline::from_certified)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let pending = PendingGroupInventory::new(outlines).unwrap();
        let selected = pending
            .seal_with_inherited([source("a")], &BTreeMap::new(), &HashMap::new())
            .unwrap();
        assert_eq!(selected.new_group_indices(), &[0, 1]);
        assert!(selected.inherited_demands().is_empty());
        assert!(matches!(
            pending.seal_with_inherited([source("unused")], &BTreeMap::new(), &HashMap::new()),
            Err(DemandError::MissingSource(missing)) if missing == source("missing")
        ));

        let selected_images = {
            let registry = ImageRegistry::new();
            let inventory = GroupInventory::new(&groups).unwrap();
            inventory
                .seal([source("a")])
                .unwrap()
                .compile(&registry)
                .unwrap()
        };
        assert_eq!(selected_images.len(), 2);
        assert_eq!(selected_images[0].group().original_ordinal(), 4);
    }
}
