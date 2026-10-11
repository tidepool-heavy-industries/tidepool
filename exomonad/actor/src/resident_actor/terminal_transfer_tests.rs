//! Owning private-publication settlement and scope atomicity.

use super::*;
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_testing::eval_harness;

async fn execute(
    actor: &LocalActorRef,
    source: &str,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
) -> crate::KernelWorkbenchReply {
    let (reply, receive) = tokio::sync::oneshot::channel();
    actor
        .address()
        .send_message(KernelMessage::Workbench {
            invocation: crate::ActorWorkbenchInvocation::unbound(
                WorkbenchRequest::from_cell_input(source),
            ),
            control,
            reply: reply.into(),
        })
        .expect("admit original workbench execution");
    receive
        .await
        .expect("owning actor settles workbench caller")
}

#[tokio::test]
async fn private_three_item_cell_refusal_keeps_receipts_and_actor_live() {
    eval_harness::require_extract();
    tokio::time::timeout(std::time::Duration::from_secs(240), async {
        let root = tempfile::tempdir().expect("session root");
        let session = tidepool_runtime::session::fresh_session_id();
        let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[])
            .expect("pure workbench compiler surface");
        let include = effects.include_paths().to_vec();
        let library = SessionLib::open(
            session,
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("declaration plane")
        .with_validation_include(include.clone());
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        let (forest, _deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(effects.preamble().to_owned(), include),
            session,
            machine,
            None,
            crate::Incarnation::FIRST,
        );
        let actor = forest
            .new_workbench(
                "private-publication-refusal".into(),
                crate::ActorCapabilities::default().with_effect_keys(Vec::new()),
            )
            .await
            .expect("root workbench");
        let context = forest
            .directory
            .session_context(actor.identity())
            .expect("actual root context");
        let public_workbench = forest.environment.runner.application_workbench();
        let before = public_workbench
            .live_bindings(context.clone())
            .await
            .expect("read original public bindings")
            .into_iter()
            .map(|binding| binding.name)
            .collect::<std::collections::BTreeSet<_>>();

        let control = crate::WorkbenchExecutionControl::untracked();
        control.publication_decision().terminate();
        assert!(!control
            .native_cancel()
            .load(std::sync::atomic::Ordering::Acquire));
        let failure = execute(
            &actor,
            "privateFirst <- pure (40 :: Int)\nprivateSecond <- pure (privateFirst + 2)\npure privateSecond",
            Some(control.clone()),
        )
        .await
        .expect_err("whole-cell publication was deterministically refused");
        let KernelInvocationFailure::Workbench(original) = failure else {
            panic!("expected publication failure, got {failure:?}");
        };
        assert_eq!(
            original.point,
            tidepool_runtime::session::WorkbenchFailurePoint::Publication {
                completed_input_units: 3,
            },
            "original execution failed before publication: {original:?}"
        );
        assert_eq!(original.receipts.len(), 3, "all three units completed privately");
        assert!(original.receipts.iter().all(|receipt| {
            receipt.status == WorkbenchItemStatus::Committed
        }));
        assert!(original.receipts[2].output.contains("42"), "{:?}", original.receipts[2]);
        assert!(matches!(
            original.publication.as_ref(),
            Some(WorkbenchPublicationOutcome::Rejected { .. })
        ));
        assert!(original
            .publication
            .as_ref()
            .expect("publication outcome")
            .public_bindings()
            .is_empty());
        let after = public_workbench
            .live_bindings(context)
            .await
            .expect("read live public bindings after refusal")
            .into_iter()
            .map(|binding| binding.name)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(after, before, "private names never entered public scope");
        assert!(actor.terminal().get().is_none(), "ordinary refusal keeps actor live");
        assert_eq!(
            control.terminal_reply(),
            Some(Err(KernelInvocationFailure::Workbench(original)))
        );
        forest.shutdown().await;
    })
    .await
    .expect("private three-item publication refusal completes");
}
