//! Scope-neutral native installation choices from one completed source output.

use super::*;
use tidepool_codegen::prepared_program::{DemandedPackageImage, LiteralOwner};
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

enum ClosedImage {
    Home(DemandedImage),
    Package(Arc<DemandedPackageImage>),
}

impl ClosedImage {
    fn image(&self) -> &Arc<CompiledProgram> {
        match self {
            Self::Home(image) => image.image(),
            Self::Package(image) => image.image(),
        }
    }
}

struct ClosedProgram {
    product: ClosedProduct,
    native: ClosedImage,
    imports: Vec<LocalExportRef>,
}

/// Immutable code, acquired products and exact ordered local import choices.
/// No field can represent an existing runtime handle or a lexical domain.
pub(in crate::session) struct ClosedInstantiationPlan {
    selection: Arc<CompletedNativeSelection>,
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
        let certification = compiled
            .certification
            .as_ref()
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        let selection = certification
            .native_selection
            .as_ref()
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?
            .clone();
        if !selection.completion().retained_requirements().is_empty() {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        let products: Vec<_> = selection
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
            .collect();
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
        let imports = products
            .iter()
            .map(|product| match product {
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
                    .map(|(binding, value)| local(LocalBinding::Package(binding.binding()), value))
                    .collect(),
            })
            .collect::<Result<Vec<_>, _>>()?;
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
        for (index, product) in products.iter().enumerate() {
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
            .zip(&imports)
            .enumerate()
            .map(|(index, (product, imports))| {
                Self::compile_product(product, imports, &products, &literals, registry)
                    .map(|image| (index, image))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let (owners, packages, sources) = Self::literal_inputs(
            compiled.prepared().globals(),
            &target_imports,
            &products,
            &literals,
        )?;
        let target = CompiledProgram::prepare_target_code(
            compiled.prepared(),
            &owners,
            &packages,
            &sources,
            registry,
        )
        .map_err(PreparedRuntimeError::Compile)?;
        let programs = products
            .into_iter()
            .zip(imports)
            .enumerate()
            .map(|(index, (product, imports))| ClosedProgram {
                product,
                imports,
                native: programs
                    .remove(&index)
                    .expect("every immutable occurrence compiled"),
            })
            .collect();
        let plan = Self {
            selection,
            programs,
            target,
            target_entry: compiled.prepared().entry(),
            target_imports,
        };
        BatchProgram::validate_closed(&plan.batch()).map_err(PreparedRuntimeError::Install)?;
        Ok(plan)
    }

    fn compile_product(
        product: &ClosedProduct,
        imports: &[LocalExportRef],
        products: &[ClosedProduct],
        literals: &BTreeMap<usize, ClosedImage>,
        registry: &ImageRegistry,
    ) -> Result<ClosedImage, PreparedRuntimeError> {
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
            .map(|image| ClosedImage::Package(Arc::new(image)))
            .map_err(PreparedRuntimeError::Compile),
            ClosedProduct::Home(row) => {
                let pending = row.product();
                let imports = owners
                    .iter()
                    .map(|owner| match owner {
                        Some(LiteralOwner::Source { version, binder }) => Ok(ImportOwner::Source {
                            version: version.clone(),
                            binder: binder.clone(),
                        }),
                        Some(LiteralOwner::Package {
                            unit,
                            module,
                            binder,
                            interface_digest,
                        }) => Ok(ImportOwner::Package {
                            unit: unit.clone(),
                            module: module.clone(),
                            binder: binder.clone(),
                            interface_digest: *interface_digest,
                        }),
                        None => Err(PreparedRuntimeError::CertifiedTargetOwners),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let group = CertifiedGroup::admit(
                    pending.owner().clone(),
                    pending.group().clone(),
                    imports,
                )?;
                DemandedImage::compile_source_with_literals(group, registry, &packages, &sources)
                    .map(ClosedImage::Home)
                    .map_err(Into::into)
            }
        }
    }

    fn literal_inputs(
        globals: &[tidepool_repr::execution_schema::GlobalDecl],
        imports: &[LocalExportRef],
        products: &[ClosedProduct],
        literals: &BTreeMap<usize, ClosedImage>,
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
            let product = products
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
        domain: tidepool_codegen::prepared_program::SourceInstanceDomain,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        use tidepool_codegen::prepared_program::{NativeArtifactBinding, NativeHomeBinding};
        let batch = plan.batch();
        BatchProgram::validate_closed(&batch).map_err(PreparedRuntimeError::Install)?;
        let mut facts = plan
            .programs
            .iter()
            .map(|program| {
                let mut facts = ProgramFacts::from_image(program.native.image(), None);
                facts.native_sources =
                    origin.for_closed_entry_sites(program.product.definitions().sites());
                facts.native_product = Some(match &program.product {
                    ClosedProduct::Home(row) => NativeProductProof::ClosedHome {
                        _selection: Arc::clone(&plan.selection),
                        _binding: row.binding(),
                    },
                    ClosedProduct::Package(binding) => NativeProductProof::Package {
                        _binding: binding.clone(),
                    },
                });
                facts
            })
            .collect::<Vec<_>>();
        let mut target = ProgramFacts::from_image(&plan.target, Some(plan.target_entry));
        target.native_sources =
            origin.for_closed_entry_sites(&plan.target.definition_facts().sites);
        target.native_product = Some(NativeProductProof::ClosedTarget {
            _selection: Arc::clone(&plan.selection),
        });
        facts.push(target);
        let direct = facts
            .iter()
            .map(|facts| facts.native_sources.clone())
            .collect::<Vec<_>>();
        let mut admitted = self.plan_batch_evidence(facts)?;
        let requested: BTreeSet<_> = plan
            .programs
            .iter()
            .flat_map(|program| &program.imports)
            .chain(&plan.target_imports)
            .map(|export| (export.program, export.value))
            .collect();
        let mut requests = Vec::new();
        for (index, value) in requested {
            let program = &plan.programs[index];
            let contract = program
                .native
                .image()
                .export_contract(value)
                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
            if contract.rep == RuntimeRep::Address {
                continue;
            }
            requests.push(
                match (&program.product, &program.native) {
                    (ClosedProduct::Home(row), ClosedImage::Home(image)) => {
                        let binding = row.binding();
                        BatchLeaseRequest::for_home(
                            index,
                            image,
                            NativeHomeBinding {
                                issuer: NativeArtifactBinding {
                                    artifact: binding.binding.artifact().0,
                                    selection: binding.binding.selection().0,
                                },
                                original_ordinal: binding.original_ordinal,
                            },
                            domain,
                            value,
                        )
                    }
                    (ClosedProduct::Package(binding), ClosedImage::Package(image)) => {
                        let binding = binding.binding();
                        BatchLeaseRequest::for_package(
                            index,
                            Arc::clone(image),
                            NativeArtifactBinding {
                                artifact: binding.artifact.0,
                                selection: binding.selection.0,
                            },
                            domain,
                            value,
                        )
                    }
                    _ => return Err(PreparedRuntimeError::CertifiedTargetOwners),
                }
                .map_err(PreparedRuntimeError::Install)?,
            );
        }
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
        Ok(CertifiedTurnInstall {
            native_sources: admitted
                .last()
                .expect("closed target facts are final")
                .facts
                .native_sources
                .clone(),
            target,
            groups: installed.programs[..installed.programs.len() - 1].to_vec(),
            leases: installed.leases,
            domain_leases: installed.source_attachments,
            admitted,
            package_updates: BTreeMap::new(),
            exports: BTreeMap::new(),
        })
    }
}
