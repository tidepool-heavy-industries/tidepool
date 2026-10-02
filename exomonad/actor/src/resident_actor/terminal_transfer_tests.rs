//! Native reply acceptance followed by deterministic private-publication refusal.

use super::*;
use crate::request::{ResponseFailure, ResponseObservation};
use tidepool_runtime::session::{insert_preamble_imports, ModuleEnv, SessionLib};
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
async fn accepted_native_reply_publication_refusal_settles_request_and_retires_actor() {
    eval_harness::require_extract();
    // This is one compiler-backed scenario. The outer deadline bounds the
    // fixture; assertions use exact request/actor events rather than sleeps.
    tokio::time::timeout(std::time::Duration::from_secs(240), async {
        let declarations = [
            tidepool_mcp::agent_tools_decl(),
            tidepool_mcp::agent_session_decl(),
            tidepool_mcp::actor_decl(),
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
            tidepool_mcp::fs_read_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::notifications_decl(),
            tidepool_mcp::console_decl(),
            tidepool_mcp::sleep_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("effect module");
        let mut include = effects.include_paths().to_vec();
        include.push(eval_harness::prelude_path());
        include.push(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../bridge/haskell/actors"),
        );
        let mut preamble = tidepool_mcp::build_preamble(&declarations, false);
        for import in [
            "qualified Tidepool.Actor as Mailbox",
            "Tidepool.Agent.Ref (AgentProtocol(..))",
            "qualified Tidepool.Agent.Ref as AgentRef",
            "qualified Tidepool.Actors.Internal.Agent as Agents",
        ] {
            preamble = insert_preamble_imports(&preamble, import);
        }
        let root = tempfile::tempdir().expect("session root");
        let session = tidepool_repr::SessionId(std::process::id() as u64 * 10_000 + 185);
        let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .expect("declaration plane")
            .with_validation_include(include.clone());
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let (forest, mut deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            None,
            crate::Incarnation::FIRST,
        );
        let requester = forest
            .new_workbench("terminal-transfer-requester".into(), crate::EffectiveRole::root())
            .await
            .expect("requesting workbench");
        let setup = execute(&requester, include_str!("terminal_transfer_setup.hs"), None)
            .await
            .expect("native receiving child admitted");
        assert_eq!(setup.status, WorkbenchRunStatus::Committed);

        // Setup has fully committed and its cell cleanup has run before the
        // child receiver is used. A parent-owned startup hole cannot pass this.
        let submission = execute(
            &requester,
            "let Right requestLabel = Agents.labelFromText \"publication-refusal\"\nresponse <- Agents.request @Int nativeAgent (Agents.assignment requestLabel ())\npure True",
            None,
        )
        .await
        .expect("typed request submitted to retained child receiver");
        assert_eq!(submission.status, WorkbenchRunStatus::Committed);
        let pending = forest.environment.requests.status_for(requester.identity()).pending_responses;
        assert_eq!(pending.len(), 1, "one original typed request");
        let request = pending[0].0;
        let target = forest.environment.requests.target_for(request).expect("exact request target");
        let child = forest.directory.resolve(target).expect("retained native child");
        loop {
            let event = deployments.recv().await.expect("native readiness deployment");
            if let LocalResidentDeployment::PolicyInstalled(installation) = event {
                if installation.actor.identity() == target {
                    assert_eq!(
                        installation.runtime_observation.snapshot().request_activation
                            .expect("initial typed activation").request,
                        request,
                    );
                    break;
                }
            }
        }

        let control = crate::WorkbenchExecutionControl::untracked();
        control.publication_decision().terminate();
        assert!(!control.native_cancel().load(std::sync::atomic::Ordering::Acquire));
        let failure = execute(&child, "respond (42 :: Int)", Some(control.clone()))
            .await
            .expect_err("original private publication refuses after native acceptance");
        let KernelInvocationFailure::TerminalTransferFailed {
            actor: failed_actor,
            request: failed_request,
            source,
        } = failure else {
            panic!("expected typed consumed-terminal failure");
        };
        assert_eq!(failed_actor, target);
        assert_eq!(failed_request, request);
        let KernelInvocationFailure::Workbench(original) = *source else {
            panic!("original publication failure retains native receipts");
        };
        assert_eq!(original.actor, target);
        assert!(original.receipts.iter().any(|receipt| {
            receipt.terminal_transfer == Some(WorkbenchTerminalTransfer::ReplyAccepted)
        }), "native reply acceptance precedes publication refusal");
        assert!(matches!(
            forest.environment.requests.observe_response(requester.identity(), request),
            Ok(ResponseObservation::Unavailable(ResponseFailure::SettlementFailed(_)))
        ));
        let terminal = child.terminal().wait().await;
        assert_eq!(terminal.kind, ActorExitKind::Failed);
        assert!(child.terminal().cleanup().expect("retained original cleanup").is_confirmed());
        assert_eq!(control.terminal_reply(), Some(Err(KernelInvocationFailure::TerminalTransferFailed {
            actor: target,
            request,
            source: Box::new(KernelInvocationFailure::Workbench(original)),
        })));
        assert!(requester.terminal().get().is_none(), "requester remains live");
        forest.shutdown().await;
    })
    .await
    .expect("native acceptance/publication/retirement fixture completes");
}
