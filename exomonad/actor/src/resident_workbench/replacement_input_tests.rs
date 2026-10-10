//! Replacement inputs retain independent caller and predecessor custody.

use tidepool_repr::{Generation, SessionId};
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_testing::effect_surface::TestEffectSurface;

use super::*;

type Runner = ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>;

struct Fixture {
    _root: tempfile::TempDir,
    surface: TestEffectSurface,
    compiled: CompiledTurn,
    bound: Vec<BoundBinder>,
}

impl Fixture {
    fn compile() -> Self {
        tidepool_testing::eval_harness::require_extract();
        let surface = TestEffectSurface::minimal(&[]).unwrap();
        let root = tempfile::tempdir().unwrap();
        let templates = resident_workbench_templates(surface.preamble(), surface.row(), "");
        let include = surface
            .include_paths()
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let TurnResult::Bind {
            compiled, bound, ..
        } = tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: None,
                    turn_text: include_str!("replacement_inputs.hs"),
                    templates: &templates,
                    include: &include,
                    session_root: root.path(),
                    inject_modules: &[],
                    gen: 1,
                    verdict: None,
                    target: None,
                    retained_imports: &[],
                },
                settlement,
            )
        })
        .expect("compile immutable native replacement inputs")
        else {
            panic!("replacement fixture must bind its two inputs");
        };
        assert_eq!(bound.len(), 2);
        Self {
            _root: root,
            surface,
            compiled,
            bound,
        }
    }

    fn machines(&self) -> (Runner, Vec<tempfile::TempDir>) {
        let machines = Arc::new(ActorMachineRegistry::new());
        let images = Arc::new(tidepool_runtime::session::ImageRegistry::new());
        let mut roots = Vec::new();
        for id in [SessionId(0xE101), SessionId(0xE102), SessionId(0xE103)] {
            let root = tempfile::tempdir().unwrap();
            let library = SessionLib::open(id, root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(self.surface.include_paths().to_vec());
            let mut session = ResidentSession::unbootstrapped(
                frunk::HNil,
                tidepool_mcp::CapturedOutput::new(),
                tidepool_runtime::DEFAULT_NURSERY_SIZE,
                Some(library),
            );
            let outcome = tidepool_testing::with_settlement(|settlement| {
                session.run_projected_bind_with_sites(
                    "replacement-native-inputs",
                    self.compiled.code(),
                    &self.bound,
                    Generation(1),
                    settlement,
                )
            })
            .unwrap();
            assert!(matches!(outcome, ResidentOutcome::BindingsCommitted { .. }));
            session.set_image_registry(Arc::clone(&images));
            assert_eq!(session.outstanding_custody(), 0);
            machines.insert_idle(id, Box::new(session));
            roots.push(root);
        }
        let source = ActorWorkbenchSource::new(
            self.surface.preamble(),
            self.surface.include_paths().to_vec(),
        );
        (ResidentActorRunner::new(machines, source), roots)
    }
}

fn context(session: SessionId) -> crate::ActorSessionContext {
    crate::ActorDescriptor::new(
        "replacement-inputs",
        crate::ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    )
    .session_context(crate::ActorRef::first(crate::ActorId(901)))
}

async fn retain(runner: &Runner, session: SessionId, name: &str) -> RootCustody {
    let name = name.to_owned();
    runner
        .access
        .with_host_machine(
            "retain-replacement-input",
            session,
            None,
            move |session, _| session.retain_binding_custody(&name).map_err(Into::into),
        )
        .await
        .unwrap()
        .unwrap()
}

async fn counts(runner: &Runner) -> Vec<usize> {
    let mut counts = Vec::new();
    for id in [SessionId(0xE101), SessionId(0xE102), SessionId(0xE103)] {
        counts.push(
            runner
                .access
                .with_host_machine("replacement-custody-count", id, None, |session, _| {
                    Ok(session.outstanding_custody())
                })
                .await
                .unwrap(),
        );
    }
    counts
}

async fn verify_checkpoint(runner: &Runner, predecessor: SessionId, value: Arc<RootCustody>) {
    let entry = retain(runner, predecessor, "replacementEntry").await;
    let context = context(predecessor);
    let realm = context.placement.resource_scope;
    let outcome = runner
        .run_rooted_application(context, entry, value, realm)
        .await
        .unwrap();
    let ResidentOutcome::Completed { result, .. } = outcome else {
        panic!("checkpoint verifier must complete: {outcome:?}");
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
}

#[tokio::test]
async fn replacement_inputs_transfer_from_caller_and_borrow_predecessor_across_placements() {
    with_test_compiler_owner(async {
        let fixture = Fixture::compile();
        let c = SessionId(0xE101);
        let p = SessionId(0xE102);
        let s = SessionId(0xE103);
        for (caller, predecessor, successor) in
            [(c, p, s), (c, p, p), (c, p, c), (p, p, s), (p, p, p)]
        {
            let (runner, _roots) = fixture.machines();
            let entry =
                crate::MailboxValue::new(caller, retain(&runner, caller, "replacementEntry").await);
            let checkpoint = Arc::new(retain(&runner, predecessor, "replacementCheckpoint").await);
            let context = context(successor);
            let realm = context.placement.resource_scope;
            let (entry, imported) = runner
                .prepare_replacement_inputs(
                    context.clone(),
                    entry,
                    predecessor,
                    Arc::clone(&checkpoint),
                )
                .await
                .unwrap();
            assert_eq!(
                Arc::ptr_eq(&checkpoint, &imported),
                predecessor == successor
            );
            let outcome = runner
                .run_rooted_application(context, entry, imported, realm)
                .await
                .unwrap();
            let ResidentOutcome::Completed { result, .. } = outcome else {
                panic!("transferred entry must complete: {outcome:?}");
            };
            assert_eq!(
                result.to_json(),
                serde_json::json!(42),
                "{caller:?}/{predecessor:?}/{successor:?}"
            );
            verify_checkpoint(&runner, predecessor, Arc::clone(&checkpoint)).await;
            assert_eq!(
                counts(&runner).await.iter().sum::<usize>(),
                1,
                "only predecessor checkpoint remains"
            );
            drop(checkpoint);
            assert_eq!(counts(&runner).await, [0, 0, 0]);
        }

        // A failure after the entry crossed must release its provisional root
        // while preserving the predecessor's independent checkpoint.
        let (runner, _roots) = fixture.machines();
        let entry = crate::MailboxValue::new(c, retain(&runner, c, "replacementEntry").await);
        let checkpoint = Arc::new(retain(&runner, p, "replacementCheckpoint").await);
        let error = runner
            .prepare_replacement_inputs(context(s), entry, c, Arc::clone(&checkpoint))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ResidentActorWorkbenchError::Resident(ResidentError::ForeignCustody)
        ));
        verify_checkpoint(&runner, p, Arc::clone(&checkpoint)).await;
        assert_eq!(counts(&runner).await, [0, 1, 0]);
        drop(checkpoint);
        assert_eq!(counts(&runner).await, [0, 0, 0]);

        // Cancelling while the caller's machine is checked out drops only the
        // affine entry. No successor root has been admitted yet.
        use std::future::Future;
        use std::task::Poll;
        let (runner, _roots) = fixture.machines();
        let entry = crate::MailboxValue::new(c, retain(&runner, c, "replacementEntry").await);
        let checkpoint = Arc::new(retain(&runner, p, "replacementCheckpoint").await);
        let caller_checkout = runner.access.machines.checkout_run(c).unwrap();
        let mut transfer = Box::pin(runner.prepare_replacement_inputs(
            context(s),
            entry,
            p,
            Arc::clone(&checkpoint),
        ));
        assert!(matches!(
            std::future::poll_fn(|cx| Poll::Ready(transfer.as_mut().poll(cx))).await,
            Poll::Pending
        ));
        drop(transfer);
        drop(caller_checkout);
        verify_checkpoint(&runner, p, Arc::clone(&checkpoint)).await;
        assert_eq!(counts(&runner).await, [0, 1, 0]);
        drop(checkpoint);
        assert_eq!(counts(&runner).await, [0, 0, 0]);
    })
    .await;
}
