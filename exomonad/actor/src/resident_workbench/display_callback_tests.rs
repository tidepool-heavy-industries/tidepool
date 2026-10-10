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
    tidepool_testing::eval_harness::require_extract();
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::console_decl()]).unwrap();
    let source = ActorWorkbenchSource::new(surface.preamble(), surface.include_paths().to_vec());
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
    let context = crate::ActorSessionContext {
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
        source_layer: Arc::from([]),
    };
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
                turn_text: &tidepool_testing::fixture_source(
                    "exomonad/actor/src/resident_workbench/display_callback_immediate.hs",
                ),
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
    let ResidentActorBoundary::DisplayPublish {
        continuation,
        callback,
        ..
    } = runner
        .capture_boundary(context.clone(), outcome, realm)
        .await
        .unwrap()
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
    let callback = Arc::new(callback);
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
    assert!(matches!(ordinary, ResidentOutcome::Completed { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(output.snapshot(), ["unauthorized callback output"]);
}
