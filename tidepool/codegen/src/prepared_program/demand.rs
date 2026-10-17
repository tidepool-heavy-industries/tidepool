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
    fn validate_domains(
        current: SourceInstanceDomain,
        contains: impl Fn(&SourceInstanceDomain) -> bool,
        targets: impl IntoIterator<Item = SourceInstanceDomain>,
    ) -> Result<(), DemandError> {
        if !contains(&current) || targets.into_iter().any(|target| !contains(&target)) {
            return Err(DemandError::MissingDomain(current));
        }
        Ok(())
    }

    fn validate_anchor(
        domain: SourceInstanceDomain,
        incoming: &SourceInstanceLease,
        previous: Option<&SourceInstanceLease>,
    ) -> Result<(), DemandError> {
        if previous.is_some_and(|old| old.instance() != incoming.instance()) {
            return Err(DemandError::ConflictingDomainInstance {
                domain,
                binder: incoming.binder().clone(),
            });
        }
        Ok(())
    }

    fn validate_binder(
        domain: SourceInstanceDomain,
        binder: &SourceBinder,
        instance: GroupInstanceId,
        handle: PreparedHandle,
        previous: Option<&SourceInstanceLease>,
    ) -> Result<(), DemandError> {
        if previous.is_some_and(|old| old.instance() != instance || old.handle() != handle) {
            return Err(DemandError::ConflictingDomainInstance {
                domain,
                binder: binder.clone(),
            });
        }
        Ok(())
    }

    /// Check the same domain and exact-instance rules without owning copies of
    /// leases or authored maps when the caller only needs validation.
    pub(crate) fn validate<'a>(
        current: SourceInstanceDomain,
        contains: impl Fn(&SourceInstanceDomain) -> bool,
        targets: impl IntoIterator<Item = SourceInstanceDomain>,
        entries: impl IntoIterator<Item = (SourceInstanceDomain, &'a SourceInstanceLease)>,
    ) -> Result<(), DemandError> {
        Self::validate_domains(current, &contains, targets)?;
        let mut inherited = BTreeMap::new();
        let mut anchors = HashMap::new();
        for (domain, lease) in entries {
            if !contains(&domain) {
                return Err(DemandError::MissingDomain(domain));
            }
            let group = (domain, lease.owner(), lease.original_ordinal());
            Self::validate_anchor(domain, lease, anchors.insert(group, lease))?;
            let previous = inherited.insert((domain, lease.binder()), lease);
            Self::validate_binder(
                domain,
                lease.binder(),
                lease.instance(),
                lease.handle(),
                previous,
            )?;
        }
        Ok(())
    }

    pub(crate) fn new(
        current: SourceInstanceDomain,
        authored: HashMap<SourceInstanceDomain, HashMap<CachedHomeOwner, SourceInstanceDomain>>,
        entries: impl IntoIterator<Item = (SourceInstanceDomain, SourceInstanceLease)>,
    ) -> Result<Self, DemandError> {
        Self::validate_domains(
            current,
            |domain| authored.contains_key(domain),
            authored
                .values()
                .flat_map(|origins| origins.values())
                .copied(),
        )?;
        let mut inherited = BTreeMap::new();
        let mut anchors = HashMap::new();

        for (domain, lease) in entries {
            if !authored.contains_key(&domain) {
                return Err(DemandError::MissingDomain(domain));
            }
            let group = (domain, lease.owner().clone(), lease.original_ordinal());
            let previous = anchors.insert(group, lease.clone());
            Self::validate_anchor(domain, &lease, previous.as_ref())?;
            let key = ScopedSourceBinder {
                domain,
                source: lease.binder().clone(),
            };
            let instance = lease.instance();
            let handle = lease.handle();
            let previous = inherited.insert(key.clone(), lease);
            Self::validate_binder(domain, &key.source, instance, handle, previous.as_ref())?;
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

#[cfg(test)]
thread_local! {
    static OUTLINE_ROWS: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
}

#[cfg(test)]
fn observe_outline_row(index: usize) {
    OUTLINE_ROWS.with(|rows| {
        let mut value = rows.get();
        value[index] += 1;
        rows.set(value);
    });
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

    fn from_certified(group: &CertifiedGroup) -> Self {
        let imports = group
            .imports()
            .iter()
            .filter_map(|origin| {
                #[cfg(test)]
                observe_outline_row(3);
                match origin {
                    ImportOwner::Source { version, binder } => Some(SourceBinder {
                        version: version.clone(),
                        binder: binder.clone(),
                    }),
                    ImportOwner::Retained { .. }
                    | ImportOwner::CodeExport { .. }
                    | ImportOwner::Package { .. } => None,
                }
            })
            .collect();
        Self {
            owner: group.owner().clone(),
            original_ordinal: group.original_ordinal(),
            tops: group
                .exported_values()
                .map(|(binder, value)| {
                    #[cfg(test)]
                    observe_outline_row(1);
                    (binder.clone(), value)
                })
                .collect(),
            imports,
        }
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
            .map(|top| {
                #[cfg(test)]
                observe_outline_row(0);
                (&top.identity, top.binding.id)
            })
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
                #[cfg(test)]
                observe_outline_row(2);
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
            .collect();
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

    pub(super) fn group(name: &str, ordinal: u32, imports: &[&str]) -> CertifiedGroup {
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

    pub(super) fn source(name: &str) -> SourceBinder {
        SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", name),
        }
    }

    fn outlined_fixture(
        helpers: usize,
        exports: usize,
        import_count: usize,
        reverse_names: bool,
        split_last: bool,
        value_offset: u32,
    ) -> (CachedHomeOwner, ProjectedGroup, Vec<ImportOwner>) {
        use tidepool_repr::execution_schema::{
            parse_projected_group, DecodeLimits, ProgramRequirements,
        };
        let mut wire = testing::wire_program();
        let Group::NonRecursive(template) = &wire.bindings[0] else {
            unreachable!()
        };
        let tops = (0..helpers + exports)
            .map(|index| {
                let mut top = template.clone();
                top.binding.id = ValueId(index as u32 + value_offset);
                top.identity = if index < helpers {
                    testing::identity("Helpers", &format!("helper{index}"))
                } else {
                    let suffix = index - helpers;
                    let name = if reverse_names { exports - 1 - suffix } else { suffix };
                    testing::identity("Fixture", &format!("export{name}"))
                };
                top
            })
            .collect::<Vec<_>>();
        wire.bindings = if split_last && tops.len() > 1 {
            vec![
                Group::Recursive(tops[..tops.len() - 1].to_vec()),
                Group::NonRecursive(tops.last().unwrap().clone()),
            ]
        } else {
            vec![Group::Recursive(tops)]
        };
        let imports = (0..import_count)
            .map(|index| {
                let binder = testing::identity("Dependency", &format!("import{index}"));
                let generation = matches!(index % 4, 2 | 3).then_some(9);
                wire.globals.push(GlobalDecl {
                    identity: binder.clone(),
                    rep: RuntimeRep::LiftedRef,
                    entry_signature: None,
                    required_evaluated: false,
                    required_generation: generation,
                });
                match index % 4 {
                    0 => ImportOwner::Source { version: ModuleVersion([8; 32]), binder },
                    1 => ImportOwner::Package {
                        unit: binder.unit.clone(), module: binder.module.clone(), binder,
                        interface_digest: [7; 32],
                    },
                    2 => ImportOwner::Retained {
                        id: tidepool_repr::SessionVarId::from_extract(index as u64),
                        generation: 9,
                    },
                    _ => ImportOwner::CodeExport {
                        binder, generation: 9, root_id: 11, interface_digest: None,
                    },
                }
            })
            .collect();
        let envelope = wire.envelope.clone();
        let requirements = ProgramRequirements {
            schema_version: envelope.schema_version,
            projection_profile: envelope.projection_profile,
            toolchain: envelope.toolchain,
            execution_abi_version: envelope.execution_abi_version,
            target: envelope.target,
        };
        let all = testing::projected_group(wire, 11).unwrap();
        let bytes = tidepool_test_data::prepared_encode::encode_projected_group(&all);
        let mut value: ciborium::value::Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let ciborium::value::Value::Array(fields) = &mut value else {
            panic!("normal group encoder must emit an array")
        };
        // Describe a raw exported suffix, then earn admission through both
        // normal readers. No projected-group field or proof is constructed here.
        let ciborium::value::Value::Array(binders) = &mut fields[3] else {
            panic!("normal group encoder must emit a binder array")
        };
        binders.drain(..helpers);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        let projected = parse_projected_group(&bytes, &requirements, DecodeLimits::default()).unwrap();
        (
            CachedHomeOwner {
                unit: "fixture".into(), module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32], product_sha256: [3; 32],
            },
            projected,
            imports,
        )
    }

    #[test]
    fn certified_outlines_match_full_definitions_and_visit_only_exports() {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestRunner};

        let check = |helpers, exports, imports, reverse, split, offset| {
            let (owner, projected, owners) = outlined_fixture(helpers, exports, imports, reverse, split, offset);
            let group = CertifiedGroup::admit(owner, projected, owners).unwrap();
            OUTLINE_ROWS.with(|rows| rows.set([0; 4]));
            let actual = SourceGroupOutline::from_certified(&group);
            let touched = OUTLINE_ROWS.with(|rows| rows.get());
            // Full recomputation never uses the certified export iterator.
            let definitions = group.definitions();
            let mut expected = BTreeMap::new();
            let mut oracle_rows = 0;
            for binding in definitions.bindings() {
                let tops = match binding {
                    Group::NonRecursive(top) => std::slice::from_ref(top),
                    Group::Recursive(tops) => tops.as_slice(),
                };
                for top in tops {
                    oracle_rows += 1;
                    if group.binders().contains(&top.identity) {
                        expected.insert(top.identity.clone(), top.binding.id);
                    }
                }
            }
            let expected_imports = definitions.globals().iter().zip(group.imports())
                .filter_map(|(global, owner)| match owner {
                    ImportOwner::Source { version, .. } => Some(SourceBinder {
                        version: version.clone(), binder: global.identity.clone(),
                    }),
                    _ => None,
                }).collect::<Vec<_>>();
            prop_assert_eq!(&actual.owner, group.owner());
            prop_assert_eq!(actual.original_ordinal, group.original_ordinal());
            prop_assert_eq!(&actual.tops, &expected);
            prop_assert_eq!(&actual.imports, &expected_imports);
            prop_assert_eq!(oracle_rows, helpers + exports);
            prop_assert_eq!(touched, [0, exports, 0, imports]);

            // The actual general route remains necessary for projected input.
            // It also sensitizes the budget to restoring that route above.
            OUTLINE_ROWS.with(|rows| rows.set([0; 4]));
            let projected = SourceGroupOutline::from_definitions(
                group.owner().clone(), group.original_ordinal(), group.binders(),
                definitions, expected_imports,
            ).unwrap();
            let general_rows = OUTLINE_ROWS.with(|rows| rows.get());
            prop_assert_eq!(projected.tops, expected);
            prop_assert_eq!(projected.imports, actual.imports);
            prop_assert_eq!(general_rows[0], helpers + exports);
            prop_assert_ne!(general_rows, touched);
            if helpers == 512 {
                eprintln!("certified outline work: {}", serde_json::json!({
                    "helpers": helpers, "exports": exports, "imports": imports,
                    "definition_rows": touched[0], "export_rows": touched[1],
                    "source_import_comparisons": touched[2], "import_rows": touched[3],
                    "general_definition_rows": general_rows[0],
                    "general_source_import_comparisons": general_rows[2],
                    "oracle_definition_rows": oracle_rows,
                }));
            }
            Ok(())
        };
        // Fixed large-helper/single-export and zero-export controls ensure the
        // budget is exercised independently of generated coverage.
        check(512, 1, 12, true, false, 3).unwrap();
        check(8, 0, 0, false, true, 0).unwrap();
        let strategy = (1usize..65, 0usize..13, 0usize..13, any::<bool>(), any::<bool>(), 0u32..4);
        let mut config = proptest::test_runner::contextualize_config(Config::default());
        config.source_file = Some(file!());
        config.test_name = Some(concat!(module_path!(), "::certified_outlines_match_full_definitions_and_visit_only_exports"));
        TestRunner::new(config).run(&strategy, |(h, e, i, r, s, o)| check(h, e, i, r, s, o)).unwrap();
    }

    #[test]
    fn certified_outline_admission_preserves_owner_import_and_inventory_refusals() {
        let (owner, projected, imports) = outlined_fixture(2, 2, 4, true, true, 1);
        let mut bad_source = imports.clone();
        let ImportOwner::Source { binder, .. } = &mut bad_source[0] else { unreachable!() };
        binder.occurrence = "foreign".into();
        let mut bad_package = imports.clone();
        let ImportOwner::Package { unit, .. } = &mut bad_package[1] else { unreachable!() };
        *unit = "foreign".into();
        let mut bad_retained = imports.clone();
        let ImportOwner::Retained { generation, .. } = &mut bad_retained[2] else { unreachable!() };
        *generation = 10;
        let mut bad_export = imports.clone();
        let ImportOwner::CodeExport { generation, .. } = &mut bad_export[3] else { unreachable!() };
        *generation = 10;
        let mut bad_source_generation = imports.clone();
        bad_source_generation[0] = ImportOwner::Retained {
            id: tidepool_repr::SessionVarId::from_extract(1), generation: 9,
        };
        for malformed in [Vec::new(), bad_source, bad_package, bad_retained, bad_export, bad_source_generation] {
            assert!(CertifiedGroup::admit(owner.clone(), projected.clone(), malformed).is_err());
        }
        for (unit, module) in [("", "Fixture"), ("foreign", "Fixture"), ("fixture", "Foreign")] {
            let mut malformed = owner.clone();
            malformed.unit = unit.into();
            malformed.module = module.into();
            assert!(CertifiedGroup::admit(malformed, projected.clone(), imports.clone()).is_err());
        }
        let valid = CertifiedGroup::admit(owner.clone(), projected.clone(), imports.clone()).unwrap();
        assert!(matches!(GroupInventory::new(&[valid.clone(), valid.clone()]), Err(DemandError::DuplicateGroup { .. })));
        let mut conflicting = owner;
        conflicting.product_sha256 = [99; 32];
        let other = CertifiedGroup::admit(conflicting, projected, imports).unwrap();
        assert!(matches!(GroupInventory::new(&[valid, other]), Err(DemandError::ConflictingOwner { .. })));
        assert!(matches!(GroupInventory::new(&[group("same", 11, &[]), group("same", 12, &[])]), Err(DemandError::DuplicateBinder(_))));
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
            .collect();
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

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::binding_table::BindingTable;
    use crate::prepared_program::{
        BatchLeaseRequest, BatchProgram, PreparedMachine, PreparedMachineOptions,
    };
    use crate::scope::{ScopeId, ScopeTree};
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};
    use std::cell::Cell;

    fn installed_leases() -> (PreparedMachine<'static>, Vec<SourceInstanceLease>) {
        let groups = [super::tests::group("a", 4, &[])];
        let inventory = GroupInventory::new(&groups).unwrap();
        let registry = ImageRegistry::new();
        let images = inventory
            .seal([super::tests::source("a")])
            .unwrap()
            .compile(&registry)
            .unwrap();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let mut leases = Vec::new();
        for _ in 0..2 {
            let receipt = machine
                .install_shared_batch_with_leases(
                    vec![BatchProgram {
                        image: Arc::clone(images[0].image()),
                        imports: vec![],
                    }],
                    vec![BatchLeaseRequest::for_demanded(
                        0,
                        &images[0],
                        &super::tests::source("a"),
                    )
                    .unwrap()],
                )
                .unwrap();
            leases.extend(receipt.leases);
        }
        (machine, leases)
    }

    #[derive(Debug, Eq, PartialEq)]
    enum Refusal {
        Missing(SourceInstanceDomain),
        Conflict(SourceInstanceDomain, SourceBinder),
    }

    fn refusal(result: Result<(), DemandError>) -> Result<(), Refusal> {
        result.map_err(|error| match error {
            DemandError::MissingDomain(domain) => Refusal::Missing(domain),
            DemandError::ConflictingDomainInstance { domain, binder } => {
                Refusal::Conflict(domain, binder)
            }
            error => panic!("unexpected domain selection error: {error:?}"),
        })
    }

    // A full pairwise scan of explicit rows, independent of the production indexes
    // and shared compatibility predicates. The incoming row owns a conflict's label.
    fn row_oracle(
        current: SourceInstanceDomain,
        authored: &HashMap<SourceInstanceDomain, HashMap<CachedHomeOwner, SourceInstanceDomain>>,
        rows: &[(SourceInstanceDomain, SourceInstanceLease)],
    ) -> Result<(), Refusal> {
        if !authored.contains_key(&current)
            || authored
                .values()
                .flat_map(|origins| origins.values())
                .any(|target| !authored.contains_key(target))
        {
            return Err(Refusal::Missing(current));
        }
        for (index, (domain, incoming)) in rows.iter().enumerate() {
            if !authored.contains_key(domain) {
                return Err(Refusal::Missing(*domain));
            }
            let previous = &rows[..index];
            if previous.iter().any(|(old_domain, old)| {
                old_domain == domain
                    && old.owner() == incoming.owner()
                    && old.original_ordinal() == incoming.original_ordinal()
                    && old.instance() != incoming.instance()
            }) || previous.iter().any(|(old_domain, old)| {
                old_domain == domain
                    && old.binder() == incoming.binder()
                    && (old.instance() != incoming.instance() || old.handle() != incoming.handle())
            }) {
                return Err(Refusal::Conflict(*domain, incoming.binder().clone()));
            }
        }
        Ok(())
    }

    fn compare(
        current: SourceInstanceDomain,
        authored: &HashMap<SourceInstanceDomain, HashMap<CachedHomeOwner, SourceInstanceDomain>>,
        rows: &[(SourceInstanceDomain, SourceInstanceLease)],
    ) -> Result<(), Refusal> {
        let expected = row_oracle(current, authored, rows);
        let owned = refusal(
            SourceDomainSelection::new(current, authored.clone(), rows.iter().cloned()).map(|_| ()),
        );
        let borrowed = refusal(SourceDomainSelection::validate(
            current,
            |domain| authored.contains_key(domain),
            authored
                .values()
                .flat_map(|origins| origins.values())
                .copied(),
            rows.iter().map(|(domain, lease)| (*domain, lease)),
        ));
        assert_eq!(owned, expected);
        assert_eq!(borrowed, expected);
        expected
    }

    #[test]
    fn borrowed_selection_matches_explicit_rows_across_domain_histories() {
        let (mut machine, leases) = installed_leases();
        let extra_handle = machine
            .retain_top(leases[0].instance().program(), leases[0].value())
            .unwrap();
        let mut tree = ScopeTree::new();
        let domains = [
            SourceInstanceDomain::single(),
            SourceInstanceDomain::for_scope(tree.mint_isolated()),
            SourceInstanceDomain::for_scope(tree.mint_isolated()),
        ];
        let closed: HashMap<_, _> = domains
            .iter()
            .map(|domain| (*domain, HashMap::new()))
            .collect();
        let first = (domains[0], leases[0].clone());
        assert_eq!(
            compare(domains[0], &closed, &[first.clone(), first.clone()]),
            Ok(())
        );
        assert_eq!(
            compare(
                domains[0],
                &closed,
                &[first.clone(), (domains[1], leases[1].clone())]
            ),
            Ok(())
        );
        assert!(matches!(
            compare(
                domains[0],
                &closed,
                &[first.clone(), (domains[0], leases[1].clone())]
            ),
            Err(Refusal::Conflict(_, _))
        ));
        let mut changed_handle = leases[0].clone();
        changed_handle.handle = extra_handle;
        assert!(matches!(
            compare(domains[0], &closed, &[first, (domains[0], changed_handle)]),
            Err(Refusal::Conflict(_, _))
        ));

        let strategy = (
            any::<[bool; 3]>(),
            0usize..3,
            proptest::collection::vec((0usize..3, 0usize..3), 0..8),
            proptest::collection::vec(
                (
                    0usize..3,
                    0usize..2,
                    0usize..2,
                    0u32..3,
                    0usize..3,
                    any::<bool>(),
                ),
                0..20,
            ),
        );
        let mut config = Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::borrowed_selection_matches_explicit_rows_across_domain_histories"
        ));
        let configured_cases = config.cases;
        let callbacks = Cell::new(0usize);
        let prefixes = Cell::new(0usize);
        let accepted = Cell::new(0usize);
        let missing = Cell::new(0usize);
        let conflicts = Cell::new(0usize);
        let observe = |result: Result<(), Refusal>| {
            prefixes.set(prefixes.get() + 1);
            match result {
                Ok(()) => accepted.set(accepted.get() + 1),
                Err(Refusal::Missing(_)) => missing.set(missing.get() + 1),
                Err(Refusal::Conflict(_, _)) => conflicts.set(conflicts.get() + 1),
            }
        };
        let result =
            TestRunner::new(config).run(&strategy, |(present, current, transitions, history)| {
                callbacks.set(callbacks.get() + 1);
                let mut authored: HashMap<_, HashMap<_, _>> = domains
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| present[*index])
                    .map(|(_, domain)| (*domain, HashMap::new()))
                    .collect();
                for (from, to) in transitions {
                    if let Some(origins) = authored.get_mut(&domains[from]) {
                        origins.insert(leases[0].owner().clone(), domains[to]);
                    }
                }
                let mut rows = Vec::new();
                observe(compare(domains[current], &authored, &rows));
                for (domain, instance, owner, ordinal, binder, alternate_handle) in history {
                    let mut lease = leases[instance].clone();
                    lease.owner.module = format!("Owner{owner}");
                    lease.original_ordinal = ordinal;
                    lease.binder = super::tests::source(&format!("binder{binder}"));
                    if alternate_handle {
                        lease.handle = extra_handle;
                    }
                    rows.push((domains[domain], lease));
                    // Every prefix is the current selection plus one incoming row.
                    observe(compare(domains[current], &authored, &rows));
                }
                Ok(())
            });
        eprintln!(
            "source_selection_campaign={}",
            serde_json::json!({
                "configured_cases": configured_cases,
                "callbacks_including_replay_and_shrinking": callbacks.get(),
                "prefixes": prefixes.get(), "accepted": accepted.get(),
                "missing_domain": missing.get(), "conflicting_instance": conflicts.get(),
            })
        );
        result.unwrap();
    }

    #[test]
    fn borrowed_selection_validation_avoids_owned_lease_allocations() {
        let (_machine, leases) = installed_leases();
        let current = SourceInstanceDomain::single();
        let authored = HashMap::from([(
            current,
            HashMap::from([(leases[0].owner().clone(), current)]),
        )]);
        let rows: Vec<_> = (0..128)
            .map(|index| {
                let mut lease = leases[0].clone();
                lease.binder = super::tests::source(&format!("selected{index}"));
                (current, lease)
            })
            .collect();
        compare(current, &authored, &rows).unwrap();
        let owned = crate::binding_table::cost_tests::measure(|| {
            for _ in 0..32 {
                std::hint::black_box(
                    SourceDomainSelection::new(current, authored.clone(), rows.iter().cloned())
                        .unwrap(),
                );
            }
        });
        let borrowed = crate::binding_table::cost_tests::measure(|| {
            for _ in 0..32 {
                SourceDomainSelection::validate(
                    current,
                    |domain| authored.contains_key(domain),
                    authored
                        .values()
                        .flat_map(|origins| origins.values())
                        .copied(),
                    rows.iter().map(|(domain, lease)| (*domain, lease)),
                )
                .unwrap();
            }
        });
        let owned_counts = serde_json::to_value(owned.1).unwrap();
        let borrowed_counts = serde_json::to_value(borrowed.1).unwrap();
        assert!(
            borrowed_counts["allocations"].as_u64().unwrap()
                < owned_counts["allocations"].as_u64().unwrap()
        );
        assert!(
            borrowed_counts["allocated_bytes"].as_u64().unwrap()
                < owned_counts["allocated_bytes"].as_u64().unwrap()
        );
        eprintln!(
            "source_selection_validation_cost={}",
            serde_json::json!({
                "selected_rows": rows.len(), "repetitions": 32,
                "measurement_scope": "current-thread validation, excluding fixture construction and JIT; owned comparison is current constructor",
                "owned": {"elapsed_ns": owned.0, "allocations": owned_counts},
                "borrowed": {"elapsed_ns": borrowed.0, "allocations": borrowed_counts},
            })
        );
    }

    #[test]
    fn domain_registration_requires_live_scope_even_for_empty_batches() {
        let (machine, leases) = installed_leases();
        let mut tree = ScopeTree::new();
        let live = tree.mint_isolated();
        let retired = tree.mint_isolated();
        tree.retire(retired);
        let mut table = BindingTable::new();
        let before = table.scope_snapshot(&tree, ScopeId::ROOT).unwrap();
        let revision = table.mutation_revision();
        assert!(
            table
                .register_source_instances_in_domains(&tree, retired, vec![], &machine)
                .is_err(),
            "an empty batch cannot issue an admission for a retired scope"
        );
        assert_eq!(table.mutation_revision(), revision);
        assert_eq!(table.scope_snapshot(&tree, ScopeId::ROOT).unwrap(), before);
        let rejected = table
            .register_source_instances_in_domains(
                &tree,
                retired,
                vec![SourceInstanceAttachment::installed(
                    SourceInstanceDomain::for_scope(retired),
                    leases[0].clone(),
                )],
                &machine,
            )
            .unwrap_err();
        assert_eq!(rejected.len(), 1);
        assert_eq!(table.mutation_revision(), revision);
        assert_eq!(table.scope_snapshot(&tree, ScopeId::ROOT).unwrap(), before);
        assert!(table.source_instances_in(&tree, retired).is_empty());
        assert!(table
            .selected_source_instances_in(&tree, retired)
            .is_empty());
        machine.validate_source_attachment(&rejected[0]).unwrap();

        let empty = table
            .register_source_instances_in_domains(&tree, live, vec![], &machine)
            .ok()
            .expect("a live scope admits an empty batch");
        assert!(empty.is_empty());
        assert_ne!(table.mutation_revision(), revision);
        assert!(table.source_instances_in(&tree, live).is_empty());
        assert!(table.selected_source_instances_in(&tree, live).is_empty());
        let admitted = table
            .register_source_instances_in_domains(
                &tree,
                live,
                vec![SourceInstanceAttachment::installed(
                    SourceInstanceDomain::for_scope(live),
                    leases[0].clone(),
                )],
                &machine,
            )
            .ok()
            .expect("a live scope admits the genuine source root");
        assert_eq!(admitted.len(), 1);
        assert_eq!(table.selected_source_instances_in(&tree, live).len(), 1);
        assert_eq!(table.source_instances_in(&tree, live).len(), 1);
    }

    #[test]
    fn late_domain_registration_conflict_preserves_snapshot_and_custody() {
        let (machine, leases) = installed_leases();
        let tree = ScopeTree::new();
        let mut table = BindingTable::new();
        let scope = ScopeId::ROOT;
        let before = table.scope_snapshot(&tree, scope).unwrap();
        let revision = table.mutation_revision();
        let custody = table.source_instances_in(&tree, scope);
        let selected = table.selected_source_instances_in(&tree, scope);
        let incoming = leases
            .into_iter()
            .map(|lease| SourceInstanceAttachment::installed(SourceInstanceDomain::single(), lease))
            .collect();
        let refused = table
            .register_source_instances_in_domains(&tree, scope, incoming, &machine)
            .unwrap_err();
        assert_eq!(refused.len(), 2);
        assert_eq!(table.scope_snapshot(&tree, scope).unwrap(), before);
        assert_eq!(table.mutation_revision(), revision);
        assert_eq!(table.source_instances_in(&tree, scope).len(), custody.len());
        assert_eq!(
            table.selected_source_instances_in(&tree, scope).len(),
            selected.len()
        );
        // Both genuine roots remain usable after refusal; no partial admission.
        for attachment in &refused {
            machine.validate_source_attachment(attachment).unwrap();
        }
    }
}
