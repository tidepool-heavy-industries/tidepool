//! Compiler validation of an exact declaration join. The public version is
//! opaque: the resident session compares it on accepted and rejected outcomes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tidepool_extract_cmd::ExtractCmd;
use tidepool_repr::{SessionModule, SessionModuleKind};

use crate::{
    cache::ProductAvailability, recovery_artifacts::CertifiedRecoveryProduct, CompileError,
};

mod planned;
pub(crate) use planned::certify_same_offer_planned_declaration;

pub use crate::declaration_context::{
    ExactCompileContext, ExactDeclarationContext, ExactSourceWitness,
    MaterializedExactDeclarationContext, RecoveredArtifactAcquisition, RecoveredArtifactInventory,
    RecoveryInventoryError, RequestAnnotations, RequestHelperRecipe,
};

mod recovery;
pub use recovery::{
    certify_recovered_declaration_tip, certify_recovered_declaration_tip_in_context,
    certify_recovered_declaration_tip_with_value_interfaces, RecoveredDeclarationTip,
    RecoveryDeclarationOrigin, RecoveryDeclarationSelection,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModuleSnapshot {
    pub module: String,
    pub path: PathBuf,
    pub sha256: String,
}

impl ModuleSnapshot {
    pub fn capture(module: String, path: PathBuf) -> Result<Self, CompileError> {
        if !path.is_absolute() {
            return Err(contract("declaration module path must be absolute"));
        }
        let sha256 = sha256(&std::fs::read(&path)?);
        Ok(Self {
            module,
            path,
            sha256,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ExportNamespace {
    Value,
    Type,
    Constructor,
    Field,
}

impl ExportNamespace {
    fn wire(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Type => "type",
            Self::Constructor => "constructor",
            Self::Field => "field",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ExportIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: ExportNamespace,
    pub occurrence: String,
    pub record_parent: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationKind {
    Value,
    Type,
    Class,
}

impl DeclarationKind {
    fn wire(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Type => "type",
            Self::Class => "class",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationExport {
    pub kind: DeclarationKind,
    pub head: ExportIdentity,
    pub children: Vec<ExportIdentity>,
}

/// A write retains the original module even when it introduced only instances.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationWrite {
    pub generation: u64,
    pub module: ModuleSnapshot,
    pub exports: Vec<DeclarationExport>,
    pub retractions: Vec<ExportIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct InstanceInventory {
    pub classes: Vec<ClassInstanceEvidence>,
    pub families: Vec<ExportIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ClassInstanceEvidence {
    pub dfun: ExportIdentity,
    pub class: ExportIdentity,
    pub selected_axioms: Vec<ExportIdentity>,
}

fn validate_instance_inventory(inventory: &InstanceInventory) -> Result<(), CompileError> {
    use std::collections::BTreeSet;
    let families = inventory.families.iter().collect::<BTreeSet<_>>();
    if families.len() != inventory.families.len() {
        return Err(contract("duplicate family axiom in declaration inventory"));
    }
    let mut dfuns = BTreeSet::new();
    for record in &inventory.classes {
        if record.dfun.namespace != ExportNamespace::Value
            || record.class.namespace != ExportNamespace::Type
            || !dfuns.insert(&record.dfun)
            || record.selected_axioms.iter().collect::<BTreeSet<_>>().len()
                != record.selected_axioms.len()
            || record
                .selected_axioms
                .iter()
                .any(|axiom| !families.contains(axiom))
        {
            return Err(contract("invalid typed class-instance inventory"));
        }
    }
    Ok(())
}

/// The original declaration module from the same exact-source validation
/// workflow. No worker or inventory scratch path is retained by this value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedAuthoredDeclaration {
    product: CertifiedRecoveryProduct,
    compiler_support: crate::artifact_inventory::CompilerSourceSupport,
    lexical_exports: Vec<DeclarationExport>,
    introduced_exports: Vec<DeclarationExport>,
    instances: InstanceInventory,
    family_closure: Vec<ExportIdentity>,
    source_sha256: [u8; 32],
    /// SHA-256 of the bound compiler producer identity used for these bytes.
    toolchain_identity_sha256: [u8; 32],
    original_imports: Vec<ExactInterfaceOwner>,
    /// Reachable inherited lexical rows used by this exact source admission.
    source_lexical_imports: Vec<ExactLexicalNode>,
}

/// Reserved declaration source checked by its actual authored admission owner.
/// Generic source compilation cannot infer this origin from module spelling.
pub(crate) struct NativeAuthoredDeclarationAdmission {
    owner: ExactModuleIdentity,
    generation: u64,
    source_path: PathBuf,
    source_sha256: [u8; 32],
}

impl NativeAuthoredDeclarationAdmission {
    pub(crate) fn from_planned(
        module: &SessionModule,
        source: &crate::declaration_context::ExactSourceAdmission,
    ) -> Result<Self, CompileError> {
        let owner = ExactModuleIdentity {
            unit: "main".into(),
            module: module.module_name(),
        };
        let mut matching = source
            .evidence()
            .modules
            .iter()
            .filter(|row| row.unit == owner.unit && row.module == owner.module && !row.boot);
        let selected = matching
            .next()
            .ok_or_else(|| contract("planned authored origin has no reserved source"))?;
        if module.kind != SessionModuleKind::Lib
            || module.gen.0 == 0
            || matching.next().is_some()
            || !selected.is_generated_source()
            || selected.product != ProductAvailability::Ready
        {
            return Err(contract(
                "planned authored origin differs from its reserved source",
            ));
        }
        Ok(Self {
            owner,
            generation: module.gen.0,
            source_path: source.witness().source_path().to_owned(),
            source_sha256: *source.witness().source_sha256(),
        })
    }

    pub(crate) fn owner(&self) -> &ExactModuleIdentity {
        &self.owner
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn source_path(&self) -> &Path {
        &self.source_path
    }
    pub(crate) fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }
}

/// Shared source selection derived from one immutable original certificate.
/// Session implementations remain dependencies, without selecting their names.
pub struct OriginalSourceLexicalSurface {
    pub roots: Vec<ExactModuleIdentity>,
    pub lexical: Vec<ExactLexicalNode>,
}

fn original_source_lexical_surface(
    original: &ExactModuleIdentity,
    imports: &std::collections::BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    inherited: &[ExactLexicalNode],
    implementations: &std::collections::BTreeMap<
        ExactModuleIdentity,
        crate::artifact_inventory::ArtifactKind,
    >,
) -> Result<OriginalSourceLexicalSurface, CompileError> {
    let roots = imports.get(original).ok_or_else(|| {
        contract("authored declaration lacks exact original source import evidence")
    })?;
    source_lexical_surface(roots, imports, inherited, implementations)
}

pub(crate) fn source_lexical_surface(
    roots: &[ExactModuleIdentity],
    imports: &std::collections::BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    inherited: &[ExactLexicalNode],
    implementations: &std::collections::BTreeMap<
        ExactModuleIdentity,
        crate::artifact_inventory::ArtifactKind,
    >,
) -> Result<OriginalSourceLexicalSurface, CompileError> {
    source_lexical_surface_inner(roots, imports, inherited, implementations, false)
}

/// Resolve only the lexical rows reachable from these exact source roots.
/// Unlike `source_lexical_surface`, this omits unrelated inherited rows so a
/// certificate can persist the authority it used without promoting the whole
/// request baseline.
pub(crate) fn source_lexical_closure(
    roots: &[ExactModuleIdentity],
    imports: &std::collections::BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    inherited: &[ExactLexicalNode],
    implementations: &std::collections::BTreeMap<
        ExactModuleIdentity,
        crate::artifact_inventory::ArtifactKind,
    >,
) -> Result<OriginalSourceLexicalSurface, CompileError> {
    source_lexical_surface_inner(roots, imports, inherited, implementations, true)
}

fn source_lexical_surface_inner(
    roots: &[ExactModuleIdentity],
    imports: &std::collections::BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    inherited: &[ExactLexicalNode],
    implementations: &std::collections::BTreeMap<
        ExactModuleIdentity,
        crate::artifact_inventory::ArtifactKind,
    >,
    reachable_only: bool,
) -> Result<OriginalSourceLexicalSurface, CompileError> {
    use std::collections::{BTreeMap, BTreeSet};
    let shared_edges = |edges: &[ExactModuleIdentity]| {
        edges
            .iter()
            .filter_map(|owner| {
                if !SessionModule::is_reserved_name(&owner.module) {
                    // Compiler-issued lexical projections are implementation
                    // interfaces even when their owner is content-addressed.
                    // Their retained artifacts do not grant historical source imports.
                    return (implementations.get(owner)
                        != Some(&crate::artifact_inventory::ArtifactKind::LexicalJoin))
                    .then(|| Ok(owner.clone()));
                }
                let module = SessionModule::from_module_name(&owner.module).filter(|module| {
                    matches!(
                        (implementations.get(owner), module.kind),
                        (
                            Some(crate::artifact_inventory::ArtifactKind::ValueInterface),
                            SessionModuleKind::Val
                        ) | (
                            Some(
                                crate::artifact_inventory::ArtifactKind::OriginalModule
                                    | crate::artifact_inventory::ArtifactKind::LexicalJoin
                            ),
                            SessionModuleKind::Lib
                        )
                    )
                });
                if owner.unit == "main"
                    && module.is_some_and(|module| module.module_name() == owner.module)
                {
                    None
                } else {
                    Some(Err(contract(
                        "source import lacks its authenticated session implementation",
                    )))
                }
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let roots = shared_edges(roots)?;
    let mut lexical = BTreeMap::new();
    for node in inherited {
        if implementations.get(&node.owner)
            == Some(&crate::artifact_inventory::ArtifactKind::LexicalJoin)
        {
            // Validate reserved session-owner spelling/role as usual, then
            // retain this interface only as an implementation dependency.
            shared_edges(std::slice::from_ref(&node.owner))?;
            continue;
        }
        if node.owner.module.starts_with("Tidepool.Session.")
            || lexical
                .insert(node.owner.clone(), shared_edges(&node.imports)?)
                .is_some()
        {
            return Err(contract(
                "shared source baseline has an invalid lexical owner",
            ));
        }
    }
    let mut pending = roots.clone();
    let mut visited = BTreeSet::new();
    while let Some(owner) = pending.pop() {
        if !visited.insert(owner.clone()) {
            continue;
        }
        let edges = imports
            .get(&owner)
            .or_else(|| lexical.get(&owner))
            .ok_or_else(|| {
                contract(format!(
                    "admitted surface root {}:{} lacks exact original source import evidence",
                    owner.unit, owner.module
                ))
            })?;
        let edges = shared_edges(edges)?;
        if lexical
            .insert(owner, edges.clone())
            .is_some_and(|prior| prior != edges)
        {
            return Err(contract(
                "admitted surface owner has conflicting original import edges",
            ));
        }
        pending.extend(edges);
    }
    let lexical = if reachable_only {
        lexical
            .into_iter()
            .filter(|(owner, _)| visited.contains(owner))
            .collect()
    } else {
        lexical
    };
    Ok(OriginalSourceLexicalSurface {
        roots,
        lexical: lexical
            .into_iter()
            .map(|(owner, imports)| ExactLexicalNode { owner, imports })
            .collect(),
    })
}

impl CertifiedAuthoredDeclaration {
    pub(crate) fn compiler_source_support(
        &self,
    ) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.compiler_support
    }

    /// Preserve strict shared-source traversal while excluding only certified
    /// session implementation anchors from lexical selection.
    pub fn shared_source_lexical_surface(
        &self,
        inherited: &[ExactLexicalNode],
    ) -> Result<OriginalSourceLexicalSurface, CompileError> {
        let original = ExactModuleIdentity {
            unit: self.product.owner().unit.clone(),
            module: self.product.owner().module.clone(),
        };
        let imports = self
            .original_home_imports()
            .map(|(owner, imports)| (owner.clone(), imports.to_vec()))
            .collect();
        let implementations = self
            .compiler_support
            .artifacts()
            .source_implementation_roles();
        let mut inherited_by_owner = std::collections::BTreeMap::new();
        for node in inherited.iter().chain(&self.source_lexical_imports) {
            if inherited_by_owner
                .insert(node.owner.clone(), node.imports.clone())
                .is_some_and(|previous| previous != node.imports)
            {
                return Err(contract(
                    "inherited source lexical authority has conflicting import edges",
                ));
            }
        }
        let inherited = inherited_by_owner
            .into_iter()
            .map(|(owner, imports)| ExactLexicalNode { owner, imports })
            .collect::<Vec<_>>();
        original_source_lexical_surface(&original, &imports, &inherited, &implementations)
    }

    pub(crate) fn source_lexical_imports(&self) -> &[ExactLexicalNode] {
        &self.source_lexical_imports
    }
    pub fn product(&self) -> &CertifiedRecoveryProduct {
        &self.product
    }
    pub(crate) fn compiler_input_projection(
        &self,
    ) -> &crate::artifact_inventory::CompilerInputProjection {
        &self.compiler_support.projection()
    }
    pub fn recovery_products(&self) -> Vec<CertifiedRecoveryProduct> {
        self.compiler_support
            .artifacts()
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                crate::artifact_inventory::ArtifactPayload::Original(product) => {
                    Some(product.clone())
                }
                _ => None,
            })
            .collect()
    }
    pub fn artifact_view(&self) -> &crate::artifact_inventory::ArtifactView {
        &self.compiler_support.artifacts()
    }
    pub fn lexical_exports(&self) -> &[DeclarationExport] {
        &self.lexical_exports
    }
    pub fn introduced_exports(&self) -> &[DeclarationExport] {
        &self.introduced_exports
    }
    pub fn instances(&self) -> &InstanceInventory {
        &self.instances
    }
    pub fn family_closure(&self) -> &[ExportIdentity] {
        &self.family_closure
    }
    pub fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }
    pub fn toolchain_identity_sha256(&self) -> [u8; 32] {
        self.toolchain_identity_sha256
    }

    /// Direct resolved home imports of the original compiler-owned modules.
    /// This inventory describes authored imports; the caller selects which
    /// nodes and edges belong to its virtual lexical graph.
    pub fn original_home_imports(
        &self,
    ) -> impl ExactSizeIterator<Item = (&ExactModuleIdentity, &[ExactModuleIdentity])> {
        self.original_imports
            .iter()
            .map(|entry| (&entry.owner, entry.requirements.as_slice()))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ExactModuleIdentity {
    pub unit: String,
    pub module: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactLexicalNode {
    pub owner: ExactModuleIdentity,
    pub imports: Vec<ExactModuleIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExactInterfaceOwner {
    pub(crate) owner: ExactModuleIdentity,
    pub(crate) requirements: Vec<ExactModuleIdentity>,
}

/// Certify the reserved G module after strict GHC inspection of `exact_source`.
/// A private wrapper forces even an instance-only module into the compiler's
/// product graph. Both transactions use the same ordered include stack; the
/// certified source digest fences the extra transaction to the checked bytes.
pub fn certify_authored_declaration(
    module: SessionModule,
    source_path: &Path,
    exact_source: &str,
    includes: &[PathBuf],
    session_root: &Path,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    certify_authored_declaration_inner(
        module,
        source_path,
        exact_source,
        includes,
        session_root,
        None,
        settlement,
    )
}

/// Certify a new original declaration against the selected exact lexical
/// graph. The certificate retains every required original product and Join
/// interface; source lookup cannot replace an admitted immutable owner.
pub fn certify_authored_declaration_in_context(
    module: SessionModule,
    source_path: &Path,
    exact_source: &str,
    includes: &[PathBuf],
    session_root: &Path,
    context: Arc<ExactDeclarationContext>,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    certify_authored_declaration_inner(
        module,
        source_path,
        exact_source,
        includes,
        session_root,
        Some(context),
        settlement,
    )
}

fn certify_authored_declaration_inner(
    module: SessionModule,
    source_path: &Path,
    exact_source: &str,
    includes: &[PathBuf],
    session_root: &Path,
    context: Option<Arc<ExactDeclarationContext>>,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CertifiedAuthoredDeclaration, CompileError> {
    if module.kind != SessionModuleKind::Lib
        || module.gen.0 == 0
        || includes.is_empty()
        || !source_path.is_absolute()
        || std::fs::canonicalize(source_path)?
            != std::fs::canonicalize(includes[0].join(module.relative_hs_path()))?
    {
        return Err(contract("authored source does not match reserved module"));
    }
    let source_sha256: [u8; 32] = Sha256::digest(exact_source.as_bytes()).into();
    if std::fs::read(source_path)? != exact_source.as_bytes() {
        return Err(contract("authored source changed before certification"));
    }
    let module_name = module.module_name();
    let authored = NativeAuthoredDeclarationAdmission {
        owner: ExactModuleIdentity {
            unit: "main".into(),
            module: module_name.clone(),
        },
        generation: module.gen.0,
        source_path: source_path.to_owned(),
        source_sha256,
    };
    let probe = format!(
        "module {} where\nimport {module_name} ()\nauthoredProductProbe = (0 :: Int)\n",
        crate::artifacts::AUTHORED_PRODUCT_PROBE_MODULE,
    );
    let compiled = crate::artifacts::compile_authored_products(
        &probe,
        "authoredProductProbe",
        includes,
        session_root,
        context.clone(),
        &authored,
        settlement,
    )?;
    if std::fs::read(source_path)? != exact_source.as_bytes() {
        return Err(contract("authored source changed during certification"));
    }
    let producer_identity = compiled
        .producer_identity
        .ok_or_else(|| contract("authored certification has no bound compiler identity"))?;
    let toolchain_identity_sha256 =
        crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
            &producer_identity,
        )
        .sha256();
    let evidence = compiled
        .module_inventory
        .ok_or_else(|| contract("authored certification has no final dependency graph"))?;
    let source_canonical = std::fs::canonicalize(source_path)?;
    let candidates = evidence
        .iter()
        .filter(|row| row.module == module_name)
        .collect::<Vec<_>>();
    if candidates.len() != 1
        || candidates[0].boot
        || candidates[0].product != ProductAvailability::Ready
        || std::fs::canonicalize(&candidates[0].source)? != source_canonical
    {
        return Err(contract(
            "authored module is absent or ambiguous in final graph",
        ));
    }
    // The generated entry probe is not an authored implementation. Keeping
    // its owner would make successive declarations retain different probes
    // under one module identity. The shared closure admission below refuses
    // any unexpected retained reference to that transient owner.
    let originals = compiled
        .source_selection
        .as_ref()
        .ok_or_else(|| contract("authored certification has no issued original selection"))?
        .selected_authored_original_closure(
            &compiled.artifact_view,
            &candidates[0].unit,
            &module_name,
        )
        .map_err(|error| contract(&format!("authored original closure: {error}")))?;
    let products = originals.products();
    let mut matches = products.iter().filter(|product| {
        product.owner().unit == candidates[0].unit && product.owner().module == module_name
    });
    let (Some(selected), None) = (matches.next(), matches.next()) else {
        return Err(contract("authored module has no unique certified product"));
    };
    if selected.source_sha256() != Some(source_sha256) {
        return Err(contract("certified authored source digest differs"));
    }
    if selected
        .module_interface()
        .map(|interface| interface.origin())
        != Some(
            crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration {
                generation: module.gen.0,
            },
        )
    {
        return Err(contract("certified authored declaration origin differs"));
    }

    let selected_owner = ExactModuleIdentity {
        unit: selected.owner().unit.clone(),
        module: selected.owner().module.clone(),
    };
    let source_imports = match compiled.exact_source_admission.as_ref() {
        Some(admission) => admission.home_imports()?,
        None => crate::declaration_context::consumed_source_home_imports(
            compiled
                .completed_source_evidence
                .as_ref()
                .ok_or_else(|| contract("authored certification has no completed source proof"))?,
            &Default::default(),
        )?,
    };
    let issued = compiled
        .source_selection
        .as_ref()
        .ok_or_else(|| contract("authored certification has no issued source selection"))?
        .compiler_support_projection(
            &compiled.artifact_view,
            compiled.exact_source_admission.as_slice(),
        )
        .map_err(|error| contract(format!("authored compiler source selection: {error}")))?;
    let artifact_context = planned::authored_interface_context(
        context.as_ref(),
        &compiled.artifact_view,
        &issued,
        products.iter().map(|product| ExactModuleIdentity {
            unit: product.owner().unit.clone(),
            module: product.owner().module.clone(),
        }),
        &source_imports,
    )?;
    let issued_inputs = crate::declaration_context::OriginalCompilerInputs::from_selection(
        compiled
            .source_selection
            .as_ref()
            .expect("validated source selection"),
        &compiled.artifact_view,
    )?;
    let artifact_context = Arc::new(
        artifact_context
            .as_ref()
            .clone()
            .retain_issued_native_offers(issued_inputs.native_offers().iter().cloned())?,
    );
    let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
    let planned::AuthoredClosureObservation {
        directory,
        artifacts,
        original_imports,
        source_lexical_imports,
    } = planned::ValidatedAuthoredClosure::admit(
        &originals,
        &selected_owner,
        toolchain_identity_sha256,
        &evidence,
        compiled.exact_source_admission.as_ref(),
        &source_imports,
        Some(&artifact_context),
        &[],
        includes,
        &mut validation,
    )?
    .materialize()?;
    let outcome = inspect_declaration_artifacts_with_producer(
        &artifacts,
        includes,
        session_root,
        Some(toolchain_identity_sha256),
        directory,
        settlement,
    )?;
    let inventories = match outcome.decision {
        JoinDecision::Accepted => outcome
            .inventories
            .ok_or_else(|| contract("accepted authored inventory is empty"))?,
        JoinDecision::Rejected { diagnostic, .. } => return Err(contract(diagnostic)),
    };
    let inventory = inventories
        .into_iter()
        .filter(|entry| {
            entry.artifact.interface.unit == selected.owner().unit
                && entry.artifact.interface.module == module_name
        })
        .collect::<Vec<_>>();
    if inventory.len() != 1 {
        return Err(contract("authored inventory has no unique G module"));
    }
    let inventory = inventory.into_iter().next().expect("count checked");
    let family_closure = outcome
        .family_closure
        .ok_or_else(|| contract("accepted authored family closure is empty"))?;
    if std::fs::read(source_path)? != exact_source.as_bytes() {
        return Err(contract(
            "authored source changed during inventory certification",
        ));
    }
    let introduced_exports = inventory
        .exports
        .iter()
        .filter(|export| {
            export.head.unit == selected.owner().unit
                && export.head.module == selected.owner().module
        })
        .cloned()
        .collect();
    let compiler_support =
        crate::declaration_context::certified_artifact_view(&originals, artifact_context.as_ref())?;
    Ok(CertifiedAuthoredDeclaration {
        product: selected.clone(),
        compiler_support,
        lexical_exports: inventory.exports,
        introduced_exports,
        instances: inventory.instances,
        family_closure,
        source_sha256,
        toolchain_identity_sha256,
        original_imports,
        source_lexical_imports,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationJoinSpec {
    pub expected_public_version: String,
    pub public_module: Option<ModuleSnapshot>,
    pub private_base: Option<ModuleSnapshot>,
    pub private_tip: Option<ModuleSnapshot>,
    pub writes: Vec<DeclarationWrite>,
    pub reserved: ReservedJoin,
    pub family_closure: Vec<ExportIdentity>,
    pub expected_exports: Vec<DeclarationExport>,
    pub expected_instances: InstanceInventory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationJoinInput {
    pub expected_public_version: String,
    pub public_module: Option<ModuleSnapshot>,
    pub private_base: Option<ModuleSnapshot>,
    pub private_tip: Option<ModuleSnapshot>,
    pub writes: Vec<DeclarationWrite>,
    pub reserved: ReservedJoin,
    pub artifacts: Vec<DeclarationArtifact>,
    pub family_closure: Vec<ExportIdentity>,
    pub expected_exports: Vec<DeclarationExport>,
    pub expected_instances: InstanceInventory,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReservedJoin {
    pub unit: String,
    pub module: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactIfaceArtifact {
    pub unit: String,
    pub module: String,
    pub path: PathBuf,
    pub sha256: String,
    pub requirements: Vec<(String, String)>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationArtifact {
    pub interface: ExactIfaceArtifact,
    pub product: Option<ModuleSnapshot>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JoinRejection {
    ArtifactChanged,
    ExportMismatch,
    ClassInstanceConflict,
    FamilyInstanceConflict,
    InstanceMismatch,
    Unprovable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum JoinDecision {
    Accepted,
    Rejected {
        reason: JoinRejection,
        diagnostic: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationJoinOutcome {
    version: u32,
    pub expected_public_version: String,
    pub request_sha256: String,
    pub reserved: ReservedJoin,
    pub interfaces_sha256: String,
    pub exports_sha256: String,
    pub instances_sha256: String,
    pub family_closure_sha256: String,
    pub artifact: Option<ModuleSnapshot>,
    pub decision: JoinDecision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertifiedDeclarationJoin {
    Accepted(AcceptedJoin),
    Rejected(RejectedJoin),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedJoin {
    // The accepted interface and its input edges are issued once together.
    // Retained input custody cannot become additional dependency authority.
    compiler_support: crate::artifact_inventory::CompilerSourceSupport,
    input: DeclarationJoinInput,
    outcome: DeclarationJoinOutcome,
    interface: crate::recovery_artifacts::CertifiedJoinedInterface,
    context: ExactDeclarationContext,
}

pub(crate) struct ValidatedJoinInputs {
    support: crate::artifact_inventory::CompilerSourceSupport,
    owners: Vec<ExactModuleIdentity>,
}

impl ValidatedJoinInputs {
    pub(crate) fn support(&self) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.support
    }
    pub(crate) fn owners(&self) -> &[ExactModuleIdentity] {
        &self.owners
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedJoin {
    input: DeclarationJoinInput,
    outcome: DeclarationJoinOutcome,
    producer: [u8; 32],
}

pub struct MaterializedDeclarationJoin {
    /// SHA-256 input bytes consumed while materializing this receipt's closure.
    pub materialization_hash_bytes: u64,
    pub join: crate::recovery_artifacts::RecoveryJoinRef,
    pub products: Vec<crate::recovery_artifacts::RecoveryArtifactRef>,
    pub module_interfaces: Vec<crate::recovery_artifacts::RecoveryModuleInterfaceRef>,
    pub anchors: Vec<crate::recovery_artifacts::RecoveryJoinRef>,
    pub value_interfaces: Vec<crate::recovery_artifacts::RecoveryValueInterfaceRef>,
    /// Durable Interface edges; native edges remain in the certified original graph.
    pub artifact_dependencies: Vec<(
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactDependency,
    )>,
}

impl AcceptedJoin {
    pub(crate) fn compiler_source_support(
        &self,
    ) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.compiler_support
    }

    pub fn input(&self) -> &DeclarationJoinInput {
        &self.input
    }
    pub fn outcome(&self) -> &DeclarationJoinOutcome {
        &self.outcome
    }
    pub fn reserved(&self) -> &ReservedJoin {
        &self.input.reserved
    }
    pub fn expected_public_version(&self) -> &str {
        &self.input.expected_public_version
    }
    pub fn request_sha256(&self) -> &str {
        &self.outcome.request_sha256
    }
    pub fn exports(&self) -> &[DeclarationExport] {
        &self.input.expected_exports
    }
    /// Exact live values needed by retained full authored native bodies,
    /// including helpers not selected for execution during this publication.
    pub fn native_binding_custody_requirements(
        &self,
    ) -> Result<Vec<crate::artifact_inventory::NativeBindingRequirement>, CompileError> {
        let owners = self
            .exports()
            .iter()
            .flat_map(|export| std::iter::once(&export.head).chain(export.children.iter()))
            .map(|identity| ExactModuleIdentity {
                unit: identity.unit.clone(),
                module: identity.module.clone(),
            })
            .collect();
        self.context
            .authored_native_binding_custody_requirements(&owners)
    }
    pub fn instances(&self) -> &InstanceInventory {
        &self.input.expected_instances
    }
    pub fn family_closure(&self) -> &[ExportIdentity] {
        &self.input.family_closure
    }
    pub fn interface_bytes(&self) -> &[u8] {
        self.interface.interface_bytes()
    }
    pub fn package_imports_bytes(&self) -> &[u8] {
        self.interface.package_imports_bytes()
    }
    pub fn recovery_products(&self) -> Vec<CertifiedRecoveryProduct> {
        self.context.recovery_products()
    }
    pub fn toolchain_identity_sha256(&self) -> [u8; 32] {
        self.interface.toolchain_identity_sha256()
    }
    pub(crate) fn context(&self) -> &ExactDeclarationContext {
        &self.context
    }
    pub(crate) fn interface(&self) -> &crate::recovery_artifacts::CertifiedJoinedInterface {
        &self.interface
    }
    pub fn artifact_id(&self) -> crate::artifact_inventory::ArtifactId {
        self.interface.artifact_id()
    }
    /// Seal this receipt's own joined interface. Inherited context artifacts
    /// remain under their exact selection and the run's retained references.
    pub fn materialize_interface(
        &self,
        root: &Path,
    ) -> Result<
        crate::recovery_artifacts::RecoveryJoinRef,
        crate::recovery_artifacts::RecoveryArtifactError,
    > {
        self.interface.materialize(root)
    }
    pub fn materialize(
        &self,
        root: &Path,
    ) -> Result<MaterializedDeclarationJoin, crate::recovery_artifacts::RecoveryArtifactError> {
        let mut work = crate::recovery_artifacts::RecoveryArtifactWork::default();
        let products = crate::recovery_artifacts::materialize_certified_products_with_work(
            root,
            self.toolchain_identity_sha256(),
            &self.recovery_products(),
            &mut work,
        )?;
        let module_interfaces =
            crate::recovery_artifacts::with_artifact_work(&mut work, |validation| {
                self.context
                    .module_interfaces()
                    .iter()
                    .map(|interface| {
                        crate::recovery_artifacts::materialize_module_interface(
                            root,
                            interface,
                            validation,
                            crate::recovery_artifacts::MaterializationMode::Durable,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?;
        let anchors = self
            .context
            .joined_interfaces()
            .iter()
            .map(|join| join.materialize_with_work(root, &mut work))
            .collect::<Result<Vec<_>, _>>()?;
        let value_interfaces = self
            .context
            .value_interfaces()
            .iter()
            .map(|value| value.materialize_with_work(root, &mut work))
            .collect::<Result<Vec<_>, _>>()?;
        let join = self.interface.materialize_with_work(root, &mut work)?;
        let mut artifact_dependencies = self.compiler_support.artifacts().interface_dependencies();
        artifact_dependencies.sort();
        artifact_dependencies.dedup();
        Ok(MaterializedDeclarationJoin {
            materialization_hash_bytes: work.hash_bytes,
            join,
            products,
            module_interfaces,
            anchors,
            value_interfaces,
            artifact_dependencies,
        })
    }
}

impl RejectedJoin {
    pub fn input(&self) -> &DeclarationJoinInput {
        &self.input
    }
    pub fn outcome(&self) -> &DeclarationJoinOutcome {
        &self.outcome
    }
    pub fn reserved(&self) -> &ReservedJoin {
        &self.input.reserved
    }
    pub fn expected_public_version(&self) -> &str {
        &self.input.expected_public_version
    }
    pub fn request_sha256(&self) -> &str {
        &self.outcome.request_sha256
    }
    pub fn toolchain_identity_sha256(&self) -> [u8; 32] {
        self.producer
    }
}

fn require_owned_artifact_paths(
    artifacts: &[DeclarationArtifact],
    directory: &tempfile::TempDir,
) -> Result<(), CompileError> {
    let root = directory.path().canonicalize()?;
    for path in artifacts.iter().flat_map(|artifact| {
        std::iter::once(&artifact.interface.path)
            .chain(artifact.product.iter().map(|product| &product.path))
    }) {
        if !path.canonicalize()?.starts_with(&root) {
            return Err(contract(
                "declaration input path lies outside its retained directory",
            ));
        }
    }
    Ok(())
}

/// Certify the owner's exact interface observation and retain its native context.
/// The worker attests interface semantics, not unconsumed native bodies or scopes.
/// Rejection is also bound to the request; runtime owns its staleness decision.
pub fn certify_declaration_join(
    spec: DeclarationJoinSpec,
    inputs: crate::declaration_context::DeclarationJoinInputs,
    includes: &[PathBuf],
    session_root: &Path,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<CertifiedDeclarationJoin, CompileError> {
    let context = inputs.context();
    let input_directory = inputs.directory();
    let input = DeclarationJoinInput {
        expected_public_version: spec.expected_public_version,
        public_module: spec.public_module,
        private_base: spec.private_base,
        private_tip: spec.private_tip,
        writes: spec.writes,
        reserved: spec.reserved,
        artifacts: inputs.artifacts().to_vec(),
        family_closure: spec.family_closure,
        expected_exports: spec.expected_exports,
        expected_instances: spec.expected_instances,
    };
    require_owned_artifact_paths(&input.artifacts, &input_directory)?;
    inputs.validate()?;
    for (index, write) in input.writes.iter().enumerate() {
        if write.generation == 0
            || SessionModule::lib(tidepool_repr::Generation(write.generation)).module_name()
                != write.module.module
            || index > 0 && input.writes[index - 1].generation >= write.generation
        {
            return Err(contract(
                "authored writes are not an ordered original generation suffix",
            ));
        }
    }
    let encoded = encode_declaration_join(&input)?;
    let execution = execute_declaration_operation(
        &encoded,
        includes,
        session_root,
        &[],
        Some(context.toolchain_identity_sha256()),
        input_directory,
        settlement,
    )?;
    let outcome = decode_declaration_join_outcome(&input, &execution.receipt)?;
    inputs.validate()?;
    match outcome.decision {
        JoinDecision::Accepted => {
            let output = outcome
                .artifact
                .as_ref()
                .ok_or_else(|| contract("accepted join has no interface"))?;
            let bytes = std::fs::read(&output.path)?;
            if sha256(&bytes) != output.sha256 {
                return Err(contract(
                    "joined interface changed after compiler validation",
                ));
            }
            let mut packages = output.path.as_os_str().to_os_string();
            packages.push(".packages");
            let packages = std::fs::read(PathBuf::from(packages))?;
            let interface =
                crate::recovery_artifacts::CertifiedJoinedInterface::from_certification(
                    execution.producer,
                    input.reserved.unit.clone(),
                    input.reserved.module.clone(),
                    bytes,
                    packages,
                )
                .map_err(|error| contract(format!("joined package witness rejected: {error}")))?;
            inputs.validate()?;
            let validated_inputs = ValidatedJoinInputs {
                support: context.compiler_source_support().clone(),
                owners: input
                    .artifacts
                    .iter()
                    .map(|artifact| ExactModuleIdentity {
                        unit: artifact.interface.unit.clone(),
                        module: artifact.interface.module.clone(),
                    })
                    .collect(),
            };
            let compiler_support =
                crate::artifact_inventory::CompilerSourceSupport::from_joined_interface(
                    &interface,
                    &validated_inputs,
                )?;
            Ok(CertifiedDeclarationJoin::Accepted(AcceptedJoin {
                compiler_support,
                input,
                outcome,
                interface,
                context: context.clone(),
            }))
        }
        JoinDecision::Rejected { .. } => Ok(CertifiedDeclarationJoin::Rejected(RejectedJoin {
            input,
            outcome,
            producer: execution.producer,
        })),
    }
}

/// Use the existing bound compiler process boundary and diagnostic policy.
/// This operation never compiles an executable or executes authored effects.
pub fn validate_declaration_join(
    input: &DeclarationJoinInput,
    includes: &[PathBuf],
    session_root: &Path,
    inject_modules: &[String],
    input_directory: Arc<tempfile::TempDir>,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<DeclarationJoinOutcome, CompileError> {
    require_owned_artifact_paths(&input.artifacts, &input_directory)?;
    let encoded = encode_declaration_join(input)?;
    let execution = execute_declaration_operation(
        &encoded,
        includes,
        session_root,
        inject_modules,
        None,
        input_directory,
        settlement,
    )?;
    decode_declaration_join_outcome(input, &execution.receipt)
}

/// Canonical definite-length CBOR arrays shared with the worker. Keeping the
/// ordered writes in this digest fences instance-only changes and retractions.
pub fn encode_declaration_join(input: &DeclarationJoinInput) -> Result<Vec<u8>, CompileError> {
    validate_instance_inventory(&input.expected_instances)?;
    if input
        .artifacts
        .iter()
        .any(|artifact| artifact.product.is_some())
    {
        return Err(contract(
            "declaration joins consume interface observations only",
        ));
    }
    let mut writer = JoinEncoder(Vec::new());
    writer.array(12);
    writer.text("TPDJOIN");
    writer.text("4");
    writer.text(&input.expected_public_version);
    writer.optional(input.public_module.as_ref())?;
    writer.optional(input.private_base.as_ref())?;
    writer.optional(input.private_tip.as_ref())?;
    writer.array(input.writes.len());
    for write in &input.writes {
        writer.array(4);
        writer.header(0, write.generation);
        writer.snapshot(&write.module)?;
        writer.exports(&write.exports);
        writer.array(write.retractions.len());
        for identity in &write.retractions {
            writer.identity(identity);
        }
    }
    writer.reserved(&input.reserved)?;
    writer.exports(&input.expected_exports);
    writer.inventory(&input.expected_instances);
    writer.artifacts(&input.artifacts)?;
    writer.identities(&input.family_closure);
    if writer.0.len() > 4 * 1024 * 1024 {
        return Err(contract("declaration join exceeds four MiB"));
    }
    Ok(writer.0)
}

pub fn decode_declaration_join_outcome(
    input: &DeclarationJoinInput,
    bytes: &[u8],
) -> Result<DeclarationJoinOutcome, CompileError> {
    let outcome: DeclarationJoinOutcome = serde_json::from_slice(bytes)
        .map_err(|error| contract(format!("invalid declaration join receipt: {error}")))?;
    let mut implementation = JoinEncoder(Vec::new());
    implementation.artifacts(&input.artifacts)?;
    let mut exports = JoinEncoder(Vec::new());
    exports.exports(&input.expected_exports);
    let mut instances = JoinEncoder(Vec::new());
    instances.inventory(&input.expected_instances);
    let mut families = JoinEncoder(Vec::new());
    families.identities(&input.family_closure);
    if outcome.version != 4
        || outcome.expected_public_version != input.expected_public_version
        || outcome.request_sha256 != sha256(&encode_declaration_join(input)?)
        || outcome.reserved != input.reserved
        || outcome.interfaces_sha256 != sha256(&implementation.0)
        || outcome.exports_sha256 != sha256(&exports.0)
        || outcome.instances_sha256 != sha256(&instances.0)
        || outcome.family_closure_sha256 != sha256(&families.0)
    {
        return Err(contract(
            "declaration join receipt belongs to a different request",
        ));
    }
    match (&outcome.decision, &outcome.artifact) {
        (JoinDecision::Accepted, Some(artifact))
            if artifact.module == input.reserved.module
                && artifact.path == input.reserved.path
                && valid_digest(&artifact.sha256) => {}
        (JoinDecision::Rejected { .. }, None) => {}
        _ => {
            return Err(contract(
                "declaration join receipt has invalid output identity",
            ))
        }
    }
    Ok(outcome)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn contract(message: impl Into<String>) -> CompileError {
    CompileError::ExtractFailed(message.into())
}

struct JoinEncoder(Vec<u8>);

impl JoinEncoder {
    fn header(&mut self, major: u8, value: u64) {
        match value {
            0..=23 => self.0.push(major | value as u8),
            24..=255 => self.0.extend_from_slice(&[major | 24, value as u8]),
            256..=65535 => {
                self.0.push(major | 25);
                self.0.extend_from_slice(&(value as u16).to_be_bytes());
            }
            65536..=4294967295 => {
                self.0.push(major | 26);
                self.0.extend_from_slice(&(value as u32).to_be_bytes());
            }
            _ => {
                self.0.push(major | 27);
                self.0.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    fn array(&mut self, count: usize) {
        self.header(0x80, count as u64);
    }
    fn text(&mut self, text: &str) {
        self.header(0x60, text.len() as u64);
        self.0.extend_from_slice(text.as_bytes());
    }
    fn optional(&mut self, snapshot: Option<&ModuleSnapshot>) -> Result<(), CompileError> {
        match snapshot {
            Some(snapshot) => self.snapshot(snapshot),
            None => {
                self.0.push(0xf6);
                Ok(())
            }
        }
    }
    fn snapshot(&mut self, snapshot: &ModuleSnapshot) -> Result<(), CompileError> {
        if !snapshot.path.is_absolute()
            || snapshot.module.is_empty()
            || !valid_digest(&snapshot.sha256)
        {
            return Err(contract("invalid declaration module snapshot"));
        }
        let path = snapshot
            .path
            .to_str()
            .ok_or_else(|| contract("non-UTF-8 declaration path"))?;
        self.array(3);
        self.text(&snapshot.module);
        self.text(path);
        self.text(&snapshot.sha256);
        Ok(())
    }
    fn reserved(&mut self, reserved: &ReservedJoin) -> Result<(), CompileError> {
        if reserved.unit.is_empty() || reserved.module.is_empty() || !reserved.path.is_absolute() {
            return Err(contract("invalid reserved Join identity"));
        }
        self.array(3);
        self.text(&reserved.unit);
        self.text(&reserved.module);
        self.text(
            reserved
                .path
                .to_str()
                .ok_or_else(|| contract("non-UTF-8 Join path"))?,
        );
        Ok(())
    }
    fn artifacts(&mut self, artifacts: &[DeclarationArtifact]) -> Result<(), CompileError> {
        self.array(artifacts.len());
        for artifact in artifacts {
            let iface = &artifact.interface;
            self.array(2);
            if iface.unit.is_empty()
                || iface.module.is_empty()
                || !iface.path.is_absolute()
                || !valid_digest(&iface.sha256)
            {
                return Err(contract("invalid exact interface artifact"));
            }
            self.array(5);
            self.text(&iface.unit);
            self.text(&iface.module);
            self.text(
                iface
                    .path
                    .to_str()
                    .ok_or_else(|| contract("non-UTF-8 interface path"))?,
            );
            self.text(&iface.sha256);
            self.array(iface.requirements.len());
            for (unit, module) in &iface.requirements {
                self.array(2);
                self.text(unit);
                self.text(module);
            }
            self.optional(artifact.product.as_ref())?;
        }
        Ok(())
    }
    fn identities(&mut self, identities: &[ExportIdentity]) {
        self.array(identities.len());
        for identity in identities {
            self.identity(identity);
        }
    }
    fn inventory(&mut self, inventory: &InstanceInventory) {
        self.array(2);
        self.array(inventory.classes.len());
        for record in &inventory.classes {
            self.array(3);
            self.identity(&record.dfun);
            self.identity(&record.class);
            self.identities(&record.selected_axioms);
        }
        self.identities(&inventory.families);
    }
    fn identity(&mut self, identity: &ExportIdentity) {
        self.array(5);
        self.text(&identity.unit);
        self.text(&identity.module);
        self.text(identity.namespace.wire());
        self.text(&identity.occurrence);
        match &identity.record_parent {
            Some(parent) => self.text(parent),
            None => self.0.push(0xf6),
        }
    }
    fn exports(&mut self, exports: &[DeclarationExport]) {
        self.array(exports.len());
        for export in exports {
            self.array(3);
            self.text(export.kind.wire());
            self.identity(&export.head);
            self.array(export.children.len());
            for child in &export.children {
                self.identity(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_source_surface_requires_exact_implementation_anchors_and_source_rows() {
        use crate::artifact_inventory::ArtifactKind;
        use std::collections::BTreeMap;
        let owner = |module: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: module.into(),
        };
        let original = owner("Tidepool.Session.Lib.G2");
        let value = owner("Tidepool.Session.Val.G1");
        let helper = owner("Helper");
        let shared = owner("SharedInstances");
        let imports = BTreeMap::from([
            (original.clone(), vec![helper.clone(), value.clone()]),
            (helper.clone(), vec![value.clone(), shared.clone()]),
            (shared.clone(), vec![]),
        ]);
        let implementations = BTreeMap::from([(value.clone(), ArtifactKind::ValueInterface)]);
        let surface =
            original_source_lexical_surface(&original, &imports, &[], &implementations).unwrap();
        assert_eq!(surface.roots, vec![helper.clone()]);
        assert_eq!(
            surface.lexical,
            vec![
                ExactLexicalNode {
                    owner: helper.clone(),
                    imports: vec![shared.clone()]
                },
                ExactLexicalNode {
                    owner: shared.clone(),
                    imports: vec![]
                },
            ]
        );
        assert_eq!(imports[&helper], vec![value.clone(), shared.clone()]);
        assert!(
            original_source_lexical_surface(&original, &imports, &[], &BTreeMap::new()).is_err()
        );
        let wrong_kind = BTreeMap::from([(value.clone(), ArtifactKind::LexicalJoin)]);
        assert!(original_source_lexical_surface(&original, &imports, &[], &wrong_kind).is_err());
        let mut missing_source = imports.clone();
        missing_source.remove(&shared);
        assert!(
            original_source_lexical_surface(&original, &missing_source, &[], &implementations)
                .is_err()
        );
        let inherited = [ExactLexicalNode {
            owner: shared.clone(),
            imports: vec![],
        }];
        assert!(original_source_lexical_surface(
            &original,
            &missing_source,
            &inherited,
            &implementations
        )
        .is_ok());
        let conflicting = [ExactLexicalNode {
            owner: helper,
            imports: vec![],
        }];
        assert!(original_source_lexical_surface(
            &original,
            &imports,
            &conflicting,
            &implementations
        )
        .is_err());
    }

    #[test]
    fn admitted_join_role_never_promotes_a_historical_projection_to_shared_source() {
        use crate::artifact_inventory::ArtifactKind;
        use std::collections::BTreeMap;
        let owner = |module: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: module.into(),
        };
        let original = owner("Tidepool.Session.Lib.G3");
        let projection = owner("ArbitraryCompilerProjection");
        let helper = owner("SharedHelper");
        let unrelated = owner("UnrelatedSharedSource");
        let imports = BTreeMap::from([
            (original.clone(), vec![projection.clone(), helper.clone()]),
            (helper.clone(), vec![projection.clone()]),
        ]);
        let inherited = [
            ExactLexicalNode {
                owner: projection.clone(),
                imports: vec![],
            },
            ExactLexicalNode {
                owner: unrelated.clone(),
                imports: vec![projection.clone()],
            },
        ];
        let roles = BTreeMap::from([(projection.clone(), ArtifactKind::LexicalJoin)]);
        let selected =
            original_source_lexical_surface(&original, &imports, &inherited, &roles).unwrap();
        assert_eq!(selected.roots, vec![helper.clone()]);
        assert_eq!(
            selected.lexical,
            vec![
                ExactLexicalNode {
                    owner: helper,
                    imports: vec![]
                },
                ExactLexicalNode {
                    owner: unrelated,
                    imports: vec![]
                }
            ]
        );
        assert!(!selected
            .lexical
            .iter()
            .any(|node| node.owner == projection || node.imports.contains(&projection)));
        // A module spelling does not certify an implementation role.
        let source =
            original_source_lexical_surface(&original, &imports, &inherited, &BTreeMap::new())
                .unwrap();
        assert!(source.lexical.iter().any(|node| node.owner == projection));
    }

    #[test]
    fn source_lexical_closure_keeps_reachable_inherited_rows_only() {
        use std::collections::BTreeMap;
        let owner = |module: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: module.into(),
        };
        let root = owner("G3");
        let inherited = owner("CheckedHomeValue");
        let child = owner("InheritedChild");
        let unrelated = owner("UnrelatedRetainedValue");
        let imports = BTreeMap::from([(root.clone(), vec![inherited.clone()])]);
        let inherited_lexical = [
            ExactLexicalNode {
                owner: inherited.clone(),
                imports: vec![child.clone()],
            },
            ExactLexicalNode {
                owner: child.clone(),
                imports: vec![],
            },
            ExactLexicalNode {
                owner: unrelated.clone(),
                imports: vec![],
            },
        ];

        let closure = source_lexical_closure(
            &imports[&root],
            &imports,
            &inherited_lexical,
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(
            closure.lexical,
            vec![
                ExactLexicalNode {
                    owner: inherited,
                    imports: vec![owner("InheritedChild")],
                },
                ExactLexicalNode {
                    owner: child,
                    imports: vec![],
                },
            ]
        );
        assert!(!closure.lexical.iter().any(|node| node.owner == unrelated));
        assert!(source_lexical_closure(
            &imports[&root],
            &imports,
            &[ExactLexicalNode {
                owner: unrelated,
                imports: vec![],
            }],
            &BTreeMap::new(),
        )
        .is_err());
    }

    fn input() -> DeclarationJoinInput {
        DeclarationJoinInput {
            expected_public_version: "paired-snapshot".into(),
            public_module: None,
            private_base: None,
            private_tip: None,
            writes: Vec::new(),
            reserved: ReservedJoin {
                unit: "main".into(),
                module: "Join".into(),
                path: "/scratch/Join.hi".into(),
            },
            expected_exports: Vec::new(),
            expected_instances: InstanceInventory::default(),
            artifacts: Vec::new(),
            family_closure: Vec::new(),
        }
    }
    fn receipt(input: &DeclarationJoinInput, decision: JoinDecision) -> DeclarationJoinOutcome {
        let mut implementation = JoinEncoder(Vec::new());
        implementation.artifacts(&input.artifacts).unwrap();
        let mut exports = JoinEncoder(Vec::new());
        exports.exports(&input.expected_exports);
        let mut instances = JoinEncoder(Vec::new());
        instances.inventory(&input.expected_instances);
        let mut families = JoinEncoder(Vec::new());
        families.identities(&input.family_closure);
        DeclarationJoinOutcome {
            version: 4,
            expected_public_version: input.expected_public_version.clone(),
            request_sha256: sha256(&encode_declaration_join(input).unwrap()),
            reserved: input.reserved.clone(),
            interfaces_sha256: sha256(&implementation.0),
            exports_sha256: sha256(&exports.0),
            instances_sha256: sha256(&instances.0),
            family_closure_sha256: sha256(&families.0),
            artifact: if decision == JoinDecision::Accepted {
                Some(ModuleSnapshot {
                    module: input.reserved.module.clone(),
                    path: input.reserved.path.clone(),
                    sha256: "0".repeat(64),
                })
            } else {
                None
            },
            decision,
        }
    }

    #[test]
    fn matches_haskell_manifest_and_receipt() {
        let input = input();
        assert_eq!(
            encode_declaration_join(&input).unwrap(),
            include_bytes!(
                "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v4.cbor"
            )
        );
        let receipt = include_bytes!(
            "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v4.json"
        );
        let accepted = decode_declaration_join_outcome(&input, receipt).unwrap();
        assert_eq!(accepted.decision, JoinDecision::Accepted);
        assert_eq!(accepted.artifact.unwrap().path, input.reserved.path);
        assert_eq!(encode_declaration_inventory(&[]).unwrap(),
            include_bytes!("../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.cbor"));
        let inventory = decode_declaration_inventory_outcome(&[], include_bytes!(
            "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v3.json")).unwrap();
        assert_eq!(inventory.decision, JoinDecision::Accepted);
    }

    #[test]
    fn matches_haskell_typed_class_family_and_consistency_subset_manifest() {
        let identity = |namespace, occurrence: &str| ExportIdentity {
            unit: "main".into(),
            module: "Original".into(),
            namespace,
            occurrence: occurrence.into(),
            record_parent: None,
        };
        let selected = identity(ExportNamespace::Type, "AssociatedAxiom");
        let standalone = identity(ExportNamespace::Type, "StandaloneAxiom");
        let hidden = identity(ExportNamespace::Type, "HiddenAxiom");
        let mut input = input();
        input.expected_instances = InstanceInventory {
            classes: vec![ClassInstanceEvidence {
                dfun: identity(ExportNamespace::Value, "$fClassInt"),
                class: identity(ExportNamespace::Type, "Class"),
                selected_axioms: vec![selected.clone()],
            }],
            families: vec![selected.clone(), standalone.clone()],
        };
        input.family_closure = vec![selected, standalone, hidden];
        assert_eq!(encode_declaration_join(&input).unwrap(), include_bytes!(
            "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v4.cbor"
        ));
        let outcome = decode_declaration_join_outcome(&input, include_bytes!(
            "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-typed-v4.json"
        )).unwrap();
        assert_eq!(outcome.decision, JoinDecision::Accepted);
    }

    #[test]
    fn join_refuses_legacy_receipt_and_native_file_inputs() {
        let mut input = input();
        assert!(decode_declaration_join_outcome(
            &input,
            include_bytes!(
                "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v3.json"
            )
        )
        .is_err());
        input.artifacts.push(DeclarationArtifact {
            interface: ExactIfaceArtifact {
                unit: "main".into(),
                module: "Missing".into(),
                path: "/missing/interface.hi".into(),
                sha256: "0".repeat(64),
                requirements: vec![],
            },
            product: Some(ModuleSnapshot {
                module: "Missing".into(),
                path: "/missing/native.tpmod".into(),
                sha256: "0".repeat(64),
            }),
        });
        assert!(encode_declaration_join(&input).is_err());
    }

    #[test]
    fn inventory_rejects_changed_artifact_provenance() {
        let artifacts = Vec::new();
        let mut encoder = JoinEncoder(Vec::new());
        encoder.artifacts(&artifacts).unwrap();
        let outcome = DeclarationInventoryOutcome {
            version: 3,
            request_sha256: sha256(&encode_declaration_inventory(&artifacts).unwrap()),
            implementation_sha256: sha256(&encoder.0),
            inventories: Some(Vec::new()),
            family_closure: Some(Vec::new()),
            decision: JoinDecision::Accepted,
        };
        let bytes = serde_json::to_vec(&outcome).unwrap();
        assert_eq!(
            decode_declaration_inventory_outcome(&artifacts, &bytes).unwrap(),
            outcome
        );
        let changed = vec![DeclarationArtifact {
            interface: ExactIfaceArtifact {
                unit: "main".into(),
                module: "Old".into(),
                path: "/scratch/Old.hi".into(),
                sha256: "0".repeat(64),
                requirements: Vec::new(),
            },
            product: None,
        }];
        assert!(decode_declaration_inventory_outcome(&changed, &bytes).is_err());
    }

    #[test]
    fn refuses_previous_untyped_inventory_schema() {
        assert!(decode_declaration_join_outcome(
            &input(),
            include_bytes!(
                "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/join-v2.json"
            )
        )
        .is_err());
        assert!(decode_declaration_inventory_outcome(
            &[],
            include_bytes!(
            "../../../bridge/haskell/test-cell-splitter/fixtures/declaration-join/inventory-v2.json"
        )
        )
        .is_err());
    }

    #[test]
    fn typed_class_records_require_selected_unique_family_axioms() {
        let identity = |namespace, occurrence: &str| ExportIdentity {
            unit: "main".into(),
            module: "Original".into(),
            namespace,
            occurrence: occurrence.into(),
            record_parent: None,
        };
        let axiom = identity(ExportNamespace::Type, "Axiom");
        let record = ClassInstanceEvidence {
            dfun: identity(ExportNamespace::Value, "$fClassInt"),
            class: identity(ExportNamespace::Type, "Class"),
            selected_axioms: vec![axiom.clone()],
        };
        let mut inventory = InstanceInventory {
            classes: vec![record.clone()],
            families: vec![axiom],
        };
        validate_instance_inventory(&inventory).unwrap();
        inventory.classes.push(record);
        assert!(validate_instance_inventory(&inventory).is_err());
        inventory.classes.pop();
        inventory.families.clear();
        assert!(validate_instance_inventory(&inventory).is_err());
    }

    #[test]
    fn rejects_cross_request_receipts_for_both_decisions() {
        let input = input();
        for decision in [
            JoinDecision::Accepted,
            JoinDecision::Rejected {
                reason: JoinRejection::ClassInstanceConflict,
                diagnostic: "duplicate instance".into(),
            },
        ] {
            let outcome = receipt(&input, decision);
            let bytes = serde_json::to_vec(&outcome).unwrap();
            assert_eq!(
                decode_declaration_join_outcome(&input, &bytes).unwrap(),
                outcome
            );
            let mut other = input.clone();
            other.expected_public_version.push_str("-new");
            assert!(decode_declaration_join_outcome(&other, &bytes).is_err());
            let mut other = input.clone();
            other.reserved.module.push_str("New");
            assert!(decode_declaration_join_outcome(&other, &bytes).is_err());
            let mut other = input.clone();
            other.writes.push(DeclarationWrite {
                generation: 7,
                module: ModuleSnapshot {
                    module: "Old".into(),
                    path: "/scratch/Old.hi".into(),
                    sha256: "0".repeat(64),
                },
                exports: Vec::new(),
                retractions: Vec::new(),
            });
            assert!(decode_declaration_join_outcome(&other, &bytes).is_err());
            let mut corrupt = outcome.clone();
            corrupt.instances_sha256 = "f".repeat(64);
            assert!(decode_declaration_join_outcome(
                &input,
                &serde_json::to_vec(&corrupt).unwrap()
            )
            .is_err());
            let mut corrupt = outcome.clone();
            corrupt.artifact = Some(ModuleSnapshot {
                module: "Wrong".into(),
                path: input.reserved.path.clone(),
                sha256: "0".repeat(64),
            });
            assert!(decode_declaration_join_outcome(
                &input,
                &serde_json::to_vec(&corrupt).unwrap()
            )
            .is_err());
        }
    }
}

struct DeclarationExecution {
    receipt: Vec<u8>,
    producer: [u8; 32],
}

fn execute_declaration_operation(
    encoded: &[u8],
    includes: &[PathBuf],
    session_root: &Path,
    inject_modules: &[String],
    expected_producer: Option<[u8; 32]>,
    input_directory: Arc<tempfile::TempDir>,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<DeclarationExecution, CompileError> {
    let scratch = Arc::new(tempfile::tempdir()?);
    let manifest = scratch.path().join("declaration-join.cbor");
    let receipt = scratch.path().join("declaration-join.json");
    std::fs::write(&manifest, encoded)?;
    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .input(&manifest)
        .includes(includes)
        .session_root(session_root)
        .declaration_join(&manifest)
        .declaration_join_out(&receipt);
    for module in inject_modules {
        command.inject_val(module);
    }
    let endpoint = command
        .bind()
        .map_err(|error| CompileError::Io(crate::extract_spawn_error(error.source)))?;
    let endpoint = crate::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
        .map_err(|error| contract(error.to_string()))?;
    let producer =
        crate::artifact_inventory::CanonicalProducerIdentity::from_compiler(endpoint.identity())
            .sha256();
    if expected_producer.is_some_and(|expected| expected != producer) {
        return Err(contract(
            "declaration operation producer differs from owned artifact producer",
        ));
    }
    crate::paths::apply_admitted_build_products_dir(&mut command, &endpoint);
    let run = endpoint.execute_with_input_directories(
        &command,
        vec![scratch.clone(), input_directory],
        |close| settlement(close),
    )?;
    crate::diag::decode_extract_result(
        run.output.status.success(),
        &run.output.stdout,
        &run.output.stderr,
    )?;
    Ok(DeclarationExecution {
        receipt: std::fs::read(receipt)?,
        producer,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationInventory {
    pub artifact: DeclarationArtifact,
    pub exports: Vec<DeclarationExport>,
    pub instances: InstanceInventory,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclarationInventoryOutcome {
    version: u32,
    pub request_sha256: String,
    pub implementation_sha256: String,
    pub inventories: Option<Vec<DeclarationInventory>>,
    pub family_closure: Option<Vec<ExportIdentity>>,
    pub decision: JoinDecision,
}

pub fn encode_declaration_inventory(
    artifacts: &[DeclarationArtifact],
) -> Result<Vec<u8>, CompileError> {
    let mut encoder = JoinEncoder(Vec::new());
    encoder.array(3);
    encoder.text("TPDINVENTORY");
    encoder.text("3");
    encoder.artifacts(artifacts)?;
    if encoder.0.len() > 4 * 1024 * 1024 {
        return Err(contract("declaration inventory exceeds four MiB"));
    }
    Ok(encoder.0)
}

/// Query exact original interface identities before the runtime computes its
/// merge. This does not replay authored declarations or publish session state.
pub fn inspect_declaration_artifacts(
    artifacts: &[DeclarationArtifact],
    includes: &[PathBuf],
    session_root: &Path,
    input_directory: Arc<tempfile::TempDir>,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<DeclarationInventoryOutcome, CompileError> {
    inspect_declaration_artifacts_with_producer(
        artifacts,
        includes,
        session_root,
        None,
        input_directory,
        settlement,
    )
}

pub(crate) fn inspect_declaration_artifacts_with_producer(
    artifacts: &[DeclarationArtifact],
    includes: &[PathBuf],
    session_root: &Path,
    expected_producer: Option<[u8; 32]>,
    input_directory: Arc<tempfile::TempDir>,
    settlement: &mut dyn FnMut(tidepool_extract_cmd::CompilerTransactionClose),
) -> Result<DeclarationInventoryOutcome, CompileError> {
    require_owned_artifact_paths(artifacts, &input_directory)?;
    let encoded = encode_declaration_inventory(artifacts)?;
    let execution = execute_declaration_operation(
        &encoded,
        includes,
        session_root,
        &[],
        expected_producer,
        input_directory,
        settlement,
    )?;
    decode_declaration_inventory_outcome(artifacts, &execution.receipt)
}

pub fn decode_declaration_inventory_outcome(
    artifacts: &[DeclarationArtifact],
    bytes: &[u8],
) -> Result<DeclarationInventoryOutcome, CompileError> {
    let outcome: DeclarationInventoryOutcome = serde_json::from_slice(bytes)
        .map_err(|error| contract(format!("invalid declaration inventory receipt: {error}")))?;
    let mut implementation = JoinEncoder(Vec::new());
    implementation.artifacts(artifacts)?;
    if outcome.version != 3
        || outcome.request_sha256 != sha256(&encode_declaration_inventory(artifacts)?)
        || outcome.implementation_sha256 != sha256(&implementation.0)
    {
        return Err(contract(
            "declaration inventory receipt belongs to a different request",
        ));
    }
    match (
        &outcome.decision,
        &outcome.inventories,
        &outcome.family_closure,
    ) {
        (JoinDecision::Accepted, Some(inventories), Some(families)) => {
            for inventory in inventories {
                validate_instance_inventory(&inventory.instances)?;
            }
            if inventories
                .iter()
                .map(|inventory| &inventory.artifact)
                .ne(artifacts.iter())
            {
                return Err(contract(
                    "declaration inventory artifact order or identity differs",
                ));
            }
            let mut actual = inventories
                .iter()
                .flat_map(|inventory| inventory.instances.families.iter().cloned())
                .collect::<Vec<_>>();
            actual.sort();
            actual.dedup();
            if &actual != families {
                return Err(contract("declaration inventory family closure differs"));
            }
        }
        (JoinDecision::Rejected { .. }, None, None) => {}
        _ => {
            return Err(contract(
                "declaration inventory receipt has invalid outcome shape",
            ))
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod authored_tests {
    fn join_spec(input: &DeclarationJoinInput) -> DeclarationJoinSpec {
        DeclarationJoinSpec {
            expected_public_version: input.expected_public_version.clone(),
            public_module: input.public_module.clone(),
            private_base: input.private_base.clone(),
            private_tip: input.private_tip.clone(),
            writes: input.writes.clone(),
            reserved: input.reserved.clone(),
            family_closure: input.family_closure.clone(),
            expected_exports: input.expected_exports.clone(),
            expected_instances: input.expected_instances.clone(),
        }
    }

    fn certify_recovered_declaration_tip_in_context(
        context: Arc<ExactDeclarationContext>,
        selection: RecoveryDeclarationSelection,
        includes: &[PathBuf],
    ) -> Result<RecoveredDeclarationTip, CompileError> {
        crate::artifacts::test_support::with_settlement(|recipient| {
            super::certify_recovered_declaration_tip_in_context(
                context, selection, includes, recipient,
            )
        })
    }

    use super::*;
    use crate::recovery_artifacts::PackageInterfaceValidation;
    use tidepool_repr::Generation;

    #[test]
    fn declaration_inventory_refuses_paths_outside_the_retained_directory_before_binding() {
        let inputs = tempfile::tempdir().unwrap();
        let unrelated = Arc::new(tempfile::tempdir().unwrap());
        let interface = inputs.path().join("Original.hi");
        std::fs::write(&interface, b"interface bytes").unwrap();
        let artifacts = vec![DeclarationArtifact {
            interface: ExactIfaceArtifact {
                unit: "main".into(),
                module: "Original".into(),
                path: interface,
                sha256: sha256(b"interface bytes"),
                requirements: Vec::new(),
            },
            product: None,
        }];
        let error =
            inspect_declaration_artifacts(&artifacts, &[], inputs.path(), unrelated, &mut |_| {
                panic!("unowned inputs must not bind a compiler")
            })
            .unwrap_err();
        assert!(
            matches!(error, CompileError::ExtractFailed(ref message) if message.contains("outside its retained directory"))
        );
    }

    #[test]
    fn authored_certification_refuses_different_source_before_compilation() {
        let root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(1));
        let path = root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "module Tidepool.Session.Lib.G1 where\n").unwrap();
        let error = crate::artifacts::test_support::certify_authored_declaration(
            module,
            &path,
            "module Tidepool.Session.Lib.G1 where\nx = 1\n",
            &[root.path().to_path_buf()],
            root.path(),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("source changed before certification"));
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn typeable_tuple_retains_canonical_package_global_certificate() {
        let root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(1));
        let source = include_str!("../tests/fixtures/typeable-tuple/G1.hs");
        let path = root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        let certified = crate::artifacts::test_support::certify_authored_declaration(
            module,
            &path,
            source,
            &[root.path().to_path_buf()],
            root.path(),
        )
        .expect("the pinned tuple representation binding has exact package evidence");
        assert!(certified
            .introduced_exports()
            .iter()
            .any(|export| { export.head.occurrence == "answer" }));
        assert!(!certified.recovery_products().is_empty());
        let context =
            ExactDeclarationContext::new(&[std::sync::Arc::new(certified)], &[], Vec::new())
                .unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let endpoint = ExtractCmd::new().unwrap().bind().unwrap();
        let request = std::sync::Arc::new(context)
            .prepare_compilation(artifacts.path(), endpoint.identity().producer_bytes())
            .expect("one preparation stage retains the complete certified package closure");
        assert!(
            request
                .groups
                .iter()
                .flat_map(|group| group.imports())
                .any(|import| {
                    matches!(import, crate::certified_products::PendingImportOwner::Package {
                unit, module, binder, ..
            } if unit == "ghc-prim" && module == "GHC.Tuple"
                && binder.namespace == "value" && binder.occurrence == "$tcTuple3")
                }),
            "the real tuple type representation must be certified, not optimized out"
        );
        let interface = &request.artifacts[0].interface.path;
        let saved = std::fs::read(interface).unwrap();
        std::fs::write(interface, b"changed after preparation").unwrap();
        assert!(
            request
                .context()
                .validate_artifacts(&request.artifacts)
                .is_err(),
            "the preparation snapshot does not authorize changed post-worker artifacts"
        );
        std::fs::write(interface, saved).unwrap();
        request
            .context()
            .validate_artifacts(&request.artifacts)
            .unwrap();
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn empty_authored_module_still_has_owned_product_and_inventory() {
        let root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(1));
        let source = "module Tidepool.Session.Lib.G1 where\n";
        let path = root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        let result = crate::artifacts::test_support::certify_authored_declaration(
            module,
            &path,
            source,
            &[root.path().to_path_buf()],
            root.path(),
        )
        .unwrap();
        assert_eq!(result.product.owner().module, module.module_name());
        assert_eq!(result.product.source_sha256(), Some(result.source_sha256));
        assert_eq!(
            result.product.module_interface().unwrap().origin(),
            crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { generation: 1 }
        );
        assert!(result.lexical_exports().is_empty());
        assert!(result.instances.classes.is_empty());
        assert!(result.instances.families.is_empty());
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn authored_declaration_in_exact_context_keeps_its_native_origin() {
        let root = tempfile::tempdir().unwrap();
        let baseline_module = SessionModule::lib(Generation(1));
        let support_source =
            include_str!("../tests/fixtures/checked-prefix-publication/PrefixSelectedSupport.hs")
                .replace(" where\n", " where\nimport PrefixUnrelatedSupport ()\n");
        std::fs::write(root.path().join("PrefixSelectedSupport.hs"), support_source).unwrap();
        std::fs::write(
            root.path().join("PrefixUnrelatedSupport.hs"),
            include_str!("../tests/fixtures/checked-prefix-publication/PrefixUnrelatedSupport.hs"),
        )
        .unwrap();
        let baseline_source = "module Tidepool.Session.Lib.G1 where\nimport PrefixSelectedSupport ()\nbaseline = (40 :: Int)\n{-# NOINLINE baseline #-}\nalternateBaseline = (41 :: Int)\n{-# NOINLINE alternateBaseline #-}\n";
        let baseline_path = root.path().join(baseline_module.relative_hs_path());
        std::fs::create_dir_all(baseline_path.parent().unwrap()).unwrap();
        std::fs::write(&baseline_path, baseline_source).unwrap();
        let baseline = Arc::new(
            crate::artifacts::test_support::certify_authored_declaration(
                baseline_module,
                &baseline_path,
                baseline_source,
                &[root.path().to_path_buf()],
                root.path(),
            )
            .expect("the same compile front door must issue the baseline producer"),
        );
        let selected_support = ExactModuleIdentity {
            unit: "main".into(),
            module: "PrefixSelectedSupport".into(),
        };
        let transitive_support = ExactModuleIdentity {
            unit: "main".into(),
            module: "PrefixUnrelatedSupport".into(),
        };
        let source_surface = baseline.shared_source_lexical_surface(&[]).unwrap();
        assert!(source_surface.lexical.iter().any(|node| {
            node.owner == selected_support && node.imports == [transitive_support.clone()]
        }));
        assert!(source_surface
            .lexical
            .iter()
            .any(|node| node.owner == transitive_support && node.imports.is_empty()));
        for support in [&selected_support, &transitive_support] {
            assert!(baseline
                .artifact_view()
                .descriptors()
                .iter()
                .any(|descriptor| descriptor.owner == *support
                    && descriptor.kind
                        == crate::artifact_inventory::ArtifactKind::CanonicalModuleInterface));
            assert!(baseline
                .recovery_products()
                .iter()
                .filter(|product| {
                    product.owner().unit == support.unit && product.owner().module == support.module
                })
                .all(|product| matches!(
                    product.module_interface().unwrap().origin(),
                    crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
                )));
        }
        let (endpoint, _) = crate::toolchain::bind_extract_endpoint().unwrap();
        let context = Arc::new(
            ExactDeclarationContext::new(
                std::slice::from_ref(&baseline),
                &[],
                vec![ExactLexicalNode {
                    owner: ExactModuleIdentity {
                        unit: baseline.product().owner().unit.clone(),
                        module: baseline_module.module_name(),
                    },
                    imports: Vec::new(),
                }],
            )
            .unwrap(),
        );
        assert_ne!(context.toolchain_identity_sha256(), [0; 32]);
        assert_eq!(
            context.toolchain_identity_sha256(),
            baseline.toolchain_identity_sha256()
        );
        let module = SessionModule::lib(Generation(2));
        let source = "module Tidepool.Session.Lib.G2 where\nimport Tidepool.Session.Lib.G1 (baseline)\nanswer = baseline + 1\n{-# NOINLINE answer #-}\n";
        let path = root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        let result = crate::artifacts::test_support::certify_authored_declaration_in_context(
            module,
            &path,
            source,
            &[root.path().to_path_buf()],
            root.path(),
            context,
        )
        .expect("the authored probe must reach its native origin issuer in an exact context");
        assert_eq!(result.product.owner().module, module.module_name());
        assert_eq!(result.product.source_sha256(), Some(result.source_sha256));
        assert_eq!(
            result.product.module_interface().unwrap().origin(),
            crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { generation: 2 }
        );
        assert_eq!(
            result.toolchain_identity_sha256(),
            baseline.toolchain_identity_sha256()
        );
        assert!(result
            .introduced_exports()
            .iter()
            .any(|export| export.head.occurrence == "answer"));
        assert!(result
            .recovery_products()
            .iter()
            .any(|product| product.owner() == baseline.product().owner()));
        crate::declaration_context::assert_selected_authored_private_inputs(
            &baseline,
            &result,
            baseline_module.gen.0,
            root.path(),
            endpoint.identity().producer_bytes(),
        );
        let result_owner = result.product().owner().clone();
        let result_interface = crate::artifact_inventory::ArtifactEntry::canonical(
            result.product().module_interface().unwrap().clone(),
        )
        .descriptor
        .id;
        let certificates = [baseline, Arc::new(result)];
        let hidden =
            Arc::new(ExactDeclarationContext::new(&certificates, &[], Vec::new()).unwrap());
        let first_root = hidden.authored_native_root(1).unwrap();
        let result_root = hidden.authored_native_root(2).unwrap();
        assert!(hidden
            .compiler_input_roles()
            .iter()
            .any(|role| role.interface() == result_interface && role.original().is_none()));
        let selected_before = hidden.artifact_view().selected_native_groups();
        let dependencies_before = hidden.artifact_view().dependencies();
        let include = [root.path().to_path_buf()];
        let private_offer = crate::artifacts::ModuleCandidateOffer::select_in_context(
            endpoint.identity().producer_bytes(),
            &include,
            &root.path().join("hidden-native-offer"),
            Arc::new(ExactCompileContext::new(Arc::clone(&hidden))),
        )
        .unwrap();
        let packet: ciborium::value::Value = ciborium::de::from_reader(
            std::fs::read(private_offer.exact_scope_path().unwrap())
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        let offered = packet.as_array().unwrap()[6]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row.as_array())
            .filter(|row| {
                row[0].as_text() == Some(&result_owner.unit)
                    && row[1].as_text() == Some(&result_owner.module)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            offered.len(),
            1,
            "hidden G2 must have its exact private native offer"
        );
        let text = |value| ciborium::value::Value::Text(value);
        assert_eq!(
            &offered[0][..5],
            &[
                text(result_owner.unit.clone()),
                text(result_owner.module.clone()),
                text(crate::checked_cell::hex(&result_owner.module_version.0)),
                text(crate::checked_cell::hex(&result_owner.skinny_iface_sha256)),
                text(crate::checked_cell::hex(&result_owner.product_sha256)),
            ]
        );
        assert!(hidden.lexical_graph().is_empty());
        assert_eq!(
            hidden.artifact_view().selected_native_groups(),
            selected_before
        );
        std::fs::remove_file(baseline_path).unwrap();
        std::fs::remove_file(path).unwrap();
        let invocation = crate::artifacts::CompileInvocation {
            source: "module ExactAuthoredConsumer where\nimport Tidepool.Session.Lib.G2 (answer)\nresult = answer\n{-# NOINLINE result #-}\n",
            targets: &["result"], include: &include,
            fallback_module_name: "ExactAuthoredConsumer",
        };
        // Private executable bytes do not authorize a fresh authored import of
        // a hidden owner. Rust retains the typed outer refusal; the diagnostic
        // renders the Haskell source-selection failure and its exact owner.
        let error = match crate::artifacts::test_support::compile_invocation_in_context(
            &invocation,
            Arc::clone(&hidden),
            |_, _, _| {},
        ) {
            Ok(_) => {
                panic!("private native availability must not grant authored lexical authority")
            }
            Err(error) => error,
        };
        let CompileError::InputRejected(diagnostics) = error else {
            panic!("hidden authored import must be an input refusal: {error:?}");
        };
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("source-selection refusal must have one complete diagnostic");
        };
        assert_eq!(diagnostic.severity, crate::diag::DiagnosticSeverity::Error);
        assert!(diagnostic.span.is_none());
        assert_eq!(
            diagnostic.message,
            format!(
                "OriginalSourceSelectionRejected (ExecutionSourceUnavailable ({:?},{:?}))",
                result_owner.unit, result_owner.module,
            )
        );
        assert_eq!(
            hidden.artifact_view().selected_native_groups(),
            selected_before
        );
        assert_eq!(hidden.artifact_view().dependencies(), dependencies_before);

        let context = Arc::new(
            ExactDeclarationContext::new(
                &certificates,
                &[],
                vec![ExactLexicalNode {
                    owner: ExactModuleIdentity {
                        unit: result_owner.unit.clone(),
                        module: result_owner.module.clone(),
                    },
                    imports: Vec::new(),
                }],
            )
            .unwrap(),
        );
        assert_eq!(context.authored_native_root(1).unwrap(), first_root);
        assert_eq!(context.authored_native_root(2).unwrap(), result_root);
        assert!(context
            .compiler_input_roles()
            .iter()
            .any(
                |role| role.interface() == result_interface && role.original() == Some(result_root)
            ));
        assert!(context
            .compiler_input_roles()
            .iter()
            .all(|role| role.original() != Some(first_root)));
        let selected_before = context.artifact_view().selected_native_groups();
        assert!(selected_before
            .iter()
            .any(|group| group.artifact == result_root));
        let compiled = crate::artifacts::test_support::compile_invocation_in_context(
            &invocation,
            Arc::clone(&context),
            |_, _, _| {},
        )
        .expect("issued lexical G2 privately retains its hidden G1 body without source replay");
        assert_eq!(
            context.artifact_view().selected_native_groups(),
            selected_before
        );
        assert_eq!(context.artifact_view().dependencies(), dependencies_before);
        assert!(compiled.targets["result"]
            .pending_imports
            .iter()
            .any(|import| {
                matches!(import, crate::certified_products::PendingImportOwner::Source { owner, .. }
                if owner == &result_owner)
            }));
        assert!(compiled.certified_groups.iter().any(|group| {
            group.owner() == &result_owner
                && group.origin() == crate::certified_products::ProductOrigin::Cached
        }));
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn authored_module_retains_owned_home_import_closure() {
        let root = tempfile::tempdir().unwrap();
        let first = SessionModule::lib(Generation(1));
        let second = SessionModule::lib(Generation(2));
        let first_source = "module Tidepool.Session.Lib.G1 where\nhelper = (41 :: Int)\n";
        let second_source = "module Tidepool.Session.Lib.G2 where\nimport Tidepool.Session.Lib.G1 (helper)\nanswer = helper + 1\n";
        for (module, source) in [(first, first_source), (second, second_source)] {
            let path = root.path().join(module.relative_hs_path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let first_certificate = std::sync::Arc::new(
            crate::artifacts::test_support::certify_authored_declaration(
                first,
                &root.path().join(first.relative_hs_path()),
                first_source,
                &[root.path().to_path_buf()],
                root.path(),
            )
            .unwrap(),
        );
        let independent = crate::artifacts::test_support::certify_authored_declaration(
            second,
            &root.path().join(second.relative_hs_path()),
            second_source,
            &[root.path().to_path_buf()],
            root.path(),
        )
        .unwrap();
        let first_interface = first_certificate
            .product()
            .module_interface()
            .unwrap()
            .clone();
        let independent_interface = independent
            .recovery_products()
            .iter()
            .find(|product| product.owner().module == first.module_name())
            .unwrap()
            .module_interface()
            .unwrap()
            .clone();
        let first_identity =
            crate::artifact_inventory::ArtifactEntry::canonical(first_interface.clone())
                .descriptor
                .id;
        let independent_identity =
            crate::artifact_inventory::ArtifactEntry::canonical(independent_interface)
                .descriptor
                .id;
        assert_ne!(
            first_identity, independent_identity,
            "the refusal control must carry different canonical artifact identities"
        );
        assert!(
            matches!(
                ExactDeclarationContext::new(
                    &[first_certificate.clone(), Arc::new(independent)], &[], Vec::new()
                ),
                Err(CompileError::ArtifactInventory(error))
                    if error.failure == crate::artifact_inventory::ArtifactInventoryFailure::OwnerConflict {
                        owner: ExactModuleIdentity { unit: "main".into(), module: first.module_name() }
                    } && matches!(&error.owner_conflict,
                        Some(crate::artifact_inventory::ArtifactOwnerConflictEvidence::CompilerRole { existing, incoming })
                            if existing.interface() == first_identity && incoming.interface() == independent_identity)
            ),
            "independent compilation cannot replace an already selected canonical owner"
        );
        let context = Arc::new(
            ExactDeclarationContext::new(std::slice::from_ref(&first_certificate), &[], Vec::new())
                .unwrap(),
        );
        let result = crate::artifacts::test_support::certify_authored_declaration_in_context(
            second,
            &root.path().join(second.relative_hs_path()),
            second_source,
            &[root.path().to_path_buf()],
            root.path(),
            context,
        )
        .unwrap();
        assert_eq!(
            result
                .recovery_products()
                .iter()
                .find(|product| product.owner().module == first.module_name())
                .unwrap()
                .module_interface()
                .unwrap()
                .clone(),
            first_interface,
            "the exact-context successor must carry its selected first canonical owner"
        );
        for module in [first, second] {
            assert!(result.recovery_products().iter().any(|product| {
                product.owner().module == module.module_name() && product.source_sha256().is_some()
            }));
        }
        assert!(result.original_home_imports().any(|(owner, imports)| {
            owner.module == second.module_name()
                && imports
                    .iter()
                    .any(|import| import.module == first.module_name())
        }));
        let merged = ExactDeclarationContext::new(
            &[first_certificate, std::sync::Arc::new(result.clone())],
            &[],
            Vec::new(),
        )
        .expect("successive authored certificates retain one exact original owner");
        assert_eq!(merged.recovery_products().len(), 2);
        assert!(result
            .introduced_exports()
            .iter()
            .any(|export| export.head.occurrence == "answer"));
        let durable = tempfile::tempdir().unwrap();
        let references = crate::recovery_artifacts::materialize_certified_products(
            durable.path(),
            result.toolchain_identity_sha256,
            &result.recovery_products(),
        )
        .unwrap();
        for module in [first, second] {
            assert!(references
                .iter()
                .any(|reference| reference.module == module.module_name()));
        }
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn accepted_join_preserves_selected_dependencies_with_historical_interface_custody() {
        use crate::artifact_inventory::{
            ArtifactDependency, ArtifactEntry, ArtifactInventory, CompilerInputProjection,
        };
        use crate::exact_context::{hydrate_context, Materializer};
        use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
        use std::collections::{BTreeMap, BTreeSet};
        let source_root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(1));
        let source = include_str!("../tests/fixtures/owned-declaration/G1.hs");
        let path = source_root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        let certificate = Arc::new(
            crate::artifacts::test_support::certify_authored_declaration(
                module,
                &path,
                source,
                &[source_root.path().to_path_buf()],
                source_root.path(),
            )
            .unwrap(),
        );
        let owner = ExactModuleIdentity {
            unit: certificate.product().owner().unit.clone(),
            module: module.module_name(),
        };
        let context = ExactDeclarationContext::new(
            &[certificate.clone()],
            &[],
            vec![ExactLexicalNode {
                owner: owner.clone(),
                imports: vec![],
            }],
        )
        .unwrap();
        let producer = context.toolchain_identity_sha256();
        // Independent issuances can have the same GHC interface observation
        // while retaining different exact native carrier and Group bindings.
        let mut branch_groups: Vec<(
            crate::artifact_inventory::ArtifactBinding,
            BTreeSet<crate::artifact_inventory::NativeGroupBinding>,
        )> = Vec::new();
        let mut branch_joins = Vec::new();
        let mut interface_observation = None;
        for marker in ["JoinScopeLeft", "JoinScopeRight"] {
            let inventory = ArtifactInventory::default();
            let mut entries = context.artifact_view().entries();
            entries.push(Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    producer,
                    "fixture",
                    marker,
                    BTreeMap::new(),
                ),
            )));
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .expect("independent join scope requires the complete native input closure");
            let branch = ExactDeclarationContext::from_authenticated_execution(
                producer,
                &view,
                context.compiler_input_projection().clone(),
                context.lexical_graph().to_vec(),
                owner.clone(),
                &[],
            )
            .unwrap();
            let graph = branch.artifact_view().capture_graph_selection();
            let original_id = branch.authored_native_root(1).unwrap();
            let carrier = *graph
                .namespace
                .iter()
                .find(|binding| binding.artifact == original_id)
                .unwrap();
            let groups = graph
                .native_groups
                .iter()
                .filter(|group| group.binding.artifact() == original_id)
                .copied()
                .collect::<BTreeSet<_>>();
            assert!(!groups.is_empty());
            if let Some((previous_carrier, previous_groups)) = branch_groups.first() {
                assert_eq!(
                    carrier.artifact, previous_carrier.artifact,
                    "both scopes must carry the same actual native payload"
                );
                assert_ne!(
                    &carrier, previous_carrier,
                    "independent scopes must differ before the second compiler operation"
                );
                assert!(
                    groups.is_disjoint(previous_groups),
                    "same-key Group issuers must differ before testing their retention"
                );
            }
            let scratch = Arc::new(tempfile::tempdir().unwrap());
            let join_inputs = branch.declaration_join_inputs(scratch.clone()).unwrap();
            let observation = join_inputs
                .artifacts()
                .iter()
                .map(|row| {
                    (
                        row.interface.unit.clone(),
                        row.interface.module.clone(),
                        row.interface.sha256.clone(),
                        row.interface.requirements.clone(),
                    )
                })
                .collect::<Vec<_>>();
            assert!(observation.iter().all(|(_, module, _, _)| module != marker));
            match &interface_observation {
                Some(previous) => assert_eq!(&observation, previous),
                None => interface_observation = Some(observation),
            }
            let anchor = join_inputs.interface(&owner).unwrap();
            let spec = DeclarationJoinSpec {
                expected_public_version: "same-interface-observation".into(),
                public_module: None,
                private_base: None,
                private_tip: Some(ModuleSnapshot {
                    module: owner.module.clone(),
                    path: anchor.path.clone(),
                    sha256: anchor.sha256.clone(),
                }),
                writes: vec![],
                reserved: ReservedJoin {
                    unit: "main".into(),
                    module: "SameInterfaceJoin".into(),
                    path: scratch.path().join("joined.hi"),
                },
                expected_exports: certificate.lexical_exports().to_vec(),
                expected_instances: certificate.instances().clone(),
                family_closure: certificate.family_closure().to_vec(),
            };
            let CertifiedDeclarationJoin::Accepted(joined) =
                crate::artifacts::test_support::with_settlement(|settlement| {
                    certify_declaration_join(
                        spec,
                        join_inputs,
                        &[source_root.path().to_path_buf()],
                        source_root.path(),
                        settlement,
                    )
                })
                .unwrap()
            else {
                panic!("same interface observation must pass its actual owner operation")
            };
            drop(branch);
            drop(view);
            drop(inventory);
            let scratch_path = scratch.path().to_path_buf();
            drop(scratch);
            assert!(
                !scratch_path.try_exists().unwrap(),
                "the actual branch request directory must be released"
            );
            let retained = joined.context().artifact_view().capture_graph_selection();
            assert!(retained.namespace.contains(&carrier));
            assert_eq!(
                retained
                    .native_groups
                    .iter()
                    .filter(|group| group.binding.artifact() == original_id)
                    .copied()
                    .collect::<BTreeSet<_>>(),
                groups
            );
            assert_eq!(
                joined.context().authored_native_root(1).unwrap(),
                original_id
            );
            joined.native_binding_custody_requirements().unwrap();
            // Later native demand enters the ordinary closed request and target
            // owner. This is admission, not execution of machine code.
            let endpoint = ExtractCmd::new().unwrap().bind().unwrap();
            let request_root = tempfile::tempdir().unwrap();
            let request = Arc::new(joined.context().clone())
                .prepare_compilation(request_root.path(), endpoint.identity().producer_bytes())
                .unwrap();
            let inputs = request.compiler_inputs();
            let validation = PackageInterfaceValidation::default();
            let selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                    inputs,
                    &inputs.metadata,
                    &validation.inventory,
                )
                .unwrap();
            let chosen = *groups.iter().next().unwrap();
            let original = crate::certified_products::finalize_original_for_inventory(
                certificate.product().clone(),
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap();
            let definitions = crate::certified_products::original_native_group_definitions(
                &original,
                chosen.original_ordinal,
            )
            .unwrap();
            let binder = match &definitions.bindings()[0] {
                tidepool_repr::execution_schema::Group::NonRecursive(binding) => {
                    binding.identity.clone()
                }
                tidepool_repr::execution_schema::Group::Recursive(bindings) => {
                    bindings[0].identity.clone()
                }
            };
            let imports = [crate::certified_products::PendingImportOwner::Source {
                owner: original.owner().clone(),
                original_ordinal: chosen.original_ordinal,
                binder,
            }];
            let mut fixed = inputs
                .artifacts()
                .inventory()
                .bind_segment(inputs.artifacts(), vec![], &selection)
                .unwrap();
            let demanded = inputs
                .artifacts()
                .inventory()
                .admit_bound_target(
                    &mut fixed,
                    inputs.artifacts(),
                    crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(
                        &imports,
                    ),
                    vec![],
                    &selection,
                )
                .unwrap();
            let demanded = demanded.bind_target(None, imports.to_vec());
            let resolved = demanded.resolve().unwrap();
            let actual = resolved.groups();
            assert!(actual.contains(&chosen));
            assert!(
                actual
                    .iter()
                    .filter(|group| group.binding.artifact() == original_id)
                    .all(|group| groups.contains(group)),
                "later demand must consume this accepted owner's exact scoped bindings"
            );
            let mut denied = imports.clone();
            let crate::certified_products::PendingImportOwner::Source {
                original_ordinal, ..
            } = &mut denied[0]
            else {
                unreachable!()
            };
            *original_ordinal = u32::MAX;
            assert!(!groups
                .iter()
                .any(|group| group.original_ordinal == u32::MAX));
            assert!(
                inputs
                    .artifacts()
                    .inventory()
                    .admit_bound_target(
                        &mut fixed,
                        inputs.artifacts(),
                        crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(
                            &denied
                        ),
                        vec![],
                        &selection,
                    )
                    .is_err(),
                "joining interfaces cannot grant an unavailable native ordinal"
            );
            branch_groups.push((carrier, groups));
            branch_joins.push(joined);
        }
        // Both accepted owners stay live: issuing the second cannot replace
        // the first one's equal-payload carrier or exact native Group choices.
        for (joined, (carrier, groups)) in branch_joins.iter().zip(&branch_groups) {
            let retained = joined.context().artifact_view().capture_graph_selection();
            assert!(retained.namespace.contains(carrier));
            assert_eq!(
                retained
                    .native_groups
                    .iter()
                    .filter(|group| group.binding.artifact() == carrier.artifact)
                    .copied()
                    .collect::<BTreeSet<_>>(),
                *groups
            );
        }
        assert_ne!(
            branch_groups[0].0, branch_groups[1].0,
            "equal interface and native bytes must exercise distinct actual carrier scopes"
        );
        assert!(
            branch_groups[0].1.is_disjoint(&branch_groups[1].1),
            "the two actual joins must retain their independently issued native Groups"
        );

        let hidden = ["Expr", owner.module.as_str()].map(|module| {
            Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    producer,
                    "main",
                    module,
                    BTreeMap::new(),
                ),
            ))
        });
        let hidden_ids = hidden
            .iter()
            .map(|entry| entry.descriptor.id)
            .collect::<Vec<_>>();
        let inventory = ArtifactInventory::default();
        let custody = inventory
            .admit_shared(&inventory.empty_view(), hidden.to_vec())
            .unwrap();
        let context = context
            .extend_issued_compiler_support(
                &crate::declaration_context::recover_fixture_compiler_support(
                    CompilerInputProjection::default(),
                    custody,
                    [],
                )
                .unwrap(),
            )
            .unwrap();
        let original_graph = context.artifact_view().capture_graph_selection();
        let original_group_roots = original_graph
            .roots
            .iter()
            .filter(|node| {
                matches!(
                    node,
                    crate::artifact_inventory::ArtifactBindingNode::Group(_)
                )
            })
            .copied()
            .collect::<BTreeSet<_>>();
        assert!(
            !original_group_roots.is_empty(),
            "fixture must exercise native demand"
        );
        assert!(
            !original_graph.native_groups.is_empty(),
            "fixture must retain native bindings"
        );
        let original_offers = context
            .compiler_source_support()
            .native_offer_observations()
            .unwrap();
        assert!(
            !original_offers.is_empty(),
            "fixture must retain issued body availability"
        );
        let offer_snapshots = context
            .compiler_source_support()
            .native_offers()
            .iter()
            .map(|offer| offer.custody_snapshot().clone())
            .collect::<Vec<_>>();

        let worker_root = Arc::new(tempfile::tempdir().unwrap());
        let (join_inputs, native_work) =
            crate::recovery_artifacts::observe_native_materialization(|| {
                context.declaration_join_inputs(worker_root.clone())
            });
        let join_inputs = join_inputs.unwrap();
        assert_eq!(
            native_work,
            (0, 0),
            "join input copied or hashed native products"
        );
        assert!(join_inputs
            .artifacts()
            .iter()
            .all(|artifact| artifact.product.is_none()));
        let artifacts = join_inputs.artifacts().to_vec();
        assert!(artifacts.iter().all(|row| row.interface.module != "Expr"));
        let original = artifacts
            .iter()
            .find(|row| row.interface.module == owner.module)
            .unwrap();
        let input = DeclarationJoinInput {
            expected_public_version: "selected-input-join".into(),
            public_module: None,
            private_base: None,
            private_tip: Some(ModuleSnapshot {
                module: owner.module.clone(),
                path: original.interface.path.clone(),
                sha256: original.interface.sha256.clone(),
            }),
            writes: vec![],
            reserved: ReservedJoin {
                unit: "main".into(),
                module: "SelectedInputJoin".into(),
                path: worker_root.path().join("joined.hi"),
            },
            artifacts,
            family_closure: certificate.family_closure().to_vec(),
            expected_exports: certificate.lexical_exports().to_vec(),
            expected_instances: certificate.instances().clone(),
        };
        let expected_targets = input
            .artifacts
            .iter()
            .map(|artifact| {
                context.compiler_input_projection().snapshot_roles()[&ExactModuleIdentity {
                    unit: artifact.interface.unit.clone(),
                    module: artifact.interface.module.clone(),
                }]
                    .interface()
            })
            .collect::<BTreeSet<_>>();
        let CertifiedDeclarationJoin::Accepted(accepted) =
            crate::artifacts::test_support::with_settlement(|settlement| {
                certify_declaration_join(
                    join_spec(&input),
                    join_inputs,
                    &[source_root.path().to_path_buf()],
                    source_root.path(),
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("expected real joined interface")
        };
        let joined_id = accepted.artifact_id();
        let accepted_support = accepted.compiler_source_support();
        let accepted_graph = accepted_support.artifacts().capture_graph_selection();
        assert_eq!(accepted_graph.native_groups, original_graph.native_groups);
        assert_eq!(
            accepted_graph
                .roots
                .iter()
                .filter(|node| matches!(
                    node,
                    crate::artifact_inventory::ArtifactBindingNode::Group(_)
                ))
                .copied()
                .collect::<BTreeSet<_>>(),
            original_group_roots
        );
        assert!(original_graph
            .selections
            .iter()
            .all(|scope| accepted_graph.selections.contains(scope)));
        assert_eq!(
            accepted_support.native_offer_observations().unwrap(),
            original_offers
        );
        assert_eq!(
            accepted_support.native_offers().len(),
            offer_snapshots.len()
        );
        for (offer, snapshot) in accepted_support
            .native_offers()
            .iter()
            .zip(&offer_snapshots)
        {
            assert!(Arc::ptr_eq(offer.custody_snapshot(), snapshot));
        }
        let joined_binding = *accepted_graph
            .namespace
            .iter()
            .find(|binding| binding.artifact == joined_id)
            .unwrap();
        let expected_bound_edges = original_graph
            .namespace
            .iter()
            .filter(|binding| expected_targets.contains(&binding.artifact))
            .map(|binding| {
                (
                    crate::artifact_inventory::ArtifactBindingNode::Artifact(joined_binding),
                    crate::artifact_inventory::ArtifactBindingNode::Artifact(*binding),
                    ArtifactDependency::Interface,
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(expected_bound_edges.len(), expected_targets.len());
        assert_eq!(
            accepted_support
                .artifacts()
                .binding_dependencies()
                .into_iter()
                .filter(|(from, _, _)| *from
                    == crate::artifact_inventory::ArtifactBindingNode::Artifact(joined_binding))
                .collect::<BTreeSet<_>>(),
            expected_bound_edges
        );

        let lexical = vec![
            ExactLexicalNode {
                owner: owner.clone(),
                imports: vec![],
            },
            ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: "main".into(),
                    module: "SelectedInputJoin".into(),
                },
                imports: vec![owner],
            },
        ];
        let live =
            ExactDeclarationContext::new(&[], &[Arc::new(accepted.clone())], lexical).unwrap();
        assert!(hidden_ids
            .iter()
            .all(|id| live.artifact_view().artifact_ids().contains(id)));
        let live_edges = live
            .artifact_view()
            .interface_dependencies()
            .into_iter()
            .filter(|(from, _, _)| *from == joined_id)
            .collect::<BTreeSet<_>>();
        assert!(!live_edges.is_empty());
        assert_eq!(
            live_edges
                .iter()
                .map(|(_, to, _)| *to)
                .collect::<BTreeSet<_>>(),
            expected_targets
        );

        assert!(live_edges.iter().all(
            |(_, to, kind)| !hidden_ids.contains(to) && *kind == ArtifactDependency::Interface
        ));
        for root in original_graph.roots.iter().filter(|node| {
            matches!(
                node,
                crate::artifact_inventory::ArtifactBindingNode::Group(_)
            )
        }) {
            assert!(live
                .artifact_view()
                .capture_graph_selection()
                .roots
                .contains(root));
        }
        let durable = tempfile::tempdir().unwrap();
        let stored = accepted.materialize(durable.path()).unwrap();
        let durable_edges = stored
            .artifact_dependencies
            .iter()
            .filter(|(from, _, _)| *from == joined_id)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(durable_edges, live_edges);
        assert!(stored
            .module_interfaces
            .iter()
            .any(|reference| reference.interface.module == "Expr"));

        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let receipt = materializer
            .materialize_context(None, &live.snapshot())
            .unwrap();
        for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let receipt = hydrate_context(&store, receipt.root()).unwrap();
        let mut joins = stored.anchors.clone();
        joins.push(stored.join);
        let graph = live.snapshot().graph_selection().unwrap();
        let recovered = crate::declaration_context::RecoveredArtifactInventory::capture_bound(
            durable.path(),
            &stored.products,
            &stored.module_interfaces,
            &joins,
            &stored.value_interfaces,
            &live.artifact_view().descriptors(),
            &graph.bindings,
            &live.artifact_view().binding_dependencies(),
        )
        .unwrap()
        .context_with_snapshot(receipt.snapshot())
        .unwrap();
        assert_eq!(
            recovered
                .artifact_view()
                .interface_dependencies()
                .into_iter()
                .filter(|(from, _, _)| *from == joined_id)
                .collect::<BTreeSet<_>>(),
            live_edges
        );
        assert!(hidden_ids
            .iter()
            .all(|id| recovered.artifact_view().artifact_ids().contains(id)));
        assert_eq!(
            recovered
                .compiler_source_support()
                .native_offer_observations()
                .unwrap(),
            original_offers
        );
        assert_eq!(recovered.semantic_sha256(), live.semantic_sha256());
    }

    struct OwnedJoinRecoveryFixture {
        recovered: ExactDeclarationContext,
        certificate: Arc<CertifiedAuthoredDeclaration>,
        accepted: AcceptedJoin,
        lexical: Vec<ExactLexicalNode>,
        module: SessionModule,
        original_root: crate::artifact_inventory::ArtifactId,
        _durable: tempfile::TempDir,
    }

    fn owned_join_recovery_fixture() -> OwnedJoinRecoveryFixture {
        let source_root = tempfile::tempdir().unwrap();
        let module = SessionModule::lib(Generation(1));
        let source = include_str!("../tests/fixtures/owned-declaration/G1.hs");
        let path = source_root.path().join(module.relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        let certificate = Arc::new(
            crate::artifacts::test_support::certify_authored_declaration(
                module,
                &path,
                source,
                &[source_root.path().to_path_buf()],
                source_root.path(),
            )
            .unwrap(),
        );
        assert_eq!(certificate.instances().classes.len(), 1);
        assert_eq!(
            certificate.instances().classes[0].class.occurrence,
            "Choice"
        );
        assert_eq!(certificate.instances().classes[0].selected_axioms.len(), 1);
        assert_eq!(certificate.instances().families.len(), 2);
        let owner = ExactModuleIdentity {
            unit: certificate.product().owner().unit.clone(),
            module: module.module_name(),
        };
        let context = ExactDeclarationContext::new(
            &[certificate.clone()],
            &[],
            vec![ExactLexicalNode {
                owner: owner.clone(),
                imports: Vec::new(),
            }],
        )
        .unwrap();
        let original_root = context.authored_native_root(1).unwrap();
        let original_entry = context
            .artifact_view()
            .entries()
            .into_iter()
            .find(|entry| {
                matches!(&entry.payload,
                crate::artifact_inventory::ArtifactPayload::Original(product)
                    if product.owner() == certificate.product().owner())
            })
            .unwrap();
        assert_eq!(original_root, original_entry.descriptor.id);
        assert!(
            certificate.product().execution_source().is_none(),
            "native authored originals must not advertise source replay"
        );
        assert!(!certificate.product().product_bytes().is_empty());
        let worker_root = Arc::new(tempfile::tempdir().unwrap());
        let unselected =
            ExactDeclarationContext::new(&[certificate.clone()], &[], Vec::new()).unwrap();
        assert_ne!(context.semantic_sha256(), unselected.semantic_sha256());
        assert_eq!(unselected.authored_native_root(1).unwrap(), original_root);
        let (join_inputs, native_work) =
            crate::recovery_artifacts::observe_native_materialization(|| {
                context.declaration_join_inputs(worker_root.clone())
            });
        let join_inputs = join_inputs.unwrap();
        assert_eq!(
            native_work,
            (0, 0),
            "join input copied or hashed native products"
        );
        assert!(join_inputs
            .artifacts()
            .iter()
            .all(|artifact| artifact.product.is_none()));
        let artifacts = join_inputs.artifacts().to_vec();
        let original = artifacts
            .iter()
            .find(|artifact| artifact.interface.module == owner.module)
            .unwrap();
        let anchor = ModuleSnapshot {
            module: owner.module.clone(),
            path: original.interface.path.clone(),
            sha256: original.interface.sha256.clone(),
        };
        let joined_module = SessionModule::lib(Generation(2)).module_name();
        let input = DeclarationJoinInput {
            expected_public_version: "paired-public-version".into(),
            public_module: None,
            private_base: None,
            private_tip: Some(anchor.clone()),
            writes: vec![DeclarationWrite {
                generation: 1,
                module: anchor,
                exports: certificate.introduced_exports().to_vec(),
                retractions: Vec::new(),
            }],
            reserved: ReservedJoin {
                unit: owner.unit.clone(),
                module: joined_module.clone(),
                path: worker_root.path().join("joined.hi"),
            },
            artifacts,
            family_closure: certificate.family_closure().to_vec(),
            expected_exports: certificate.lexical_exports().to_vec(),
            expected_instances: certificate.instances().clone(),
        };
        let expected_request = sha256(&encode_declaration_join(&input).unwrap());
        let expected_producer = context.toolchain_identity_sha256();
        let mut rejected_input = input.clone();
        rejected_input.reserved.module = SessionModule::lib(Generation(3)).module_name();
        rejected_input.reserved.path = worker_root.path().join("rejected.hi");
        rejected_input.expected_exports[0]
            .head
            .occurrence
            .push_str("Missing");
        let CertifiedDeclarationJoin::Rejected(rejected) =
            crate::artifacts::test_support::with_settlement(|settlement| {
                certify_declaration_join(
                    join_spec(&rejected_input),
                    context
                        .declaration_join_inputs(worker_root.clone())
                        .unwrap(),
                    &[source_root.path().to_path_buf()],
                    source_root.path(),
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("expected fenced rejection")
        };
        assert_eq!(rejected.input(), &rejected_input);
        assert_eq!(rejected.expected_public_version(), "paired-public-version");
        assert_eq!(
            rejected.request_sha256(),
            sha256(&encode_declaration_join(&rejected_input).unwrap())
        );
        assert_eq!(rejected.toolchain_identity_sha256(), expected_producer);
        let CertifiedDeclarationJoin::Accepted(accepted) =
            crate::artifacts::test_support::with_settlement(|settlement| {
                certify_declaration_join(
                    join_spec(&input),
                    join_inputs,
                    &[source_root.path().to_path_buf()],
                    source_root.path(),
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("expected accepted owned join")
        };
        assert_eq!(accepted.request_sha256(), expected_request);
        assert_eq!(accepted.expected_public_version(), "paired-public-version");
        assert_eq!(accepted.toolchain_identity_sha256(), expected_producer);
        assert!(!accepted.package_imports_bytes().is_empty());
        let lexical = vec![ExactLexicalNode {
            owner: ExactModuleIdentity {
                unit: owner.unit,
                module: joined_module,
            },
            imports: Vec::new(),
        }];
        let joined_context = context
            .clone()
            .extend(&[], &[Arc::new(accepted.clone())], lexical.clone())
            .unwrap();
        let descriptors = joined_context.artifact_view().descriptors();
        drop(worker_root);
        drop(source_root);
        let durable = tempfile::tempdir().unwrap();
        let own_join = accepted.materialize_interface(durable.path()).unwrap();
        assert_eq!(
            accepted.artifact_id(),
            crate::artifact_inventory::ArtifactDescriptor::from_recovery_join(&own_join).id
        );
        let stored = accepted.materialize(durable.path()).unwrap();
        assert_eq!(stored.join, own_join);
        let verified =
            crate::recovery_artifacts::verify_materialized_join(durable.path(), &stored.join)
                .unwrap();
        assert_eq!(verified.interface_bytes, accepted.interface_bytes());
        let mut joins = stored.anchors.clone();
        joins.push(stored.join.clone());
        use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
        let store = MemoryBlockstore::new();
        let mut writer = crate::exact_context::Materializer::new(&store);
        let receipt = writer
            .materialize_context(None, &joined_context.snapshot())
            .unwrap();
        for (cid, bytes) in writer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let receipt = crate::exact_context::hydrate_context(&store, receipt.root()).unwrap();
        let snapshot = receipt.snapshot();
        let graph = snapshot.graph_selection().unwrap();
        let inventory = crate::declaration_context::RecoveredArtifactInventory::capture_bound(
            durable.path(),
            &stored.products,
            &stored.module_interfaces,
            &joins,
            &stored.value_interfaces,
            &descriptors,
            &graph.bindings,
            &snapshot.binding_dependencies().unwrap(),
        )
        .unwrap();
        let mut recovered = inventory.context_with_snapshot(snapshot).unwrap();
        recovered.bind_snapshot_baseline(&receipt).unwrap();
        assert_eq!(
            recovered.semantic_sha256(),
            joined_context.semantic_sha256()
        );
        assert_eq!(recovered.toolchain_identity_sha256(), expected_producer);
        assert_eq!(recovered.authored_native_root(1).unwrap(), original_root);
        assert!(certificate.product().source_sha256().is_some());
        assert!(recovered
            .recovery_products()
            .iter()
            .find(|product| product.owner() == certificate.product().owner())
            .unwrap()
            .source_sha256()
            .is_none());
        assert!(recovered.authored_native_root(2).is_err());
        OwnedJoinRecoveryFixture {
            recovered,
            certificate,
            accepted,
            lexical,
            module,
            original_root,
            _durable: durable,
        }
    }

    fn assert_interface_only_recovery(
        fixture: &OwnedJoinRecoveryFixture,
    ) -> RecoveredDeclarationTip {
        let recovered = fixture.recovered.clone();
        let certificate = &fixture.certificate;
        let (recovered_join, native_work) =
            crate::recovery_artifacts::observe_native_materialization(|| {
                certify_recovered_declaration_tip_in_context(
                    Arc::new(recovered.clone()),
                    RecoveryDeclarationSelection {
                        origin: RecoveryDeclarationOrigin::Join,
                        root: recovered.lexical_graph()[0].owner.clone(),
                        lexical: recovered.lexical_graph().to_vec(),
                        exports: certificate.lexical_exports().to_vec(),
                        instances: certificate.instances().clone(),
                        family_closure: certificate.family_closure().to_vec(),
                    },
                    &[],
                )
            });
        let recovered_join = recovered_join.unwrap();
        assert_eq!(
            native_work,
            (0, 0),
            "recovery certifier copied or hashed native bytes"
        );
        let scratch = tempfile::tempdir().unwrap();
        let (old_route, old_work) =
            crate::recovery_artifacts::observe_native_materialization(|| {
                recovered.materialize_scratch(&scratch)
            });
        old_route.unwrap();
        assert!(
            old_work.0 > 0 && old_work.1 > 0,
            "full native scratch must exercise the observed boundary"
        );
        let inspection = Arc::new(recovered.clone())
            .recovery_inspection_inputs()
            .unwrap();
        let path = inspection
            .interface(recovered_join.root())
            .unwrap()
            .path
            .clone();
        crate::artifacts::test_support::with_settlement(|settlement| {
            inspection.inspect(&[], settlement)
        })
        .unwrap();
        let mut clean_closes = 0;
        let mutation = crate::artifacts::test_support::with_settlement(|settlement| {
            inspection.inspect(&[], &mut |close| {
                if matches!(close, tidepool_extract_cmd::CompilerTransactionClose::Clean) {
                    clean_closes += 1;
                    std::fs::write(&path, b"changed during real compiler inspection").unwrap();
                }
                settlement(close);
            })
        });
        assert_eq!(
            clean_closes, 1,
            "mutation requires the real inventory transaction's clean close"
        );
        assert!(
            matches!(mutation,
                Err(CompileError::ExtractFailed(ref diagnostic))
                    if diagnostic == "exact declaration context: recovery inspection interface differs from its issuing context"
            ),
            "inspection must reach its owning interface-diff refusal: {mutation:?}"
        );

        assert_eq!(recovered_join.authored_generation(), None);
        recovered_join
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn recovered_join_inspection_never_materializes_native_products() {
        assert_interface_only_recovery(&owned_join_recovery_fixture());
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn owned_join_retains_original_products_and_recovers_after_all_request_scratch_is_deleted() {
        let fixture = owned_join_recovery_fixture();
        assert_interface_only_recovery(&fixture);
        let OwnedJoinRecoveryFixture {
            recovered,
            certificate,
            accepted,
            lexical,
            module,
            original_root,
            _durable,
        } = fixture;
        let recovered_identity = recovered.semantic_sha256();
        let selected_instances = certificate.instances().clone();
        let extended = recovered
            .extend(&[certificate], &[Arc::new(accepted)], lexical)
            .unwrap();
        assert_eq!(extended.semantic_sha256(), recovered_identity);
        assert_eq!(extended.authored_native_root(1).unwrap(), original_root);
        assert!(extended
            .recovery_products()
            .iter()
            .find(|product| product.owner().module == module.module_name())
            .unwrap()
            .source_sha256()
            .is_none());
        assert!(extended.authored_native_root(2).is_err());
        let extended = Arc::new(extended);
        let fresh_root = tempfile::tempdir().unwrap();
        let fresh_module = SessionModule::lib(Generation(3));
        let fresh_source = include_str!("../tests/fixtures/owned-declaration/G3.hs");
        let fresh_path = fresh_root.path().join(fresh_module.relative_hs_path());
        std::fs::create_dir_all(fresh_path.parent().unwrap()).unwrap();
        std::fs::write(&fresh_path, fresh_source).unwrap();
        let fresh_certificate = Arc::new(
            crate::artifacts::test_support::certify_authored_declaration_in_context(
                fresh_module,
                &fresh_path,
                fresh_source,
                &[fresh_root.path().to_path_buf()],
                fresh_root.path(),
                extended.clone(),
            )
            .unwrap(),
        );
        assert_eq!(fresh_certificate.introduced_exports().len(), 1);
        assert_eq!(
            fresh_certificate.introduced_exports()[0].head.occurrence,
            "freshAnswer"
        );
        assert!(fresh_certificate
            .lexical_exports()
            .iter()
            .any(|export| export.head.occurrence == "answer"
                && export.head.module == module.module_name()));
        let fresh_owner = ExactModuleIdentity {
            unit: fresh_certificate.product().owner().unit.clone(),
            module: fresh_module.module_name(),
        };
        let joined_owner = ExactModuleIdentity {
            unit: fresh_owner.unit.clone(),
            module: SessionModule::lib(Generation(2)).module_name(),
        };
        assert!(fresh_certificate
            .original_home_imports()
            .any(|(owner, imports)| owner == &fresh_owner && imports == [joined_owner.clone()]));
        let next_context = ExactDeclarationContext::new(
            &[fresh_certificate.clone()],
            &[],
            vec![
                ExactLexicalNode {
                    owner: joined_owner.clone(),
                    imports: Vec::new(),
                },
                ExactLexicalNode {
                    owner: fresh_owner.clone(),
                    imports: vec![joined_owner.clone()],
                },
            ],
        )
        .unwrap();
        assert_eq!(next_context.joined_interfaces().len(), 1);
        assert_eq!(next_context.authored_native_root(1).unwrap(), original_root);
        assert_ne!(next_context.authored_native_root(3).unwrap(), original_root);
        let recovery_selection = |generation| RecoveryDeclarationSelection {
            origin: RecoveryDeclarationOrigin::Authored {
                generation,
                introduced_exports: fresh_certificate.introduced_exports().to_vec(),
            },
            root: fresh_owner.clone(),
            lexical: next_context.lexical_graph().to_vec(),
            exports: fresh_certificate.lexical_exports().to_vec(),
            instances: fresh_certificate.instances().clone(),
            family_closure: fresh_certificate.family_closure().to_vec(),
        };
        let mut not_a_join = recovery_selection(3);
        not_a_join.origin = RecoveryDeclarationOrigin::Join;
        let wrong_role = certify_recovered_declaration_tip_in_context(
            Arc::new(next_context.clone()),
            not_a_join,
            &[fresh_root.path().to_path_buf()],
        )
        .unwrap_err();
        assert!(wrong_role
            .to_string()
            .contains("recovered join root has no certified lexical-join role"));
        let wrong_original = certify_recovered_declaration_tip_in_context(
            Arc::new(next_context.clone()),
            recovery_selection(1),
            &[fresh_root.path().to_path_buf()],
        )
        .unwrap_err();
        assert!(wrong_original
            .to_string()
            .contains("recovered authored delta differs from its original compiler inventory"));
        let recovered_fresh = certify_recovered_declaration_tip_in_context(
            Arc::new(next_context.clone()),
            recovery_selection(3),
            &[fresh_root.path().to_path_buf()],
        )
        .unwrap();
        assert_eq!(recovered_fresh.authored_generation(), Some(3));
        assert_eq!(recovered_fresh.root(), &fresh_owner);
        for original in extended.recovery_products() {
            let retained = next_context
                .recovery_products()
                .into_iter()
                .find(|product| product.owner() == original.owner())
                .unwrap();
            assert_eq!(retained.product_bytes(), original.product_bytes());
            assert_eq!(retained.interface_bytes(), original.interface_bytes());
        }
        let next_worker = Arc::new(tempfile::tempdir().unwrap());
        let next_join_inputs = next_context
            .declaration_join_inputs(next_worker.clone())
            .unwrap();
        let next_artifacts = next_join_inputs.artifacts().to_vec();
        let snapshot = |owner: &ExactModuleIdentity| {
            let artifact = next_artifacts
                .iter()
                .find(|artifact| {
                    artifact.interface.unit == owner.unit
                        && artifact.interface.module == owner.module
                })
                .unwrap();
            ModuleSnapshot {
                module: owner.module.clone(),
                path: artifact.interface.path.clone(),
                sha256: artifact.interface.sha256.clone(),
            }
        };
        let next_input = DeclarationJoinInput {
            expected_public_version: "next-paired-version".into(),
            public_module: Some(snapshot(&joined_owner)),
            private_base: Some(snapshot(&joined_owner)),
            private_tip: Some(snapshot(&fresh_owner)),
            writes: vec![DeclarationWrite {
                generation: 3,
                module: snapshot(&fresh_owner),
                exports: fresh_certificate.introduced_exports().to_vec(),
                retractions: Vec::new(),
            }],
            reserved: ReservedJoin {
                unit: fresh_owner.unit.clone(),
                module: SessionModule::lib(Generation(4)).module_name(),
                path: next_worker.path().join("joined-next.hi"),
            },
            artifacts: next_artifacts,
            family_closure: fresh_certificate.family_closure().to_vec(),
            expected_exports: fresh_certificate.lexical_exports().to_vec(),
            expected_instances: selected_instances,
        };
        let CertifiedDeclarationJoin::Accepted(next_join) =
            crate::artifacts::test_support::with_settlement(|settlement| {
                certify_declaration_join(
                    join_spec(&next_input),
                    next_join_inputs,
                    &[fresh_root.path().to_path_buf()],
                    fresh_root.path(),
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("expected second accepted join after source-hidden authored certification")
        };
        assert_eq!(next_join.context().joined_interfaces().len(), 1);
        drop(fresh_root);
        drop(next_worker);
        let second_durable = tempfile::tempdir().unwrap();
        next_join.materialize(second_durable.path()).unwrap();
        let next_join = Arc::new(next_join);
        let downstream_context = Arc::new(
            ExactDeclarationContext::new(
                &[],
                &[next_join.clone()],
                vec![ExactLexicalNode {
                    owner: ExactModuleIdentity {
                        unit: fresh_owner.unit.clone(),
                        module: SessionModule::lib(Generation(4)).module_name(),
                    },
                    imports: Vec::new(),
                }],
            )
            .unwrap(),
        );
        let downstream = include_str!("../tests/fixtures/owned-declaration/ExactConsumer.hs");
        let source_free = tempfile::tempdir().unwrap();
        let include = [source_free.path().to_path_buf()];
        let compiled = crate::artifacts::test_support::compile_invocation_in_context(
            &crate::artifacts::CompileInvocation {
                source: downstream,
                targets: &["downstream"],
                include: &include,
                fallback_module_name: "ExactConsumer",
            },
            downstream_context,
            |_, _, _| {},
        )
        .unwrap();
        assert!(compiled.targets.contains_key("downstream"));
        for original in next_join.recovery_products() {
            for group in compiled.certified_groups.iter().filter(|group| {
                group.owner().unit == original.owner().unit
                    && group.owner().module == original.owner().module
            }) {
                assert_eq!(group.owner(), original.owner());
            }
            assert!(compiled
                .recovery_products
                .iter()
                .any(|product| product.owner() == original.owner()
                    && product.interface_bytes() == original.interface_bytes()
                    && product.product_bytes() == original.product_bytes()));
        }
        let admitted = compiled
            .exact_source_admission
            .as_ref()
            .expect("frontdoor must retain exact source admission");
        assert!(admitted
            .witness()
            .matches_source(admitted.witness().source_path(), downstream));
        assert!(admitted.evidence().valid(downstream));
        let mut raw_evidence: serde_json::Value =
            serde_json::from_slice(&admitted.evidence_bytes()).unwrap();
        raw_evidence["cache_safe"] = false.into();
        raw_evidence["selection_complete"] = false.into();
        admitted
            .validate_ineligible_evidence(&serde_json::to_vec(&raw_evidence).unwrap())
            .unwrap();
        raw_evidence["cache_safe"] = true.into();
        raw_evidence["selection_complete"] = true.into();
        assert!(admitted
            .validate_ineligible_evidence(&serde_json::to_vec(&raw_evidence).unwrap())
            .is_err());
        let mut ordinary_evidence = admitted.evidence().as_ref().clone().into_evidence();
        ordinary_evidence.cache_safe = false;
        ordinary_evidence.selection_complete = false;
        assert!(!ordinary_evidence.valid(downstream));
        let next_worker = Arc::new(tempfile::tempdir().unwrap());
        let exact = extended.materialize(next_worker.path()).unwrap();
        extended.validate_artifacts(&exact.artifacts).unwrap();
        let mut corrupted = exact.artifacts.clone();
        corrupted.pop();
        assert!(extended.validate_artifacts(&corrupted).is_err());
        let artifact = &exact.artifacts[0];
        std::fs::write(&artifact.interface.path, b"changed").unwrap();
        assert!(extended.validate_artifacts(&exact.artifacts).is_err());
    }
}
