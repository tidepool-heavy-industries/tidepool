//! A prepared root's first effect publishes without awaiting a provider consumer.

use super::*;
use harness::store::actor_output::{ActorOutputExecution, ActorOutputOrigin};
use tidepool_runtime::session::ModuleEnv;

#[tokio::test]
async fn prepared_first_display_returns_admission_before_consumer_and_survives_failure() {
    tidepool_testing::eval_harness::require_extract();
    for (source, fails) in [
        (
            tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/prepared_display_success.hs",
            ),
            false,
        ),
        (
            tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/prepared_display_failure.hs",
            ),
            true,
        ),
    ] {
        let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
        let run_root = tempfile::tempdir().unwrap();
        let (manager, bindings) = actor_worktree_resources_at(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(run_root.path())
                .unwrap()
                .child("worktrees")
                .unwrap(),
            repository.path(),
        )
        .unwrap();
        let authority = ActorWorktreeAuthority::new(
            runtime_namespace(run_root.path()),
            Arc::new(Mutex::new(bindings)),
        );
        let worktree = ActorWorktreeHandler::new(WorktreeHandler::from_manager(manager), authority);
        let journal = tidepool_handlers::JournalHandler::new(
            tidepool_handlers::SegmentPath::create_exclusive(run_root.path().join("journal.jsonl"))
                .unwrap(),
        )
        .unwrap();
        let declarations = exomonad_effect_declarations();
        let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
        let mut include = effects.include_paths().to_vec();
        include.push(crate::haskell_sources::ensure_exomonad_haskell().unwrap());
        include.push(tidepool_testing::eval_harness::prelude_path());
        let preamble = insert_preamble_imports(
            &tidepool_mcp::build_notebook_preamble(&declarations, false),
            "qualified Tidepool.Effects.Core as Core",
        );
        let templates = resident_workbench_templates(&preamble, "'[Console]", "");
        let session = tidepool_runtime::session::fresh_session_id();
        let library = SessionLib::open(
            session,
            run_root.path().join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(include.clone());
        let mut machine = ResidentSession::unbootstrapped(
            hlist![
                source_handler(None),
                journal,
                RepoEventHandler::with_source(
                    Box::new(InertObservationSource),
                    EventConfig::default(),
                ),
                ActorBoundWorktreeHandler::new(worktree.clone()),
                ActorWorktreeRegistryHandler::new(worktree.clone()),
                ActorWorktreeAllocationHandler::new(worktree.clone()),
                ActorWorktreeIntegrationHandler::new(worktree.clone()),
                worktree,
            ],
            CapturedOutput::new(),
            DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        let retained = machine.prepared_retained();
        let include_refs = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let compiled = match run_turn(HaskellTurnRequest {
            exact_context: None,
            session_id: None,
            turn_text: source,
            templates: &templates,
            include: &include_refs,
            session_root: run_root.path(),
            inject_modules: &[],
            gen: 1,
            verdict: None,
            target: None,
            retained_imports: &retained,
        })
        .unwrap()
        {
            TurnResult::Expr { compiled, .. } => compiled,
            other => panic!("prepared display must compile as an expression: {other:?}"),
        };
        machine.set_effect_execution(
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        let outcome = machine
            .run_with_sites("prepared_first_display", compiled.code())
            .unwrap();
        let descriptor = ActorDescriptor::new(
            "prepared-display",
            ActorPlacement {
                session,
                resource_scope: tidepool_codegen::suspension::RealmId::fresh(),
                lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
            },
        );
        let (forest, mut deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            None,
            exomonad_actor::Incarnation::FIRST,
        );
        // No receiver/provider has been started. The timeout only detects the
        // circular admission dependency, not a performance threshold.
        let (actor, task) = tokio::time::timeout(
            Duration::from_secs(30),
            forest.admit_root(descriptor, outcome),
        )
        .await
        .expect("root admission must return before the output consumer starts")
        .unwrap();
        let request = loop {
            match deployments.recv().await.unwrap() {
                LocalResidentDeployment::DisplayPublished(request) => break request,
                event => panic!(
                    "first authored effect must publish display: {}",
                    event.kind()
                ),
            }
        };
        assert_eq!(request.actor, actor.identity());
        assert_eq!(request.page_ordinal, 1);
        assert!(request.operation.is_none());
        assert!(request.outcome().is_none());
        assert_eq!(request.page.text, "prepared startup output");
        forest.authorize_display_publication(&request).unwrap();
        let store = display_output::open_run_store(run_root.path()).unwrap();
        let run = runtime_namespace(run_root.path());
        display_output::publish(&forest, &store, &run, None, None, &request);
        let origin = ActorOutputOrigin {
            run,
            native_actor: actor.identity().id.0,
            incarnation: actor.identity().incarnation.0,
        };
        let history = store.actor_output_page(&origin, 0, 10).unwrap();
        assert_eq!(history.outputs.len(), 1);
        let output = &history.outputs[0];
        assert!(output.emission().conversation.is_none());
        assert_eq!(
            output.emission().execution,
            ActorOutputExecution::ActorProgram
        );
        assert_eq!(output.emission().page.text, "prepared startup output");
        let Some(exomonad_actor::DisplayPublicationOutcome::Published(reference)) =
            request.outcome()
        else {
            panic!("the production Store adapter must return the canonical reference")
        };
        assert_eq!(reference.sequence, output.reference().sequence);
        assert_eq!(reference.run, origin.run);
        task.await.unwrap();
        let terminal = actor.terminal().wait().await;
        assert_eq!(
            terminal.kind == ActorExitKind::Failed,
            fails,
            "{terminal:?}"
        );
        if !fails {
            assert_eq!(terminal.kind, ActorExitKind::Completed);
        }
        assert_eq!(
            store
                .actor_output_page(&origin, 0, 10)
                .unwrap()
                .outputs
                .len(),
            1
        );
        assert!(matches!(
            deployments.recv().await,
            Some(LocalResidentDeployment::Retired { actor: retired, .. })
                if retired == actor.identity()
        ));
        forest.shutdown().await;
    }
}
