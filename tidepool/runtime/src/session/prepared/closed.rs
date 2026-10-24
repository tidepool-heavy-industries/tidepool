//! Compiled image projection of one issuer-owned linked target.

use super::*;
use super::super::admission::AuthorizedRetainedValue;
use tidepool_toolchain::checked_cell::ExactCheckedItem;
use tidepool_codegen::binding_table::WorldNativeSlotPermit;
use tidepool_codegen::prepared_program::{
    LiteralOwner, NativeDomainSelection, NativeExportRequest, PreparedNativeCode,
    ScopedNativeExport, SourceInstanceDomain, WorldNativeInstanceWitness,
};
use tidepool_repr::native_code::{
    NativeCodeBody, NativeCodeId, NativeCodeImport, NativeCodeRef, NativeRetainedRequirement,
};
use tidepool_toolchain::linked_native::{
    AcquiredNativeCode, AcquiredNativeImport, LinkedNativeTarget, LinkedNativeTargetImport,
    NativeCodeGrant, NativeOccurrenceId,
};

pub(in crate::session) struct RetainedNativeRequirement {
    leaf: NativeRetainedRequirement,
    owner: AcquiredNativeCode,
    global_index: u32,
    binding: tidepool_toolchain::artifact_inventory::ArtifactBinding,
    rep: RuntimeRep,
    entry_signature: Option<Signature>,
    required_evaluated: bool,
}
impl RetainedNativeRequirement {
    fn from_declaration(
        declaration: &tidepool_repr::execution_schema::GlobalDecl,
        signatures: &[Signature],
        leaf: NativeRetainedRequirement,
        owner: AcquiredNativeCode,
        global_index: u32,
        binding: tidepool_toolchain::artifact_inventory::ArtifactBinding,
    ) -> Result<Self, PreparedRuntimeError> {
        if declaration.identity != *leaf.identity()
            || declaration.required_generation != Some(leaf.generation())
        {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        Ok(Self {
            leaf,
            owner,
            global_index,
            binding,
            rep: declaration.rep,
            entry_signature: declaration
                .entry_signature
                .map(|id| {
                    signatures
                        .get(id.0 as usize)
                        .cloned()
                        .ok_or_else(|| PreparedRuntimeError::certified_target_owners())
                })
                .transpose()?,
            required_evaluated: declaration.required_evaluated,
        })
    }
}

/// A code-only membership loan. This cannot issue source or runtime authority.
pub(in crate::session) struct ClosedNativeDefinition<'a> {
    definitions: DefinitionsView<'a>,
}
impl ClosedNativeDefinition<'_> {
    pub(in crate::session) fn sites(&self) -> &[tidepool_repr::execution_schema::SiteRow] {
        self.definitions.sites()
    }
}
/// This loan is issued only while consuming an actual PACKAGE acquisition.
pub(in crate::session) struct ClosedPackageDefinition<'a> {
    code: &'a NativeCodeRef,
}
impl ClosedPackageDefinition<'_> {
    pub(in crate::session) fn sites(&self) -> &[tidepool_repr::execution_schema::SiteRow] {
        self.code.body().definitions().sites()
    }
}

/// Images borrow their bodies and ordered child choices from one linked owner.
/// No local HOME/PACKAGE graph or acquisition index is reconstructed here.
pub(in crate::session) struct NativeInstantiationPlan {
    linked: Arc<LinkedNativeTarget>,
    programs: BTreeMap<NativeCodeId, Arc<PreparedNativeCode>>,
    target: Arc<CompiledProgram>,
}

pub(in crate::session) struct ClosedInstantiationPlan {
    native: Arc<NativeInstantiationPlan>,
}
impl ClosedInstantiationPlan {
    pub(in crate::session) fn native(&self) -> &Arc<NativeInstantiationPlan> {
        &self.native
    }
    pub(in crate::session) fn prepare(
        compiled: &super::super::turn::CompiledTurn,
        registry: &ImageRegistry,
    ) -> Result<Self, PreparedRuntimeError> {
        Self::prepare_code(&compiled.code(), registry)
    }
    pub(in crate::session) fn prepare_code(
        code: &super::super::turn::TurnCode<'_>,
        registry: &ImageRegistry,
    ) -> Result<Self, PreparedRuntimeError> {
        Self::restrict(Arc::new(NativeInstantiationPlan::prepare_code(
            code, registry,
        )?))
    }
    pub(in crate::session) fn restrict(
        native: Arc<NativeInstantiationPlan>,
    ) -> Result<Self, PreparedRuntimeError> {
        if native
            .linked
            .imports()
            .iter()
            .any(|import| matches!(import, LinkedNativeTargetImport::RetainedCapture(_)))
            || native.programs.values().any(|program| {
                program
                    .code()
                    .imports()
                    .any(|import| matches!(import, NativeCodeImport::Retained(_)))
            })
        {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        BatchProgram::validate_closed(&native.closed_batch()?)
            .map_err(PreparedRuntimeError::Install)?;
        Ok(Self { native })
    }
    fn target_definition(&self) -> ClosedNativeDefinition<'_> {
        ClosedNativeDefinition {
            definitions: self.native.linked.prepared().definitions(),
        }
    }
}

impl NativeInstantiationPlan {
    pub(in crate::session) fn prepare_code(
        code: &super::super::turn::TurnCode<'_>,
        registry: &ImageRegistry,
    ) -> Result<Self, PreparedRuntimeError> {
        let linked = code
            .certification()
            .and_then(|certification| certification.linked_target.as_ref())
            .ok_or_else(|| PreparedRuntimeError::certified_target_owners())?;
        if !Arc::ptr_eq(linked.prepared(), code.prepared_owner()) {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        // The issuer has already validated every acquired occurrence. The image
        // projection may share equal code, without replacing those grant owners.
        let codes = linked
            .acquired()
            .map(|acquired| (acquired.code().id(), acquired.code().clone()))
            .collect::<BTreeMap<_, _>>();
        let mut programs = BTreeMap::new();
        for (id, node) in &codes {
            let definitions = node.body().definitions();
            if definitions.globals().is_empty()
                && matches!(definitions.bindings(), [Group::NonRecursive(top)] if matches!(top.binding.rhs, HeapRhs::Bytes(_)))
            {
                programs.insert(
                    *id,
                    Arc::new(
                        PreparedNativeCode::prepare(
                            node.clone(),
                            &[],
                            &Default::default(),
                            &Default::default(),
                            registry,
                        )
                        .map_err(PreparedRuntimeError::Compile)?,
                    ),
                );
            }
        }
        for (id, node) in &codes {
            if programs.contains_key(id) {
                continue;
            }
            let (owners, packages, sources) = Self::literal_inputs(
                node.body().definitions().globals(),
                node.imports(),
                &programs,
            )?;
            programs.insert(
                *id,
                Arc::new(
                    PreparedNativeCode::prepare(
                        node.clone(),
                        &owners,
                        &packages,
                        &sources,
                        registry,
                    )
                    .map_err(PreparedRuntimeError::Compile)?,
                ),
            );
        }
        let (owners, packages, sources) = Self::literal_inputs(
            linked.prepared().globals(),
            Self::target_imports(linked),
            &programs,
        )?;
        let target = CompiledProgram::prepare_target_code(
            linked.prepared(),
            &owners,
            &packages,
            &sources,
            registry,
        )
        .map_err(PreparedRuntimeError::Compile)?;
        Ok(Self {
            linked: Arc::clone(linked),
            programs,
            target,
        })
    }
    fn target_imports(
        linked: &LinkedNativeTarget,
    ) -> impl ExactSizeIterator<Item = NativeCodeImport> + '_ {
        linked.imports().iter().map(|import| match import {
            LinkedNativeTargetImport::Code { acquired, value } => NativeCodeImport::Code {
                code: acquired.code().clone(),
                value: *value,
            },
            LinkedNativeTargetImport::RetainedCapture(requirement) => {
                NativeCodeImport::Retained(requirement.requirement().clone())
            }
        })
    }
    pub(in crate::session) fn matches_code(&self, code: &super::super::turn::TurnCode<'_>) -> bool {
        Arc::ptr_eq(self.linked.prepared(), code.prepared_owner())
            && code
                .certification()
                .and_then(|certification| certification.linked_target.as_ref())
                .is_some_and(|linked| Arc::ptr_eq(linked, &self.linked))
    }
    pub(in crate::session) fn has_request_sites(&self) -> bool {
        !self.linked.prepared().sites().is_empty()
            || self
                .programs
                .values()
                .any(|program| !program.code().body().definitions().sites().is_empty())
    }
    pub(in crate::session) fn target_image(&self) -> &Arc<CompiledProgram> {
        &self.target
    }
    pub(in crate::session) fn images(&self) -> impl Iterator<Item = &Arc<CompiledProgram>> {
        std::iter::once(&self.target).chain(self.programs.values().map(|program| program.image()))
    }
    fn literal_inputs(
        globals: &[tidepool_repr::execution_schema::GlobalDecl],
        imports: impl ExactSizeIterator<Item = NativeCodeImport>,
        literals: &BTreeMap<NativeCodeId, Arc<PreparedNativeCode>>,
    ) -> Result<
        (
            Vec<Option<LiteralOwner>>,
            BTreeMap<SymbolIdentity, PackageLiteral>,
            BTreeMap<SourceBinder, SourceLiteral>,
        ),
        PreparedRuntimeError,
    > {
        if globals.len() != imports.len() {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        let mut owners = Vec::with_capacity(globals.len());
        let mut packages = BTreeMap::new();
        let mut sources = BTreeMap::new();
        for (global, import) in globals.iter().zip(imports) {
            let NativeCodeImport::Code { code, value } = import else {
                owners.push(None);
                continue;
            };
            match code.body() {
                NativeCodeBody::Home(body) => {
                    let source = SourceBinder {
                        version: body.owner().module_version.clone(),
                        binder: global.identity.clone(),
                    };
                    owners.push(Some(LiteralOwner::Source {
                        version: source.version.clone(),
                        binder: source.binder.clone(),
                    }));
                    if let Some(image) = literals.get(&code.id()) {
                        if let Some(literal) = image.image().source_literals().remove(&source) {
                            if sources
                                .insert(source.clone(), literal.clone())
                                .is_some_and(|old| old != literal)
                            {
                                return Err(DemandError::DuplicateBinder(source).into());
                            }
                        }
                    }
                }
                NativeCodeBody::Package(body) => {
                    let owner = body.owner();
                    owners.push(Some(LiteralOwner::Package {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                        binder: global.identity.clone(),
                        interface_digest: owner.interface_sha256,
                    }));
                    if let Some(image) = literals.get(&code.id()) {
                        if let Some(literal) = image.package_literal(value) {
                            if packages
                                .insert(global.identity.clone(), literal.clone())
                                .is_some_and(|old| old != literal)
                            {
                                return Err(PreparedRuntimeError::certified_target_owners());
                            }
                        }
                    }
                }
            }
        }
        Ok((owners, packages, sources))
    }
    fn closed_batch(&self) -> Result<Vec<BatchProgram>, PreparedRuntimeError> {
        let indices = self
            .programs
            .keys()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect::<BTreeMap<_, _>>();
        let imports = |edges: Vec<NativeCodeImport>| {
            edges
                .into_iter()
                .map(|edge| match edge {
                    NativeCodeImport::Code { code, value } => Ok(BatchImport::Source {
                        group: *indices
                            .get(&code.id())
                            .ok_or_else(|| PreparedRuntimeError::certified_target_owners())?,
                        binding: value,
                    }),
                    NativeCodeImport::Retained(_) => {
                        Err(PreparedRuntimeError::certified_target_owners())
                    }
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let mut batch = self
            .programs
            .values()
            .map(|program| {
                Ok(BatchProgram {
                    image: program.image().clone(),
                    imports: imports(program.code().imports().collect())?,
                })
            })
            .collect::<Result<Vec<_>, PreparedRuntimeError>>()?;
        batch.push(BatchProgram {
            image: self.target.clone(),
            imports: imports(Self::target_imports(&self.linked).collect())?,
        });
        Ok(batch)
    }
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct NativeOccurrence {
    program: NativeCodeId,
    domain: SourceInstanceDomain,
}

enum ScopedNativeSlot {
    Local {
        occurrence: NativeOccurrence,
        acquired: AcquiredNativeCode,
        value: ValueId,
    },
    Retained(AuthorizedRetainedValue),
}

fn same_import_destinations(left: &[ScopedNativeSlot], right: &[ScopedNativeSlot]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (
                    ScopedNativeSlot::Local {
                        occurrence: left,
                        value: left_value,
                        ..
                    },
                    ScopedNativeSlot::Local {
                        occurrence: right,
                        value: right_value,
                        ..
                    },
                ) => left == right && left_value == right_value,
                (
                    ScopedNativeSlot::Retained(left),
                    ScopedNativeSlot::Retained(right),
                ) => left.value().handle == right.value().handle,
                _ => false,
            })
}

/// One install's selection from the actual binding view. Reusable code does
/// not retain this observation or its existing physical instance leases.
pub(in crate::session) struct NativeInstantiation {
    plan: Arc<NativeInstantiationPlan>,
    anchors: BTreeMap<NativeOccurrence, NativeInstanceLease>,
    native_anchors: BTreeMap<NativeOccurrence, WorldNativeInstanceWitness>,
    native_permits: BTreeMap<(NativeOccurrence, ValueId), WorldNativeSlotPermit>,
    programs: BTreeMap<NativeOccurrence, Vec<ScopedNativeSlot>>,
    fresh_acquisitions: BTreeMap<NativeOccurrence, BTreeSet<NativeOccurrenceId>>,
    target_imports: Vec<ScopedNativeSlot>,
    retained_obligations: Vec<AuthorizedRetainedValue>,
    expected_execution: Option<(ExactCheckedItem, [u8; 32])>,
}

/// Runtime source evidence selected from the current admitted output. Fresh
/// closed package code belongs to this installing operation; domain selection
/// does not replace the source transaction's actual prerequisite admission.
pub(in crate::session) struct NativeInstallationSources {
    target: NativeSourceCustody,
    // None is an explicitly historical definition, never current source.
    programs: BTreeMap<NativeOccurrenceId, Option<NativeSourceCustody>>,
}

impl NativeInstallationSources {
    pub(in crate::session) fn current(
        plan: &NativeInstantiationPlan,
        current: &super::super::turn::CurrentNativeSources<'_>,
    ) -> Result<Self, PreparedRuntimeError> {
        let target = current.target(&plan.linked)?;
        let mut programs = BTreeMap::new();
        // Equal executable identity never substitutes an acquired source grant.
        for acquired in plan.linked.acquired() {
            let source = match acquired.grant() {
                NativeCodeGrant::Home { .. } => current.group(&plan.linked, &acquired)?,
                NativeCodeGrant::Package { .. } => Some(current.package(ClosedPackageDefinition {
                    code: acquired.code(),
                })),
            };
            if programs.insert(acquired.occurrence(), source).is_some() {
                return Err(PreparedRuntimeError::certified_target_owners());
            }
        }
        Ok(Self { target, programs })
    }
    pub(in crate::session) fn closed(
        plan: &ClosedInstantiationPlan,
        origin: &super::super::resident::CapturedSourceOrigin,
    ) -> Self {
        Self {
            target: origin.for_closed_definition(plan.target_definition()),
            programs: plan
                .native
                .linked
                .acquired()
                .map(|acquired| {
                    (
                        acquired.occurrence(),
                        Some(origin.for_closed_definition(ClosedNativeDefinition {
                            definitions: acquired.code().body().definitions(),
                        })),
                    )
                })
                .collect(),
        }
    }
}

impl NativeInstantiationPlan {
    fn authorize_retained_imports(
        expected_execution: Option<(&ExactCheckedItem, [u8; 32])>,
        linked: &Arc<LinkedNativeTarget>,
        owner: &AcquiredNativeCode,
        retained: &mut impl FnMut(
            &Arc<RetainedNativeRequirement>,
        ) -> Result<AuthorizedRetainedValue, PreparedRuntimeError>,
    ) -> Result<Vec<AuthorizedRetainedValue>, PreparedRuntimeError> {
        let mut authorized = Vec::new();
        let definitions = owner.code().body().definitions();
        if definitions.globals().len() != owner.imports().len() {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        for (global_index, (declaration, import)) in definitions
            .globals()
            .iter()
            .zip(owner.imports())
            .enumerate()
        {
            if let AcquiredNativeImport::Retained {
                requirement: leaf,
                binding,
            } = import
            {
                let requirement = Arc::new(RetainedNativeRequirement::from_declaration(
                    declaration,
                    definitions.signatures(),
                    leaf,
                    owner.clone(),
                    u32::try_from(global_index)
                        .map_err(|_| PreparedRuntimeError::certified_target_owners())?,
                    binding,
                )?);
                authorized.push(Self::resolve_retained(expected_execution, linked, &requirement, retained)?);
            }
        }
        Ok(authorized)
    }

    fn resolve_retained(
        expected_execution: Option<(&ExactCheckedItem, [u8; 32])>,
        linked: &Arc<LinkedNativeTarget>,
        requirement: &Arc<RetainedNativeRequirement>,
        retained: &mut impl FnMut(&Arc<RetainedNativeRequirement>) -> Result<AuthorizedRetainedValue, PreparedRuntimeError>,
    ) -> Result<AuthorizedRetainedValue, PreparedRuntimeError> {
        let Some((item, admission_digest)) = expected_execution else {
            return Err(PreparedRuntimeError::certified_target_owners());
        };
        let authorized = retained(requirement)?;
        if !authorized.matches_slot(item, admission_digest, linked, requirement) {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        Ok(authorized)
    }

    pub(in crate::session) fn linked(&self) -> &Arc<LinkedNativeTarget> { &self.linked }

    pub(in crate::session) fn target_constructors(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::ConstructorDecl> + Clone {
        self.target.constructor_declarations()
    }
    pub(in crate::session) fn dependency_constructors(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::ConstructorDecl> {
        self.programs
            .values()
            .flat_map(|program| program.image().constructor_declarations())
    }
    fn scoped_slots(
        expected_execution: Option<(&ExactCheckedItem, [u8; 32])>,
        linked: &Arc<LinkedNativeTarget>,
        caller: SourceInstanceDomain,
        definitions: DefinitionsView<'_>,
        owner: &AcquiredNativeCode,
        selection: &NativeDomainSelection,
        retained: &mut impl FnMut(
            &Arc<RetainedNativeRequirement>,
        ) -> Result<AuthorizedRetainedValue, PreparedRuntimeError>,
    ) -> Result<Vec<ScopedNativeSlot>, PreparedRuntimeError> {
        let imports = owner.imports();
        if definitions.globals().len() != imports.len() {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        definitions
            .globals()
            .iter()
            .zip(imports)
            .enumerate()
            .map(|(global_index, (declaration, import))| match import {
                AcquiredNativeImport::Code { acquired, value } => {
                    Self::scoped_code(caller, acquired, value, selection)
                }
                AcquiredNativeImport::Retained {
                    requirement: leaf,
                    binding,
                } => {
                    let requirement = Arc::new(RetainedNativeRequirement::from_declaration(
                        declaration,
                        definitions.signatures(),
                        leaf,
                        owner.clone(),
                        u32::try_from(global_index)
                            .map_err(|_| PreparedRuntimeError::certified_target_owners())?,
                        binding,
                    )?);
                    let authorized = Self::resolve_retained(expected_execution, linked, &requirement, retained)?;
                    Ok(ScopedNativeSlot::Retained(authorized))
                }
            })
            .collect()
    }
    fn scoped_code(
        caller: SourceInstanceDomain,
        acquired: AcquiredNativeCode,
        value: ValueId,
        selection: &NativeDomainSelection,
    ) -> Result<ScopedNativeSlot, PreparedRuntimeError> {
        let code = acquired.code();
        let domain = match code.body() {
            NativeCodeBody::Home(body) => selection.domain_for_owner(caller, body.owner())?,
            NativeCodeBody::Package(_) => caller,
        };
        Ok(ScopedNativeSlot::Local {
            occurrence: NativeOccurrence {
                program: code.id(),
                domain,
            },
            acquired,
            value,
        })
    }
    pub(in crate::session) fn instantiate(
        self: &Arc<Self>,
        expected_execution: Option<(&ExactCheckedItem, [u8; 32])>,
        selection: NativeDomainSelection,
        mut native_slot: impl FnMut(
            &NativeExportRequest,
            Option<&NativeInstanceLease>,
        ) -> Result<
            (WorldNativeSlotPermit, Option<WorldNativeInstanceWitness>),
            PreparedRuntimeError,
        >,
        mut retained: impl FnMut(
            &Arc<RetainedNativeRequirement>,
        ) -> Result<AuthorizedRetainedValue, PreparedRuntimeError>,
    ) -> Result<NativeInstantiation, PreparedRuntimeError> {
        let definitions = self.linked.prepared().definitions();
        if definitions.globals().len() != self.linked.imports().len() {
            return Err(PreparedRuntimeError::certified_target_owners());
        }
        let mut covered_entry_slots = BTreeMap::new();
        let target_imports = definitions
            .globals()
            .iter()
            .zip(self.linked.imports())
            .map(|(declaration, import)| match import {
                LinkedNativeTargetImport::Code { acquired, value } => {
                    Self::scoped_code(selection.current(), acquired.clone(), *value, &selection)
                }
                LinkedNativeTargetImport::RetainedCapture(capture) => {
                    let entry_owner = capture.entry_owner().ok_or_else(PreparedRuntimeError::certified_target_owners)?;
                    let requirement = Arc::new(RetainedNativeRequirement::from_declaration(
                        declaration,
                        definitions.signatures(),
                        capture.requirement().clone(),
                        entry_owner.owner().clone(),
                        entry_owner.global_index(),
                        entry_owner.binding(),
                    )?);
                    let authorized = Self::resolve_retained(expected_execution, &self.linked, &requirement, &mut retained)?;
                    if Some(entry_owner.owner().occurrence()) == self.linked.entry_occurrence() {
                        covered_entry_slots.insert(entry_owner.global_index(), (entry_owner.binding(), capture.requirement().clone()));
                    }
                    Ok(ScopedNativeSlot::Retained(authorized))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut pending = target_imports
            .iter()
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Local {
                    occurrence,
                    acquired,
                    value,
                } => Some((*occurrence, acquired.clone(), *value)),
                ScopedNativeSlot::Retained(_) => None,
            })
            .collect::<Vec<_>>();
        let mut visited = BTreeSet::new();
        let mut anchors = BTreeMap::new();
        let mut native_anchors = BTreeMap::new();
        let mut native_permits = BTreeMap::new();
        let mut programs = BTreeMap::new();
        let mut fresh_acquisitions =
            BTreeMap::<NativeOccurrence, BTreeSet<NativeOccurrenceId>>::new();
        while let Some((occurrence, acquired, value)) = pending.pop() {
            if !visited.insert((acquired.occurrence(), occurrence.domain)) {
                continue;
            }
            let program = self
                .programs
                .get(&occurrence.program)
                .ok_or_else(|| PreparedRuntimeError::certified_target_owners())?;
            let binding = program.code().id();
            let anchor = selection
                .roots()
                .range(
                    ScopedNativeExport {
                        domain: occurrence.domain,
                        binding,
                        value: ValueId(0),
                    }..,
                )
                .next()
                .and_then(|(key, lease)| {
                    (key.domain == occurrence.domain && key.binding == binding).then_some(lease)
                });
            let request = program
                .request(occurrence.domain, value)
                .map_err(PreparedRuntimeError::Install)?;
            let (permit, witness) = native_slot(&request, anchor)?;
            if anchor.is_some() && witness.is_none() {
                return Err(PreparedRuntimeError::certified_target_owners());
            }
            native_permits.insert((occurrence, value), permit);
            let anchored = witness.is_some();
            if let Some(witness) = witness {
                native_anchors.insert(occurrence, witness);
            }
            if let Some(anchor) = anchor {
                anchors.insert(occurrence, anchor.clone());
            }
            if anchored {
                continue;
            }
            let imports = Self::scoped_slots(
                expected_execution,
                &self.linked,
                occurrence.domain,
                program.code().body().definitions(),
                &acquired,
                &selection,
                &mut retained,
            )?;
            pending.extend(imports.iter().filter_map(|slot| match slot {
                ScopedNativeSlot::Local {
                    occurrence,
                    acquired,
                    value,
                } => Some((*occurrence, acquired.clone(), *value)),
                ScopedNativeSlot::Retained(_) => None,
            }));
            fresh_acquisitions
                .entry(occurrence)
                .or_default()
                .insert(acquired.occurrence());
            match programs.entry(occurrence) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(imports);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    if !same_import_destinations(entry.get(), &imports) {
                        return Err(PreparedRuntimeError::certified_target_owners());
                    }
                }
            }
        }
        // Existing physical code keeps its installed child destinations.
        // Authorizing another acquisition must not instantiate those children
        // again. The actual linked owner still supplies every retained import
        // obligation, including descendants behind an anchored parent.
        let mut bound = fresh_acquisitions
            .values()
            .flatten()
            .copied()
            .collect::<BTreeSet<_>>();
        if let Some(entry_occurrence) = self.linked.entry_occurrence()
            && let Some(entry_owner) = self.linked.get(entry_occurrence)
        {
            let expected_entry_slots = entry_owner.imports().enumerate().filter_map(|(index, import)| match import {
                AcquiredNativeImport::Retained { requirement, binding } =>
                    u32::try_from(index).ok().map(|slot| (slot, (binding, requirement))),
                AcquiredNativeImport::Code { .. } => None,
            }).collect::<BTreeMap<_, _>>();
            if covered_entry_slots == expected_entry_slots { bound.insert(entry_occurrence); }
        }
        let mut retained_obligations = Vec::new();
        for acquired in self
            .linked
            .acquired()
            .filter(|acquired| !bound.contains(&acquired.occurrence()))
        {
            retained_obligations.extend(Self::authorize_retained_imports(expected_execution, &self.linked, &acquired, &mut retained)?);
        }
        let requested = programs
            .values()
            .flatten()
            .chain(&target_imports)
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Local {
                    occurrence, value, ..
                } => Some((*occurrence, *value)),
                ScopedNativeSlot::Retained(_) => None,
            })
            .collect::<BTreeSet<_>>();
        for (occurrence, value) in requested {
            if native_permits.contains_key(&(occurrence, value)) {
                continue;
            }
            let program = &self.programs[&occurrence.program];
            let request = program
                .request(occurrence.domain, value)
                .map_err(PreparedRuntimeError::Install)?;
            let (permit, witness) = native_slot(&request, anchors.get(&occurrence))?;
            if witness.as_ref().map(|w| w.instance())
                != native_anchors.get(&occurrence).map(|w| w.instance())
            {
                return Err(PreparedRuntimeError::certified_target_owners());
            }
            native_permits.insert((occurrence, value), permit);
        }
        Ok(NativeInstantiation {
            plan: self.clone(),
            anchors,
            native_anchors,
            native_permits,
            programs,
            fresh_acquisitions,
            target_imports,
            retained_obligations,
            expected_execution: expected_execution.map(|(item, digest)| (item.clone(), digest)),
        })
    }
}

impl RetainedNativeRequirement {
    pub(in crate::session) fn owner(&self) -> &AcquiredNativeCode { &self.owner }
    pub(in crate::session) fn global_index(&self) -> u32 { self.global_index }
    pub(in crate::session) fn binding(&self) -> tidepool_toolchain::artifact_inventory::ArtifactBinding { self.binding }
    pub(in crate::session) fn leaf(&self) -> &NativeRetainedRequirement { &self.leaf }
    pub(in crate::session) fn identity(&self) -> &SymbolIdentity {
        self.leaf.identity()
    }
    pub(in crate::session) fn generation(&self) -> u64 {
        self.leaf.generation()
    }
}

impl NativeInstantiation {
    pub(in crate::session) fn plan(&self) -> &NativeInstantiationPlan {
        &self.plan
    }

    pub(in crate::session) fn shares_plan(&self, plan: &Arc<NativeInstantiationPlan>) -> bool {
        Arc::ptr_eq(&self.plan, plan)
    }

    pub(in crate::session) fn retained_values(
        &self,
    ) -> impl Iterator<Item = &AuthorizedRetainedValue> {
        self.programs
            .values()
            .flatten()
            .chain(&self.target_imports)
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Retained(authorized) => Some(authorized),
                ScopedNativeSlot::Local { .. } => None,
            })
            .chain(&self.retained_obligations)
    }
}

impl PreparedEngine {
    pub(in crate::session) fn install_native_turn(
        &mut self,
        instantiation: &NativeInstantiation,
        sources: &NativeInstallationSources,
        bindings: &BindingTable,
        scopes: &tidepool_codegen::scope::ScopeTree,
        scope: tidepool_codegen::scope::ScopeId,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        let plan = &instantiation.plan;
        for authorized in instantiation.retained_values() {
            let Some((item, digest)) = &instantiation.expected_execution else {
                return Err(PreparedRuntimeError::certified_target_owners());
            };
            if !authorized.matches_slot(item, *digest, &plan.linked, authorized.requirement()) {
                return Err(PreparedRuntimeError::certified_target_owners());
            }
            let requirement = authorized.requirement();
            let capture = authorized.value();
            let entry = bindings
                .get(capture.id)
                .ok_or_else(|| PreparedRuntimeError::certified_target_owners())?;
            if entry.value.identity != *requirement.identity()
                || entry.module
                    != super::super::SessionModule::val(tidepool_repr::Generation(
                        requirement.generation(),
                    ))
                || entry.value.handle != capture.handle
                || capture.identity != *requirement.identity()
                || capture.generation != requirement.generation()
                || capture.handle.rep() != requirement.rep
                || self.machine.prepared_handle_of(capture.handle.raw()) != Some(capture.handle)
                || requirement.entry_signature.is_some()
            {
                return Err(PreparedRuntimeError::certified_target_owners());
            }
            if requirement.required_evaluated
                && !self
                    .machine
                    .handle_is_evaluated(capture.handle)
                    .map_err(PreparedRuntimeError::Install)?
            {
                return Err(PreparedRuntimeError::certified_target_owners());
            }
        }
        // Resolution retains weak choices, including observed vacancies. Check
        // them under the final source/snapshot admission before staging a batch;
        // a caller cannot turn an intervening sibling install into a duplicate.
        for ((occurrence, _), permit) in &instantiation.native_permits {
            if !bindings.validate_native_slot_permit(scopes, scope, permit) {
                return Err(PreparedRuntimeError::SourceScopeAdmission);
            }
            let observed = self
                .machine
                .lookup_world_native_slot(permit, instantiation.anchors.get(occurrence))
                .map_err(PreparedRuntimeError::Install)?;
            let expected = instantiation.native_anchors.get(occurrence);
            if observed.as_ref().map(|witness| witness.instance())
                != expected.map(|witness| witness.instance())
            {
                return Err(PreparedRuntimeError::SourceScopeAdmission);
            }
            if let Some(witness) = expected {
                self.machine
                    .validate_world_native_witness(witness, permit)
                    .map_err(PreparedRuntimeError::Install)?;
            }
        }
        let fresh = instantiation.programs.keys().copied().collect::<Vec<_>>();
        let remapped = fresh
            .iter()
            .enumerate()
            .map(|(index, occurrence)| (*occurrence, index))
            .collect::<BTreeMap<_, _>>();
        let requested = instantiation
            .programs
            .values()
            .flatten()
            .chain(&instantiation.target_imports)
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Local {
                    occurrence, value, ..
                } => Some((*occurrence, *value)),
                ScopedNativeSlot::Retained(_) => None,
            })
            .collect::<BTreeSet<_>>();
        // Resolve historical source custody before acquiring any mutable native
        // resources. A retained code row cannot become current merely because
        // this target installs equal code. Anchored occurrences are absent from
        // fresh and keep their original ProgramFacts; closed installation supplies
        // its own origin.
        let program_sources = fresh
            .iter()
            .map(|occurrence| {
                let program = &plan.programs[&occurrence.program];
                let mut merged = NativeSourceCustody::default();
                for acquired in &instantiation.fresh_acquisitions[occurrence] {
                    match &sources.programs[acquired] {
                        Some(current) => merged.merge(current),
                        None if program.code().body().definitions().sites().is_empty() => {}
                        None => return Err(PreparedRuntimeError::certified_target_owners()),
                    }
                }
                Ok((*occurrence, merged))
            })
            .collect::<Result<BTreeMap<_, _>, PreparedRuntimeError>>()?;
        let mut late_roots = Vec::<NativeInstanceLease>::new();
        let mut native_slots = Vec::with_capacity(instantiation.native_permits.len());
        let result = (|| {
            let mut attachments = Vec::new();
            let mut existing = BTreeMap::new();
            let mut requests = Vec::new();
            for (occurrence, value) in requested {
                let program = &plan.programs[&occurrence.program];
                let contract = program
                    .image()
                    .export_contract(value)
                    .ok_or_else(|| PreparedRuntimeError::certified_target_owners())?;
                let permit = instantiation
                    .native_permits
                    .get(&(occurrence, value))
                    .ok_or_else(|| PreparedRuntimeError::certified_target_owners())?;
                if let Some(witness) = instantiation.native_anchors.get(&occurrence) {
                    let held = self
                        .programs
                        .get(&witness.instance().program())
                        .ok_or_else(|| PreparedRuntimeError::certified_target_owners())?;
                    let Some(NativeProgramCode::Dependency { code: held_code }) = &held.code_owner
                    else {
                        return Err(PreparedRuntimeError::certified_target_owners());
                    };
                    if held_code.code().id() != program.code().id()
                        || !Arc::ptr_eq(&held.definitions, program.image().definition_facts())
                    {
                        return Err(PreparedRuntimeError::certified_target_owners());
                    }
                }
                if contract.rep == RuntimeRep::Address {
                    continue;
                }
                if let Some(witness) = instantiation.native_anchors.get(&occurrence) {
                    let attachment =
                        match bindings.retained_world_native_attachment(permit, witness) {
                            Some(held) => held,
                            None => {
                                let issued = self
                                    .machine
                                    .retain_world_native_export(witness, permit)
                                    .map_err(PreparedRuntimeError::Install)?;
                                late_roots.push(issued.lease().clone());
                                issued
                            }
                        };
                    existing.insert((occurrence, value), attachment.lease().clone());
                    attachments.push(attachment);
                } else {
                    requests.push(BatchLeaseRequest::new(
                        remapped[&occurrence],
                        permit.request().clone(),
                    ));
                }
            }
            let imports = |slots: &[ScopedNativeSlot]| -> Result<Vec<_>, PreparedRuntimeError> {
                slots
                    .iter()
                    .map(|slot| match slot {
                        ScopedNativeSlot::Local {
                            occurrence, value, ..
                        } => {
                            if let Some(lease) = existing.get(&(*occurrence, *value)) {
                                Ok(BatchImport::Existing {
                                    handle: lease.handle(),
                                    entry_signature: lease.entry_signature().cloned(),
                                })
                            } else if let Some(witness) =
                                instantiation.native_anchors.get(occurrence)
                            {
                                let permit = instantiation
                                    .native_permits
                                    .get(&(*occurrence, *value))
                                    .ok_or_else(|| {
                                        PreparedRuntimeError::certified_target_owners()
                                    })?;
                                self.machine
                                    .world_native_literal_import(witness, permit)
                                    .map_err(PreparedRuntimeError::Install)
                            } else {
                                Ok(BatchImport::Source {
                                    group: *remapped.get(occurrence).ok_or_else(|| {
                                        PreparedRuntimeError::certified_target_owners()
                                    })?,
                                    binding: *value,
                                })
                            }
                        }
                        ScopedNativeSlot::Retained(authorized) => {
                            if !Arc::ptr_eq(authorized.target(), &plan.linked) {
                                return Err(PreparedRuntimeError::certified_target_owners());
                            }
                            let capture = authorized.value();
                            Ok(BatchImport::Existing {
                                handle: capture.handle,
                                entry_signature: None,
                            })
                        }
                    })
                    .collect()
            };
            let mut batch = fresh
                .iter()
                .map(|occurrence| {
                    Ok(BatchProgram {
                        image: Arc::clone(plan.programs[&occurrence.program].image()),
                        imports: imports(&instantiation.programs[occurrence])?,
                    })
                })
                .collect::<Result<Vec<_>, PreparedRuntimeError>>()?;
            batch.push(BatchProgram {
                image: Arc::clone(&plan.target),
                imports: imports(&instantiation.target_imports)?,
            });
            let make_facts = |occurrence: Option<NativeOccurrence>| -> Result<ProgramFacts, PreparedRuntimeError> {
                let (mut facts, imports) = match occurrence {
                    Some(occurrence) => {
                        let program = &plan.programs[&occurrence.program];
                        let mut facts = ProgramFacts::from_image(program.image(), None);
                        facts.native_sources = program_sources[&occurrence].clone();
                        facts.code_owner = Some(NativeProgramCode::Dependency { code: Arc::clone(program) });
                        (facts, &instantiation.programs[&occurrence])
                    }
                    None => {
                        let mut facts = ProgramFacts::from_image(&plan.target, Some(plan.linked.entry()));
                        facts.native_sources = sources.target.clone();
                        facts.code_owner = Some(NativeProgramCode::Invocation { _target: Arc::clone(&plan.linked) });
                        (facts, &instantiation.target_imports)
                    }
                };
                for import in imports {
                    match import {
                        ScopedNativeSlot::Local { occurrence, value, .. } => {
                            // Reuse retains original defining source facts,
                            // including literal imports that have no heap lease.
                            let reused = instantiation.native_anchors.get(occurrence)
                                .map(|witness| witness.instance())
                                .or_else(|| existing.get(&(*occurrence, *value)).map(|lease| lease.instance()));
                            if let Some(instance) = reused {
                                let held = self.programs.get(&instance.program()).ok_or_else(|| PreparedRuntimeError::certified_target_owners())?;
                                facts.native_sources.merge(&held.native_sources);
                            }
                        }
                        ScopedNativeSlot::Retained(authorized) => {
                            facts.native_sources.merge(&authorized.value().provenance().map_err(|_| PreparedRuntimeError::certified_target_owners())?.source_custody());
                        }
                    }
                }
                Ok(facts)
            };
            let mut facts = fresh
                .iter()
                .map(|occurrence| make_facts(Some(*occurrence)))
                .collect::<Result<Vec<_>, _>>()?;
            facts.push(make_facts(None)?);
            let settled = SettledIds::from_facts(self.programs.values().chain(facts.iter()))?;
            facts.last_mut().expect("target is final").settled = settled;
            let direct = facts
                .iter()
                .map(|facts| facts.native_sources.clone())
                .collect::<Vec<_>>();
            let mut admitted = self.plan_batch_evidence(facts)?;
            let installed = self
                .machine
                .install_shared_batch_with_leases(batch, requests)
                .map_err(PreparedRuntimeError::Install)?;
            assert_eq!(installed.programs.len(), admitted.len());
            assert_eq!(installed.causal_members.len(), admitted.len());
            for (facts, members) in admitted.iter_mut().zip(&installed.causal_members) {
                let mut sources = NativeSourceCustody::default();
                for member in members {
                    sources.merge(&direct[*member]);
                }
                facts.facts.native_sources = sources;
            }
            for program in &installed.programs {
                self.machine
                    .pin(*program)
                    .expect("batch returned installed program");
            }
            for ((occurrence, _), permit) in &instantiation.native_permits {
                let instance = match instantiation.native_anchors.get(occurrence) {
                    Some(witness) => witness.instance(),
                    None => installed.programs[remapped[occurrence]].into(),
                };
                native_slots.push((permit.clone(), instance));
            }
            let target = *installed.programs.last().expect("target is final");
            attachments.extend(installed.source_attachments);
            Ok(CertifiedTurnInstall {
                native_sources: admitted
                    .last()
                    .expect("target is final")
                    .facts
                    .native_sources
                    .clone(),
                target,
                groups: installed.programs[..installed.programs.len() - 1].to_vec(),
                leases: installed.leases,
                domain_leases: attachments,
                native_slots,
                admitted,
            })
        })();
        match result {
            Ok(mut installed) => {
                installed.leases.extend(late_roots);
                Ok(installed)
            }
            Err(error) => {
                for lease in late_roots {
                    assert!(
                        self.release(lease.handle()),
                        "failed exact install retains sibling root"
                    );
                }
                Err(error)
            }
        }
    }
}
