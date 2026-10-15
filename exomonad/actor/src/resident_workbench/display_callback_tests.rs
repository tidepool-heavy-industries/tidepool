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
    with_test_compiler_owner(expansion_fences_immediate_handler_with_compiler_owner()).await;
}

async fn prepare_callback_fixture(cell: String, private: bool) -> CallbackFixture {
    tidepool_testing::eval_harness::require_extract();
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::console_decl()]).unwrap();
    let source = ActorWorkbenchSource::new(surface.preamble(), surface.include_paths().to_vec())
        .with_imports("qualified Tidepool.Lookup.Navigation as Nav\nqualified Tidepool.Aeson.FromJSON as Json\nTidepool.Inspection (displaySelectable, DisplayTree(..), displayTree)");
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
    let mut context = crate::ActorSessionContext {
        actor: crate::ActorRef::first(crate::ActorId(1)),
        placement: crate::ActorPlacement {
            session: session_id,
            lexical_scope: session.mint_isolated_scope(),
            resource_scope: RealmId::fresh(),
        },
        effect_policy: EffectRunPolicy::HandleOrSuspend,
        live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        source_imports: crate::ActorSourceImports::default(),
        haskell_effects_alias: surface.row().into(),
        source_layer: Vec::new().into(),
    };
    let public_context = context.clone();
    let admission = private.then(|| {
        Arc::new(
            session
                .begin_ephemeral_private_execution(context.placement.lexical_scope)
                .unwrap(),
        )
    });
    if let Some(admission) = &admission {
        context.placement.lexical_scope = admission.private_scope();
    }
    session
        .set_actor_execution(
            context.run_context(),
            context.effect_policy,
            context.live_payload,
        )
        .unwrap();
    let view = actor_compile_view(&session, &context, &source).unwrap();
    let prepared = source.prepare(&view);
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
                target: None,
                retained_imports: &[],
            },
            settlement,
        )
    })
    .unwrap() else {
        panic!("display callback fixture must compile as a bind");
    };
    let outcome = tidepool_testing::with_settlement(|settlement| {
        session.run_with_sites("displayCallbackFixture", compiled.code(), settlement)
    })
    .unwrap();
    let machines = Arc::new(ActorMachineRegistry::<ImmediatePrintProbe, CapturedOutput>::new());
    machines.insert_idle(session_id, Box::new(session));
    let runner = ResidentActorRunner::new(machines, source);
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

async fn expansion_fences_immediate_handler_with_compiler_owner() {
    let fixture = prepare_callback_fixture(
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
    with_test_compiler_owner(async {
        let mut fixture = prepare_callback_fixture(tidepool_testing::fixture_source("exomonad/actor/src/resident_workbench/display_callback_retained.hs"), true).await;
        let scope_a = fixture.execution.context.placement.lexical_scope;
        drop(fixture.admission.take());
        let scope_b = fixture.runner.access.with_machine(fixture.public_context.clone(), move |session, _, _| {
            let scope_b = session.mint_isolated_scope();
            // A later checkout drains completed admission leases. The callback's
            // owner-derived lease must preserve its exact original scope.
            let _ = session.begin_ephemeral_private_execution(scope_b).map_err(ResidentError::Session)?;
            Ok(scope_b)
        }).await.unwrap();
        assert_ne!(scope_a, scope_b);
        let (page, _) = fixture.runner.expand_display(fixture.execution.context.clone(), fixture.callback.clone(), (1, 1, 1), 1, 8192).await.unwrap();
        assert_eq!(page.text, "detail A");
        assert_eq!(page.identity, (1, 1, 1));
        assert!(page.expansions.is_empty());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        fixture.execution.scope.take();
        fixture.runner.access.with_machine(fixture.public_context.clone(), |session, context, _| {
            let _ = session.begin_ephemeral_private_execution(context.placement.lexical_scope).map_err(ResidentError::Session)?;
            Ok(())
        }).await.unwrap();
        let error = fixture.runner.expand_display(fixture.execution.context.clone(), fixture.callback.clone(), (1, 1, 1), 1, 8192).await.unwrap_err();
        assert!(matches!(error, ResidentActorWorkbenchError::Resident(ResidentError::Session(tidepool_runtime::session::SessionError::DeadScope(scope))) if scope == scope_a), "{error}");
    }).await;
}

#[tokio::test]
async fn filesystem_errors_and_lookup_batch_retain_expandable_display() {
    with_test_compiler_owner(async {
        let fixture = prepare_callback_fixture(
            tidepool_testing::fixture_source(
                "exomonad/actor/src/resident_workbench/display_lookup_errors.hs",
            ),
            false,
        )
        .await;
        assert!(
            fixture.initial.text.contains("retained-query"),
            "{}",
            fixture.initial.text
        );
        assert!(fixture
            .initial
            .expansions
            .iter()
            .any(|(_, label)| label == "index" || label == "hits"));
        assert!(!fixture.initial.unavailable);
        assert!(!fixture.initial.expansions.is_empty());
        let mut pending = fixture.initial.expansions.clone();
        let mut rendered = fixture.initial.text.clone();
        let mut callback = fixture.callback.clone();
        for _ in 0..64 {
            let Some((key, _)) = pending.first().cloned() else {
                break;
            };
            let (page, retained) = fixture
                .runner
                .expand_display(
                    fixture.execution.context.clone(),
                    callback,
                    (1, 1, 1),
                    key,
                    8192,
                )
                .await
                .unwrap();
            assert!(!page.unavailable);
            rendered.push_str(&page.text);
            pending = page.expansions;
            callback = Arc::new(retained);
        }
        assert!(pending.is_empty(), "expansion did not settle");
        for name in [
            "retained-query",
            "end-of-retained-query",
            "second-query",
            "missing-api",
            "retained contract hash token",
        ] {
            assert!(rendered.contains(name), "missing {name}: {rendered}");
        }
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        let errors = prepare_callback_fixture(
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
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retained_lookup_selection_revisits_and_expands_without_lookup_or_effects() {
    with_test_compiler_owner(retained_lookup_selection_with_compiler_owner()).await;
}

async fn retained_lookup_selection_with_compiler_owner() {
    let fixture = prepare_callback_fixture(
        tidepool_testing::fixture_source(
            "exomonad/actor/src/resident_workbench/display_lookup_navigation.hs",
        ),
        false,
    )
    .await;
    assert!(fixture.initial.text.contains("original-capture"));
    let selection = |section: serde_json::Value| {
        serde_json::json!({
            "address":{"resultOrdinal":1,"entryOrdinal":1,"section":section},"lineWindow":null
        })
    };
    let request = serde_json::json!([
        selection(serde_json::json!({"tag":"Contract"})),
        selection(serde_json::json!({"tag":"Setup","contents":1})),
        selection(serde_json::json!({"tag":"Example"}))
    ])
    .to_string();
    let identity = (1, 1, 1);
    let mut callback = fixture.callback.clone();
    for _ in 0..2 {
        let (page, retained) = fixture
            .runner
            .navigate_display(
                fixture.execution.context.clone(),
                callback,
                identity,
                0,
                Some(request.clone()),
                8192,
            )
            .await
            .unwrap();
        assert!(!page.unavailable);
        let mut text = page.text;
        let mut keys = page.expansions;
        callback = Arc::new(retained);
        for _ in 0..128 {
            let Some((key, _)) = keys.first() else {
                break;
            };
            let (page, retained) = fixture
                .runner
                .expand_display(
                    fixture.execution.context.clone(),
                    callback,
                    identity,
                    *key,
                    8192,
                )
                .await
                .unwrap();
            assert!(!page.unavailable);
            text.push_str(&page.text);
            keys = page.expansions;
            callback = Arc::new(retained);
        }
        assert!(keys.is_empty());
        for marker in [
            "original-capture",
            "main-digest",
            "exact-owner-contract-marker",
            "complete-setup-marker",
            "last-main-marker",
        ] {
            assert!(text.contains(marker), "missing {marker}: {text}");
        }
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(fixture.output.snapshot().is_empty());
    crate::resident_actor::assert_selectable_display_lifetime(
        Arc::try_unwrap(callback).expect("last returned callback has one native owner"),
        fixture.execution,
    );
}
