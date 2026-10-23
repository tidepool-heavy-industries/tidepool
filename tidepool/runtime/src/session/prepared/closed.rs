//! Compiled image projection of one issuer-owned linked target.

use super::*;
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
    origin: RetainedNativeOrigin,
    rep: RuntimeRep,
    entry_signature: Option<Signature>,
    required_evaluated: bool,
}
pub(in crate::session) enum RetainedNativeOrigin {
    Target {
        global_index: u32,
    },
    Acquired {
        owner: AcquiredNativeCode,
        global_index: u32,
        binding: tidepool_toolchain::artifact_inventory::ArtifactBinding,
    },
}
impl RetainedNativeRequirement {
    fn from_declaration(
        declaration: &tidepool_repr::execution_schema::GlobalDecl,
        signatures: &[Signature],
        leaf: NativeRetainedRequirement,
        origin: RetainedNativeOrigin,
    ) -> Result<Self, PreparedRuntimeError> {
        if declaration.identity != *leaf.identity()
            || declaration.required_generation != Some(leaf.generation())
        {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        Ok(Self {
            leaf,
            origin,
            rep: declaration.rep,
            entry_signature: declaration
                .entry_signature
                .map(|id| {
                    signatures
                        .get(id.0 as usize)
                        .cloned()
                        .ok_or(PreparedRuntimeError::CertifiedTargetOwners)
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
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        if !Arc::ptr_eq(linked.prepared(), code.prepared_owner()) {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
                NativeCodeImport::Retained(requirement.clone())
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
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
                                return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
                            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?,
                        binding: value,
                    }),
                    NativeCodeImport::Retained(_) => {
                        Err(PreparedRuntimeError::CertifiedTargetOwners)
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
    Retained {
        requirement: Arc<RetainedNativeRequirement>,
        value: Arc<super::super::admission::ScopedResolvedValue>,
    },
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
                    ScopedNativeSlot::Retained { value: left, .. },
                    ScopedNativeSlot::Retained { value: right, .. },
                ) => left.id == right.id && left.handle == right.handle,
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
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
        caller: SourceInstanceDomain,
        definitions: DefinitionsView<'_>,
        owner: &AcquiredNativeCode,
        selection: &NativeDomainSelection,
        retained: &mut impl FnMut(
            &RetainedNativeRequirement,
        ) -> Result<
            Arc<super::super::admission::ScopedResolvedValue>,
            PreparedRuntimeError,
        >,
    ) -> Result<Vec<ScopedNativeSlot>, PreparedRuntimeError> {
        let imports = owner.imports();
        if definitions.globals().len() != imports.len() {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
                        RetainedNativeOrigin::Acquired {
                            owner: owner.clone(),
                            global_index: u32::try_from(global_index)
                                .map_err(|_| PreparedRuntimeError::CertifiedTargetOwners)?,
                            binding,
                        },
                    )?);
                    let value = retained(&requirement)?;
                    Ok(ScopedNativeSlot::Retained { requirement, value })
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
        selection: NativeDomainSelection,
        mut native_slot: impl FnMut(
            &NativeExportRequest,
            Option<&NativeInstanceLease>,
        ) -> Result<
            (WorldNativeSlotPermit, Option<WorldNativeInstanceWitness>),
            PreparedRuntimeError,
        >,
        mut retained: impl FnMut(
            &RetainedNativeRequirement,
        ) -> Result<
            Arc<super::super::admission::ScopedResolvedValue>,
            PreparedRuntimeError,
        >,
    ) -> Result<NativeInstantiation, PreparedRuntimeError> {
        let definitions = self.linked.prepared().definitions();
        if definitions.globals().len() != self.linked.imports().len() {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        let target_imports = definitions
            .globals()
            .iter()
            .zip(self.linked.imports())
            .enumerate()
            .map(|(global_index, (declaration, import))| match import {
                LinkedNativeTargetImport::Code { acquired, value } => {
                    Self::scoped_code(selection.current(), acquired.clone(), *value, &selection)
                }
                LinkedNativeTargetImport::RetainedCapture(leaf) => {
                    let requirement = Arc::new(RetainedNativeRequirement::from_declaration(
                        declaration,
                        definitions.signatures(),
                        leaf.clone(),
                        RetainedNativeOrigin::Target {
                            global_index: u32::try_from(global_index)
                                .map_err(|_| PreparedRuntimeError::CertifiedTargetOwners)?,
                        },
                    )?);
                    let value = retained(&requirement)?;
                    Ok(ScopedNativeSlot::Retained { requirement, value })
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
                ScopedNativeSlot::Retained { .. } => None,
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
                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
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
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
            }
            native_permits.insert((occurrence, value), permit);
            let anchored = witness.is_some();
            if let Some(witness) = witness {
                native_anchors.insert(occurrence, witness);
            }
            if let Some(anchor) = anchor {
                anchors.insert(occurrence, anchor.clone());
            }
            let imports = Self::scoped_slots(
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
                ScopedNativeSlot::Retained { .. } => None,
            }));
            // Every acquisition is authorized before executable sharing,
            // including a new acquisition of an already installed image.
            if anchored {
                continue;
            }
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
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
                    }
                }
            }
        }
        let requested = programs
            .values()
            .flatten()
            .chain(&target_imports)
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Local {
                    occurrence, value, ..
                } => Some((*occurrence, *value)),
                ScopedNativeSlot::Retained { .. } => None,
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
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
        })
    }
}

impl RetainedNativeRequirement {
    pub(in crate::session) fn origin(&self) -> &RetainedNativeOrigin {
        &self.origin
    }
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
    ) -> impl Iterator<Item = &Arc<super::super::admission::ScopedResolvedValue>> {
        self.programs
            .values()
            .flatten()
            .chain(&self.target_imports)
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Retained { value, .. } => Some(value),
                ScopedNativeSlot::Local { .. } => None,
            })
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
                ScopedNativeSlot::Retained { .. } => None,
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
                        None => return Err(PreparedRuntimeError::CertifiedTargetOwners),
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
                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                let permit = instantiation
                    .native_permits
                    .get(&(occurrence, value))
                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                if let Some(witness) = instantiation.native_anchors.get(&occurrence) {
                    let held = self
                        .programs
                        .get(&witness.instance().program())
                        .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                    let Some(NativeProgramCode::Dependency { code: held_code }) = &held.code_owner
                    else {
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
                    };
                    if held_code.code().id() != program.code().id()
                        || !Arc::ptr_eq(&held.definitions, program.image().definition_facts())
                    {
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
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
                                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                                self.machine
                                    .world_native_literal_import(witness, permit)
                                    .map_err(PreparedRuntimeError::Install)
                            } else {
                                Ok(BatchImport::Source {
                                    group: *remapped
                                        .get(occurrence)
                                        .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?,
                                    binding: *value,
                                })
                            }
                        }
                        ScopedNativeSlot::Retained {
                            requirement,
                            value: capture,
                        } => {
                            let entry = bindings
                                .get(capture.id)
                                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                            if entry.value.identity != *requirement.identity()
                                || entry.module
                                    != super::super::SessionModule::val(tidepool_repr::Generation(
                                        requirement.generation(),
                                    ))
                                || entry.value.handle != capture.handle
                                || capture.identity != *requirement.identity()
                                || capture.generation != requirement.generation()
                                || capture.handle.rep() != requirement.rep
                                || requirement.entry_signature.is_some()
                            {
                                return Err(PreparedRuntimeError::CertifiedTargetOwners);
                            }
                            if requirement.required_evaluated
                                && !self
                                    .machine
                                    .handle_is_evaluated(capture.handle)
                                    .map_err(PreparedRuntimeError::Install)?
                            {
                                return Err(PreparedRuntimeError::CertifiedTargetOwners);
                            }
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
                                let held = self.programs.get(&instance.program()).ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                                facts.native_sources.merge(&held.native_sources);
                            }
                        }
                        ScopedNativeSlot::Retained { value, .. } => {
                            facts.native_sources.merge(&value.provenance().map_err(|_| PreparedRuntimeError::CertifiedTargetOwners)?.source_custody());
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
