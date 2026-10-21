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

enum ClosedProduct {
    Home(Arc<CompletedNativeSourceRow>),
    Package(PackageCodeBinding),
}

impl ClosedProduct {
    fn definitions(&self) -> DefinitionsView<'_> {
        match self {
            Self::Home(row) => row.product().group().definitions(),
            Self::Package(binding) => binding.product().code().definitions(),
        }
    }
}

enum ClosedCompiledProduct {
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

impl ClosedCompiledProduct {
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

struct ClosedProgram {
    native: ClosedCompiledProduct,
    imports: Vec<LocalExportRef>,
}

/// A cheap structural proof from the actual target/certification pair. It
/// retains no images, live handles or source authority.
pub(in crate::session) struct ClosedNativeSelection {
    selection: Arc<CompletedNativeSelection>,
    target: Arc<PreparedProgram>,
}

/// Definition membership lent only by an actual closed selection.
pub(in crate::session) struct ClosedNativeDefinition<'a> {
    definitions: DefinitionsView<'a>,
}

impl ClosedNativeDefinition<'_> {
    pub(in crate::session) fn sites(&self) -> &[tidepool_repr::execution_schema::SiteRow] {
        self.definitions.sites()
    }
}

impl ClosedNativeSelection {
    /// A known capture is distinct from absent or unready native evidence.
    pub(in crate::session) fn from_code(
        code: &super::super::turn::TurnCode<'_>,
    ) -> Result<Option<Self>, PreparedRuntimeError> {
        let selection = code
            .certification()
            .and_then(|certification| certification.native_selection.as_ref())
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        if !selection.completion().retained_requirements().is_empty()
            || selection
                .target_imports()
                .iter()
                .any(|import| matches!(import, CompletedNativeTargetImport::RetainedCapture { .. }))
            || selection.completion().rows().any(|row| {
                row.imports()
                    .iter()
                    .any(|import| matches!(import, CompletedNativeImport::Retained { .. }))
            })
        {
            return Ok(None);
        }
        Ok(Some(Self {
            selection: Arc::clone(selection),
            target: Arc::clone(code.prepared_owner()),
        }))
    }

    fn target(&self) -> ClosedNativeDefinition<'_> {
        ClosedNativeDefinition {
            definitions: self.target.definitions(),
        }
    }

    fn definition<'a>(&'a self, product: &'a ClosedCompiledProduct) -> ClosedNativeDefinition<'a> {
        ClosedNativeDefinition {
            definitions: product.definitions(),
        }
    }
}

/// Immutable code, acquired products and exact ordered local import choices.
/// Construction validates the complete image/import contract before publication.
/// No field can represent an existing runtime handle or a lexical domain.
pub(in crate::session) struct ClosedInstantiationPlan {
    closed: ClosedNativeSelection,
    programs: Vec<ClosedProgram>,
    target: Arc<CompiledProgram>,
    target_entry: ValueId,
    target_imports: Vec<LocalExportRef>,
}

impl ClosedInstantiationPlan {
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
        let Some(closed) = ClosedNativeSelection::from_code(code)? else {
            return Ok(None);
        };
        let selection = &closed.selection;
        let products = selection
            .completion()
            .rows()
            .cloned()
            .map(ClosedProduct::Home)
            .chain(
                selection
                    .completion()
                    .package_bindings()
                    .map(ClosedProduct::Package),
            )
            .collect::<Vec<_>>();
        let mut indices = BTreeMap::new();
        for (index, product) in products.iter().enumerate() {
            let key = match product {
                ClosedProduct::Home(row) => LocalBinding::Home(row.binding()),
                ClosedProduct::Package(binding) => LocalBinding::Package(binding.binding()),
            };
            if indices.insert(key, index).is_some() {
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
            }
        }
        let local = |key, value| {
            indices
                .get(&key)
                .copied()
                .map(|program| LocalExportRef { program, value })
                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)
        };
        let products = products
            .into_iter()
            .map(|product| {
                let imports = match &product {
                    ClosedProduct::Home(row) => row
                        .imports()
                        .iter()
                        .map(|import| match import {
                            CompletedNativeImport::Source { binding, value } => {
                                local(LocalBinding::Home(*binding), *value)
                            }
                            CompletedNativeImport::Package { binding, value } => {
                                local(LocalBinding::Package(binding.binding()), *value)
                            }
                            CompletedNativeImport::Retained { .. } => {
                                Err(PreparedRuntimeError::CertifiedTargetOwners)
                            }
                        })
                        .collect::<Result<Vec<_>, _>>(),
                    ClosedProduct::Package(binding) => binding
                        .import_links()
                        .map(|(binding, value)| {
                            local(LocalBinding::Package(binding.binding()), value)
                        })
                        .collect(),
                }?;
                Ok((product, imports))
            })
            .collect::<Result<Vec<_>, PreparedRuntimeError>>()?;
        let target_imports = selection
            .target_imports()
            .iter()
            .map(|import| match import {
                CompletedNativeTargetImport::Source { binding, value } => {
                    local(LocalBinding::Home(*binding), *value)
                }
                CompletedNativeTargetImport::Package { binding, value } => {
                    local(LocalBinding::Package(binding.binding()), *value)
                }
                CompletedNativeTargetImport::RetainedCapture { .. } => {
                    Err(PreparedRuntimeError::CertifiedTargetOwners)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Literal storage is compiled from its exact leaf product before any
        // consumer. Cyclic executable edges need no compilation ordering.
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
            closed.target.globals(),
            &target_imports,
            &products,
            &literals,
        )?;
        let target = CompiledProgram::prepare_target_code(
            &closed.target,
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
            .map(|(index, (_, imports))| ClosedProgram {
                imports,
                native: programs
                    .remove(&index)
                    .expect("every immutable occurrence compiled"),
            })
            .collect();
        let target_entry = closed.target.entry();
        let plan = Self {
            closed,
            programs,
            target,
            target_entry,
            target_imports,
        };
        BatchProgram::validate_closed(&plan.batch()).map_err(PreparedRuntimeError::Install)?;
        Ok(Some(plan))
    }

    fn compile_product(
        product: &ClosedProduct,
        imports: &[LocalExportRef],
        products: &[(ClosedProduct, Vec<LocalExportRef>)],
        literals: &BTreeMap<usize, ClosedCompiledProduct>,
        registry: &ImageRegistry,
    ) -> Result<ClosedCompiledProduct, PreparedRuntimeError> {
        let (owners, packages, sources) =
            Self::literal_inputs(product.definitions().globals(), imports, products, literals)?;
        match product {
            ClosedProduct::Package(binding) => DemandedPackageImage::prepare(
                binding.product().code().clone(),
                &owners,
                &packages,
                &sources,
                registry,
            )
            .map(|image| ClosedCompiledProduct::Package {
                binding: binding.clone(),
                image: Arc::new(image),
            })
            .map_err(PreparedRuntimeError::Compile),
            ClosedProduct::Home(row) => {
                let pending = row.product();
                let code = tidepool_repr::execution_schema::CertifiedGroupCode::admit(
                    pending.owner().clone(),
                    pending.group().clone(),
                )?;
                DemandedHomeImage::prepare(code, &owners, &packages, &sources, registry)
                    .map(|image| ClosedCompiledProduct::Home {
                        row: Arc::clone(row),
                        image,
                    })
                    .map_err(PreparedRuntimeError::Compile)
            }
        }
    }

    fn literal_inputs(
        globals: &[tidepool_repr::execution_schema::GlobalDecl],
        imports: &[LocalExportRef],
        products: &[(ClosedProduct, Vec<LocalExportRef>)],
        literals: &BTreeMap<usize, ClosedCompiledProduct>,
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
            let (product, _) = products
                .get(import.program)
                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
            match product {
                ClosedProduct::Home(row) => {
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
                ClosedProduct::Package(binding) => {
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

    fn batch(&self) -> Vec<BatchProgram> {
        self.programs
            .iter()
            .map(|program| (program.native.image(), &program.imports))
            .chain(std::iter::once((&self.target, &self.target_imports)))
            .map(|(image, imports)| BatchProgram {
                image: Arc::clone(image),
                imports: imports
                    .iter()
                    .map(|import| BatchImport::Source {
                        group: import.program,
                        binding: import.value,
                    })
                    .collect(),
            })
            .collect()
    }
}

impl ClosedInstantiationPlan {
    pub(in crate::session) fn target_constructors(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::ConstructorDecl> + Clone {
        self.target.constructor_declarations()
    }
    pub(in crate::session) fn dependency_constructors(
        &self,
    ) -> impl Iterator<Item = &tidepool_repr::execution_schema::ConstructorDecl> {
        self.programs
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
            .map(|export| export.program)
            .collect::<Vec<_>>();
        while let Some(index) = pending.pop() {
            if !needed.insert(index) || anchors[index].is_some() {
                continue;
            }
            pending.extend(
                plan.programs[index]
                    .imports
                    .iter()
                    .map(|export| export.program),
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
            .map(|export| (export.program, export.value))
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
            let imports = |imports: &[LocalExportRef]| -> Result<Vec<_>, PreparedRuntimeError> {
                imports
                    .iter()
                    .map(|export| {
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
                            origin.for_closed_definition(plan.closed.definition(&program.native));
                        facts.native_product = Some(match &program.native {
                            ClosedCompiledProduct::Home { row, .. } => {
                                NativeProductProof::ClosedHome {
                                    _selection: Arc::clone(&plan.closed.selection),
                                    _binding: row.binding(),
                                }
                            }
                            ClosedCompiledProduct::Package { binding, .. } => {
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
                        facts.native_sources = origin.for_closed_definition(plan.closed.target());
                        facts.native_product = Some(NativeProductProof::ClosedTarget {
                            _selection: Arc::clone(&plan.closed.selection),
                        });
                        (facts, &plan.target_imports)
                    }
                };
                for import in imports {
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
