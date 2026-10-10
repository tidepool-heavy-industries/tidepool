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

/// Runtime selection context minted by the existing lexical scope owner.
/// This identity is never reconstructed from durable source loss metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceInstanceDomain {
    namespace: crate::binding_table::BindingTipId,
    slot: u64,
}

impl SourceInstanceDomain {
    pub fn single() -> Self {
        Self::for_scope(crate::scope::ScopeId::ROOT)
    }
    pub(crate) fn for_scope(scope: crate::scope::ScopeId) -> Self {
        Self::in_view(crate::binding_table::BindingTipId(0), scope.0)
    }
    pub(crate) fn in_view(namespace: crate::binding_table::BindingTipId, slot: u64) -> Self {
        Self { namespace, slot }
    }
}

/// An exact implementation selected within one captured mutable environment.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScopedSourceBinder {
    pub domain: SourceInstanceDomain,
    pub source: SourceBinder,
}

/// Selected live instances and authored origins from one owning binding view.
/// Construction remains inside the existing scope/custody owner.
#[derive(Clone)]
pub struct SourceDomainSelection {
    current: SourceInstanceDomain,
    authored: HashMap<SourceInstanceDomain, HashMap<CachedHomeOwner, SourceInstanceDomain>>,
    inherited: BTreeMap<ScopedSourceBinder, SourceInstanceLease>,
    anchors: HashMap<(SourceInstanceDomain, CachedHomeOwner, u32), SourceInstanceLease>,
}

impl SourceDomainSelection {
    pub(crate) fn new(
        current: SourceInstanceDomain,
        authored: HashMap<SourceInstanceDomain, HashMap<CachedHomeOwner, SourceInstanceDomain>>,
        entries: impl IntoIterator<Item = (SourceInstanceDomain, SourceInstanceLease)>,
    ) -> Result<Self, DemandError> {
        if !authored.contains_key(&current)
            || authored
                .values()
                .flat_map(|origins| origins.values())
                .any(|target| !authored.contains_key(target))
        {
            return Err(DemandError::MissingDomain(current));
        }
        let mut inherited = BTreeMap::new();
        let mut anchors = HashMap::new();

        for (domain, lease) in entries {
            if !authored.contains_key(&domain) {
                return Err(DemandError::MissingDomain(domain));
            }
            let group = (domain, lease.owner().clone(), lease.original_ordinal());
            if anchors
                .insert(group, lease.clone())
                .is_some_and(|old: SourceInstanceLease| old.instance() != lease.instance())
            {
                return Err(DemandError::ConflictingDomainInstance {
                    domain,
                    binder: lease.binder().clone(),
                });
            }
            let key = ScopedSourceBinder {
                domain,
                source: lease.binder().clone(),
            };
            if inherited.insert(key.clone(), lease.clone()).is_some_and(
                |old: SourceInstanceLease| {
                    old.instance() != lease.instance() || old.handle() != lease.handle()
                },
            ) {
                return Err(DemandError::ConflictingDomainInstance {
                    domain,
                    binder: key.source,
                });
            }
        }
        Ok(Self {
            current,
            authored,
            inherited,
            anchors,
        })
    }

    pub fn current(&self) -> SourceInstanceDomain {
        self.current
    }
    pub fn inherited(&self) -> &BTreeMap<ScopedSourceBinder, SourceInstanceLease> {
        &self.inherited
    }
    pub fn domain_for_owner(
        &self,
        caller: SourceInstanceDomain,
        owner: &CachedHomeOwner,
    ) -> Result<SourceInstanceDomain, DemandError> {
        let origins = self
            .authored
            .get(&caller)
            .ok_or(DemandError::MissingDomain(caller))?;
        Ok(origins.get(owner).copied().unwrap_or(caller))
    }
}

/// One sealed original group instance demand with its complete source import
/// selections. The immutable original arena remains separate from these choices.
#[derive(Clone)]
pub struct ScopedSourceGroupDemand {
    index: usize,
    domain: SourceInstanceDomain,
    owner: CachedHomeOwner,
    ordinal: u32,
    imports: BTreeMap<SourceBinder, ScopedSourceBinder>,
}

impl ScopedSourceGroupDemand {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn domain(&self) -> SourceInstanceDomain {
        self.domain
    }
    pub fn owner(&self) -> &CachedHomeOwner {
        &self.owner
    }
    pub fn original_ordinal(&self) -> u32 {
        self.ordinal
    }
    pub fn source_imports(&self) -> &BTreeMap<SourceBinder, ScopedSourceBinder> {
        &self.imports
    }
}

#[derive(Clone)]
pub struct ScopedInheritedSourceDemand {
    domain: SourceInstanceDomain,
    demand: InheritedSourceDemand,
}

impl ScopedInheritedSourceDemand {
    pub fn domain(&self) -> SourceInstanceDomain {
        self.domain
    }
    pub fn demand(&self) -> &InheritedSourceDemand {
        &self.demand
    }
    pub fn into_demand(self) -> InheritedSourceDemand {
        self.demand
    }
}

pub struct PendingScopedSourceDemand {
    groups: Vec<ScopedSourceGroupDemand>,
    inherited: Vec<ScopedInheritedSourceDemand>,
    target: BTreeMap<SourceBinder, ScopedSourceBinder>,
}

impl PendingScopedSourceDemand {
    pub fn into_parts(
        self,
    ) -> (
        Vec<ScopedSourceGroupDemand>,
        Vec<ScopedInheritedSourceDemand>,
        BTreeMap<SourceBinder, ScopedSourceBinder>,
    ) {
        (self.groups, self.inherited, self.target)
    }
}

/// One native root admission. Existing physical roots require a sealed demand
/// whose exact anchor is revalidated by the owning binding table.
pub struct SourceInstanceAttachment {
    domain: SourceInstanceDomain,
    lease: SourceInstanceLease,
    inherited: Option<InheritedSourceDemand>,
}

static_assertions::assert_not_impl_any!(SourceInstanceAttachment: Clone, Copy);

impl SourceInstanceAttachment {
    pub(in crate::prepared_program) fn installed(
        domain: SourceInstanceDomain,
        lease: SourceInstanceLease,
    ) -> Self {
        Self {
            domain,
            lease,
            inherited: None,
        }
    }
    pub(crate) fn inherited(
        demand: &InheritedSourceDemand,
        lease: SourceInstanceLease,
    ) -> Result<Self, DemandError> {
        if lease.instance() != demand.anchor().instance()
            || lease.owner() != demand.owner()
            || lease.original_ordinal() != demand.original_ordinal()
            || lease.binder() != demand.binder()
            || lease.value() != demand.value()
        {
            return Err(DemandError::InvalidInheritedInstance(
                demand.binder().clone(),
            ));
        }
        Ok(Self {
            domain: demand.domain(),
            lease,
            inherited: Some(demand.clone()),
        })
    }
    pub(crate) fn domain(&self) -> SourceInstanceDomain {
        self.domain
    }
    pub fn lease(&self) -> &SourceInstanceLease {
        &self.lease
    }
    pub(crate) fn demand(&self) -> Option<&InheritedSourceDemand> {
        self.inherited.as_ref()
    }
    pub(crate) fn into_parts(self) -> (SourceInstanceDomain, SourceInstanceLease) {
        (self.domain, self.lease)
    }
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
    #[error("exact prepared native image is unavailable")]
    MissingPreparedNativeImage,
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
    #[error(
        "source selection domain {domain:?} contains conflicting mutable instances for {binder:?}"
    )]
    ConflictingDomainInstance {
        domain: SourceInstanceDomain,
        binder: SourceBinder,
    },
    #[error("source selection domain {0:?} is absent or not closed")]
    MissingDomain(SourceInstanceDomain),
    #[error("domain demand does not match original certified group {owner:?} ordinal {ordinal}")]
    InvalidScopedGroup {
        owner: CachedHomeOwner,
        ordinal: u32,
    },
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

    pub fn seal_in_domains(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
        selected: &SourceDomainSelection,
    ) -> Result<PendingScopedSourceDemand, DemandError> {
        self.index.seal_in_domains(roots, selected)
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
    domain: SourceInstanceDomain,
}

impl InheritedSourceDemand {
    pub fn domain(&self) -> SourceInstanceDomain {
        self.domain
    }
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

#[derive(Clone)]
pub struct ScopedCertifiedGroup {
    original: CertifiedGroup,
    demand: ScopedSourceGroupDemand,
}

impl ScopedCertifiedGroup {
    pub fn admit(
        original: CertifiedGroup,
        demand: ScopedSourceGroupDemand,
    ) -> Result<Self, DemandError> {
        let sources: BTreeSet<_> = original
            .imports()
            .iter()
            .filter_map(|owner| match owner {
                ImportOwner::Source { version, binder } => Some(SourceBinder {
                    version: version.clone(),
                    binder: binder.clone(),
                }),
                _ => None,
            })
            .collect();
        if original.owner() != &demand.owner
            || original.original_ordinal() != demand.ordinal
            || sources != demand.imports.keys().cloned().collect()
        {
            return Err(DemandError::InvalidScopedGroup {
                owner: original.owner().clone(),
                ordinal: original.original_ordinal(),
            });
        }
        Ok(Self { original, demand })
    }
    pub fn original(&self) -> &CertifiedGroup {
        &self.original
    }
    pub fn demand(&self) -> &ScopedSourceGroupDemand {
        &self.demand
    }
}

pub struct ScopedDemandedImage {
    original: DemandedImage,
    demand: ScopedSourceGroupDemand,
}

impl ScopedDemandedImage {
    pub fn admit(
        original: DemandedImage,
        source: ScopedCertifiedGroup,
    ) -> Result<Self, DemandError> {
        if original.group() != source.original() {
            return Err(DemandError::InvalidScopedGroup {
                owner: source.original.owner().clone(),
                ordinal: source.original.original_ordinal(),
            });
        }
        Ok(Self {
            original,
            demand: source.demand,
        })
    }
    pub fn into_image(mut self) -> DemandedImage {
        self.original.demand = Some(self.demand);
        self.original
    }
    pub fn original(&self) -> &DemandedImage {
        &self.original
    }
    pub fn demand(&self) -> &ScopedSourceGroupDemand {
        &self.demand
    }
}

pub struct DemandedImage {
    group: CertifiedGroup,
    image: Arc<CompiledProgram>,
    demand: Option<ScopedSourceGroupDemand>,
}

impl DemandedImage {
    /// Compile one already selected original group outside the machine
    /// checkout. The batch installer still checks the complete closed set
    /// before publishing any native program.
    pub fn compile(group: CertifiedGroup, registry: &ImageRegistry) -> Result<Self, DemandError> {
        let image = registry.get_or_compile_group(&group, || {
            CompiledProgram::compile_certified_group(&group).map(Arc::new)
        })?;
        Ok(Self {
            group,
            image,
            demand: None,
        })
    }

    /// Specialize only immutable package literals supplied by the runtime's
    /// exact target. The original product and every global ID stay unchanged.
    pub fn compile_with_package_literals(
        group: CertifiedGroup,
        registry: &ImageRegistry,
        supplied: &BTreeMap<SymbolIdentity, super::PackageLiteral>,
    ) -> Result<Self, DemandError> {
        Self::compile_with_literals(group, registry, supplied, &BTreeMap::new())
    }

    /// Preserve the original dependency graph while importing only authenticated
    /// immutable package or original-source byte storage.
    pub fn compile_with_literals(
        group: CertifiedGroup,
        registry: &ImageRegistry,
        packages: &BTreeMap<SymbolIdentity, super::PackageLiteral>,
        sources: &BTreeMap<SourceBinder, super::SourceLiteral>,
    ) -> Result<Self, DemandError> {
        let literals = Self::select_literals(&group, packages, sources)?;
        let image =
            registry.get_or_compile_literal_group(&group.code_identity(), &literals, || {
                CompiledProgram::compile_certified_group_with_literals(&group, &literals)
                    .map(Arc::new)
            })?;
        Ok(Self {
            group,
            image,
            demand: None,
        })
    }

    /// Resolve an already prepared image with the ordinary literal selector.
    /// An unavailable exact key is refused without joining or creating a flight.
    pub fn lookup_with_literals(
        group: CertifiedGroup,
        registry: &ImageRegistry,
        packages: &BTreeMap<SymbolIdentity, super::PackageLiteral>,
        sources: &BTreeMap<SourceBinder, super::SourceLiteral>,
    ) -> Result<Self, DemandError> {
        let literals = Self::select_literals(&group, packages, sources)?;
        let image = registry
            .lookup_literal_group(&group.code_identity(), &literals)
            .ok_or(DemandError::MissingPreparedNativeImage)?;
        Ok(Self {
            group,
            image,
            demand: None,
        })
    }

    fn select_literals(
        group: &CertifiedGroup,
        packages: &BTreeMap<SymbolIdentity, super::PackageLiteral>,
        sources: &BTreeMap<SourceBinder, super::SourceLiteral>,
    ) -> Result<super::package_literals::GroupPackageLiterals, CompileError> {
        let literals =
            super::package_literals::GroupPackageLiterals::select(&group, packages, sources)
                .map_err(|mut error| {
                    if let CompileError::Unsupported(super::Unsupported::Global(refusal)) =
                        &mut error
                    {
                        refusal.source_group =
                            Some((group.owner().clone(), group.original_ordinal()));
                    }
                    error
                })?;
        Ok(literals)
    }

    pub fn domain(&self) -> SourceInstanceDomain {
        self.demand
            .as_ref()
            .map(|plan| plan.domain)
            .unwrap_or_else(|| SourceInstanceDomain::for_scope(crate::scope::ScopeId::ROOT))
    }
    pub fn qualified_source(
        &self,
        source: &SourceBinder,
    ) -> Result<ScopedSourceBinder, DemandError> {
        match &self.demand {
            Some(plan) => plan
                .imports
                .get(source)
                .cloned()
                .ok_or_else(|| DemandError::MissingSource(source.clone())),
            None => Ok(ScopedSourceBinder {
                domain: self.domain(),
                source: source.clone(),
            }),
        }
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

    pub fn seal_in_domains(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
        selected: &SourceDomainSelection,
    ) -> Result<
        (
            Vec<ScopedCertifiedGroup>,
            Vec<ScopedInheritedSourceDemand>,
            BTreeMap<SourceBinder, ScopedSourceBinder>,
        ),
        DemandError,
    > {
        let demand = self.index.seal_in_domains(roots, selected)?;
        let groups = demand
            .groups
            .into_iter()
            .map(|plan| ScopedCertifiedGroup::admit(self.groups[plan.index].clone(), plan))
            .collect::<Result<_, _>>()?;
        Ok((groups, demand.inherited, demand.target))
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

    fn qualified_source(
        &self,
        caller: SourceInstanceDomain,
        source: SourceBinder,
        selected: &SourceDomainSelection,
    ) -> Result<ScopedSourceBinder, DemandError> {
        let index = *self
            .binders
            .get(&source)
            .ok_or_else(|| DemandError::MissingSource(source.clone()))?;
        Ok(ScopedSourceBinder {
            domain: selected.domain_for_owner(caller, &self.groups[index].owner)?,
            source,
        })
    }

    fn seal_in_domains(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
        selected: &SourceDomainSelection,
    ) -> Result<PendingScopedSourceDemand, DemandError> {
        let target = roots
            .into_iter()
            .map(|source| {
                self.qualified_source(selected.current, source.clone(), selected)
                    .map(|qualified| (source, qualified))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut pending: VecDeque<_> = target.values().cloned().collect();
        let mut reachable = BTreeMap::new();
        let mut inherited = BTreeMap::new();
        while let Some(key) = pending.pop_front() {
            let index = *self
                .binders
                .get(&key.source)
                .ok_or_else(|| DemandError::MissingSource(key.source.clone()))?;
            let group = &self.groups[index];
            if let Some(lease) = selected.inherited.get(&key) {
                if lease.binder() != &key.source
                    || lease.owner() != &group.owner
                    || lease.original_ordinal() != group.original_ordinal
                {
                    return Err(DemandError::InvalidInheritedInstance(key.source));
                }
                continue;
            }
            if let Some(anchor) =
                selected
                    .anchors
                    .get(&(key.domain, group.owner.clone(), group.original_ordinal))
            {
                if anchor.owner() != &group.owner
                    || anchor.original_ordinal() != group.original_ordinal
                    || anchor.binder().version != group.owner.module_version
                    || !group.tops.contains_key(&anchor.binder().binder)
                {
                    return Err(DemandError::InvalidInheritedInstance(key.source));
                }
                inherited
                    .entry(key.clone())
                    .or_insert_with(|| ScopedInheritedSourceDemand {
                        domain: key.domain,
                        demand: InheritedSourceDemand {
                            owner: group.owner.clone(),
                            original_ordinal: group.original_ordinal,
                            value: group.tops[&key.source.binder],
                            binder: key.source,
                            anchor: anchor.clone(),
                            domain: key.domain,
                        },
                    });
                continue;
            }
            if reachable.contains_key(&(key.domain, index)) {
                continue;
            }
            let imports = group
                .imports
                .iter()
                .cloned()
                .map(|source| {
                    self.qualified_source(key.domain, source.clone(), selected)
                        .map(|qualified| (source, qualified))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            pending.extend(imports.values().cloned());
            reachable.insert(
                (key.domain, index),
                ScopedSourceGroupDemand {
                    index,
                    domain: key.domain,
                    owner: group.owner.clone(),
                    ordinal: group.original_ordinal,
                    imports,
                },
            );
        }
        Ok(PendingScopedSourceDemand {
            groups: reachable.into_values().collect(),
            inherited: inherited.into_values().collect(),
            target,
        })
    }

    fn seal(
        &self,
        roots: impl IntoIterator<Item = SourceBinder>,
        existing: &BTreeMap<SourceBinder, SourceInstanceLease>,
        anchors: &HashMap<(CachedHomeOwner, u32), SourceInstanceLease>,
    ) -> Result<PendingSealedDemand, DemandError> {
        let domain = SourceInstanceDomain::for_scope(crate::scope::ScopeId::ROOT);
        let selected = SourceDomainSelection {
            current: domain,
            authored: HashMap::from([(domain, HashMap::new())]),
            inherited: existing
                .iter()
                .map(|(source, lease)| {
                    (
                        ScopedSourceBinder {
                            domain,
                            source: source.clone(),
                        },
                        lease.clone(),
                    )
                })
                .collect(),
            anchors: anchors
                .iter()
                .map(|((owner, ordinal), lease)| ((domain, owner.clone(), *ordinal), lease.clone()))
                .collect(),
        };
        let scoped = self.seal_in_domains(roots, &selected)?;
        Ok(PendingSealedDemand {
            indices: scoped.groups.into_iter().map(|group| group.index).collect(),
            inherited: scoped
                .inherited
                .into_iter()
                .map(|selected| selected.demand)
                .collect(),
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
    fn late_source_demand_installs_new_group_without_recompiling_or_replacing_first() {
        use crate::prepared_program::{
            BatchProgram, PreparedCallOptions, PreparedMachine, PreparedMachineOptions,
            PreparedResult,
        };
        use crate::suspension::RealmId;
        use tidepool_repr::execution_schema::ValueId;

        let groups = [
            group("first", 4, &[]),
            group("later", 9, &[]),
            group("unavailable", 12, &["missing"]),
        ];
        let inventory = GroupInventory::new(&groups).unwrap();
        let registry = ImageRegistry::new();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let mut installed = Vec::new();
        for name in ["first", "later"] {
            let demand = inventory.seal([source(name)]).unwrap();
            let images = demand.compile(&registry).unwrap();
            assert_eq!(images.len(), 1);
            assert_eq!(
                images[0].group().original_ordinal(),
                groups[installed.len()].original_ordinal()
            );
            let programs = machine
                .install_shared_batch(vec![BatchProgram {
                    image: Arc::clone(images[0].image()),
                    imports: vec![],
                }])
                .unwrap();
            installed.push(programs[0]);
            for program in &installed {
                let value = machine
                    .run_entry_retained(
                        *program,
                        ValueId(0),
                        &[],
                        PreparedCallOptions {
                            observation_budget: 0,
                            collect_before_observation: false,
                        },
                        RealmId::ROOT,
                    )
                    .unwrap();
                assert_eq!(value.values, vec![PreparedResult::Scalar(42)]);
            }
            assert_eq!(machine.residency().programs, installed.len());
            assert_eq!(registry.misses(), installed.len() as u64);
        }
        let first = inventory.seal([source("first")]).unwrap();
        let reused = first.compile(&registry).unwrap();
        let retained_image = Arc::downgrade(reused[0].image());
        assert_eq!(registry.misses(), 2);
        assert_eq!(registry.hits(), 1);
        assert_eq!(reused[0].group().original_ordinal(), 4);
        assert!(matches!(
            inventory.seal([source("unavailable")]),
            Err(DemandError::MissingSource(missing)) if missing == source("missing")
        ));
        assert_eq!(machine.residency().programs, 2);
        let retired = machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(retired.programs.len(), 2);
        assert_eq!(machine.residency().programs, 0);
        assert!(retained_image.upgrade().is_some());
        drop(reused);
        assert!(retained_image.upgrade().is_none());
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
    fn native_group_preparation_needs_no_retained_runtime_owner() {
        let mut wire = testing::wire_program();
        let binder = testing::identity("Fixture", "historical");
        wire.globals.push(GlobalDecl {
            identity: binder,
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: Some(7),
        });
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
            module: "Fixture".into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: [2; 32],
            product_sha256: [3; 32],
        };
        let projected = testing::projected_group(wire, 9).unwrap();
        let code = tidepool_repr::execution_schema::CertifiedGroupCode::admit(
            owner.clone(),
            projected.clone(),
        )
        .unwrap();
        let registry = ImageRegistry::new();
        let native = CompiledProgram::prepare_group_code(
            &code,
            &[None],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &registry,
        )
        .unwrap();
        for id in [17, 81] {
            let scoped = CertifiedGroup::admit(
                owner.clone(),
                projected.clone(),
                vec![ImportOwner::Retained {
                    id: tidepool_repr::SessionVarId::from_extract(id),
                    generation: 7,
                }],
            )
            .unwrap();
            let installed_plan = DemandedImage::compile(scoped, &registry).unwrap();
            assert!(
                Arc::ptr_eq(&native, installed_plan.image()),
                "fresh retained owner identity affects installation, not native code"
            );
        }
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
