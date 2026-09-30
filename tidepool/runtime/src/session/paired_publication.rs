//! Owned compiler receipts for the declaration half of a paired publication.

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_codegen::binding_table::SourceLeaseKey;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionModule};
use tidepool_toolchain::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, CertifiedDeclarationJoin, DeclarationJoinInput,
    DeclarationWrite, ExactDeclarationContext, ExactLexicalNode, ExactModuleIdentity, JoinDecision,
    JoinRejection, ModuleSnapshot, RejectedJoin, ReservedJoin,
};

use super::persistent::PersistentSession;
use super::render::{DeclTurn, JoinedDeclaration};
use super::{
    recovery, DeclarationSource, PublicManifestBase, PublicVisibilitySnapshot, RecoveryPublicOwner,
    SessionError, SessionLib, StagedPublicManifest,
};

/// The exact session baseline and original authored evidence captured under
/// checkout. Certification consumes this value without borrowing the machine.
pub struct DeclarationPublicationBase {
    public: PublicManifestBase,
    reserved: Generation,
    original: Generation,
    authored: Arc<CertifiedAuthoredDeclaration>,
    turn: DeclTurn,
    includes: Vec<PathBuf>,
    session_root: PathBuf,
    expected_public_version: String,
}

pub enum CertifiedDeclarationPublication {
    Accepted(AcceptedDeclarationPublication),
    Rejected(RejectedDeclarationPublication),
}

/// A protected producer receipt and the unchanged paired session baseline.
/// Artifacts and manifest bytes are staged before the commit checkout.
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

fn invalid(base: &PublicManifestBase, detail: impl Into<String>) -> SessionError {
    SessionError::RecoveryManifest {
        path: base.path.clone(),
        detail: detail.into(),
    }
}

fn snapshot_digest(snapshot: &PublicVisibilitySnapshot) -> serde_json::Value {
    serde_json::json!({
        "scope": snapshot.scope.0,
        "epoch": snapshot.epoch,
        "declaration_tip": snapshot.declaration_tip.0,
        "machine_incarnation": snapshot.machine_incarnation.map(|id| id.0),
        "bindings": snapshot.bindings.iter().map(|(name, id)| (name, id.raw())).collect::<Vec<_>>(),
        "source_instances": snapshot.source_instances.iter().map(|key| {
            let identity = &key.binder.binder;
            serde_json::json!({
                "instance": key.instance.raw(),
                "module_version": key.binder.version.0,
                "unit": identity.unit,
                "module": identity.module,
                "namespace": identity.namespace,
                "occurrence": identity.occurrence,
                "record_parent": identity.record_parent,
            })
        }).collect::<Vec<_>>(),
    })
}

fn paired_version(base: &PublicManifestBase) -> String {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "tidepool-paired-declaration-baseline-v1",
        "session": base.session.0,
        "owner": base.owner,
        "graph": base.graph.checksum,
        "high_water": base.graph.high_water.0,
        "public": snapshot_digest(&base.expected_public),
        "private": snapshot_digest(&base.expected_private),
    }))
    .expect("paired baseline contains serializable scalar identities");
    blake3::hash(&bytes).to_hex().to_string()
}

impl PersistentSession {
    /// Snapshot a first authored declaration and exact value/source writes for
    /// one publication. The admitted snapshot identifies the execution's base;
    /// each retry captures the current public binding winners without replaying
    /// execution. Rich declaration suffixes require the exact-context authored
    /// producer and remain refused at this boundary.
    pub fn snapshot_declaration_publication(
        &mut self,
        owner: RecoveryPublicOwner,
        admitted: &PublicVisibilitySnapshot,
        private_scope: ScopeId,
        write_ids: Vec<tidepool_repr::SessionVarId>,
        source_keys: Vec<SourceLeaseKey>,
    ) -> Result<DeclarationPublicationBase, SessionError> {
        let public = self.snapshot_publication(
            owner.clone(),
            admitted.scope,
            private_scope,
            write_ids.clone(),
            source_keys.clone(),
        )?;
        if private_scope == admitted.scope
            || admitted.declaration_tip != Generation(0)
            || public.expected_public.declaration_tip != Generation(0)
        {
            return Err(invalid(
                &public,
                "declaration publication requires an empty admitted and current public declaration view",
            ));
        }
        let original = public.expected_private.declaration_tip;
        let lib = self.lib();
        let authored = lib.log.certified_authored_arc_at(original).ok_or_else(|| {
            invalid(
                &public,
                "private declaration has no retained authored certificate",
            )
        })?;
        let turn = lib
            .log
            .turn(original)
            .ok_or(SessionError::StaleStagedDeclaration)?
            .clone();
        let node = public
            .graph
            .nodes
            .iter()
            .find(|node| node.id == original)
            .ok_or_else(|| {
                invalid(
                    &public,
                    "private declaration is absent from the durable graph",
                )
            })?;
        if turn.parent.is_some()
            || !turn.retracts.is_empty()
            || !turn.workbench_imports.specs().is_empty()
            || node.kind != recovery::RecoveryNodeKind::Authored
            || node.parent.is_some()
            || !node.live_dependencies.is_empty()
            || node.state != recovery::RecoveryNodeState::ExactArtifactClosure
            || !authored.instances().classes.is_empty()
            || !authored.instances().families.is_empty()
            || !authored.family_closure().is_empty()
            || authored.introduced_exports() != authored.lexical_exports()
        {
            return Err(invalid(
                &public,
                "private authored declaration requires unsupported rich evidence",
            ));
        }
        // Reservation changes the complete graph, so the actual paired
        // baseline must be captured again after the identity is durably burned.
        let reserved = self.lib_mut().reserve_join_generation_durable()?;
        let public = self.snapshot_publication(
            owner,
            admitted.scope,
            private_scope,
            write_ids,
            source_keys,
        )?;
        let expected_public_version = paired_version(&public);
        let declared_names = turn
            .items
            .iter()
            .flat_map(super::ExportItem::all_names)
            .collect::<Vec<_>>();
        if public.write_ids.iter().any(|id| {
            self.bindings()
                .get(*id)
                .is_some_and(|entry| declared_names.contains(&entry.name.0.as_str()))
        }) {
            return Err(invalid(&public, "a final value write overlaps a retained declaration without a certified retraction"));
        }
        Ok(DeclarationPublicationBase {
            public,
            reserved,
            original,
            authored,
            turn,
            includes: self.lib().extra_include.clone(),
            session_root: self.lib().root.clone(),
            expected_public_version,
        })
    }

    /// A rejected compiler receipt has authority only while its complete
    /// paired baseline still matches. A stale rejection restages the same
    /// execution intent rather than becoming a user-visible declaration error.
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
        let lib = self.lib();
        Ok(lib.public_manifest_baseline_is_current(
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

    /// Compile against immutable owned artifacts after releasing checkout.
    /// Scratch anchors disappear before this returns; both outcomes retain
    /// their exact input, producer identity, and session baseline.
    pub fn certify(self) -> Result<CertifiedDeclarationPublication, SessionError> {
        let owner = self.authored.product().owner();
        let context = ExactDeclarationContext::new(
            &[self.authored.clone()],
            &[],
            vec![ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: owner.unit.clone(),
                    module: owner.module.clone(),
                },
                imports: Vec::new(),
            }],
        )?;
        let scratch = tempfile::tempdir()?;
        let materialized = context.materialize(scratch.path())?;
        let artifact = materialized
            .artifacts
            .iter()
            .find(|artifact| {
                artifact.interface.unit == owner.unit && artifact.interface.module == owner.module
            })
            .ok_or_else(|| {
                invalid(
                    &self.public,
                    "owned context lacks the original authored interface",
                )
            })?;
        let anchor = ModuleSnapshot {
            module: owner.module.clone(),
            path: artifact.interface.path.clone(),
            sha256: artifact.interface.sha256.clone(),
        };
        let input = DeclarationJoinInput {
            expected_public_version: self.expected_public_version.clone(),
            public_module: None,
            private_base: None,
            private_tip: Some(anchor.clone()),
            writes: vec![DeclarationWrite {
                generation: self.original.0,
                module: anchor,
                exports: self.authored.introduced_exports().to_vec(),
                retractions: Vec::new(),
            }],
            reserved: ReservedJoin {
                unit: owner.unit.clone(),
                module: SessionModule::lib(self.reserved).module_name(),
                path: scratch.path().join("joined.hi"),
            },
            artifacts: materialized.artifacts,
            family_closure: self.authored.family_closure().to_vec(),
            expected_exports: self.authored.lexical_exports().to_vec(),
            expected_instances: self.authored.instances().clone(),
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
    /// Materialize the sealed original closure and stage one complete paired
    /// manifest. No compiler invocation or filesystem staging remains in the
    /// final publication checkout.
    pub fn stage(self) -> Result<StagedPublicManifest, SessionError> {
        let Self { mut base, receipt } = self;
        let context = Arc::new(ExactDeclarationContext::new(
            &[],
            std::slice::from_ref(&receipt),
            vec![ExactLexicalNode {
                owner: ExactModuleIdentity {
                    unit: receipt.reserved().unit.clone(),
                    module: receipt.reserved().module.clone(),
                },
                imports: Vec::new(),
            }],
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
        let original_node = base
            .public
            .graph
            .nodes
            .iter()
            .find(|node| node.id == base.original)
            .ok_or_else(|| invalid(&base.public, "original authored graph node disappeared"))?;
        let exports = original_node.exports.clone();
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
        base.public.graph.nodes.push(recovery::RecoveryNode {
            id: base.reserved,
            parent: None,
            kind: recovery::RecoveryNodeKind::Join,
            implementation_refs: vec![base.original],
            artifact_refs,
            exports,
            retracts: Vec::new(),
            workbench_imports: base.turn.workbench_imports.specs().to_vec(),
            instances: recovery::RecoveryInstanceInventory::default(),
            live_dependencies: Vec::new(),
            state: recovery::RecoveryNodeState::ExactArtifactClosure,
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
        base.turn.parent = None;
        base.turn.sources.clear();
        base.turn.normalized = DeclarationSource {
            prologue: Default::default(),
            body: String::new(),
        };
        let declaration = PreparedDeclarationPublication {
            generation: base.reserved,
            declared_names: base
                .turn
                .items
                .iter()
                .flat_map(super::ExportItem::all_names)
                .map(str::to_owned)
                .collect(),
            joined: JoinedDeclaration {
                turn: base.turn,
                evidence: receipt,
                context,
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

impl SessionLib {
    pub(super) fn declaration_publication_is_ready(&self, ticket: &StagedPublicManifest) -> bool {
        ticket.declaration.as_ref().is_none_or(|declaration| {
            self.log.is_reserved(declaration.generation)
                && declaration.joined.turn.parent == None
                && self.scope_tip(ticket.public_scope) == Generation(0)
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
        assert_eq!(context.lexical_graph().len(), 1);
        assert_eq!(
            context.lexical_graph()[0].owner.module,
            SessionModule::lib(generation).module_name()
        );
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
