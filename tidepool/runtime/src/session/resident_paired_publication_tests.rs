//! Atomic publication histories through the actual resident World pipeline.

use super::world_cell_test_support::{
    bootstrap_world_for, execute_fixture_cell_for, plan_fixture_cell_for,
    source_action_with_original, InputRecipe,
};
use super::*;
use crate::session::paired_publication::{
    tests::{
        assert_allocation_preserves_publication_semantics, assert_original_writes,
        assert_tip_instance_counts, reject_missing_export, reused_projection,
    },
    AcceptedDeclarationPublication, CertifiedDeclarationPublication,
    DeclarationPublicationRejection, ExecutionPublication, FrozenWorldExecution,
};
use crate::session::{
    recovery, Generation, ModuleEnv, PublicManifestCommit, PublicationDecision, PublicationPhase,
    RecoveryPublicOwner, SessionId, SessionLib,
};
use std::collections::BTreeSet;
use tidepool_toolchain::declaration_join::ExactDeclarationContext;

#[derive(Clone)]
struct QuietOutput;
impl OutputSink for QuietOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}
type PublicationSession = ResidentSession<frunk::HNil, QuietOutput>;

fn begin_execution(
    resident: &mut PublicationSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    owner: &RecoveryPublicOwner,
) -> Arc<crate::session::PrivateExecutionAdmission> {
    Arc::new(
        source_action_with_original(resident, source.original_source().clone(), |resident, _| {
            resident.begin_durable_private_execution(owner, source.scope())
        })
        .unwrap(),
    )
}

fn certify_declarations(
    resident: &mut PublicationSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    owner: RecoveryPublicOwner,
    intent: Arc<FrozenWorldExecution>,
) -> AcceptedDeclarationPublication {
    source_action_with_original(
        resident,
        source.original_source().clone(),
        |resident, report| {
            let base = resident
                .state
                .restage_declaration_publication(owner, intent)
                .unwrap();
            match base.certify(report).unwrap() {
                CertifiedDeclarationPublication::Accepted(accepted) => accepted,
                CertifiedDeclarationPublication::Rejected(_) => {
                    panic!("actual World declaration join rejected")
                }
            }
        },
    )
}

fn restage_execution(
    resident: &mut PublicationSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    owner: RecoveryPublicOwner,
    intent: Arc<FrozenWorldExecution>,
) -> ExecutionPublication {
    source_action_with_original(resident, source.original_source().clone(), |resident, _| {
        resident
            .restage_execution_publication(owner, intent)
            .unwrap()
    })
}

fn begin_ephemeral_execution(
    resident: &mut PublicationSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
) -> Arc<crate::session::PrivateExecutionAdmission> {
    Arc::new(
        source_action_with_original(resident, source.original_source().clone(), |resident, _| {
            resident.begin_ephemeral_private_execution(source.scope())
        })
        .unwrap(),
    )
}

fn ephemeral_binding_stage(
    resident: &mut PublicationSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    intent: Arc<FrozenWorldExecution>,
) -> crate::session::StagedPublicManifest {
    source_action_with_original(resident, source.original_source().clone(), |resident, _| {
        let ExecutionPublication::Bindings(base) = resident
            .restage_ephemeral_execution_publication(intent)
            .unwrap()
        else {
            panic!("actual checked execution contains no declaration writes");
        };
        base.stage().unwrap()
    })
}

fn execute_cell(
    resident: &mut PublicationSession,
    source_owner: &Arc<crate::session::RuntimeLexicalScopeLease>,
    recipe: &InputRecipe,
    execution: &Arc<crate::session::PrivateExecutionAdmission>,
    source: &str,
) -> Generation {
    execute_fixture_cell_for(resident, source_owner, recipe, execution, source).0
}

fn binding_id(resident: &PublicationSession, scope: ScopeId, name: &str) -> SessionVarId {
    resident
        .public_visibility_snapshot_in(scope)
        .unwrap()
        .bindings
        .into_iter()
        .find_map(|(found, id)| (found == name).then_some(id))
        .unwrap()
}

#[test]
fn paired_independent_authored_rebase_preserves_originals_and_current_winners() {
    use tidepool_toolchain::artifact_inventory::ArtifactBindingNode;

    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let mut lib = SessionLib::open(SessionId(994), root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let public_source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = public_source.scope();
    let owner =
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/rebase").unwrap(), 1)
            .unwrap();
    assert!(matches!(
        resident
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &public_source);
    let execution_a = begin_execution(&mut resident, &public_source, &owner);
    let execution_b = begin_execution(&mut resident, &public_source, &owner);
    let private_a = execution_a.private_scope();
    let private_b = execution_b.private_scope();
    let original_a = execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &execution_a,
        include_str!("fixtures/paired-rebase-A.hs"),
    );
    let original_b = execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &execution_b,
        include_str!("fixtures/paired-rebase-B.hs"),
    );
    // This history preserves independently authored native closures through
    // publication and recovery. Equal-payload issuer affinity is a separate
    // compiler-source merge contract, not an independent-compile premise.
    let branch_closures =
        [(private_a, original_a), (private_b, original_b)].map(|(scope, generation)| {
            let context = resident
                .state
                .lib()
                .current_exact_context_in(scope)
                .unwrap();
            let artifacts = context.artifact_view();
            let graph = artifacts.capture_graph_selection();
            let original = context.authored_native_root(generation.0).unwrap();
            let edges = artifacts.binding_dependencies();
            let mut closure = graph
                .native_groups
                .iter()
                .filter(|group| group.binding.artifact() == original)
                .copied()
                .map(ArtifactBindingNode::Group)
                .collect::<BTreeSet<_>>();
            assert!(!closure.is_empty(), "authored body has native groups");
            loop {
                let before = closure.len();
                for (source, target, _) in &edges {
                    if closure.contains(source) {
                        closure.insert(*target);
                    }
                }
                if closure.len() == before {
                    break;
                }
            }
            let required_edges = edges
                .into_iter()
                .filter(|(source, _, _)| closure.contains(source))
                .collect::<Vec<_>>();
            (closure, required_edges)
        });
    execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &execution_a,
        "let fromA = (73 :: Int)",
    );
    execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &execution_b,
        "let fromB = (74 :: Int)",
    );
    let value_a_id = binding_id(&resident, private_a, "fromA");
    let value_b_id = binding_id(&resident, private_b, "fromB");
    let intent_a = resident.freeze_private_execution(&execution_a).unwrap();
    let intent_b = resident.freeze_private_execution(&execution_b).unwrap();

    let old_b = certify_declarations(
        &mut resident,
        &public_source,
        owner.clone(),
        intent_b.clone(),
    )
    .stage()
    .unwrap();
    let first = certify_declarations(
        &mut resident,
        &public_source,
        owner.clone(),
        intent_a.clone(),
    );
    let first_generation = first.reserved_generation();
    assert!(matches!(
        resident
            .attempt_public_manifest(first.stage().unwrap(), &PublicationDecision::new(),)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    let first_public = resident
        .state
        .public_visibility_snapshot_in(public)
        .unwrap();
    let decision = PublicationDecision::new();
    assert!(matches!(
        resident.attempt_public_manifest(old_b, &decision).unwrap(),
        crate::session::PublicationAttempt::Rebase(_)
    ));
    assert_eq!(decision.phase(), PublicationPhase::Running);
    assert_eq!(
        resident
            .state
            .public_visibility_snapshot_in(public)
            .unwrap(),
        first_public
    );

    let cancelled = certify_declarations(
        &mut resident,
        &public_source,
        owner.clone(),
        intent_b.clone(),
    );
    assert_eq!(
        cancelled
            .accepted_join_for_test()
            .input()
            .public_module
            .as_ref()
            .unwrap()
            .module,
        SessionModule::lib(first_generation).module_name()
    );
    let cancellation = PublicationDecision::new();
    cancellation.request_cancellation();
    assert!(matches!(
        resident
            .attempt_public_manifest(cancelled.stage().unwrap(), &cancellation,)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Cancelled
    ));
    assert_eq!(
        resident
            .state
            .public_visibility_snapshot_in(public)
            .unwrap(),
        first_public
    );

    let retry = certify_declarations(
        &mut resident,
        &public_source,
        owner.clone(),
        intent_b.clone(),
    );
    assert!(Arc::ptr_eq(retry.intent(), &intent_b));
    assert_original_writes(retry.intent(), resident.state.lib(), &[(original_b, 0)]);
    let generation = retry.reserved_generation();
    let proof = retry.accepted_join_for_test().clone();
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
    assert!(matches!(
        resident
            .attempt_public_manifest(retry.stage().unwrap(), &decision,)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert_eq!(decision.phase(), PublicationPhase::Published);
    let published = resident
        .state
        .public_visibility_snapshot_in(public)
        .unwrap();
    assert_eq!(published.declaration_tip, generation);
    assert_eq!(published.epoch, first_public.epoch + 1);
    assert!(published.bindings.contains(&("fromA".into(), value_a_id)));
    assert!(published.bindings.contains(&("fromB".into(), value_b_id)));
    let turn = resident.state.lib().log.turn(generation).unwrap();
    assert_eq!(turn.items.len(), 4);
    assert!(turn.sources.is_empty());
    assert_eq!(
        turn.value_types.get("answer"),
        resident
            .state
            .lib()
            .log
            .turn(original_b)
            .unwrap()
            .value_types
            .get("answer")
    );
    let graph = crate::session::recovery_test_fixture::graph(resident.state.lib());
    let joined = graph.node(generation).unwrap().unwrap();
    let mut expected_refs = vec![original_b, first_generation];
    expected_refs.sort();
    assert_eq!(joined.implementation_refs, expected_refs);
    assert_eq!(joined.exports.len(), proof.exports().len());
    assert!(joined.exports.iter().all(|export| {
        export
            .children
            .iter()
            .all(|child| child.occurrence != "OldShape")
    }));
    assert_eq!(
        graph.public_surfaces().next().unwrap().declaration_root,
        Some(generation)
    );
    resident.retire_scope(private_a);
    resident.retire_scope(private_b);
    assert!(resident.state.bindings().get(value_a_id).is_some());
    assert!(resident.state.bindings().get(value_b_id).is_some());
    let view = resident.state.compile_view_in(public).unwrap();
    let context = view.exact_declaration_context().unwrap();
    assert_eq!(
        context.lexical_graph().len(),
        1 + resident
            .state
            .lib()
            .log
            .joined_at(generation)
            .unwrap()
            .surface
            .lexical_graph()
            .len()
    );
    assert!(context
        .lexical_graph()
        .iter()
        .any(|node| node.owner.module == SessionModule::lib(generation).module_name()));
    assert!(context.lexical_graph().iter().all(|node| {
        ![original_a, original_b]
            .iter()
            .any(|original| node.owner.module == SessionModule::lib(*original).module_name())
    }));
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
    let assert_closures = |context: &ExactDeclarationContext| {
        let graph = context.artifact_view().capture_graph_selection();
        let edges = context.artifact_view().binding_dependencies();
        for (closure, required_edges) in &branch_closures {
            for node in closure {
                assert!(
                    match node {
                        ArtifactBindingNode::Artifact(binding) => graph.bindings.contains(binding),
                        ArtifactBindingNode::Group(group) => graph.native_groups.contains(group),
                    },
                    "publication lost exact historical dependency {node:?}"
                );
            }
            for edge in required_edges {
                assert!(
                    edges.contains(edge),
                    "publication lost exact native edge {edge:?}"
                );
            }
        }
    };
    assert_closures(context);
    drop(view);
    drop(graph);
    drop(proof);
    drop(materialized);
    drop(resident);
    drop(intent_a);
    drop(intent_b);
    drop(execution_a);
    drop(execution_b);
    drop(public_source);
    let mut reopened_lib =
        SessionLib::open(SessionId(995), root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(recipe.include.clone());
    let reopened_recovery =
        crate::session::recovery_test_fixture::RecoveryFixture::reopen(recovery.owner.clone());
    tidepool_testing::with_settlement(|settlement| {
        reopened_recovery.attach_root(&mut reopened_lib, settlement)
    })
    .unwrap();
    let mut reopened = PersistentSession::new(Some(reopened_lib), 1024 * 1024);
    let public = reopened.recover_public_scope(&owner).unwrap();
    assert_eq!(reopened.lib().scope_tip(public), generation);
    let cold_view = reopened.compile_view_in(public).unwrap();
    assert_closures(cold_view.exact_declaration_context().unwrap());
}

#[test]
fn paired_first_authored_publication_preserves_atomic_visibility() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let mut lib = SessionLib::open(SessionId(993), root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let public_source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = public_source.scope();
    let owner =
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/paired").unwrap(), 1)
            .unwrap();
    assert!(matches!(
        resident
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &public_source);
    let baseline = begin_execution(&mut resident, &public_source, &owner);
    execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &baseline,
        "let answer = (71 :: Int)",
    );
    let old_answer_id = binding_id(&resident, baseline.private_scope(), "answer");
    let baseline_intent = resident.freeze_private_execution(&baseline).unwrap();
    let ExecutionPublication::Bindings(baseline_publication) = restage_execution(
        &mut resident,
        &public_source,
        owner.clone(),
        baseline_intent.clone(),
    ) else {
        panic!("the baseline contains only an actual checked binding");
    };
    assert!(matches!(
        resident
            .attempt_public_manifest(
                baseline_publication.stage().unwrap(),
                &PublicationDecision::new()
            )
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    let metadata_execution = begin_execution(&mut resident, &public_source, &owner);
    execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &metadata_execution,
        "()",
    );
    let metadata_intent = resident
        .freeze_private_execution(&metadata_execution)
        .unwrap();
    assert!(metadata_intent.native_write_ids().is_empty());
    let execution = begin_execution(&mut resident, &public_source, &owner);
    let private = execution.private_scope();
    let original = execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &execution,
        "data PrivateFlag = PrivateFlag\nanswer :: Int\nanswer = 42",
    );
    let authored = resident
        .state
        .lib()
        .log
        .certified_authored_arc_at(original)
        .unwrap();
    execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &execution,
        "let other = (72 :: Int)",
    );
    let other_id = binding_id(&resident, private, "other");
    let intent = resident.freeze_private_execution(&execution).unwrap();
    let incarnation = resident
        .public_visibility_snapshot_in(public)
        .unwrap()
        .machine_incarnation;

    let first = certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone());
    // A real certifier rejects an altered export inventory from this owned closure.
    let rejected = reject_missing_export(first, &authored);
    assert!(matches!(
        resident
            .state
            .revalidate_declaration_rejection(&rejected)
            .unwrap(),
        DeclarationPublicationRejection::Rejected { .. }
    ));
    let ExecutionPublication::Bindings(binding_publication) = restage_execution(
        &mut resident,
        &public_source,
        owner.clone(),
        metadata_intent,
    ) else {
        panic!("metadata refresh uses an actual completed no-change World execution");
    };
    let binding_stage = binding_publication.stage().unwrap();
    assert!(matches!(
        resident
            .attempt_public_manifest(binding_stage, &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert_eq!(
        resident
            .state
            .revalidate_declaration_rejection(&rejected)
            .unwrap(),
        DeclarationPublicationRejection::Stale
    );

    let cancelled_stage =
        certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone())
            .stage()
            .unwrap();
    let cancelled = PublicationDecision::new();
    cancelled.request_cancellation();
    assert!(matches!(
        resident
            .attempt_public_manifest(cancelled_stage, &cancelled)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Cancelled
    ));
    assert_eq!(resident.state.lib().scope_tip(public), Generation(0));
    assert!(resident.state.bindings().get(old_answer_id).is_some());

    let stale_stage =
        certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone())
            .stage()
            .unwrap();
    resident
        .state
        .lib_mut()
        .reserve_declaration_generations_durable(65)
        .unwrap();
    let decision = PublicationDecision::new();
    assert!(matches!(
        resident
            .attempt_public_manifest(stale_stage, &decision)
            .unwrap(),
        crate::session::PublicationAttempt::RestageMetadata(_)
    ));
    assert_eq!(decision.phase(), PublicationPhase::Running);
    assert_eq!(resident.state.lib().scope_tip(public), Generation(0));

    let retry = certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone());
    let generation = retry.reserved_generation();
    let proof = retry.accepted_join_for_test().clone();
    let ticket = retry.stage().unwrap();
    let before_epoch = resident
        .state
        .public_visibility_snapshot_in(public)
        .unwrap()
        .epoch;
    assert!(matches!(
        resident
            .attempt_public_manifest(ticket, &decision)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert_eq!(decision.phase(), PublicationPhase::Published);
    let published = resident
        .state
        .public_visibility_snapshot_in(public)
        .unwrap();
    assert_eq!(published.epoch, before_epoch + 1);
    assert_eq!(published.declaration_tip, generation);
    assert_eq!(published.machine_incarnation, incarnation);
    assert!(!published.bindings.iter().any(|(name, _)| name == "answer"));
    assert!(published
        .bindings
        .iter()
        .any(|(name, id)| name == "other" && *id == other_id));
    let compiled_against = resident.state.compile_view_in(public).unwrap();
    let context = compiled_against
        .exact_declaration_context()
        .unwrap()
        .clone();
    assert_eq!(
        context.lexical_graph().len(),
        1 + resident
            .state
            .lib()
            .log
            .joined_at(generation)
            .unwrap()
            .surface
            .lexical_graph()
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
        resident
            .state
            .compile_view_in(public)
            .unwrap()
            .exact_declaration_context()
            .unwrap(),
    ));
    let graph = crate::session::recovery_test_fixture::graph(resident.state.lib());
    let joined = graph.node(generation).unwrap().unwrap();
    assert_eq!(joined.kind, recovery::RecoveryNodeKind::Join);
    assert_eq!(joined.implementation_refs, vec![original]);
    assert_eq!(
        graph.public_surfaces().next().unwrap().declaration_root,
        Some(generation)
    );
    assert!(!graph
        .public_surfaces()
        .next()
        .unwrap()
        .bindings
        .keys()
        .any(|name| name == "answer"));
    assert!(
        matches!(resident.state.lib().log.turn(generation), Some(turn) if turn.sources.is_empty())
    );
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::super::render::render_module(
            &resident.state.lib().log,
            generation,
            &ModuleEnv::standalone_default(),
        )
    }))
    .is_err());
    let stored = proof.materialize(root.path()).unwrap();
    assert_eq!(
        tidepool_toolchain::recovery_artifacts::verify_materialized_join(root.path(), &stored.join)
            .unwrap()
            .interface_bytes,
        proof.interface_bytes()
    );
    assert_eq!(
        recovery.state().revision,
        resident
            .state
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .store_revision
    );
    assert_eq!(
        resident
            .state
            .public_visibility_snapshot_in(public)
            .unwrap(),
        published
    );
    resident.retire_scope(private);
    assert!(resident.state.bindings().get(other_id).is_some());
    assert_eq!(resident.state.lib().scope_tip(public), generation);
    let retained = resident.state.compile_view_in(public).unwrap();
    assert!(retained.is_current_for(&compiled_against));
    assert!(Arc::ptr_eq(
        retained.exact_declaration_context().unwrap(),
        &context
    ));
}

#[test]
fn paired_rich_fixed_suffix_rebases_nonempty_base_and_final_value_winners() {
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let mut lib =
        SessionLib::open(SessionId(996), root.path(), ModuleEnv::standalone_default()).unwrap();
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let public_source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = public_source.scope();
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/rich-fixed").unwrap(),
        1,
    )
    .unwrap();
    assert!(matches!(
        resident
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &public_source);
    let base_admission = begin_execution(&mut resident, &public_source, &owner);
    let base_original = execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &base_admission,
        include_str!("fixtures/paired-rich-base.hs"),
    );
    let base_intent = resident.freeze_private_execution(&base_admission).unwrap();
    let base = certify_declarations(&mut resident, &public_source, owner.clone(), base_intent);
    assert_eq!(base.accepted_join_for_test().instances().classes.len(), 1);
    assert!(matches!(
        resident
            .attempt_public_manifest(base.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));

    let a = begin_execution(&mut resident, &public_source, &owner);
    let b = begin_execution(&mut resident, &public_source, &owner);
    assert_ne!(a.admitted_public().unwrap().declaration_tip, Generation(0));
    let (a_first, mut a_writes, _) = execute_fixture_cell_for(
        &mut resident,
        &public_source,
        &recipe,
        &a,
        include_str!("fixtures/paired-rich-A1.hs"),
    );
    let (a_second, second_writes, _) = execute_fixture_cell_for(
        &mut resident,
        &public_source,
        &recipe,
        &a,
        include_str!("fixtures/paired-rich-A2.hs"),
    );
    a_writes.extend(second_writes);
    // Actual checked Value ownership replaces retractValue's declaration head.
    // baseValue remains a declaration, so B's later declaration wins on rebase.
    execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &a,
        "let retractValue = (401 :: Int)",
    );
    let value_id = binding_id(&resident, a.private_scope(), "retractValue");
    let intent = resident.freeze_private_execution(&a).unwrap();
    let expected_writes = a_writes
        .iter()
        .map(|generation| (*generation, 0))
        .collect::<Vec<_>>();
    assert_original_writes(&intent, resident.state.lib(), &expected_writes);
    let high_water = resident.state.lib().log.generation();
    let repeated = resident.freeze_private_execution(&a).unwrap();
    assert!(Arc::ptr_eq(&intent, &repeated));
    assert_eq!(resident.state.lib().log.generation(), high_water);
    assert_eq!(intent.native_write_ids(), [value_id]);
    let old_a = certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone());
    let original_attempt = old_a.reserved_generation();
    let old_a = old_a.stage().unwrap();

    let b_original = execute_cell(
        &mut resident,
        &public_source,
        &recipe,
        &b,
        include_str!("fixtures/paired-rich-B.hs"),
    );
    let b_intent = resident.freeze_private_execution(&b).unwrap();
    let b_publication =
        certify_declarations(&mut resident, &public_source, owner.clone(), b_intent);
    let b_generation = b_publication.reserved_generation();
    assert!(b_generation > original_attempt);
    assert!(matches!(
        resident
            .attempt_public_manifest(b_publication.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    let decision = PublicationDecision::new();
    assert!(matches!(
        resident.attempt_public_manifest(old_a, &decision).unwrap(),
        crate::session::PublicationAttempt::Rebase(_)
    ));
    assert_eq!(decision.phase(), PublicationPhase::Running);
    let cancelled =
        certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone());
    let cancelled_generation = cancelled.reserved_generation();
    assert!(cancelled_generation > b_generation);
    assert!(Arc::ptr_eq(cancelled.intent(), &intent));
    let cancellation = PublicationDecision::new();
    cancellation.request_cancellation();
    assert!(matches!(
        resident
            .attempt_public_manifest(cancelled.stage().unwrap(), &cancellation)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Cancelled
    ));
    assert_eq!(resident.state.lib().scope_tip(public), b_generation);
    let refused =
        certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone());
    let refused_generation = refused.reserved_generation();
    assert!(refused_generation > cancelled_generation);
    assert!(Arc::ptr_eq(refused.intent(), &intent));
    let refused = refused.stage().unwrap();
    let before_refusal = recovery.state();
    recovery
        .store
        .inject_fault(crate::session::recovery_store::StoreFault::BeforeCommit);
    assert!(matches!(
        resident
            .attempt_public_manifest(refused, &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Refused { .. }
    ));
    assert_eq!(recovery.state(), before_refusal);
    assert_eq!(resident.state.lib().scope_tip(public), b_generation);
    assert_eq!(
        resident
            .state
            .public_visibility_snapshot_in(a.private_scope()),
        Some(intent.private_snapshot().clone())
    );
    // A new attempt may refill its allocation range. Its ticket must pin
    // the post-reservation scalar revisions without changing causal facts.
    let old_ceiling = resident.state.lib().durable_allocation_ceiling().unwrap();
    let remaining = usize::try_from(old_ceiling.0 - resident.state.lib().generation().0).unwrap();
    resident
        .state
        .lib_mut()
        .reserve_declaration_generations_durable(remaining)
        .unwrap();
    let publication =
        certify_declarations(&mut resident, &public_source, owner.clone(), intent.clone());
    let reserved = publication.reserved_generation();
    assert!(reserved > old_ceiling);
    assert!(reserved > refused_generation);
    assert!(Arc::ptr_eq(publication.intent(), &intent));
    let proof = publication.accepted_join_for_test().clone();
    assert_eq!(proof.instances().classes.len(), 2);
    assert_eq!(proof.instances().families.len(), 2);
    assert!(proof.family_closure().len() >= 2);
    for (name, original) in [
        ("firstPrivate", a_first),
        ("privateWinner", a_second),
        ("fromLaterPublic", b_original),
        ("baseValue", b_original),
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
    assert!(!proof
        .exports()
        .iter()
        .any(|export| export.head.occurrence == "retractValue"));
    let record = proof
        .exports()
        .iter()
        .find(|export| export.head.occurrence == "PublicRecord")
        .unwrap();
    assert_eq!(
        record.head.module,
        SessionModule::lib(base_original).module_name()
    );
    assert!(record
        .children
        .iter()
        .any(|child| child.occurrence == "publicField"
            && child.record_parent.as_deref() == Some("PublicRecord")));
    assert!(matches!(
        resident
            .attempt_public_manifest(publication.stage().unwrap(), &decision)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert_eq!(resident.state.lib().scope_tip(public), reserved);
    assert!(resident
        .state
        .public_visibility_snapshot_in(public)
        .unwrap()
        .bindings
        .iter()
        .any(|(name, id)| name == "retractValue" && *id == value_id));
    let graph = crate::session::recovery_test_fixture::graph(resident.state.lib());
    let node = graph.node(reserved).unwrap().unwrap();
    assert!(node
        .implementation_refs
        .iter()
        .all(|reference| *reference < reserved));
    assert!(node.implementation_refs.contains(&b_generation));
    assert_eq!(node.instances.classes.len(), 2);
    assert_original_writes(&intent, resident.state.lib(), &expected_writes);
    assert_eq!(intent.native_write_ids(), [value_id]);
    let cold = recovery::load_domain_graph(
        Arc::new(crate::session::RunRecoveryStore::open(recovery.store.path()).unwrap()),
        crate::session::RecoveryDomainId::Root,
    )
    .unwrap();
    let cold_node = cold.node(reserved).unwrap().unwrap();
    assert_eq!(*cold_node, *node);
    assert!(cold.node(original_attempt).unwrap().is_none());
    assert!(cold.node(cancelled_generation).unwrap().is_none());
    assert!(cold.node(refused_generation).unwrap().is_none());
    let cold_surface = cold.surface(&owner).unwrap().unwrap();
    assert_eq!(cold_surface.declaration_root, Some(reserved));
    assert_eq!(
        cold_surface.bindings["retractValue"].variable,
        value_id.raw()
    );
    cold.context(&cold_node.context_root).unwrap();
    assert_eq!(
        resident.state.lib().import_line_in(public).unwrap(),
        format!("import {}", SessionModule::lib(reserved).module_name())
    );
    resident.retire_scope(a.private_scope());
    assert!(resident.state.bindings().get(value_id).is_some());

    let unsupported = begin_execution(&mut resident, &public_source, &owner);
    let (_, _, completed_prefix) = execute_fixture_cell_for(
        &mut resident,
        &public_source,
        &recipe,
        &unsupported,
        "let refusalProbe = (402 :: Int)",
    );
    let probe_id = binding_id(&resident, unsupported.private_scope(), "refusalProbe");
    let before_public = resident.public_visibility_snapshot_in(public).unwrap();
    let before_prefix = completed_prefix.snapshot();
    let original = public_source.original_source().clone();
    source_action_with_original(&mut resident, original, |resident, report| {
        resident.state.retract_many_in(
            unsupported.private_scope(),
            &["retractValue".into(), "baseValue".into()],
            report,
        )
    })
    .unwrap();
    let before_refused_freeze = tidepool_extract_cmd::extract_spawn_count();
    assert!(matches!(
        resident.freeze_private_execution(&unsupported),
        Err(SessionError::WorldCellPublicationBaseStale)
    ));
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        before_refused_freeze
    );
    assert_eq!(
        resident.public_visibility_snapshot_in(public).unwrap(),
        before_public
    );
    assert!(Arc::ptr_eq(&before_prefix, &completed_prefix.snapshot()));
    assert_eq!(
        binding_id(&resident, unsupported.private_scope(), "refusalProbe"),
        probe_id
    );
    assert!(resident.captured.snapshot().is_empty());
}

#[test]
fn paired_reused_projection_keeps_nominal_owner_and_atomic_arbitration() {
    tidepool_testing::eval_harness::require_extract();
    for durable in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
        let recipe = InputRecipe::capture(
            effects.preamble().to_owned(),
            effects.row().to_owned(),
            effects.include_paths().to_vec(),
        );
        let mut lib = SessionLib::open(
            SessionId(9831),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(recipe.include.clone());
        let owner =
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/reuse").unwrap(), 1)
                .unwrap();
        let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
            .unwrap();
        let mut resident =
            ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
        let source = resident.mint_notebook_scope_with_lease(None).unwrap();
        let public = source.scope();
        if durable {
            assert!(matches!(
                resident
                    .initialize_durable_public_scope(owner.clone(), public)
                    .unwrap(),
                PublicManifestCommit::Durable
            ));
        }
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: public,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        bootstrap_world_for(&mut resident, &recipe, &source);
        let execution = Arc::new(
            source_action_with_original(
                &mut resident,
                source.original_source().clone(),
                |resident, _| {
                    if durable {
                        resident.begin_durable_private_execution(&owner, public)
                    } else {
                        resident.begin_ephemeral_private_execution(public)
                    }
                },
            )
            .unwrap(),
        );
        let original = execute_cell(
            &mut resident,
            &source,
            &recipe,
            &execution,
            "answer :: Int\nanswer = 42",
        );
        let projection = resident
            .state
            .lib()
            .log
            .projection_at(original)
            .unwrap()
            .clone();
        let intent = resident.freeze_private_execution(&execution).unwrap();
        let restage = |resident: &mut PublicationSession| {
            source_action_with_original(
                resident,
                source.original_source().clone(),
                |resident, report| {
                    let publication = if durable {
                        resident.restage_execution_publication(owner.clone(), intent.clone())
                    } else {
                        resident.restage_ephemeral_execution_publication(intent.clone())
                    }
                    .unwrap();
                    let ExecutionPublication::Declarations(base) = publication else {
                        panic!("authored publication");
                    };
                    match base.certify(report).unwrap() {
                        CertifiedDeclarationPublication::Accepted(publication) => publication,
                        CertifiedDeclarationPublication::Rejected(_) => {
                            panic!("completed authored projection rejected")
                        }
                    }
                },
            )
        };
        let publication = restage(&mut resident);
        assert!(Arc::ptr_eq(reused_projection(&publication), &projection));
        assert_eq!(
            publication.accepted_join_for_test().reserved().module,
            projection.module_name()
        );
        let cancelled = PublicationDecision::new();
        cancelled.request_cancellation();
        assert!(matches!(
            resident
                .attempt_public_manifest(publication.stage().unwrap(), &cancelled)
                .map(crate::session::settled_manifest_attempt)
                .unwrap(),
            PublicManifestCommit::Cancelled
        ));
        assert_eq!(resident.state.lib().scope_tip(public), Generation(0));
        if durable {
            let before = recovery.state();
            recovery
                .store
                .inject_fault(crate::session::recovery_store::StoreFault::BeforeCommit);
            let publication = restage(&mut resident);
            let decision = PublicationDecision::new();
            assert!(matches!(
                resident
                    .attempt_public_manifest(publication.stage().unwrap(), &decision)
                    .map(crate::session::settled_manifest_attempt)
                    .unwrap(),
                PublicManifestCommit::Refused { .. }
            ));
            assert_eq!(decision.phase(), PublicationPhase::Terminated);
            assert_eq!(resident.state.lib().scope_tip(public), Generation(0));
            assert_eq!(recovery.state(), before);
        }
        let publication = restage(&mut resident);
        assert_allocation_preserves_publication_semantics(&publication, resident.state.lib_mut());
        drop(publication);
        let decision = PublicationDecision::new();
        let publication = restage(&mut resident);
        assert!(Arc::ptr_eq(reused_projection(&publication), &projection));
        let published_generation = publication.reserved_generation();
        let commit = resident
            .attempt_public_manifest(publication.stage().unwrap(), &decision)
            .map(crate::session::settled_manifest_attempt)
            .unwrap();
        assert!(matches!(
            (durable, commit),
            (true, PublicManifestCommit::Durable) | (false, PublicManifestCommit::Ephemeral)
        ));
        assert_eq!(decision.phase(), PublicationPhase::Published);
        assert_eq!(resident.state.lib().scope_tip(public), published_generation);
        assert!(Arc::ptr_eq(
            &resident
                .state
                .lib()
                .current_exact_context_in(public)
                .unwrap(),
            projection.composition().context()
        ));
        assert_eq!(
            resident.state.lib().import_line_in(public).unwrap(),
            format!("import {}", projection.module_name())
        );
        assert!(Arc::ptr_eq(
            resident
                .state
                .lib()
                .current_declaration_projection_in(public)
                .unwrap()
                .receipt(),
            projection.receipt()
        ));
        resident.retire_scope(execution.private_scope());
        assert_eq!(
            resident.state.lib().import_line_in(public).unwrap(),
            format!("import {}", projection.module_name())
        );
        if durable {
            let retained = recovery
                .store
                .read_public_owner(
                    crate::session::recovery_store::RecoveryDomainId::Root,
                    &owner,
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                retained.epoch,
                resident
                    .public_visibility_snapshot_in(public)
                    .unwrap()
                    .epoch
            );
            let graph = crate::session::recovery_test_fixture::graph(resident.state.lib());
            let summary = graph.node(published_generation).unwrap().unwrap();
            assert_eq!(summary.kind, recovery::RecoveryNodeKind::Join);
            assert_eq!(summary.parent, None);
        }
    }
}

#[test]
fn paired_reused_projection_restores_detached_summary_under_original_interface() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    let open = |id| {
        SessionLib::open(
            SessionId(id),
            root.path().join("compiler"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(recipe.include.clone())
    };
    let mut lib = open(9833);
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/reopened-reuse").unwrap(),
        1,
    )
    .unwrap();
    resident
        .initialize_durable_public_scope(owner.clone(), public)
        .unwrap();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &source);
    let execution = begin_execution(&mut resident, &source, &owner);
    execute_cell(
        &mut resident,
        &source,
        &recipe,
        &execution,
        "answer :: Int\nanswer = 1",
    );
    execute_cell(
        &mut resident,
        &source,
        &recipe,
        &execution,
        "answer :: Int\nanswer = 42",
    );
    let intent = resident.freeze_private_execution(&execution).unwrap();
    let publication = certify_declarations(&mut resident, &source, owner.clone(), intent.clone());
    reused_projection(&publication);
    let module = publication
        .accepted_join_for_test()
        .reserved()
        .module
        .clone();
    let published_generation = publication.reserved_generation();
    assert!(matches!(
        resident
            .attempt_public_manifest(publication.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    resident
        .state
        .validate_durable_public_admission(&owner, public)
        .unwrap();
    let retained = recovery.state();
    drop(resident);
    drop(execution);
    drop(intent);
    drop(source);
    let reopened_recovery =
        crate::session::recovery_test_fixture::RecoveryFixture::reopen(recovery.owner.clone());
    assert_eq!(reopened_recovery.state(), retained);
    let mut lib = open(9834);
    tidepool_testing::with_settlement(|settlement| {
        reopened_recovery.attach_root(&mut lib, settlement)
    })
    .unwrap();
    let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
    let public = persistent.recover_public_scope(&owner).unwrap();
    assert_eq!(persistent.lib().scope_tip(public), published_generation);
    assert_eq!(
        persistent.lib().import_line_in(public).unwrap(),
        format!("import {module}")
    );
    let exports = persistent
        .lib()
        .log
        .certified_exports_at(published_generation)
        .unwrap();
    assert_eq!(
        exports
            .iter()
            .filter(|export| export.head.occurrence == "answer")
            .count(),
        1
    );
    let source = persistent
        .adopt_initial_notebook_scope_with_source(public, None)
        .unwrap();
    let mut reopened =
        ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
    reopened
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut reopened, &recipe, &source);
    let continuation = begin_execution(&mut reopened, &source, &owner);
    execute_cell(
        &mut reopened,
        &source,
        &recipe,
        &continuation,
        "next :: Int\nnext = answer + 1",
    );
    let continuation = reopened.freeze_private_execution(&continuation).unwrap();
    let publication = certify_declarations(&mut reopened, &source, owner, continuation);
    reused_projection(&publication);
    let continuation_generation = publication.reserved_generation();
    assert!(matches!(
        reopened
            .attempt_public_manifest(publication.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert_eq!(
        reopened.state.lib().scope_tip(public),
        continuation_generation
    );
}

#[test]
fn paired_authored_parent_inventory_survives_value_only_tip_and_instance_only_suffix() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let mut lib = SessionLib::open(SessionId(985), root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/authored-inventory").unwrap(),
        1,
    )
    .unwrap();
    resident
        .initialize_durable_public_scope(owner.clone(), public)
        .unwrap();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &source);
    let baseline = begin_execution(&mut resident, &source, &owner);
    let initial = execute_cell(
        &mut resident,
        &source,
        &recipe,
        &baseline,
        include_str!("fixtures/paired-rich-base.hs"),
    );
    let value_only = execute_cell(
        &mut resident,
        &source,
        &recipe,
        &baseline,
        include_str!("fixtures/paired-public-value-only.hs"),
    );
    assert!(resident
        .state
        .lib()
        .log
        .certified_authored_at(value_only)
        .unwrap()
        .instances()
        .classes
        .is_empty());
    assert_tip_instance_counts(resident.state.lib(), value_only, 1, 1);
    let baseline_intent = resident.freeze_private_execution(&baseline).unwrap();
    let baseline_publication =
        certify_declarations(&mut resident, &source, owner.clone(), baseline_intent);
    assert!(matches!(
        resident
            .attempt_public_manifest(
                baseline_publication.stage().unwrap(),
                &PublicationDecision::new()
            )
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    let a = begin_execution(&mut resident, &source, &owner);
    let b = begin_execution(&mut resident, &source, &owner);
    let private = execute_cell(
        &mut resident,
        &source,
        &recipe,
        &a,
        include_str!("fixtures/paired-instance-only.hs"),
    );
    assert!(resident
        .state
        .lib()
        .log
        .certified_authored_at(private)
        .unwrap()
        .introduced_exports()
        .is_empty());
    let intent = resident.freeze_private_execution(&a).unwrap();
    let accepted_a = certify_declarations(&mut resident, &source, owner.clone(), intent.clone());
    assert_eq!(
        accepted_a
            .accepted_join_for_test()
            .instances()
            .classes
            .len(),
        2
    );
    assert_eq!(
        accepted_a
            .accepted_join_for_test()
            .instances()
            .families
            .len(),
        2
    );
    assert!(accepted_a
        .accepted_join_for_test()
        .exports()
        .iter()
        .any(|export| export.head.module == SessionModule::lib(initial).module_name()));
    let conflicting = execute_cell(
        &mut resident,
        &source,
        &recipe,
        &b,
        include_str!("fixtures/paired-instance-only.hs"),
    );
    assert_ne!(private, conflicting);
    let b_intent = resident.freeze_private_execution(&b).unwrap();
    let b_publication = certify_declarations(&mut resident, &source, owner.clone(), b_intent);
    assert!(matches!(
        resident
            .attempt_public_manifest(b_publication.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert!(matches!(
        source_action_with_original(
            &mut resident,
            source.original_source().clone(),
            |resident, report| {
                resident
                    .state
                    .restage_declaration_publication(owner, intent)
                    .unwrap()
                    .certify(report)
            }
        )
        .unwrap(),
        CertifiedDeclarationPublication::Rejected(_)
    ));
}

#[test]
fn paired_ephemeral_completion_order_rebases_without_replaying_native_writes() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let lib = SessionLib::open(
        SessionId(4480),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(recipe.include.clone());
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &source);
    let a = begin_ephemeral_execution(&mut resident, &source);
    let b = begin_ephemeral_execution(&mut resident, &source);
    let (_, _, a_prefix) =
        execute_fixture_cell_for(&mut resident, &source, &recipe, &a, "let x = (701 :: Int)");
    let a_id = binding_id(&resident, a.private_scope(), "x");
    let (_, _, b_prefix) =
        execute_fixture_cell_for(&mut resident, &source, &recipe, &b, "let x = (702 :: Int)");
    let b_id = binding_id(&resident, b.private_scope(), "x");
    let completed_a = a_prefix.snapshot();
    let completed_b = b_prefix.snapshot();
    let a_intent = resident.freeze_private_execution(&a).unwrap();
    let b_intent = resident.freeze_private_execution(&b).unwrap();
    let before_publication = tidepool_extract_cmd::extract_spawn_count();
    let stale_a = ephemeral_binding_stage(&mut resident, &source, a_intent.clone());
    let b_stage = ephemeral_binding_stage(&mut resident, &source, b_intent);
    assert!(matches!(
        resident
            .attempt_public_manifest(b_stage, &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Ephemeral
    ));
    assert_eq!(binding_id(&resident, public, "x"), b_id);
    let decision = PublicationDecision::new();
    assert!(matches!(
        resident
            .attempt_public_manifest(stale_a, &decision)
            .unwrap(),
        crate::session::PublicationAttempt::Rebase(_)
    ));
    assert_eq!(decision.phase(), PublicationPhase::Running);
    let current_a = ephemeral_binding_stage(&mut resident, &source, a_intent.clone());
    assert!(matches!(
        resident
            .attempt_public_manifest(current_a, &decision)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Ephemeral
    ));
    assert_eq!(decision.phase(), PublicationPhase::Published);
    assert_eq!(binding_id(&resident, public, "x"), a_id);
    assert!(resident.state.bindings().get(b_id).is_some());
    assert!(resident.state.lib().durable_graph.is_none());
    assert!(Arc::ptr_eq(
        &resident.freeze_private_execution(&a).unwrap(),
        &a_intent
    ));
    assert!(Arc::ptr_eq(&a_prefix.snapshot(), &completed_a));
    assert!(Arc::ptr_eq(&b_prefix.snapshot(), &completed_b));
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        before_publication
    );
}

#[test]
fn paired_ephemeral_ticket_keeps_attached_manifest_and_admission_mode() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let mut lib = SessionLib::open(
        SessionId(4481),
        root.path().join("source"),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(recipe.include.clone());
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &source);
    let admission = begin_ephemeral_execution(&mut resident, &source);
    execute_cell(
        &mut resident,
        &source,
        &recipe,
        &admission,
        "let local = (703 :: Int)",
    );
    let id = binding_id(&resident, admission.private_scope(), "local");
    let before_freeze = resident.state.lib().generation();
    let before_freeze_revision = resident.state.lib().log.publication_revision();
    let intent = resident.freeze_private_execution(&admission).unwrap();
    assert_eq!(resident.state.lib().generation(), before_freeze);
    assert_eq!(
        resident.state.lib().log.publication_revision(),
        before_freeze_revision
    );
    let before = recovery.state();
    let cancelled = PublicationDecision::new();
    cancelled.request_cancellation();
    let stage = ephemeral_binding_stage(&mut resident, &source, intent.clone());
    assert_eq!(resident.state.lib().generation(), before_freeze);
    assert_eq!(
        resident.state.lib().log.publication_revision(),
        before_freeze_revision
    );
    assert!(matches!(
        resident
            .attempt_public_manifest(stage, &cancelled)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Cancelled
    ));
    assert!(!resident
        .public_visibility_snapshot_in(public)
        .unwrap()
        .bindings
        .iter()
        .any(|(name, _)| name == "local"));

    // Reserve the unrelated identity through its actual World plan. Its checked
    // adoption changes causal facts after the ticket without allocating again.
    let unrelated = begin_ephemeral_execution(&mut resident, &source);
    let planned = plan_fixture_cell_for(
        &mut resident,
        &source,
        &recipe,
        "unrelated :: Int\nunrelated = 704",
        unrelated.clone(),
    )
    .unwrap();
    let original = source.original_source().clone();
    let settled = original
        .with_source_compiler_transaction(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, report| turn::prepare_cell_program_admitted(owner, planned, report),
        )
        .unwrap();
    let (checked, program) = turn::finish_cell_program_admitted(settled).unwrap();
    assert_eq!(checked.items.len(), 1);
    let prefix = resident.begin_cell_program(program).unwrap().unwrap();
    let stale = ephemeral_binding_stage(&mut resident, &source, intent.clone());
    let generation = resident.state.lib().generation();
    let revision = resident.state.lib().log.publication_revision();
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: unrelated.private_scope(),
            ..SessionRunContext::ROOT
        })
        .unwrap();
    source_action_with_original(&mut resident, original, |resident, _| {
        let reservation = resident
            .admit_checked_item(prefix.clone(), checked.checked_item(0).unwrap())
            .unwrap();
        resident.adopt_checked_declaration(reservation).unwrap();
    });
    assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 1);
    assert_eq!(resident.state.lib().generation(), generation);
    assert_ne!(resident.state.lib().log.publication_revision(), revision);
    let decision = PublicationDecision::new();
    assert!(matches!(
        resident.attempt_public_manifest(stale, &decision).unwrap(),
        crate::session::PublicationAttempt::Settled(PublicManifestCommit::Ephemeral)
    ));
    assert_eq!(decision.phase(), PublicationPhase::Published);
    assert_eq!(recovery.state(), before);
    assert_eq!(resident.state.lib().generation(), generation);
    assert_eq!(binding_id(&resident, public, "local"), id);
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/ephemeral-mode").unwrap(),
        1,
    )
    .unwrap();
    resident
        .bind_durable_public_scope(owner.clone(), public)
        .unwrap();
    assert!(source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, _| { resident.restage_execution_publication(owner, intent) }
    )
    .is_err());
    assert!(source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, _| { resident.begin_ephemeral_private_execution(public) }
    )
    .is_err());
    assert_eq!(recovery.state(), before);
}

#[test]
fn paired_ephemeral_foreign_runtime_ticket_never_claims() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let make = || {
        let lib = SessionLib::open(
            SessionId(4482),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(recipe.include.clone());
        let mut resident =
            ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
        let source = resident.mint_notebook_scope_with_lease(None).unwrap();
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: source.scope(),
                ..SessionRunContext::ROOT
            })
            .unwrap();
        bootstrap_world_for(&mut resident, &recipe, &source);
        (resident, source)
    };
    let (mut first, source) = make();
    let public = source.scope();
    // The actual published World issues an unchanged completed frontier.
    let admission = begin_ephemeral_execution(&mut first, &source);
    let intent = first.freeze_private_execution(&admission).unwrap();
    let ticket = ephemeral_binding_stage(&mut first, &source, intent.clone());
    let (mut second, second_source) = make();
    let second_public = second_source.scope();
    let second_admission = begin_ephemeral_execution(&mut second, &second_source);
    second.freeze_private_execution(&second_admission).unwrap();
    assert_eq!(
        first.public_visibility_snapshot_in(public),
        second.public_visibility_snapshot_in(second_public)
    );
    assert_eq!(
        first.state.lib().log.publication_revision(),
        second.state.lib().log.publication_revision()
    );
    let decision = PublicationDecision::new();
    assert!(matches!(
        second.attempt_public_manifest(ticket, &decision).unwrap(),
        crate::session::PublicationAttempt::Settled(PublicManifestCommit::Refused { .. })
    ));
    assert_eq!(decision.phase(), PublicationPhase::Running);
    assert!(source_action_with_original(
        &mut second,
        second_source.original_source().clone(),
        |resident, _| { resident.restage_ephemeral_execution_publication(intent) }
    )
    .is_err());
}

#[test]
fn paired_foreign_staged_ticket_never_claims_publication() {
    tidepool_testing::eval_harness::require_extract();
    let root = tempfile::tempdir().unwrap();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    let mut lib = SessionLib::open(SessionId(986), root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut first =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let source = first.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/foreign-stage").unwrap(),
        1,
    )
    .unwrap();
    first
        .initialize_durable_public_scope(owner.clone(), public)
        .unwrap();
    first
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut first, &recipe, &source);
    let private = begin_execution(&mut first, &source, &owner);
    let intent = first.freeze_private_execution(&private).unwrap();
    let ExecutionPublication::Bindings(base) =
        restage_execution(&mut first, &source, owner.clone(), intent.clone())
    else {
        panic!("actual unchanged World publishes bindings");
    };
    let ticket = base.stage().unwrap();
    let mut second_lib =
        SessionLib::open(SessionId(986), root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(recipe.include.clone());
    tidepool_testing::with_settlement(|settlement| {
        recovery.attach(
            &mut second_lib,
            crate::session::recovery_store::RecoveryDomainId::Session(SessionId(986)),
            settlement,
        )
    })
    .unwrap();
    let mut second =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(second_lib));
    let second_source = second.mint_notebook_scope_with_lease(None).unwrap();
    let second_public = second_source.scope();
    second
        .initialize_durable_public_scope(owner.clone(), second_public)
        .unwrap();
    second
        .set_run_context(SessionRunContext {
            lexical_scope: second_public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut second, &recipe, &second_source);
    let second_private = begin_execution(&mut second, &second_source, &owner);
    second.freeze_private_execution(&second_private).unwrap();
    assert_eq!(
        private.admitted_public().unwrap(),
        second_private.admitted_public().unwrap()
    );
    assert_eq!(
        first.public_visibility_snapshot_in(private.private_scope()),
        second.public_visibility_snapshot_in(second_private.private_scope())
    );
    let decision = PublicationDecision::new();
    assert!(matches!(
        second.attempt_public_manifest(ticket, &decision).unwrap(),
        crate::session::PublicationAttempt::Settled(PublicManifestCommit::Refused { .. })
    ));
    assert_eq!(decision.phase(), PublicationPhase::Running);
    let ExecutionPublication::Bindings(base) =
        restage_execution(&mut first, &source, owner, intent)
    else {
        panic!("original unchanged World still owns its binding publication");
    };
    assert!(matches!(
        first
            .attempt_public_manifest(base.stage().unwrap(), &decision)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
}
