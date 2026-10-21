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

pub(in crate::session) struct RetainedNativeRequirement {
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
    Retained(Arc<RetainedNativeRequirement>),
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

/// Package products admit only exact package dependency edges and no retained
/// slots. This proof says nothing about captures in their installing target.
pub(in crate::session) struct ClosedPackageDefinition<'a> {
    binding: &'a PackageCodeBinding,
}

impl ClosedPackageDefinition<'_> {
    pub(in crate::session) fn sites(&self) -> &[tidepool_repr::execution_schema::SiteRow] {
        self.binding.product().code().definitions().sites()
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
    native: Arc<NativeInstantiationPlan>,
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
        let selection = code
            .certification()
            .and_then(|certification| certification.native_selection.as_ref())
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        if has_retained_inputs(selection) {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        Self::restrict(Arc::new(NativeInstantiationPlan::prepare_code(
            code, registry,
        )?))
    }

    /// Restrict this exact prepared plan without rebuilding its imports or images.
    pub(in crate::session) fn restrict(
        native: Arc<NativeInstantiationPlan>,
    ) -> Result<Self, PreparedRuntimeError> {
        if has_retained_inputs(&native.selection) {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        BatchProgram::validate_closed(&native.closed_batch()?)
            .map_err(PreparedRuntimeError::Install)?;
        Ok(Self { native })
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
                                .map(|requirement| {
                                    NativeImportSlot::Retained(Arc::new(requirement))
                                }),
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
                .map(|requirement| NativeImportSlot::Retained(Arc::new(requirement))),
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

    pub(in crate::session) fn matches_code(&self, code: &super::super::turn::TurnCode<'_>) -> bool {
        Arc::ptr_eq(&self.target_definitions, code.prepared_owner())
            && code
                .certification()
                .and_then(|certification| certification.native_selection.as_ref())
                .is_some_and(|selection| Arc::ptr_eq(selection, &self.selection))
    }

    pub(in crate::session) fn has_request_sites(&self) -> bool {
        !self.target_definitions.sites().is_empty()
            || self
                .programs
                .iter()
                .any(|program| !program.native.definitions().sites().is_empty())
    }

    pub(in crate::session) fn target_image(&self) -> &Arc<CompiledProgram> {
        &self.target
    }

    pub(in crate::session) fn images(&self) -> impl Iterator<Item = &Arc<CompiledProgram>> {
        std::iter::once(&self.target)
            .chain(self.programs.iter().map(|program| program.native.image()))
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
                        let NativeCompiledProduct::Package { image, .. } = image else {
                            return Err(PreparedRuntimeError::CertifiedTargetOwners);
                        };
                        if let Some(literal) = image.package_literal(import.value) {
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

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct NativeOccurrence {
    program: usize,
    domain: SourceInstanceDomain,
}

enum ScopedNativeSlot {
    Local {
        occurrence: NativeOccurrence,
        value: ValueId,
    },
    Retained(Arc<RetainedNativeRequirement>),
}

/// One install's selection from the actual binding view. Reusable code does
/// not retain this observation or its existing physical instance leases.
pub(in crate::session) struct NativeInstantiation {
    plan: Arc<NativeInstantiationPlan>,
    selection: NativeDomainSelection,
    anchors: BTreeMap<NativeOccurrence, NativeInstanceLease>,
    programs: BTreeMap<NativeOccurrence, Vec<ScopedNativeSlot>>,
    target_imports: Vec<ScopedNativeSlot>,
}

pub(in crate::session) struct ResolvedNativeCapture {
    pub(in crate::session) id: SessionVarId,
    pub(in crate::session) identity: SymbolIdentity,
    pub(in crate::session) generation: u64,
    pub(in crate::session) handle: PreparedHandle,
    pub(in crate::session) sources: NativeSourceCustody,
}

/// Runtime source evidence selected from the current admitted output. Fresh
/// closed package code belongs to this installing operation; domain selection
/// does not replace the source transaction's actual prerequisite admission.
pub(in crate::session) struct NativeInstallationSources {
    target: NativeSourceCustody,
    programs: Vec<NativeSourceCustody>,
}

impl NativeInstallationSources {
    pub(in crate::session) fn current(
        plan: &NativeInstantiationPlan,
        current: &super::super::turn::CurrentNativeSources<'_>,
    ) -> Self {
        Self {
            target: current.target(),
            programs: plan
                .programs
                .iter()
                .map(|program| match &program.native {
                    NativeCompiledProduct::Home { row, .. } => current.group(row),
                    NativeCompiledProduct::Package { binding, .. } => {
                        current.package(ClosedPackageDefinition { binding })
                    }
                })
                .collect(),
        }
    }

    pub(in crate::session) fn closed(
        plan: &ClosedInstantiationPlan,
        origin: &super::super::resident::CapturedSourceOrigin,
    ) -> Self {
        Self {
            target: origin.for_closed_definition(plan.target_definition()),
            programs: plan
                .native
                .programs
                .iter()
                .map(|program| origin.for_closed_definition(plan.definition(&program.native)))
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
            .iter()
            .flat_map(|program| program.native.image().constructor_declarations())
    }

    pub(in crate::session) fn instantiate(
        self: &Arc<Self>,
        selection: NativeDomainSelection,
    ) -> Result<NativeInstantiation, PreparedRuntimeError> {
        let slots =
            |caller, imports: &[NativeImportSlot]| -> Result<Vec<_>, PreparedRuntimeError> {
                imports
                    .iter()
                    .map(|slot| match slot {
                        NativeImportSlot::Retained(requirement) => {
                            Ok(ScopedNativeSlot::Retained(requirement.clone()))
                        }
                        NativeImportSlot::Local(export) => {
                            let program = self
                                .programs
                                .get(export.program)
                                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                            let domain = match &program.native {
                                NativeCompiledProduct::Home { row, .. } => {
                                    selection.domain_for_owner(caller, row.product().owner())?
                                }
                                NativeCompiledProduct::Package { .. } => caller,
                            };
                            Ok(ScopedNativeSlot::Local {
                                occurrence: NativeOccurrence {
                                    program: export.program,
                                    domain,
                                },
                                value: export.value,
                            })
                        }
                    })
                    .collect()
            };
        let target_imports = slots(selection.current(), &self.target_imports)?;
        let mut pending = target_imports
            .iter()
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Local { occurrence, .. } => Some(*occurrence),
                ScopedNativeSlot::Retained(_) => None,
            })
            .collect::<Vec<_>>();
        let mut visited = BTreeSet::new();
        let mut anchors = BTreeMap::new();
        let mut programs = BTreeMap::new();
        while let Some(occurrence) = pending.pop() {
            if !visited.insert(occurrence) {
                continue;
            }
            let program = &self.programs[occurrence.program];
            let binding = program.native.binding_key();
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
            if let Some(anchor) = anchor {
                anchors.insert(occurrence, anchor.clone());
                continue;
            }
            let imports = slots(occurrence.domain, &program.imports)?;
            pending.extend(imports.iter().filter_map(|slot| match slot {
                ScopedNativeSlot::Local { occurrence, .. } => Some(*occurrence),
                ScopedNativeSlot::Retained(_) => None,
            }));
            programs.insert(occurrence, imports);
        }
        Ok(NativeInstantiation {
            plan: Arc::clone(self),
            selection,
            anchors,
            programs,
            target_imports,
        })
    }
}

impl RetainedNativeRequirement {
    pub(in crate::session) fn identity(&self) -> &SymbolIdentity {
        &self.identity
    }
    pub(in crate::session) fn generation(&self) -> u64 {
        self.generation
    }
}

impl NativeInstantiation {
    pub(in crate::session) fn plan(&self) -> &NativeInstantiationPlan {
        &self.plan
    }

    pub(in crate::session) fn shares_plan(&self, plan: &Arc<NativeInstantiationPlan>) -> bool {
        Arc::ptr_eq(&self.plan, plan)
    }

    pub(in crate::session) fn retained_requirements(
        &self,
    ) -> impl Iterator<Item = &RetainedNativeRequirement> {
        self.programs
            .values()
            .flatten()
            .chain(&self.target_imports)
            .filter_map(|slot| match slot {
                ScopedNativeSlot::Retained(requirement) => Some(requirement.as_ref()),
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
        captures: &BTreeMap<(SymbolIdentity, u64), ResolvedNativeCapture>,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        let plan = &instantiation.plan;
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
                ScopedNativeSlot::Local { occurrence, value } => Some((*occurrence, *value)),
                ScopedNativeSlot::Retained(_) => None,
            })
            .collect::<BTreeSet<_>>();
        let mut late_roots = Vec::<NativeInstanceLease>::new();
        let result = (|| {
            let mut attachments = Vec::new();
            let mut existing = BTreeMap::new();
            let mut requests = Vec::new();
            for (occurrence, value) in requested {
                let program = &plan.programs[occurrence.program];
                let contract = program
                    .native
                    .image()
                    .export_contract(value)
                    .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                if contract.rep == RuntimeRep::Address {
                    continue;
                }
                let request = program.native.request(occurrence.domain, value)?;
                if let Some(anchor) = instantiation.anchors.get(&occurrence) {
                    let held = self
                        .programs
                        .get(&anchor.instance().program())
                        .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                    if !Arc::ptr_eq(&held.definitions, program.native.image().definition_facts()) {
                        return Err(PreparedRuntimeError::CertifiedTargetOwners);
                    }
                    let attachment = if let Some(held) = bindings
                        .retained_native_sibling_attachment(occurrence.domain, anchor, value)
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
                    existing.insert((occurrence, value), attachment.lease().clone());
                    attachments.push(attachment);
                } else {
                    requests.push(BatchLeaseRequest::new(remapped[&occurrence], request));
                }
            }
            let imports = |slots: &[ScopedNativeSlot]| -> Result<Vec<_>, PreparedRuntimeError> {
                slots
                    .iter()
                    .map(|slot| match slot {
                        ScopedNativeSlot::Local { occurrence, value } => {
                            if let Some(lease) = existing.get(&(*occurrence, *value)) {
                                Ok(BatchImport::Existing {
                                    handle: lease.handle(),
                                    entry_signature: lease.entry_signature().cloned(),
                                })
                            } else {
                                Ok(BatchImport::Source {
                                    group: *remapped
                                        .get(occurrence)
                                        .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?,
                                    binding: *value,
                                })
                            }
                        }
                        ScopedNativeSlot::Retained(requirement) => {
                            let capture = captures
                                .get(&(requirement.identity.clone(), requirement.generation))
                                .ok_or_else(|| {
                                    PreparedRuntimeError::MissingRetainedCertifiedOwner {
                                        identity: requirement.identity.clone(),
                                        generation: requirement.generation,
                                    }
                                })?;
                            let entry = bindings
                                .get(capture.id)
                                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                            if entry.value.identity != requirement.identity
                                || entry.module
                                    != super::super::SessionModule::val(tidepool_repr::Generation(
                                        requirement.generation,
                                    ))
                                || entry.value.handle != capture.handle
                                || capture.identity != requirement.identity
                                || capture.generation != requirement.generation
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
                        image: Arc::clone(plan.programs[occurrence.program].native.image()),
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
                        let program = &plan.programs[occurrence.program];
                        let mut facts = ProgramFacts::from_image(program.native.image(), None);
                        facts.native_sources = sources.programs[occurrence.program].clone();
                        facts.native_product = Some(match &program.native {
                            NativeCompiledProduct::Home { row, .. } => NativeProductProof::SelectedHome { _selection: Arc::clone(&plan.selection), _binding: row.binding() },
                            NativeCompiledProduct::Package { binding, .. } => NativeProductProof::Package { _binding: binding.clone() },
                        });
                        (facts, &instantiation.programs[&occurrence])
                    }
                    None => {
                        let mut facts = ProgramFacts::from_image(&plan.target, Some(plan.target_entry));
                        facts.native_sources = sources.target.clone();
                        facts.native_product = Some(NativeProductProof::SelectedTarget { _selection: Arc::clone(&plan.selection) });
                        (facts, &instantiation.target_imports)
                    }
                };
                for import in imports {
                    match import {
                        ScopedNativeSlot::Local { occurrence, value } => {
                            if let Some(lease) = existing.get(&(*occurrence, *value)) {
                                let held = self.programs.get(&lease.instance().program()).ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                                facts.native_sources.merge(&held.native_sources);
                            }
                        }
                        ScopedNativeSlot::Retained(requirement) => {
                            let capture = captures.get(&(requirement.identity.clone(), requirement.generation))
                                .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
                            facts.native_sources.merge(&capture.sources);
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
