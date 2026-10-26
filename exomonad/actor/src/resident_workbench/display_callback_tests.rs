use std::sync::atomic::{AtomicUsize, Ordering};

use tidepool_effect::dispatch::{EffectContext, EffectDispatch, Response};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_mcp::CapturedOutput;
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_testing::effect_surface::TestEffectSurface;

use super::*;

struct ImmediatePrintProbe(Arc<AtomicUsize>);

impl DispatchEffect<CapturedOutput> for ImmediatePrintProbe {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Option<Response>, EffectError> {
        match crate::generated::console::ConsoleReq::from_value(request, cx.table()) {
            Ok(crate::generated::console::ConsoleReq::Print(text)) => {
                self.0.fetch_add(1, Ordering::SeqCst);
                cx.user().push(text);
                cx.respond(()).map(Some)
            }
            Ok(_) | Err(BridgeError::UnknownDataCon(_)) => Ok(None),
            Err(error) => Err(EffectError::Bridge(error)),
        }
    }

    fn prepare_dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<EffectDispatch, EffectError> {
        self.dispatch(request, cx).map(|response| match response {
            Some(response) => EffectDispatch::Immediate(response),
            None => EffectDispatch::Unhandled,
        })
    }
}

#[tokio::test]
async fn expansion_fences_immediate_handler_before_callback_input() {
    crate::resident_workbench::with_test_compiler_owner(
        expansion_fences_immediate_handler_with_compiler_owner,
    )
    .await;
}

async fn prepare_callback_fixture(
    source: crate::BoundActorWorkbenchSource,
    cell: String,
    private: bool,
) -> CallbackFixture {
    tidepool_testing::eval_harness::require_extract();
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::console_decl()]).unwrap();
    let root = tempfile::tempdir().unwrap();
    let session_id = tidepool_repr::SessionId((u64::from(std::process::id()) << 16) | 4_247);
    let lib = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(surface.include_paths().to_vec());
    let calls = Arc::new(AtomicUsize::new(0));
    let output = CapturedOutput::new();
    let mut session = ResidentSession::unbootstrapped(
        ImmediatePrintProbe(Arc::clone(&calls)),
        output.clone(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let source_lease = session.mint_notebook_scope_with_lease(None).unwrap();
    let (captured, _source_owner) =
        super::request_tests::world_query_test_source_files_with_support(
            &[],
            surface.include_paths(),
        );
    let mut context = {
        let context_actor = crate::ActorRef::first(crate::ActorId(1));
        let context_placement = crate::ActorPlacement {
            session: session_id,
            lexical_scope: source_lease.scope(),
            resource_scope: RealmId::fresh(),
        };
        crate::ActorSessionContext {
            actor: context_actor,
            placement: context_placement,
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: crate::ActorSourceImports::default(),
            haskell_effects_alias: surface.row().into(),
            current_model: None,
            source_layer: crate::ActorSourceRoots::Complete(captured),
            ..crate::ActorDescriptor::new("fixture", context_placement)
                .admit_independent_world()
                .session_context(context_actor)
        }
    };
    let public_context = context.clone();
    let machines = Arc::new(ActorMachineRegistry::<ImmediatePrintProbe, CapturedOutput>::new());
    machines.insert_idle(session_id, Box::new(session));
    let runner = ResidentActorRunner::new(machines.clone(), source.clone());
    runner
        .adopt_final_source_scope(public_context.clone())
        .await
        .unwrap();
    let admission = if private {
        let descriptor = crate::ActorDescriptor::new("display fixture", public_context.placement);
        let owner =
            crate::resident_actor::WorkbenchPublicOwner::issue(&public_context, &descriptor, None)
                .unwrap();
        let execution = runner
            .begin_private_execution(
                public_context.clone(),
                owner,
                tidepool_runtime::session::PublicationDecision::new(),
                None,
            )
            .await
            .unwrap();
        Some(execution.admission.clone())
    } else {
        None
    };
    if let Some(admission) = &admission {
        context.placement.lexical_scope = admission.private_scope();
    }
    let (mut session, receipt) = machines.checkout_run(session_id).unwrap().into_parts();
    session
        .set_actor_execution(
            context.run_context(),
            context.effect_policy,
            context.live_payload,
        )
        .unwrap();
    let view = actor_compile_view(&session, &context, source.config()).unwrap();
    let prepared = source.config().prepare(&view);
    let templates =
        resident_workbench_templates(&prepared.preamble, surface.row(), &prepared.imports);
    let include = prepared
        .include
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    let TurnResult::Bind { compiled, .. } = tidepool_testing::with_settlement(|settlement| {
        run_turn(
            TurnRequest {
                exact_context: None,
                session_id: Some(session_id),
                turn_text: &cell,
                templates: &templates,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &[],
                gen: view.next_value_generation().0,
                verdict: None,
                retained_imports: &[],
            },
            settlement,
        )
    })
    .unwrap() else {
        panic!("display callback fixture must compile as a bind");
    };
    let settled = session
        .with_source_compiler_transaction(
            source_lease.original_source().clone(),
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |session, settlement| {
                assert_eq!(
                    session.run_context().lexical_scope,
                    context.placement.lexical_scope
                );
                session.run_with_sites("displayCallbackFixture", compiled.code(), settlement)
            },
        )
        .unwrap();
    assert_eq!(
        settled.close(),
        &tidepool_runtime::CompilerTransactionClose::NotStarted
    );
    let outcome = settled.into_outcome().action.unwrap();
    let holes = session
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    machines.settle_suspended(receipt, session, holes);
    let realm = context.placement.resource_scope;
    let boundary = runner
        .capture_boundary(context.clone(), outcome, realm)
        .await
        .unwrap();
    let boundary = if let ResidentActorBoundary::DisplayAllowance { continuation } = boundary {
        let outcome = runner
            .resume_value(context.clone(), continuation, 1024_i64)
            .await
            .unwrap();
        runner
            .capture_boundary(context.clone(), outcome, realm)
            .await
            .unwrap()
    } else {
        boundary
    };
    let ResidentActorBoundary::DisplayPublish {
        continuation,
        callback,
        output: initial,
        ..
    } = boundary
    else {
        panic!("raw DisplayWith must retain the original callback");
    };
    let identity = (1_i64, 1_i64, 1_i64);
    assert!(matches!(
        runner
            .resume_value(context.clone(), continuation, identity)
            .await
            .unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    let scope = admission
        .as_ref()
        .map(|admission| admission.retained_lexical_scope());
    CallbackFixture {
        _root: root,
        runner,
        calls,
        output,
        public_context,
        admission,
        callback: Arc::new(callback),
        initial,
        execution: crate::request::sources::CallbackExecution {
            context,
            snapshot: None,
            scope,
        },
    }
}

struct CallbackFixture {
    initial: tidepool_runtime::session::WorkbenchDisplayPage,
    _root: tempfile::TempDir,
    runner: ResidentActorRunner<ImmediatePrintProbe, CapturedOutput>,
    calls: Arc<AtomicUsize>,
    output: CapturedOutput,
    public_context: crate::ActorSessionContext,
    admission: Option<Arc<tidepool_runtime::session::PrivateExecutionAdmission>>,
    callback: Arc<RootCustody>,
    execution: crate::request::sources::CallbackExecution,
}

async fn expansion_fences_immediate_handler_with_compiler_owner(
    preparation: crate::CompilerPreparationClient,
) {
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::console_decl()]).unwrap();
    let source = ActorWorkbenchSource::new(surface.preamble(), surface.include_paths().to_vec())
        .with_imports("qualified Tidepool.Aeson.FromJSON as Json\nTidepool.Inspection (displaySelectable, DisplayTree(..), displayTree)").bind_preparation(preparation);

    let fixture = prepare_callback_fixture(
        source.clone(),
        tidepool_testing::fixture_source(
            "exomonad/actor/src/resident_workbench/display_callback_immediate.hs",
        ),
        false,
    )
    .await;
    let CallbackFixture {
        runner,
        calls,
        output,
        callback,
        execution,
        ..
    } = fixture;
    let context = execution.context;
    let identity = (1_i64, 1_i64, 1_i64);
    let error = runner
        .expand_display(context.clone(), Arc::clone(&callback), identity, 1, 8192)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ResidentActorWorkbenchError::ActorProtocol(ref detail)
        if detail == "display callback crossed an unauthorized boundary")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(output.snapshot().is_empty());

    // The same retained callback really dispatches Print under the ordinary
    // policy; rejection above must happen before entering the mutable handler.
    let ordinary = runner
        .access
        .with_machine(context, move |session, context, _| {
            tidepool_testing::with_settlement(|settlement| {
                session.run_rooted_entry_borrowed(
                    "ordinaryDisplayCallback",
                    &callback,
                    0,
                    context.placement.resource_scope,
                    None,
                    settlement,
                )
            })
            .map_err(ResidentActorWorkbenchError::Resident)
        })
        .await
        .unwrap();
    assert!(matches!(ordinary, ResidentOutcome::Completed { output, .. }
        if output == ["unauthorized callback output"]));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(output.snapshot().is_empty());
}

#[tokio::test]
async fn retained_display_runs_after_private_admission_settles_and_current_scope_changes() {
    crate::resident_workbench::with_test_compiler_owner(retained_display_runs_after_private_admission_settles_and_current_scope_changes_with_preparation).await;
}

async fn retained_display_runs_after_private_admission_settles_and_current_scope_changes_with_preparation(
    preparation: crate::CompilerPreparationClient,
) {
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::console_decl()]).unwrap();
    let source = ActorWorkbenchSource::new(surface.preamble(), surface.include_paths().to_vec())
        .with_imports("qualified Tidepool.Aeson.FromJSON as Json\nTidepool.Inspection (displaySelectable, DisplayTree(..), displayTree)").bind_preparation(preparation);

    let mut fixture = prepare_callback_fixture(
        source.clone(),
        tidepool_testing::fixture_source(
            "exomonad/actor/src/resident_workbench/display_callback_retained.hs",
        ),
        true,
    )
    .await;
    let scope_a = fixture.execution.context.placement.lexical_scope;
    drop(fixture.admission.take());
    let descriptor =
        crate::ActorDescriptor::new("display fixture", fixture.public_context.placement);
    let owner = crate::resident_actor::WorkbenchPublicOwner::issue(
        &fixture.public_context,
        &descriptor,
        None,
    )
    .unwrap();
    let execution_b = fixture
        .runner
        .begin_private_execution(
            fixture.public_context.clone(),
            owner,
            tidepool_runtime::session::PublicationDecision::new(),
            None,
        )
        .await
        .unwrap();
    let scope_b = execution_b.private_scope;
    let mut context_b = fixture.public_context.clone();
    context_b.placement.lexical_scope = scope_b;
    fixture
        .runner
        .access
        .with_machine(context_b, |session, context, _| {
            assert_eq!(
                session.run_context().lexical_scope,
                context.placement.lexical_scope
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_ne!(scope_a, scope_b);
    let (page, _) = fixture
        .runner
        .expand_display(
            fixture.execution.context.clone(),
            fixture.callback.clone(),
            (1, 1, 1),
            1,
            8192,
        )
        .await
        .unwrap();
    assert_eq!(page.text, "detail A");
    assert_eq!(page.identity, (1, 1, 1));
    assert!(page.expansions.is_empty());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.execution.scope.take();
    fixture
        .runner
        .access
        .with_compiler_world(
            fixture.public_context.clone(),
            None,
            |session, context, _| {
                let _ = session
                    .begin_ephemeral_private_execution(context.placement.lexical_scope)
                    .map_err(ResidentError::Session)?;
                Ok(())
            },
        )
        .await
        .unwrap();
    let error = fixture
        .runner
        .expand_display(
            fixture.execution.context.clone(),
            fixture.callback.clone(),
            (1, 1, 1),
            1,
            8192,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ResidentActorWorkbenchError::Resident(ResidentError::Session(tidepool_runtime::session::SessionError::DeadScope(scope))) if scope == scope_a),
        "{error}"
    );
}

#[tokio::test]
async fn filesystem_errors_retain_expandable_display() {
    crate::resident_workbench::with_test_compiler_owner(
        filesystem_errors_retain_expandable_display_with_preparation,
    )
    .await;
}

async fn filesystem_errors_retain_expandable_display_with_preparation(
    preparation: crate::CompilerPreparationClient,
) {
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::console_decl()]).unwrap();
    let source = ActorWorkbenchSource::new(surface.preamble(), surface.include_paths().to_vec())
        .bind_preparation(preparation);
    let errors = prepare_callback_fixture(
        source,
        tidepool_testing::fixture_source(
            "exomonad/actor/src/resident_workbench/display_filesystem_errors.hs",
        ),
        false,
    )
    .await;
    assert!(!errors.initial.unavailable);
    for name in [
        "FsNotFound",
        "FsNotUtf8",
        "FsSandbox",
        "FsBadRegex",
        "FsIo",
        "FsDurabilityUnknown",
        "FsNonUtf8Path",
    ] {
        assert!(
            errors.initial.text.contains(name),
            "missing {name}: {}",
            errors.initial.text
        );
    }
    assert!(errors.initial.text.contains("Right (Left"));
}
