//! Owned execution intent and compiler receipts for paired publication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_codegen::binding_table::SourceLeaseKey;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionModule, SessionVarId};
use tidepool_toolchain::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, CertifiedDeclarationJoin, DeclarationExport,
    DeclarationJoinInput, DeclarationWrite, ExactDeclarationContext, ExactLexicalNode,
    ExactModuleIdentity, ExportIdentity, InstanceInventory, JoinDecision, JoinRejection,
    ModuleSnapshot, RejectedJoin, ReservedJoin,
};

use super::persistent::PersistentSession;
use super::render::{
    extend_exports_by_head, AdmittedDeclarationSurface, DeclTurn, JoinedDeclaration,
};
use super::{
    recovery, DeclarationSource, PublicManifestBase, PublicVisibilitySnapshot, RecoveryPublicOwner,
    SessionError, SessionLib, StagedPublicManifest,
};

#[derive(Clone)]
struct DeclarationTip {
    generation: Generation,
    turn: DeclTurn,
    context: Arc<ExactDeclarationContext>,
    surface: AdmittedDeclarationSurface,
    exports: Vec<DeclarationExport>,
    instances: InstanceInventory,
    families: Vec<ExportIdentity>,
}

#[derive(Clone)]
struct AuthoredWrite {
    generation: Generation,
    turn: DeclTurn,
    evidence: Arc<CertifiedAuthoredDeclaration>,
    retractions: Vec<ExportIdentity>,
}

/// Immutable final execution writes. A public retry changes only its merge
/// baseline, never this private suffix, exact winners, leases, or reserved ID.
/// The private scope keeps live roots owned until publication or abandonment.
pub struct FinalExecutionIntent {
    admitted: PublicVisibilitySnapshot,
    private: PublicVisibilitySnapshot,
    private_base: Option<DeclarationTip>,
    writes: Vec<AuthoredWrite>,
    write_ids: Vec<SessionVarId>,
    source_keys: Vec<SourceLeaseKey>,
    reserved: Generation,
}

/// One current public graph paired with the unchanged final execution intent.
pub struct DeclarationPublicationBase {
    public: PublicManifestBase,
    reserved: Generation,
    intent: Arc<FinalExecutionIntent>,
    current_public: Option<DeclarationTip>,
    surface: AdmittedDeclarationSurface,
    includes: Vec<PathBuf>,
    session_root: PathBuf,
    expected_public_version: String,
}

pub enum CertifiedDeclarationPublication {
    Accepted(AcceptedDeclarationPublication),
    Rejected(RejectedDeclarationPublication),
}

pub struct AcceptedDeclarationPublication {
    base: DeclarationPublicationBase,
    receipt: Arc<AcceptedJoin>,
}

pub struct RejectedDeclarationPublication {
    base: DeclarationPublicationBase,
    receipt: RejectedJoin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclarationPublicationRejection {
    Stale,
    Rejected {
        reason: JoinRejection,
        diagnostic: String,
    },
}

pub(super) struct PreparedDeclarationPublication {
    pub(super) generation: Generation,
    pub(super) joined: JoinedDeclaration,
    pub(super) declared_names: Vec<String>,
}

fn invalid_at(path: &Path, detail: impl Into<String>) -> SessionError {
    SessionError::RecoveryManifest {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn invalid(base: &PublicManifestBase, detail: impl Into<String>) -> SessionError {
    invalid_at(&base.path, detail)
}

fn module_owner(certificate: &CertifiedAuthoredDeclaration) -> ExactModuleIdentity {
    let owner = certificate.product().owner();
    ExactModuleIdentity {
        unit: owner.unit.clone(),
        module: owner.module.clone(),
    }
}

fn tip(lib: &SessionLib, generation: Generation) -> Result<Option<DeclarationTip>, SessionError> {
    if generation == Generation(0) {
        return Ok(None);
    }
    let turn = lib
        .log
        .turn(generation)
        .ok_or(SessionError::StaleStagedDeclaration)?
        .clone();
    let context = lib
        .log
        .joined_context_at(generation)
        .ok_or_else(|| invalid_at(&lib.root, "declaration tip lacks a protected exact context"))?;
    let surface = lib
        .log
        .admitted_surface_at(generation)
        .ok_or_else(|| {
            invalid_at(
                &lib.root,
                "declaration tip lacks its admitted lexical surface",
            )
        })?
        .clone();
    let (exports, instances, families) = if let Some(joined) = lib.log.joined_at(generation) {
        (
            joined.evidence.exports().to_vec(),
            joined.evidence.instances().clone(),
            joined.evidence.family_closure().to_vec(),
        )
    } else if let Some(authored) = lib.log.certified_authored_at(generation) {
        (
            authored.lexical_exports().to_vec(),
            authored.instances().clone(),
            authored.family_closure().to_vec(),
        )
    } else {
        return Err(invalid_at(
            &lib.root,
            "declaration tip has no compiler certificate",
        ));
    };
    Ok(Some(DeclarationTip {
        generation,
        turn,
        context,
        surface,
        exports,
        instances,
        families,
    }))
}

fn snapshot_digest(snapshot: &PublicVisibilitySnapshot) -> serde_json::Value {
    serde_json::json!({
        "scope": snapshot.scope.0, "epoch": snapshot.epoch,
        "declaration_tip": snapshot.declaration_tip.0,
        "machine_incarnation": snapshot.machine_incarnation.map(|id| id.0),
        "bindings": snapshot.bindings.iter().map(|(name, id)| (name, id.raw())).collect::<Vec<_>>(),
        "source_instances": snapshot.source_instances.iter().map(|key| {
            let identity = &key.binder.binder;
            serde_json::json!({"instance": key.instance.raw(), "module_version": key.binder.version.0,
                "unit": identity.unit, "module": identity.module, "namespace": identity.namespace,
                "occurrence": identity.occurrence, "record_parent": identity.record_parent})
        }).collect::<Vec<_>>(),
    })
}

fn paired_version(base: &PublicManifestBase) -> String {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "tidepool-paired-declaration-baseline-v2", "session": base.session.0,
        "owner": base.owner, "graph": base.graph.checksum, "high_water": base.graph.high_water.0,
        "public": snapshot_digest(&base.expected_public), "private": snapshot_digest(&base.expected_private),
    })).expect("paired baseline contains serializable scalar identities");
    blake3::hash(&bytes).to_hex().to_string()
}

fn merge_surface(
    path: &Path,
    mut prior: AdmittedDeclarationSurface,
    addition: &AdmittedDeclarationSurface,
) -> Result<AdmittedDeclarationSurface, SessionError> {
    prior.roots.extend_from_slice(&addition.roots);
    prior.roots.sort();
    prior.roots.dedup();
    let mut lexical = prior
        .lexical
        .into_iter()
        .map(|node| (node.owner, node.imports))
        .collect::<BTreeMap<_, _>>();
    for node in &addition.lexical {
        if lexical
            .get(&node.owner)
            .is_some_and(|edges| edges != &node.imports)
        {
            return Err(invalid_at(
                path,
                "admitted surface owner has conflicting original import edges",
            ));
        }
        lexical.insert(node.owner.clone(), node.imports.clone());
    }
    prior.lexical = lexical
        .into_iter()
        .map(|(owner, imports)| ExactLexicalNode { owner, imports })
        .collect();
    Ok(prior)
}

fn extend_admitted_surface(
    path: &Path,
    authored: &CertifiedAuthoredDeclaration,
    prior: AdmittedDeclarationSurface,
) -> Result<AdmittedDeclarationSurface, SessionError> {
    let imports = authored
        .original_home_imports()
        .map(|(owner, imports)| (owner.clone(), imports))
        .collect::<BTreeMap<_, _>>();
    let original = module_owner(authored);
    let roots = imports.get(&original).ok_or_else(|| {
        invalid_at(
            path,
            "authored declaration lacks exact original source import evidence",
        )
    })?;
    // Session interfaces are implementation anchors. Only explicitly admitted
    // shared source imports become roots of the virtual lexical graph.
    let roots = roots
        .iter()
        .filter(|owner| !owner.module.starts_with("Tidepool.Session."))
        .cloned()
        .collect::<Vec<_>>();
    let mut pending = roots.clone();
    let mut lexical = BTreeMap::new();
    while let Some(owner) = pending.pop() {
        if lexical.contains_key(&owner) {
            continue;
        }
        let edges = imports
            .get(&owner)
            .ok_or_else(|| {
                invalid_at(
                    path,
                    "admitted surface root lacks exact original source import evidence",
                )
            })?
            .to_vec();
        if edges
            .iter()
            .any(|edge| edge.module.starts_with("Tidepool.Session."))
        {
            return Err(invalid_at(
                path,
                "shared source surface depends on an unselected session module",
            ));
        }
        pending.extend(edges.clone());
        lexical.insert(owner, edges);
    }
    merge_surface(
        path,
        prior,
        &AdmittedDeclarationSurface {
            roots,
            lexical: lexical
                .into_iter()
                .map(|(owner, imports)| ExactLexicalNode { owner, imports })
                .collect(),
        },
    )
}

pub(super) fn authored_context(
    lib: &SessionLib,
    parent: Generation,
    certificate: &CertifiedAuthoredDeclaration,
) -> Result<(Arc<ExactDeclarationContext>, AdmittedDeclarationSurface), SessionError> {
    let prior = tip(lib, parent)?;
    let surface = extend_admitted_surface(
        &lib.root,
        certificate,
        prior
            .as_ref()
            .map(|tip| tip.surface.clone())
            .unwrap_or_default(),
    )?;
    let mut lexical = surface.lexical.clone();
    lexical.push(ExactLexicalNode {
        owner: module_owner(certificate),
        imports: surface.roots.clone(),
    });
    let certificate = Arc::new(certificate.clone());
    let context = if let Some(prior) = prior {
        (*prior.context)
            .clone()
            .extend(&[certificate], &[], lexical)?
    } else {
        ExactDeclarationContext::new(&[certificate], &[], lexical)?
    };
    Ok((Arc::new(context), surface))
}

pub(super) fn exact_retractions(
    lib: &SessionLib,
    parent: Generation,
    names: &[String],
) -> Result<Vec<ExportIdentity>, SessionError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let parent = tip(lib, parent)?
        .ok_or_else(|| invalid_at(&lib.root, "retraction has no exact declaration baseline"))?;
    names
        .iter()
        .map(|name| {
            parent
                .exports
                .iter()
                .find(|export| &export.head.occurrence == name)
                .map(|export| export.head.clone())
                .ok_or_else(|| invalid_at(&lib.root, "retraction lacks its selected exact head"))
        })
        .collect()
}

pub(super) fn recovery_instances(
    instances: &InstanceInventory,
    closure: &[ExportIdentity],
) -> recovery::RecoveryInstanceInventory {
    recovery::RecoveryInstanceInventory {
        classes: instances
            .classes
            .iter()
            .map(|instance| recovery::RecoveryInstanceEvidence {
                dfun: super::authored_identity(&instance.dfun).expect("compiler identity"),
                class: super::authored_identity(&instance.class).expect("compiler identity"),
                selected: true,
                selected_axioms: instance
                    .selected_axioms
                    .iter()
                    .map(|axiom| super::authored_identity(axiom).expect("compiler identity"))
                    .collect(),
            })
            .collect(),
        selected_family_axioms: instances
            .families
            .iter()
            .map(|axiom| super::authored_identity(axiom).expect("compiler identity"))
            .collect(),
        family_consistency_closure: closure
            .iter()
            .map(|axiom| super::authored_identity(axiom).expect("compiler identity"))
            .collect(),
    }
}

fn merge_instances(
    path: &Path,
    mut selected: InstanceInventory,
    addition: &InstanceInventory,
) -> Result<InstanceInventory, SessionError> {
    for instance in &addition.classes {
        if let Some(prior) = selected
            .classes
            .iter()
            .find(|prior| prior.dfun == instance.dfun)
        {
            if prior != instance {
                return Err(invalid_at(
                    path,
                    "exact dfun has conflicting class or associated-family evidence",
                ));
            }
        } else {
            selected.classes.push(instance.clone());
        }
    }
    selected.classes.sort();
    selected.families.extend_from_slice(&addition.families);
    selected.families.sort();
    selected.families.dedup();
    Ok(selected)
}

impl FinalExecutionIntent {
    pub fn reserved_generation(&self) -> Generation {
        self.reserved
    }
    pub fn admitted_public(&self) -> &PublicVisibilitySnapshot {
        &self.admitted
    }
    pub fn private_scope(&self) -> ScopeId {
        self.private.scope
    }
}

impl PersistentSession {
    /// Freeze the execution's final current writes once. Historical binding
    /// writes shadowed within the same execution do not become final winners.
    pub fn freeze_execution_intent(
        &mut self,
        admitted: &PublicVisibilitySnapshot,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
    ) -> Result<Arc<FinalExecutionIntent>, SessionError> {
        if private_scope == admitted.scope || !self.scope_tree().is_live(private_scope) {
            return Err(SessionError::DeadScope(private_scope));
        }
        let private = self
            .public_visibility_snapshot_in(private_scope)
            .ok_or(SessionError::MissingDeclarationLibrary)?;
        let lib = self.lib();
        let chain = lib.log.chain_from_root(private.declaration_tip);
        let start = if admitted.declaration_tip == Generation(0) {
            0
        } else {
            chain
                .iter()
                .position(|generation| *generation == admitted.declaration_tip)
                .ok_or_else(|| {
                    invalid_at(
                        &lib.root,
                        "private declaration suffix does not descend from its admitted base",
                    )
                })?
                + 1
        };
        let private_base = tip(lib, admitted.declaration_tip)?;
        let mut writes = Vec::new();
        for generation in &chain[start..] {
            let evidence = lib
                .log
                .certified_authored_arc_at(*generation)
                .ok_or_else(|| {
                    invalid_at(
                        &lib.root,
                        "private suffix contains an uncertified authored declaration",
                    )
                })?;
            let turn = lib
                .log
                .turn(*generation)
                .ok_or(SessionError::StaleStagedDeclaration)?
                .clone();
            let retractions =
                exact_retractions(lib, turn.parent.unwrap_or(Generation(0)), &turn.retracts)?;
            writes.push(AuthoredWrite {
                generation: *generation,
                turn,
                evidence,
                retractions,
            });
        }
        if writes.is_empty() {
            return Err(invalid_at(
                &lib.root,
                "declaration intent has no authored suffix",
            ));
        }
        for id in &write_ids {
            if self
                .bindings()
                .get(*id)
                .is_none_or(|entry| entry.scope != private_scope)
            {
                return Err(SessionError::InvalidPublicBindingPromotion(
                    tidepool_codegen::binding_table::BindingPromotionError::MissingOrForeignBinding,
                ));
            }
        }
        let current_ids = private
            .bindings
            .iter()
            .map(|(_, id)| id.raw())
            .collect::<BTreeSet<_>>();
        let mut write_ids = write_ids
            .into_iter()
            .filter(|id| current_ids.contains(&id.raw()))
            .collect::<Vec<_>>();
        write_ids.sort_by_key(|id| id.raw());
        write_ids.dedup();
        let source_keys = source_keys
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.bindings()
            .prepare_exact_publication_in(
                self.scope_tree(),
                private_scope,
                admitted.scope,
                &write_ids,
                &source_keys,
            )
            .map_err(SessionError::InvalidPublicBindingPromotion)?;
        let reserved = self.lib_mut().reserve_join_generation_durable()?;
        Ok(Arc::new(FinalExecutionIntent {
            admitted: admitted.clone(),
            private,
            private_base,
            writes,
            write_ids,
            source_keys,
            reserved,
        }))
    }

    /// Capture only the latest public merge baseline for a fixed execution.
    pub fn restage_declaration_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        intent: Arc<FinalExecutionIntent>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        let public = self.snapshot_publication(
            owner,
            intent.admitted.scope,
            intent.private.scope,
            intent.write_ids.clone(),
            intent.source_keys.clone(),
        )?;
        if public.expected_private != intent.private || !self.lib().log.is_reserved(intent.reserved)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let current_public = tip(self.lib(), public.expected_public.declaration_tip)?;
        let mut surface = current_public
            .as_ref()
            .map(|tip| tip.surface.clone())
            .unwrap_or_default();
        for write in &intent.writes {
            surface = extend_admitted_surface(&public.path, &write.evidence, surface)?;
        }
        let expected_public_version = paired_version(&public);
        Ok(DeclarationPublicationBase {
            reserved: intent.reserved,
            public,
            intent,
            current_public,
            surface,
            includes: self.lib().extra_include.clone(),
            session_root: self.lib().root.clone(),
            expected_public_version,
        })
    }

    pub fn snapshot_declaration_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        admitted: &PublicVisibilitySnapshot,
        private_scope: ScopeId,
        write_ids: Vec<SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        let intent =
            self.freeze_execution_intent(admitted, private_scope, write_ids, source_keys)?;
        self.restage_declaration_publication(owner, intent)
    }

    pub fn revalidate_declaration_rejection(
        &self,
        rejected: &RejectedDeclarationPublication,
    ) -> Result<DeclarationPublicationRejection, SessionError> {
        if !self.declaration_baseline_is_current(&rejected.base.public)? {
            return Ok(DeclarationPublicationRejection::Stale);
        }
        match &rejected.receipt.outcome().decision {
            JoinDecision::Rejected { reason, diagnostic } => {
                Ok(DeclarationPublicationRejection::Rejected {
                    reason: *reason,
                    diagnostic: diagnostic.clone(),
                })
            }
            JoinDecision::Accepted => unreachable!("protected rejected receipt"),
        }
    }

    fn declaration_baseline_is_current(
        &self,
        base: &PublicManifestBase,
    ) -> Result<bool, SessionError> {
        if !self.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        Ok(self.lib().public_manifest_baseline_is_current(
            base.session,
            &base.path,
            &base.owner,
            base.public_scope,
            &base.graph.checksum,
            base.graph.high_water,
        )? && self.publication_views_are_current(&base.expected_public, &base.expected_private))
    }
}

impl DeclarationPublicationBase {
    pub fn reserved_generation(&self) -> Generation {
        self.reserved
    }
    pub fn intent(&self) -> &Arc<FinalExecutionIntent> {
        &self.intent
    }

    pub fn certify(self) -> Result<CertifiedDeclarationPublication, SessionError> {
        let mut lexical = self.surface.lexical.clone();
        let mut selected_tips = BTreeSet::new();
        for generation in self
            .current_public
            .iter()
            .map(|tip| tip.generation)
            .chain(self.intent.private_base.iter().map(|tip| tip.generation))
            .chain(self.intent.writes.iter().map(|write| write.generation))
        {
            selected_tips.insert(generation);
        }
        let authored = self
            .intent
            .writes
            .iter()
            .map(|write| write.evidence.clone())
            .collect::<Vec<_>>();
        let owner = authored
            .last()
            .expect("nonempty frozen suffix")
            .product()
            .owner();
        for generation in selected_tips {
            lexical.push(ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: owner.unit.clone(),
                    module: SessionModule::lib(generation).module_name(),
                },
                imports: Vec::new(),
            });
        }
        let context = if let Some(public) = &self.current_public {
            (*public.context).clone().extend(&authored, &[], lexical)?
        } else {
            ExactDeclarationContext::new(&authored, &[], lexical)?
        };
        let scratch = tempfile::tempdir()?;
        let materialized = context.materialize(scratch.path())?;
        let anchor = |generation: Generation| -> Result<ModuleSnapshot, SessionError> {
            let module = SessionModule::lib(generation).module_name();
            let artifact = materialized
                .artifacts
                .iter()
                .find(|artifact| {
                    artifact.interface.unit == owner.unit && artifact.interface.module == module
                })
                .ok_or_else(|| {
                    invalid(
                        &self.public,
                        "owned context lacks a selected exact interface",
                    )
                })?;
            Ok(ModuleSnapshot {
                module,
                path: artifact.interface.path.clone(),
                sha256: artifact.interface.sha256.clone(),
            })
        };
        let mut expected_exports = self
            .current_public
            .as_ref()
            .map(|tip| tip.exports.clone())
            .unwrap_or_default();
        let mut expected_instances = self
            .current_public
            .as_ref()
            .map(|tip| tip.instances.clone())
            .unwrap_or_default();
        let mut family_closure = self
            .current_public
            .as_ref()
            .map(|tip| tip.families.clone())
            .unwrap_or_default();
        let mut writes = Vec::new();
        for write in &self.intent.writes {
            expected_exports.retain(|export| !write.retractions.contains(&export.head));
            extend_exports_by_head(
                &mut expected_exports,
                write.evidence.introduced_exports(),
                |export| export.head.occurrence.as_str(),
            );
            // Selection is exact and additive. A missing private head or a
            // retracted spelling never removes a latest-public dfun/axiom.
            expected_instances = merge_instances(
                &self.public.path,
                expected_instances,
                write.evidence.instances(),
            )?;
            family_closure.extend_from_slice(write.evidence.family_closure());
            writes.push(DeclarationWrite {
                generation: write.generation.0,
                module: anchor(write.generation)?,
                exports: write.evidence.introduced_exports().to_vec(),
                retractions: write.retractions.clone(),
            });
        }
        family_closure.sort();
        family_closure.dedup();
        let input = DeclarationJoinInput {
            expected_public_version: self.expected_public_version.clone(),
            public_module: self
                .current_public
                .as_ref()
                .map(|tip| anchor(tip.generation))
                .transpose()?,
            private_base: self
                .intent
                .private_base
                .as_ref()
                .map(|tip| anchor(tip.generation))
                .transpose()?,
            private_tip: Some(anchor(self.intent.private.declaration_tip)?),
            writes,
            reserved: ReservedJoin {
                unit: owner.unit.clone(),
                module: SessionModule::lib(self.reserved).module_name(),
                path: scratch.path().join("joined.hi"),
            },
            artifacts: materialized.artifacts,
            family_closure,
            expected_exports,
            expected_instances,
        };
        match tidepool_toolchain::declaration_join::certify_declaration_join(
            input,
            &context,
            &self.includes,
            &self.session_root,
        )? {
            CertifiedDeclarationJoin::Accepted(receipt) => Ok(
                CertifiedDeclarationPublication::Accepted(AcceptedDeclarationPublication {
                    base: self,
                    receipt: Arc::new(receipt),
                }),
            ),
            CertifiedDeclarationJoin::Rejected(receipt) => Ok(
                CertifiedDeclarationPublication::Rejected(RejectedDeclarationPublication {
                    base: self,
                    receipt,
                }),
            ),
        }
    }
}

impl AcceptedDeclarationPublication {
    pub fn intent(&self) -> &Arc<FinalExecutionIntent> {
        &self.base.intent
    }
    pub fn stage(self) -> Result<StagedPublicManifest, SessionError> {
        let Self { mut base, receipt } = self;
        let context = Arc::new(ExactDeclarationContext::new(
            &[],
            std::slice::from_ref(&receipt),
            std::iter::once(ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: receipt.reserved().unit.clone(),
                    module: receipt.reserved().module.clone(),
                },
                imports: base.surface.roots.clone(),
            })
            .chain(base.surface.lexical.iter().cloned())
            .collect(),
        )?);
        let checksum = base.public.graph.checksum.clone();
        let high_water = base.public.graph.high_water;
        let root = base
            .public
            .path
            .parent()
            .ok_or_else(|| invalid(&base.public, "manifest has no parent"))?;
        let materialized = receipt
            .materialize(root)
            .map_err(|error| invalid(&base.public, error.to_string()))?;
        let exports = receipt
            .exports()
            .iter()
            .map(|export| {
                super::certified_recovery_export(export)
                    .ok_or_else(|| invalid(&base.public, "unsupported joined export identity"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut implementation_refs = base
            .intent
            .writes
            .iter()
            .map(|write| write.generation)
            .collect::<Vec<_>>();
        if let Some(public) = &base.current_public {
            implementation_refs.push(public.generation);
        }
        let artifacts = materialized
            .products
            .into_iter()
            .map(recovery::RecoveryArtifactClosure::Home)
            .chain(
                materialized
                    .anchors
                    .into_iter()
                    .map(recovery::RecoveryArtifactClosure::Join),
            )
            .chain(std::iter::once(recovery::RecoveryArtifactClosure::Join(
                materialized.join,
            )))
            .collect::<Vec<_>>();
        let artifact_refs = artifacts
            .iter()
            .map(recovery::RecoveryArtifactClosure::key)
            .collect();
        let live_dependencies = implementation_refs
            .iter()
            .filter_map(|id| base.public.graph.nodes.iter().find(|node| node.id == *id))
            .flat_map(|node| node.live_dependencies.clone())
            .collect::<Vec<_>>();
        let mut workbench_imports = base
            .current_public
            .as_ref()
            .map(|tip| tip.turn.workbench_imports.clone())
            .unwrap_or_default();
        for write in &base.intent.writes {
            workbench_imports.extend(&write.turn.workbench_imports);
        }
        base.public.graph.nodes.push(recovery::RecoveryNode {
            id: base.reserved,
            parent: None,
            kind: recovery::RecoveryNodeKind::Join,
            implementation_refs,
            artifact_refs,
            exports,
            lexical_roots: vec![ExactModuleIdentity {
                unit: receipt.reserved().unit.clone(),
                module: receipt.reserved().module.clone(),
            }],
            lexical: context.lexical_graph().to_vec(),
            retracts: Vec::new(),
            workbench_imports: workbench_imports.specs().to_vec(),
            instances: recovery_instances(receipt.instances(), receipt.family_closure()),
            state: if live_dependencies.is_empty() {
                recovery::RecoveryNodeState::ExactArtifactClosure
            } else {
                recovery::RecoveryNodeState::LiveValueDependency {
                    reason: "joined declaration retains exact live dependencies".into(),
                }
            },
            live_dependencies,
        });
        for artifact in artifacts {
            if !base
                .public
                .graph
                .artifacts
                .iter()
                .any(|existing| existing.key() == artifact.key())
            {
                base.public.graph.artifacts.push(artifact);
            }
        }
        if let Some(surface) = base
            .public
            .graph
            .public_surfaces
            .iter_mut()
            .find(|surface| surface.owner == base.public.owner)
        {
            surface.declaration_root = Some(base.reserved);
        } else {
            base.public
                .graph
                .public_surfaces
                .push(recovery::RecoveryPublicSurface {
                    owner: base.public.owner.clone(),
                    declaration_root: Some(base.reserved),
                    epoch: 0,
                    bindings: Vec::new(),
                    source_instances: Vec::new(),
                });
        }
        base.public
            .graph
            .seal()
            .map_err(|error| invalid(&base.public, error.to_string()))?;
        let mut turn = base
            .current_public
            .as_ref()
            .map(|tip| tip.turn.clone())
            .unwrap_or_else(|| base.intent.writes[0].turn.clone());
        if base.current_public.is_none() {
            turn.items.clear();
            turn.value_types.clear();
            turn.workbench_imports = super::SourceImports::new();
        }
        for write in &base.intent.writes {
            turn.items.retain(|item| {
                !write
                    .retractions
                    .iter()
                    .any(|identity| item.head_name() == identity.occurrence)
            });
            turn.value_types
                .retain(|name, _| !write.turn.retracts.contains(name));
            extend_exports_by_head(
                &mut turn.items,
                &write.turn.items,
                super::ExportItem::head_name,
            );
            turn.value_types.extend(write.turn.value_types.clone());
            turn.workbench_imports.extend(&write.turn.workbench_imports);
        }
        // Exact compiler winners own the final source surface. An exact
        // retraction of an old head cannot erase a concurrently replaced head
        // merely because its printed occurrence happens to match.
        turn.items = receipt
            .exports()
            .iter()
            .map(|export| match export.kind {
                tidepool_toolchain::declaration_join::DeclarationKind::Value => {
                    super::ExportItem::Value {
                        name: export.head.occurrence.clone(),
                    }
                }
                tidepool_toolchain::declaration_join::DeclarationKind::Type => {
                    super::ExportItem::Type {
                        name: export.head.occurrence.clone(),
                        cons: export
                            .children
                            .iter()
                            .map(|child| child.occurrence.clone())
                            .collect(),
                    }
                }
                tidepool_toolchain::declaration_join::DeclarationKind::Class => {
                    super::ExportItem::Class {
                        name: export.head.occurrence.clone(),
                        methods: export
                            .children
                            .iter()
                            .map(|child| child.occurrence.clone())
                            .collect(),
                    }
                }
            })
            .collect();
        let private_owners = base
            .intent
            .writes
            .iter()
            .map(|write| module_owner(&write.evidence))
            .collect::<BTreeSet<_>>();
        let mut declared_names = receipt
            .exports()
            .iter()
            .filter(|export| {
                private_owners.contains(&ExactModuleIdentity {
                    unit: export.head.unit.clone(),
                    module: export.head.module.clone(),
                })
            })
            .flat_map(|export| std::iter::once(&export.head).chain(export.children.iter()))
            .map(|identity| identity.occurrence.clone())
            .collect::<Vec<_>>();
        let final_bindings = base
            .intent
            .write_ids
            .iter()
            .filter_map(|id| {
                base.intent
                    .private
                    .bindings
                    .iter()
                    .find(|(_, current)| current == id)
            })
            .map(|(name, _)| name)
            .collect::<BTreeSet<_>>();
        declared_names.retain(|name| !final_bindings.contains(name));
        declared_names.sort();
        declared_names.dedup();
        turn.value_types.retain(|name, _| {
            turn.items
                .iter()
                .any(|item| matches!(item, super::ExportItem::Value { name: head } if head == name))
        });
        turn.parent = None;
        turn.sources.clear();
        turn.retracts.clear();
        turn.normalized = DeclarationSource {
            prologue: Default::default(),
            body: String::new(),
        };
        let declaration = PreparedDeclarationPublication {
            generation: base.reserved,
            declared_names,
            joined: JoinedDeclaration {
                turn,
                evidence: receipt,
                context,
                surface: base.surface,
            },
        };
        base.public
            .final_bindings
            .retain(|(name, _)| !declaration.declared_names.contains(name));
        let mut ticket = base.public.stage()?;
        ticket.base_checksum = checksum;
        ticket.base_high_water = high_water;
        ticket.declaration = Some(declaration);
        Ok(ticket)
    }
}

impl RejectedDeclarationPublication {
    pub fn intent(&self) -> &Arc<FinalExecutionIntent> {
        &self.base.intent
    }
}

impl SessionLib {
    pub(super) fn declaration_publication_is_ready(&self, ticket: &StagedPublicManifest) -> bool {
        ticket.declaration.as_ref().is_none_or(|declaration| {
            self.log.is_reserved(declaration.generation)
                && declaration.joined.turn.parent.is_none()
                && self.scope_tip(ticket.public_scope) == ticket.expected_public.declaration_tip
                && self.tips.contains_key(&ticket.public_scope)
                && declaration.joined.evidence.reserved().module
                    == SessionModule::lib(declaration.generation).module_name()
        })
    }
    pub(super) fn commit_prepared_declaration(
        &mut self,
        scope: ScopeId,
        declaration: PreparedDeclarationPublication,
    ) {
        let generation = declaration.generation;
        assert!(self.log.commit_reserved(generation, declaration.joined));
        *self
            .tips
            .get_mut(&scope)
            .expect("declaration tip preflight") = generation;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        ModuleEnv, PublicManifestCommit, PublicationDecision, PublicationPhase, SessionId,
        SourceImports,
    };

    fn accepted(base: DeclarationPublicationBase) -> AcceptedDeclarationPublication {
        match base.certify().unwrap() {
            CertifiedDeclarationPublication::Accepted(accepted) => accepted,
            CertifiedDeclarationPublication::Rejected(rejected) => {
                panic!("unexpected rejection: {:?}", rejected.receipt.outcome())
            }
        }
    }

    #[test]
    fn paired_independent_authored_rebase_preserves_originals_and_current_winners() {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(994), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let private_a = session.mint_detached_scope(public).unwrap();
        let private_b = session.mint_detached_scope(public).unwrap();
        let owner =
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/rebase").unwrap(), 1)
                .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let admitted = session.public_visibility_snapshot_in(public).unwrap();
        for (scope, source) in [
            (private_a, include_str!("fixtures/paired-rebase-A.hs")),
            (private_b, include_str!("fixtures/paired-rebase-B.hs")),
        ] {
            let receipt = session
                .lib()
                .declaration_receipt(&[source])
                .unwrap()
                .unwrap();
            let (candidate, values) = session
                .render_declaration_candidate_in(scope, &receipt, &SourceImports::new())
                .unwrap();
            let staged = crate::session::validate_declaration_candidate(
                candidate,
                session.lib().include_dir(),
            )
            .unwrap()
            .with_visible_values(values);
            session.adopt_staged_declaration_in(staged).unwrap();
        }
        let original_a = session.lib().scope_tip(private_a);
        let original_b = session.lib().scope_tip(private_b);
        let authored_b = session
            .lib()
            .log
            .certified_authored_arc_at(original_b)
            .unwrap();
        let value_a =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "fromA", 73);
        let value_a_id = value_a.id;
        session.bind_in(private_a, value_a).unwrap();
        let value_b =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "fromB", 74);
        let value_b_id = value_b.id;
        session.bind_in(private_b, value_b).unwrap();

        let old_b = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private_b,
                    vec![value_b_id],
                    vec![],
                )
                .unwrap(),
        )
        .stage()
        .unwrap();
        let first = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private_a,
                    vec![value_a_id],
                    vec![],
                )
                .unwrap(),
        );
        let first_generation = first.base.reserved;
        assert_eq!(session.publish_staged_public_manifest(
            first.stage().unwrap(), &PublicationDecision::new(),
        ).unwrap(), PublicManifestCommit::Durable);
        let first_public = session.public_visibility_snapshot_in(public).unwrap();
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(old_b, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            first_public
        );

        let cancelled = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private_b,
                    vec![value_b_id],
                    vec![],
                )
                .unwrap(),
        );
        assert_eq!(
            cancelled
                .receipt
                .input()
                .public_module
                .as_ref()
                .unwrap()
                .module,
            SessionModule::lib(first_generation).module_name()
        );
        let cancellation = PublicationDecision::new();
        cancellation.request_cancellation();
        assert_eq!(
            session
                .publish_staged_public_manifest(cancelled.stage().unwrap(), &cancellation,)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            first_public
        );

        let retry = accepted(
            session
                .snapshot_declaration_publication(
                    owner,
                    &admitted,
                    private_b,
                    vec![value_b_id],
                    vec![],
                )
                .unwrap(),
        );
        assert!(Arc::ptr_eq(
            &retry.base.intent.writes.last().unwrap().evidence,
            &authored_b
        ));
        let generation = retry.base.reserved;
        let proof = retry.receipt.clone();
        for (name, original) in [
            ("keepA", original_a),
            ("keepB", original_b),
            ("answer", original_b),
            ("PublicShape", original_b),
        ] {
            let export = proof
                .exports()
                .iter()
                .find(|export| export.head.occurrence == name)
                .unwrap();
            assert_eq!(
                export.head.module,
                SessionModule::lib(original).module_name()
            );
        }
        let shape = proof
            .exports()
            .iter()
            .find(|export| export.head.occurrence == "PublicShape")
            .unwrap();
        assert_eq!(
            shape
                .children
                .iter()
                .map(|child| child.occurrence.as_str())
                .collect::<Vec<_>>(),
            vec!["NewShape"]
        );
        assert_eq!(
            session
                .publish_staged_public_manifest(retry.stage().unwrap(), &decision,)
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(decision.phase(), PublicationPhase::Published);
        let published = session.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(published.declaration_tip, generation);
        assert_eq!(published.epoch, first_public.epoch + 1);
        assert!(published.bindings.contains(&("fromA".into(), value_a_id)));
        assert!(published.bindings.contains(&("fromB".into(), value_b_id)));
        let turn = session.lib().log.turn(generation).unwrap();
        assert_eq!(turn.items.len(), 4);
        assert!(turn.sources.is_empty());
        assert_eq!(
            turn.value_types.get("answer"),
            session
                .lib()
                .log
                .turn(original_b)
                .unwrap()
                .value_types
                .get("answer")
        );
        let graph = recovery::read_v2(&path, root.path())
            .unwrap()
            .unwrap()
            .graph;
        let joined = graph
            .nodes
            .iter()
            .find(|node| node.id == generation)
            .unwrap();
        let mut expected_refs = vec![original_b, first_generation];
        expected_refs.sort();
        assert_eq!(joined.implementation_refs, expected_refs);
        assert_eq!(joined.exports.len(), proof.exports().len());
        assert!(joined.exports.iter().all(|export| export
            .children
            .iter()
            .all(|child| child.occurrence != "OldShape")));
        assert_eq!(graph.public_surfaces[0].declaration_root, Some(generation));
        session.retire_scope(private_a);
        session.retire_scope(private_b);
        assert!(session.bindings().get(value_a_id).is_some());
        assert!(session.bindings().get(value_b_id).is_some());
        let view = session.compile_view_in(public).unwrap();
        let context = view.exact_declaration_context().unwrap();
        assert_eq!(
            context.lexical_graph().len(),
            1 + session
                .lib()
                .log
                .joined_at(generation)
                .unwrap()
                .surface
                .lexical
                .len()
        );
        assert!(context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == SessionModule::lib(generation).module_name()));
        assert!(context
            .lexical_graph()
            .iter()
            .all(|node| ![original_a, original_b]
                .iter()
                .any(|original| node.owner.module == SessionModule::lib(*original).module_name())));
        for original in [original_a, original_b] {
            assert!(context.recovery_products().iter().any(|product| {
                product.owner().module == SessionModule::lib(original).module_name()
            }));
        }
        let materialized = proof.materialize(root.path()).unwrap();
        assert!(materialized
            .anchors
            .iter()
            .any(|anchor| anchor.module == SessionModule::lib(first_generation).module_name()));
        tidepool_toolchain::recovery_artifacts::verify_materialized_join(
            root.path(),
            &materialized.join,
        )
        .unwrap();
    }

    #[test]
    fn paired_first_authored_publication_fences_outcomes_and_finishes_uncertain_swap() {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib =
            SessionLib::open(SessionId(993), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(&path).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let private = session.mint_detached_scope(public).unwrap();
        let owner =
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/paired").unwrap(), 1)
                .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let admitted = session.public_visibility_snapshot_in(public).unwrap();
        let receipt = session
            .lib()
            .declaration_receipt(&["data PrivateFlag = PrivateFlag\nanswer :: Int\nanswer = 42"])
            .unwrap()
            .unwrap();
        let (candidate, values) = session
            .render_declaration_candidate_in(private, &receipt, &SourceImports::new())
            .unwrap();
        let staged =
            crate::session::validate_declaration_candidate(candidate, session.lib().include_dir())
                .unwrap()
                .with_visible_values(values);
        session.adopt_staged_declaration_in(staged).unwrap();
        assert_eq!(session.lib().scope_tip(private), Generation(1));
        let authored = session
            .lib()
            .log
            .certified_authored_arc_at(Generation(1))
            .unwrap();

        // The original declaration is compiled once. These value writes use
        // genuine registered native roots, and are staged independently.
        let old_answer =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "answer", 71);
        let old_answer_id = old_answer.id;
        session.bind_in(public, old_answer).unwrap();
        let other =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "other", 72);
        let other_id = other.id;
        session.bind_in(private, other).unwrap();
        let incarnation = session
            .public_visibility_snapshot_in(public)
            .unwrap()
            .machine_incarnation;

        let first = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private,
                    vec![other_id],
                    vec![],
                )
                .unwrap(),
        );
        // Obtain a real protected rejection from the same owned closure. Only
        // the expected inventory is wrong; no runtime validation is bypassed.
        let context = ExactDeclarationContext::new(
            &[authored.clone()],
            &[],
            vec![ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: authored.product().owner().unit.clone(),
                    module: authored.product().owner().module.clone(),
                },
                imports: Vec::new(),
            }],
        )
        .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let artifacts = context.materialize(scratch.path()).unwrap().artifacts;
        let original = artifacts
            .iter()
            .find(|artifact| artifact.interface.module == authored.product().owner().module)
            .unwrap();
        let anchor = ModuleSnapshot {
            module: original.interface.module.clone(),
            path: original.interface.path.clone(),
            sha256: original.interface.sha256.clone(),
        };
        let mut input = first.receipt.input().clone();
        input.artifacts = artifacts;
        input.private_tip = Some(anchor.clone());
        input.writes[0].module = anchor;
        input.reserved.path = scratch.path().join("rejected.hi");
        input.expected_exports[0]
            .head
            .occurrence
            .push_str("Missing");
        let CertifiedDeclarationJoin::Rejected(receipt) =
            tidepool_toolchain::declaration_join::certify_declaration_join(
                input,
                &context,
                &first.base.includes,
                &first.base.session_root,
            )
            .unwrap()
        else {
            panic!("expected protected inventory rejection");
        };
        let rejected = RejectedDeclarationPublication {
            base: first.base,
            receipt,
        };
        assert!(matches!(
            session.revalidate_declaration_rejection(&rejected).unwrap(),
            DeclarationPublicationRejection::Rejected { .. }
        ));
        let binding_stage = session
            .snapshot_binding_publication(owner.clone(), public, private, vec![])
            .unwrap()
            .stage()
            .unwrap();
        assert_eq!(
            session
                .publish_staged_public_manifest(binding_stage, &PublicationDecision::new())
                .unwrap(),
            PublicManifestCommit::Durable
        );
        assert_eq!(
            session.revalidate_declaration_rejection(&rejected).unwrap(),
            DeclarationPublicationRejection::Stale
        );

        let cancelled_stage = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private,
                    vec![other_id],
                    vec![],
                )
                .unwrap(),
        )
        .stage()
        .unwrap();
        let cancelled = PublicationDecision::new();
        cancelled.request_cancellation();
        assert_eq!(
            session
                .publish_staged_public_manifest(cancelled_stage, &cancelled)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert_eq!(session.lib().scope_tip(public), Generation(0));
        assert!(session.bindings().get(old_answer_id).is_some());

        let stale_stage = accepted(
            session
                .snapshot_declaration_publication(
                    owner.clone(),
                    &admitted,
                    private,
                    vec![other_id],
                    vec![],
                )
                .unwrap(),
        )
        .stage()
        .unwrap();
        session.lib_mut().reserve_join_generation_durable().unwrap();
        let decision = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(stale_stage, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert_eq!(session.lib().scope_tip(public), Generation(0));

        let retry = accepted(
            session
                .snapshot_declaration_publication(owner, &admitted, private, vec![other_id], vec![])
                .unwrap(),
        );
        let generation = retry.base.reserved;
        let proof = retry.receipt.clone();
        let ticket = retry.stage().unwrap();
        let before_epoch = session.public_visibility_snapshot_in(public).unwrap().epoch;
        session.lib_mut().fail_recovery_durability_once = true;
        assert!(matches!(
            session
                .publish_staged_public_manifest(ticket, &decision)
                .unwrap(),
            PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
        ));
        assert_eq!(decision.phase(), PublicationPhase::Published);
        let published = session.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(published.epoch, before_epoch + 1);
        assert_eq!(published.declaration_tip, generation);
        assert_eq!(published.machine_incarnation, incarnation);
        assert!(!published.bindings.iter().any(|(name, _)| name == "answer"));
        assert!(published
            .bindings
            .iter()
            .any(|(name, id)| name == "other" && *id == other_id));
        let compiled_against = session.compile_view_in(public).unwrap();
        let context = compiled_against
            .exact_declaration_context()
            .unwrap()
            .clone();
        assert_eq!(
            context.lexical_graph().len(),
            1 + session
                .lib()
                .log
                .joined_at(generation)
                .unwrap()
                .surface
                .lexical
                .len()
        );
        assert!(context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == SessionModule::lib(generation).module_name()));
        assert!(context
            .lexical_graph()
            .iter()
            .all(|node| node.owner.module != authored.product().owner().module));
        assert!(context
            .recovery_products()
            .iter()
            .any(|product| product.owner() == authored.product().owner()));
        assert!(Arc::ptr_eq(
            compiled_against.exact_declaration_context().unwrap(),
            session
                .compile_view_in(public)
                .unwrap()
                .exact_declaration_context()
                .unwrap(),
        ));
        let graph = recovery::read_v2(&path, root.path())
            .unwrap()
            .unwrap()
            .graph;
        let joined = graph
            .nodes
            .iter()
            .find(|node| node.id == generation)
            .unwrap();
        assert_eq!(joined.kind, recovery::RecoveryNodeKind::Join);
        assert_eq!(joined.implementation_refs, vec![Generation(1)]);
        assert_eq!(graph.public_surfaces[0].declaration_root, Some(generation));
        assert!(!graph.public_surfaces[0]
            .bindings
            .iter()
            .any(|binding| binding.name == "answer"));
        assert!(
            matches!(session.lib().log.turn(generation), Some(turn) if turn.sources.is_empty())
        );
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::super::render::render_module(
                &session.lib().log,
                generation,
                &ModuleEnv::standalone_default(),
            )
        }))
        .is_err());
        let stored = proof.materialize(root.path()).unwrap();
        assert_eq!(
            tidepool_toolchain::recovery_artifacts::verify_materialized_join(
                root.path(),
                &stored.join
            )
            .unwrap()
            .interface_bytes,
            proof.interface_bytes()
        );
        let bytes = std::fs::read(&path).unwrap();
        session.lib_mut().confirm_recovery_durability().unwrap();
        session.lib_mut().confirm_recovery_durability().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            published
        );
        session.retire_scope(private);
        assert!(session.bindings().get(other_id).is_some());
        assert_eq!(session.lib().scope_tip(public), generation);
        let retained = session.compile_view_in(public).unwrap();
        assert!(retained.is_current_for(&compiled_against));
        assert!(Arc::ptr_eq(
            retained.exact_declaration_context().unwrap(),
            &context
        ));
    }
}
