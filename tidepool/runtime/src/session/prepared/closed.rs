//! Scope-neutral native installation choices from one completed source output.

use super::*;
use tidepool_codegen::prepared_program::{DemandedHomeImage, DemandedPackageImage, LiteralOwner};
use tidepool_codegen::prepared_program::{
    NativeArtifactBinding, NativeBindingKey, NativeDomainSelection, NativeExportRequest,
    NativeHomeBinding, ScopedNativeExport, SourceInstanceDomain,
};
use tidepool_toolchain::artifact_inventory::{
    ArtifactBinding, CompletedNativeImport, CompletedNativeSelection, CompletedNativeSourceRow,
    CompletedNativeTargetImport, NativeGroupBinding, PackageCodeBinding,
};

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum LocalBinding {
    Home(NativeGroupBinding),
    Package(ArtifactBinding),
}

/// The index belongs only to this plan. It never authorizes a live root.
#[derive(Clone, Copy)]
struct LocalExportRef {
    program: usize,
    value: ValueId,
}

enum NativeProduct {
    Home(Arc<CompletedNativeSourceRow>),
    Package(PackageCodeBinding),
}

impl NativeProduct {
    fn definitions(&self) -> DefinitionsView<'_> {
        match self {
            Self::Home(row) => row.product().group().definitions(),
            Self::Package(binding) => binding.product().code().definitions(),
        }
    }
}

enum NativeCompiledProduct {
    Home {
        row: Arc<CompletedNativeSourceRow>,
        image: DemandedHomeImage,
    },
    Package {
        binding: PackageCodeBinding,
        image: Arc<DemandedPackageImage>,
    },
}

fn home_key(row: &CompletedNativeSourceRow) -> NativeHomeBinding {
    let binding = row.binding();
    NativeHomeBinding {
        issuer: NativeArtifactBinding {
            artifact: binding.binding.artifact().0,
            selection: binding.binding.selection().0,
        },
        original_ordinal: binding.original_ordinal,
    }
}

fn package_key(binding: &PackageCodeBinding) -> NativeArtifactBinding {
    let binding = binding.binding();
    NativeArtifactBinding {
        artifact: binding.artifact.0,
        selection: binding.selection.0,
    }
}

impl NativeCompiledProduct {
    fn binding_key(&self) -> NativeBindingKey {
        match self {
            Self::Home { row, .. } => NativeBindingKey::Home(home_key(row)),
            Self::Package { binding, .. } => NativeBindingKey::Package(package_key(binding)),
        }
    }

    fn definitions(&self) -> DefinitionsView<'_> {
        match self {
            Self::Home { row, .. } => row.product().group().definitions(),
            Self::Package { binding, .. } => binding.product().code().definitions(),
        }
    }

    fn image(&self) -> &Arc<CompiledProgram> {
        match self {
            Self::Home { image, .. } => image.image(),
            Self::Package { image, .. } => image.image(),
        }
    }

    fn request(
        &self,
        domain: SourceInstanceDomain,
        value: ValueId,
    ) -> Result<NativeExportRequest, PreparedRuntimeError> {
        match self {
            Self::Home { row, image } => {
                NativeExportRequest::for_home_code(image, home_key(row), domain, value)
            }
            Self::Package { binding, image } => NativeExportRequest::for_package(
                Arc::clone(image),
                package_key(binding),
                domain,
                value,
            ),
        }
        .map_err(PreparedRuntimeError::Install)
    }
}

/// A retained import is a requirement on an actual scoped value, never a root.
/// Its constructor checks the compiler's ordered declaration before compilation.
enum RetainedNativeOrigin {
    Home(ArtifactBinding),
    TargetCapture,
}

struct RetainedNativeRequirement {
    identity: SymbolIdentity,
    generation: u64,
    origin: RetainedNativeOrigin,
    rep: RuntimeRep,
    entry_signature: Option<Signature>,
    required_evaluated: bool,
}

impl RetainedNativeRequirement {
    fn from_declaration(
        declaration: &tidepool_repr::execution_schema::GlobalDecl,
        signatures: &[Signature],
        identity: &SymbolIdentity,
        generation: u64,
        origin: RetainedNativeOrigin,
    ) -> Result<Self, PreparedRuntimeError> {
        if declaration.identity != *identity || declaration.required_generation != Some(generation)
        {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        Ok(Self {
            identity: identity.clone(),
            generation,
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

enum NativeImportSlot {
    Local(LocalExportRef),
    Retained(RetainedNativeRequirement),
}

struct NativeProgram {
    native: NativeCompiledProduct,
    imports: Vec<NativeImportSlot>,
}

/// Definition membership lent only by a checked capture-free plan.
pub(in crate::session) struct ClosedNativeDefinition<'a> {
    definitions: DefinitionsView<'a>,
}

impl ClosedNativeDefinition<'_> {
    pub(in crate::session) fn sites(&self) -> &[tidepool_repr::execution_schema::SiteRow] {
        self.definitions.sites()
    }
}

/// Immutable code, acquired products and exact ordered import choices.
/// No field can represent an existing runtime handle or a lexical domain.
pub(in crate::session) struct NativeInstantiationPlan {
    selection: Arc<CompletedNativeSelection>,
    target_definitions: Arc<PreparedProgram>,
    programs: Vec<NativeProgram>,
    target: Arc<CompiledProgram>,
    target_entry: ValueId,
    target_imports: Vec<NativeImportSlot>,
}

/// A checked restriction of the same immutable plan to zero retained inputs.
/// Only this view may launch a reusable source in an unrelated fresh domain.
pub(in crate::session) struct ClosedInstantiationPlan {
    native: NativeInstantiationPlan,
}

fn has_retained_inputs(selection: &CompletedNativeSelection) -> bool {
    !selection.completion().retained_requirements().is_empty()
        || selection
            .target_imports()
            .iter()
            .any(|import| matches!(import, CompletedNativeTargetImport::RetainedCapture { .. }))
        || selection.completion().rows().any(|row| {
            row.imports()
                .iter()
                .any(|import| matches!(import, CompletedNativeImport::Retained { .. }))
        })
}

impl ClosedInstantiationPlan {
    pub(in crate::session) fn native(&self) -> &NativeInstantiationPlan {
        &self.native
    }

    pub(super) fn prepare(
        compiled: &super::super::turn::CompiledTurn,
        registry: &ImageRegistry,
    ) -> Result<Self, PreparedRuntimeError> {
        Self::prepare_code(&compiled.code(), registry)?
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)
    }

    pub(in crate::session) fn prepare_code(
        code: &super::super::turn::TurnCode<'_>,
        registry: &ImageRegistry,
    ) -> Result<Option<Self>, PreparedRuntimeError> {
        let selection = code
            .certification()
            .and_then(|certification| certification.native_selection.as_ref())
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        if has_retained_inputs(selection) {
            return Ok(None);
        }
        let native = NativeInstantiationPlan::prepare_code(code, registry)?;
        BatchProgram::validate_closed(&native.closed_batch()?)
            .map_err(PreparedRuntimeError::Install)?;
        Ok(Some(Self { native }))
    }

    fn target_definition(&self) -> ClosedNativeDefinition<'_> {
        ClosedNativeDefinition {
            definitions: self.native.target_definitions.definitions(),
        }
    }

    fn definition<'a>(&'a self, product: &'a NativeCompiledProduct) -> ClosedNativeDefinition<'a> {
        ClosedNativeDefinition {
            definitions: product.definitions(),
        }
    }
}

impl NativeInstantiationPlan {
    pub(in crate::session) fn prepare_code(
        code: &super::super::turn::TurnCode<'_>,
        registry: &ImageRegistry,
    ) -> Result<Self, PreparedRuntimeError> {
        let selection = code
            .certification()
            .and_then(|certification| certification.native_selection.as_ref())
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        let products = selection
            .completion()
            .rows()
            .cloned()
            .map(NativeProduct::Home)
            .chain(
                selection
                    .completion()
                    .package_bindings()
                    .map(NativeProduct::Package),
            )
            .collect::<Vec<_>>();
        let mut indices = BTreeMap::new();
        for (index, product) in products.iter().enumerate() {
            let key = match product {
                NativeProduct::Home(row) => LocalBinding::Home(row.binding()),
                NativeProduct::Package(binding) => LocalBinding::Package(binding.binding()),
            };
            if indices.insert(key, index).is_some() {
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
            }
        }
        let local = |key, value| {
            indices
                .get(&key)
                .copied()
                .map(|program| NativeImportSlot::Local(LocalExportRef { program, value }))
                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)
        };
        let products = products
            .into_iter()
            .map(|product| {
                let imports = match &product {
                    NativeProduct::Home(row) => {
                        if row.imports().len() != product.definitions().globals().len() {
                            return Err(PreparedRuntimeError::CertifiedTargetOwners);
                        }
                        row.imports()
                            .iter()
                            .zip(product.definitions().globals())
                            .map(|(import, declaration)| match import {
                                CompletedNativeImport::Source { binding, value } => {
                                    local(LocalBinding::Home(*binding), *value)
                                }
                                CompletedNativeImport::Package { binding, value } => {
                                    local(LocalBinding::Package(binding.binding()), *value)
                                }
                                CompletedNativeImport::Retained {
                                    binding,
                                    identity,
                                    generation,
                                } => RetainedNativeRequirement::from_declaration(
                                    declaration,
                                    product.definitions().signatures(),
                                    identity,
                                    *generation,
                                    RetainedNativeOrigin::Home(*binding),
                                )
                                .map(NativeImportSlot::Retained),
                            })
                            .collect::<Result<Vec<_>, _>>()
                    }
                    NativeProduct::Package(binding) => binding
                        .import_links()
                        .map(|(binding, value)| {
                            local(LocalBinding::Package(binding.binding()), value)
                        })
                        .collect(),
                }?;
                Ok((product, imports))
            })
            .collect::<Result<Vec<_>, PreparedRuntimeError>>()?;
        if selection.target_imports().len() != code.prepared().globals().len() {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        let target_imports = selection
            .target_imports()
            .iter()
            .zip(code.prepared().globals())
            .map(|(import, declaration)| match import {
                CompletedNativeTargetImport::Source { binding, value } => {
                    local(LocalBinding::Home(*binding), *value)
                }
                CompletedNativeTargetImport::Package { binding, value } => {
                    local(LocalBinding::Package(binding.binding()), *value)
                }
                CompletedNativeTargetImport::RetainedCapture {
                    identity,
                    generation,
                } => RetainedNativeRequirement::from_declaration(
                    declaration,
                    code.prepared().signatures(),
                    identity,
                    *generation,
                    RetainedNativeOrigin::TargetCapture,
                )
                .map(NativeImportSlot::Retained),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut literals = BTreeMap::new();
        for (index, (product, _)) in products.iter().enumerate() {
            let definitions = product.definitions();
            if definitions.globals().is_empty()
                && matches!(definitions.bindings(), [Group::NonRecursive(top)] if matches!(top.binding.rhs, HeapRhs::Bytes(_)))
            {
                literals.insert(
                    index,
                    Self::compile_product(product, &[], &products, &literals, registry)?,
                );
            }
        }
        let mut programs = products
            .iter()
            .enumerate()
            .filter(|(index, _)| !literals.contains_key(index))
            .map(|(index, (product, imports))| {
                Self::compile_product(product, imports, &products, &literals, registry)
                    .map(|image| (index, image))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let (owners, packages, sources) = Self::literal_inputs(
            code.prepared().globals(),
            &target_imports,
            &products,
            &literals,
        )?;
        let target = CompiledProgram::prepare_target_code(
            code.prepared(),
            &owners,
            &packages,
            &sources,
            registry,
        )
        .map_err(PreparedRuntimeError::Compile)?;
        programs.append(&mut literals);
        let programs = products
            .into_iter()
            .enumerate()
            .map(|(index, (_, imports))| NativeProgram {
                imports,
                native: programs
                    .remove(&index)
                    .expect("every immutable occurrence compiled"),
            })
            .collect();
        Ok(Self {
            selection: Arc::clone(selection),
            target_definitions: Arc::clone(code.prepared_owner()),
            programs,
            target,
            target_entry: code.prepared().entry(),
            target_imports,
        })
    }

    fn compile_product(
        product: &NativeProduct,
        imports: &[NativeImportSlot],
        products: &[(NativeProduct, Vec<NativeImportSlot>)],
        literals: &BTreeMap<usize, NativeCompiledProduct>,
        registry: &ImageRegistry,
    ) -> Result<NativeCompiledProduct, PreparedRuntimeError> {
        let (owners, packages, sources) =
            Self::literal_inputs(product.definitions().globals(), imports, products, literals)?;
        match product {
            NativeProduct::Package(binding) => DemandedPackageImage::prepare(
                binding.product().code().clone(),
                &owners,
                &packages,
                &sources,
                registry,
            )
            .map(|image| NativeCompiledProduct::Package {
                binding: binding.clone(),
                image: Arc::new(image),
            })
            .map_err(PreparedRuntimeError::Compile),
            NativeProduct::Home(row) => {
                let pending = row.product();
                let code = tidepool_repr::execution_schema::CertifiedGroupCode::admit(
                    pending.owner().clone(),
                    pending.group().clone(),
                )?;
                DemandedHomeImage::prepare(code, &owners, &packages, &sources, registry)
                    .map(|image| NativeCompiledProduct::Home {
                        row: Arc::clone(row),
                        image,
                    })
                    .map_err(PreparedRuntimeError::Compile)
            }
        }
    }

    fn literal_inputs(
        globals: &[tidepool_repr::execution_schema::GlobalDecl],
        imports: &[NativeImportSlot],
        products: &[(NativeProduct, Vec<NativeImportSlot>)],
        literals: &BTreeMap<usize, NativeCompiledProduct>,
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
            let NativeImportSlot::Local(import) = import else {
                owners.push(None);
                continue;
            };
            let (product, _) = products
                .get(import.program)
                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
            match product {
                NativeProduct::Home(row) => {
                    let source = SourceBinder {
                        version: row.product().owner().module_version.clone(),
                        binder: global.identity.clone(),
                    };
                    owners.push(Some(LiteralOwner::Source {
                        version: source.version.clone(),
                        binder: source.binder.clone(),
                    }));
                    if let Some(image) = literals.get(&import.program) {
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
                NativeProduct::Package(binding) => {
                    let owner = binding.product().code().owner();
                    owners.push(Some(LiteralOwner::Package {
                        unit: owner.unit.clone(),
                        module: owner.module.clone(),
                        binder: global.identity.clone(),
                        interface_digest: owner.interface_sha256,
                    }));
                    if let Some(image) = literals.get(&import.program) {
                        let storage = image.image().package_literals(|unit, module| {
                            (unit == owner.unit && module == owner.module)
                                .then_some(owner.interface_sha256)
                        });
                        if let Some(literal) = storage.get(&global.identity) {
                            if packages
                                .insert(global.identity.clone(), literal.clone())
                                .is_some_and(|old| old != *literal)
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
        self.programs
            .iter()
            .map(|program| (program.native.image(), &program.imports))
            .chain(std::iter::once((&self.target, &self.target_imports)))
            .map(|(image, imports)| {
                Ok(BatchProgram {
                    image: Arc::clone(image),
                    imports: imports
                        .iter()
                        .map(|import| match import {
                            NativeImportSlot::Local(import) => Ok(BatchImport::Source {
                                group: import.program,
                                binding: import.value,
                            }),
                            NativeImportSlot::Retained(_) => {
                                Err(PreparedRuntimeError::CertifiedTargetOwners)
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                })
            })
            .collect()
    }
}

impl ClosedInstantiationPlan {
    pub(in crate::session) fn target_constructors(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::ConstructorDecl> + Clone {
        self.native.target.constructor_declarations()
    }
    pub(in crate::session) fn dependency_constructors(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::ConstructorDecl> {
        self.native
            .programs
            .iter()
            .flat_map(|program| program.native.image().constructor_declarations())
    }
}

impl PreparedEngine {
    pub(in crate::session) fn install_closed_turn(
        &mut self,
        plan: &ClosedInstantiationPlan,
        origin: &super::super::resident::CapturedSourceOrigin,
        selection: &NativeDomainSelection,
        bindings: &BindingTable,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        let closed = plan;
        let plan = closed.native();
        let domain = selection.current();
        let anchors = plan
            .programs
            .iter()
            .map(|program| {
                let binding = program.native.binding_key();
                selection
                    .roots()
                    .range(
                        ScopedNativeExport {
                            domain,
                            binding,
                            value: ValueId(0),
                        }..,
                    )
                    .next()
                    .and_then(|(key, lease)| {
                        (key.domain == domain && key.binding == binding).then_some(lease)
                    })
            })
            .collect::<Vec<_>>();
        // An already installed group retains its exact environment. Only walk
        // beyond newly instantiated groups; incoming edges do not mint instances.
        let mut needed = BTreeSet::new();
        let mut pending = plan
            .target_imports
            .iter()
            .map(|export| match export {
                NativeImportSlot::Local(export) => export.program,
                NativeImportSlot::Retained(_) => unreachable!("closed restriction"),
            })
            .collect::<Vec<_>>();
        while let Some(index) = pending.pop() {
            if !needed.insert(index) || anchors[index].is_some() {
                continue;
            }
            pending.extend(
                plan.programs[index]
                    .imports
                    .iter()
                    .map(|export| match export {
                        NativeImportSlot::Local(export) => export.program,
                        NativeImportSlot::Retained(_) => unreachable!("closed restriction"),
                    }),
            );
        }
        let fresh = needed
            .iter()
            .copied()
            .filter(|index| anchors[*index].is_none())
            .collect::<Vec<_>>();
        let remapped = fresh
            .iter()
            .enumerate()
            .map(|(new, old)| (*old, new))
            .collect::<BTreeMap<_, _>>();
        let requested = fresh
            .iter()
            .flat_map(|index| &plan.programs[*index].imports)
            .chain(&plan.target_imports)
            .map(|export| match export {
                NativeImportSlot::Local(export) => (export.program, export.value),
                NativeImportSlot::Retained(_) => unreachable!("closed restriction"),
            })
            .collect::<BTreeSet<_>>();
        let mut late_roots = Vec::<NativeInstanceLease>::new();
        let result = (|| {
            let mut attachments = Vec::new();
            let mut existing = BTreeMap::new();
            let mut requests = Vec::new();
            for (index, value) in requested {
                let program = &plan.programs[index];
                let contract = program
                    .native
                    .image()
                    .export_contract(value)
                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                // Address constants are immutable literal storage, never managed roots.
                if contract.rep == RuntimeRep::Address {
                    continue;
                }
                let request = program.native.request(domain, value)?;
                if let Some(anchor) = anchors[index] {
                    let held = self
                        .programs
                        .get(&anchor.instance().program())
                        .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                    if !Arc::ptr_eq(&held.definitions, program.native.image().definition_facts()) {
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
                    }
                    // The held instance, not an immutable image or source token,
                    // determines the reused physical environment and its origins.
                    let attachment = if let Some(held) =
                        bindings.retained_native_sibling_attachment(domain, anchor, value)
                    {
                        held
                    } else {
                        let issued = self
                            .machine
                            .retain_native_sibling(anchor, request)
                            .map_err(PreparedRuntimeError::Install)?;
                        late_roots.push(issued.lease().clone());
                        issued
                    };
                    existing.insert((index, value), attachment.lease().clone());
                    attachments.push(attachment);
                } else {
                    requests.push(BatchLeaseRequest::new(remapped[&index], request));
                }
            }
            let imports = |imports: &[NativeImportSlot]| -> Result<Vec<_>, PreparedRuntimeError> {
                imports
                    .iter()
                    .map(|export| {
                        let NativeImportSlot::Local(export) = export else {
                            return Err(PreparedRuntimeError::CertifiedTargetOwners);
                        };
                        if let Some(lease) = existing.get(&(export.program, export.value)) {
                            Ok(BatchImport::Existing {
                                handle: lease.handle(),
                                entry_signature: lease.entry_signature().cloned(),
                            })
                        } else {
                            Ok(BatchImport::Source {
                                group: *remapped
                                    .get(&export.program)
                                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?,
                                binding: export.value,
                            })
                        }
                    })
                    .collect()
            };
            let mut batch = fresh
                .iter()
                .map(|index| {
                    let program = &plan.programs[*index];
                    Ok(BatchProgram {
                        image: Arc::clone(program.native.image()),
                        imports: imports(&program.imports)?,
                    })
                })
                .collect::<Result<Vec<_>, PreparedRuntimeError>>()?;
            batch.push(BatchProgram {
                image: Arc::clone(&plan.target),
                imports: imports(&plan.target_imports)?,
            });
            let make_facts = |index: Option<usize>| -> Result<ProgramFacts, PreparedRuntimeError> {
                let (mut facts, imports) = match index {
                    Some(index) => {
                        let program = &plan.programs[index];
                        let mut facts = ProgramFacts::from_image(program.native.image(), None);
                        facts.native_sources =
                            origin.for_closed_definition(closed.definition(&program.native));
                        facts.native_product = Some(match &program.native {
                            NativeCompiledProduct::Home { row, .. } => {
                                NativeProductProof::ClosedHome {
                                    _selection: Arc::clone(&plan.selection),
                                    _binding: row.binding(),
                                }
                            }
                            NativeCompiledProduct::Package { binding, .. } => {
                                NativeProductProof::Package {
                                    _binding: binding.clone(),
                                }
                            }
                        });
                        (facts, &program.imports)
                    }
                    None => {
                        let mut facts =
                            ProgramFacts::from_image(&plan.target, Some(plan.target_entry));
                        facts.native_sources =
                            origin.for_closed_definition(closed.target_definition());
                        facts.native_product = Some(NativeProductProof::ClosedTarget {
                            _selection: Arc::clone(&plan.selection),
                        });
                        (facts, &plan.target_imports)
                    }
                };
                for import in imports {
                    let NativeImportSlot::Local(import) = import else {
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
                    };
                    if let Some(lease) = existing.get(&(import.program, import.value)) {
                        let held = self
                            .programs
                            .get(&lease.instance().program())
                            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                        facts.native_sources.merge(&held.native_sources);
                    }
                }
                Ok(facts)
            };
            let mut facts = fresh
                .iter()
                .map(|index| make_facts(Some(*index)))
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
                    .expect("batch returned an installed program");
            }
            let target = *installed.programs.last().expect("closed target is final");
            attachments.extend(installed.source_attachments);
            Ok(CertifiedTurnInstall {
                native_sources: admitted
                    .last()
                    .expect("target facts are final")
                    .facts
                    .native_sources
                    .clone(),
                target,
                groups: installed.programs[..installed.programs.len() - 1].to_vec(),
                leases: installed.leases,
                domain_leases: attachments,
                admitted,
                package_updates: BTreeMap::new(),
                exports: BTreeMap::new(),
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
