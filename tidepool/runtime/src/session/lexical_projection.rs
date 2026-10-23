//! Compiler-issued declaration projections. Logical authored generations keep
//! their original native owners; lexical interfaces carry the selected groups.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_repr::Generation;
use tidepool_toolchain::declaration_join::{
    certify_declaration_join, AcceptedJoin, CertifiedAuthoredDeclaration, CertifiedDeclarationJoin,
    DeclarationExport, DeclarationJoinSpec, ExactDeclarationContext, ExactModuleIdentity,
    ExportIdentity, InstanceInventory, ModuleSnapshot, PublicationComposition, ReservedJoin,
};

use super::render::AdmittedDeclarationSurface;
use super::{DeclarationRetraction, SessionError, SessionLib};

/// A compiler-certified lexical interface together with the exact original
/// implementation closure. Clones retain the same issued interface and bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertifiedDeclarationProjection {
    composition: PublicationComposition,
}

impl CertifiedDeclarationProjection {
    pub fn module_name(&self) -> &str {
        &self.composition.join().reserved().module
    }
    pub fn context(&self) -> &Arc<ExactDeclarationContext> {
        self.composition.context()
    }
    pub fn receipt(&self) -> &Arc<AcceptedJoin> {
        self.composition.join()
    }
    pub(crate) fn composition(&self) -> &PublicationComposition {
        &self.composition
    }
}

/// Immutable selected declaration facts captured under their runtime owner.
/// Artifact presence never constructs this lexical selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeclarationProjectionBaseline {
    pub(super) generation: Generation,
    pub(super) owner: ExactModuleIdentity,
    pub(super) context: Arc<ExactDeclarationContext>,
    pub(super) surface: AdmittedDeclarationSurface,
    pub(super) exports: Vec<DeclarationExport>,
    pub(super) instances: InstanceInventory,
    pub(super) families: Vec<ExportIdentity>,
}

impl DeclarationProjectionBaseline {
    pub(super) fn capture(
        lib: &SessionLib,
        generation: Generation,
    ) -> Result<Option<Self>, SessionError> {
        Ok(
            super::paired_publication::tip(lib, generation)?.map(|tip| Self {
                generation: tip.generation,
                owner: tip.owner,
                context: tip.context,
                surface: tip.surface,
                exports: tip.exports,
                instances: tip.instances,
                families: tip.families,
            }),
        )
    }
}

/// The immutable native declaration and its separately issued lexical view.
/// Preparation performs compiler work; adoption only validates and installs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PreparedAuthoredDeclaration {
    pub(super) generation: Generation,
    pub(super) parent: Generation,
    pub(super) evidence: Arc<CertifiedAuthoredDeclaration>,
    pub(super) projection: Arc<CertifiedDeclarationProjection>,
    pub(super) surface: AdmittedDeclarationSurface,
}

impl PreparedAuthoredDeclaration {
    pub(super) fn next_baseline(&self) -> DeclarationProjectionBaseline {
        let owner = self.evidence.product().owner();
        DeclarationProjectionBaseline {
            generation: self.generation,
            owner: ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            },
            context: self.projection.context().clone(),
            surface: self.surface.clone(),
            exports: self.projection.receipt().exports().to_vec(),
            instances: self.projection.receipt().instances().clone(),
            families: self.projection.receipt().family_closure().to_vec(),
        }
    }
}

pub(super) fn prepare_authored_projection(
    baseline: Option<&DeclarationProjectionBaseline>,
    generation: Generation,
    evidence: Arc<CertifiedAuthoredDeclaration>,
    retractions: &[DeclarationRetraction],
    includes: &[PathBuf],
    root: &Path,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Arc<PreparedAuthoredDeclaration>, SessionError> {
    let original = evidence.product().owner();
    if original.unit != "main"
        || original.module != tidepool_repr::SessionModule::lib(generation).module_name()
    {
        return Err(SessionError::StaleStagedDeclaration);
    }
    let owner = ExactModuleIdentity {
        unit: original.unit.clone(),
        module: original.module.clone(),
    };
    let exports = super::render::select_authored_exports(
        baseline
            .map(|prior| prior.exports.as_slice())
            .unwrap_or_default(),
        retractions,
        evidence.introduced_exports(),
    );
    let instances = super::paired_publication::merge_instances(
        root,
        baseline
            .map(|prior| prior.instances.clone())
            .unwrap_or_default(),
        evidence.instances(),
    )?;
    let mut families = baseline
        .map(|prior| prior.families.clone())
        .unwrap_or_default();
    families.extend_from_slice(evidence.family_closure());
    families.sort();
    families.dedup();
    let surface = evidence.extend_source_surface(
        &baseline
            .map(|prior| prior.surface.clone())
            .unwrap_or_default(),
    )?;
    let context = baseline
        .map(|prior| (*prior.context).clone())
        .unwrap_or_else(ExactDeclarationContext::empty)
        .admit_authored_publication(std::slice::from_ref(&evidence), &surface)?;
    let projection = issue_projection(
        &context, &owner, &surface, &exports, &instances, &families, includes, root, settlement,
    )?;
    Ok(Arc::new(PreparedAuthoredDeclaration {
        generation,
        parent: baseline
            .map(|prior| prior.generation)
            .unwrap_or(Generation(0)),
        evidence,
        projection,
        surface,
    }))
}

/// Both cumulative private views and selected actor membranes use this issuer.
/// The compiler verifies exact export groups and the full instance/family closure.
#[allow(clippy::too_many_arguments)]
pub(super) fn issue_projection(
    context: &ExactDeclarationContext,
    original_tip: &ExactModuleIdentity,
    surface: &AdmittedDeclarationSurface,
    exports: &[DeclarationExport],
    instances: &InstanceInventory,
    families: &[ExportIdentity],
    includes: &[PathBuf],
    root: &Path,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Arc<CertifiedDeclarationProjection>, SessionError> {
    let mut exports = exports.to_vec();
    for export in &mut exports {
        export.children.sort();
    }
    exports.sort_by(|left, right| left.head.cmp(&right.head));
    let encoded = serde_json::to_vec(&(
        context.semantic_sha256(),
        surface.roots(),
        surface.lexical_graph(),
        &exports,
        instances,
        families,
    ))
    .expect("projection identities contain only serializable scalar fields");
    let mut hash = blake3::Hasher::new();
    hash.update(b"tidepool-certified-declaration-projection-v1\0");
    hash.update(&encoded);
    let digest = hash.finalize().to_hex().to_string();
    let module = format!("Tidepool.Actor.Surface.H{digest}");
    let scratch = Arc::new(tempfile::tempdir()?);
    let join_inputs = context.declaration_join_inputs(Arc::clone(&scratch))?;
    let original = join_inputs
        .interface(original_tip)
        .ok_or(SessionError::StaleStagedDeclaration)?;
    let input = DeclarationJoinSpec {
        expected_public_version: digest,
        public_module: None,
        private_base: None,
        private_tip: Some(ModuleSnapshot {
            module: original.module.clone(),
            path: original.path.clone(),
            sha256: original.sha256.clone(),
        }),
        writes: Vec::new(),
        reserved: ReservedJoin {
            unit: "main".into(),
            module,
            path: scratch.path().join("projection.hi"),
        },
        family_closure: families.to_vec(),
        expected_exports: exports,
        expected_instances: instances.clone(),
    };
    let receipt = match certify_declaration_join(input, join_inputs, includes, root, settlement)? {
        CertifiedDeclarationJoin::Accepted(receipt) => Arc::new(receipt),
        CertifiedDeclarationJoin::Rejected(rejected) => {
            return Err(SessionError::Compile(crate::CompileError::ExtractFailed(
                format!(
                    "declaration projection rejected: {:?}",
                    rejected.outcome().decision
                ),
            )))
        }
    };
    let composition = context.with_joined_publication(&receipt, surface)?;
    Ok(Arc::new(CertifiedDeclarationProjection { composition }))
}
