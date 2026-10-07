//! Request-local native reply helpers and private-publication settlement.

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
    run_native_reply_case(NativeReplyCase::PublicationRefused).await;
}

#[tokio::test]
async fn request_local_reply_helper_settles_exact_request_and_keeps_actor_live() {
    run_native_reply_case(NativeReplyCase::Committed).await;
}

#[derive(Clone, Copy)]
enum NativeReplyCase {
    Committed,
    PublicationRefused,
}

async fn run_native_reply_case(case: NativeReplyCase) {
    eval_harness::require_extract();
    // Fixed compiler-backed setup precedes the reply settlement deadline.
    // Readiness and retirement still use exact request/actor events.
    let fixture_started = std::time::Instant::now();
    eprintln!("native terminal fixture: setup started");
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
    let include = crate::resident_workbench::request_tests::fixture_include_roots(&effects);
    let mut preamble = tidepool_mcp::build_notebook_preamble(&declarations, false);
    for import in [
        "qualified Tidepool.Actor as Mailbox",
        "Tidepool.Agent.Reply (Replies, retainRequest)",
        "Tidepool.Agent.Watch (Watches)",
        "Tidepool.Agent.Ref.Internal (AgentProtocol(..))",
        "Tidepool.Effects.Core (WorkerLifetime(..))",
        "qualified Tidepool.Agent.Ref.Internal as AgentRef",
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
        .new_workbench(
            "terminal-transfer-requester".into(),
            crate::ActorCapabilities::default().with_effect_keys(vec![
                crate::ActorEffectKey::Actor,
                crate::ActorEffectKey::Replies,
            ]),
        )
        .await
        .expect("requesting workbench");
    let setup = execute(&requester, include_str!("terminal_transfer_setup.hs"), None)
        .await
        .expect("native receiving child admitted");
    assert_eq!(setup.status, WorkbenchRunStatus::Committed, "{setup:?}");
    eprintln!(
        "native terminal fixture: setup committed elapsed_ms={}",
        fixture_started.elapsed().as_millis()
    );

    // Setup has fully committed and its cell cleanup has run before the
    // child receiver is used. A parent-owned startup hole cannot pass this.
    let submission = execute(
        &requester,
        include_str!("terminal_transfer_request.hs"),
        None,
    )
    .await
    .expect("typed request submitted to retained child receiver");
    assert_eq!(
        submission.status,
        WorkbenchRunStatus::Committed,
        "{submission:?}"
    );
    eprintln!(
        "native terminal fixture: request submitted elapsed_ms={}",
        fixture_started.elapsed().as_millis()
    );
    let status = forest.environment.requests.status_for(requester.identity());
    let unavailable_terminals = status
        .unavailable_responses
        .iter()
        .map(|(request, _, _)| {
            let target = forest.environment.requests.target_for(*request);
            let terminal = target
                .and_then(|target| forest.directory.resolve(target))
                .and_then(|actor| actor.terminal().get());
            (*request, target, terminal)
        })
        .collect::<Vec<_>>();
    assert_eq!(status.pending_responses.len(), 1,
            "one original typed request; submission={submission:?}; ready={:?}; unavailable={:?}; unavailable_terminals={unavailable_terminals:?}",
            status.ready_responses, status.unavailable_responses);
    let request = status.pending_responses[0].0;
    let target = forest
        .environment
        .requests
        .target_for(request)
        .expect("exact request target");
    let child = forest
        .directory
        .resolve(target)
        .expect("retained native child");
    tokio::time::timeout(std::time::Duration::from_secs(240), async {
        loop {
            let event = deployments
                .recv()
                .await
                .expect("native readiness deployment");
            match event {
                LocalResidentDeployment::PolicyInstalled(installation)
                    if installation.actor.identity() == target =>
                {
                    assert_eq!(
                        installation
                            .runtime_observation
                            .snapshot()
                            .request_activation
                            .expect("initial typed activation")
                            .request,
                        request,
                    );
                    break;
                }
                LocalResidentDeployment::ChildExited { notice }
                    if notice.child.identity() == target =>
                {
                    panic!("native target exited before request activation: {notice:?}");
                }
                LocalResidentDeployment::Retired { actor, terminal } if actor == target => {
                    panic!("native target retired before request activation: {terminal:?}");
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "native request activation did not complete: {error}; graph={:?}; requests={:?}",
            forest.inspect_host_graph(),
            {
                let status = forest.environment.requests.status_for(requester.identity());
                (
                    status.pending_responses,
                    status.ready_responses,
                    status.unavailable_responses,
                )
            },
        );
    });
    eprintln!("native terminal fixture: request activated actor={target:?} request={request:?} elapsed_ms={}", fixture_started.elapsed().as_millis());

    // Bound the actual reply/publication/retirement protocol independently
    // of compiling the fixed setup and activating the native receiver.
    tokio::time::timeout(std::time::Duration::from_secs(240), async {
        let control = crate::WorkbenchExecutionControl::untracked();
        if let NativeReplyCase::Committed = case {
            let reply = execute(&child, "respond (42 :: Int)", Some(control.clone()))
                .await
                .expect("request-local helper settles its actual typed request");
            assert_eq!(reply.status, WorkbenchRunStatus::Replied, "{reply:?}");
            assert_eq!(
                reply
                    .items
                    .iter()
                    .filter(|receipt| {
                        receipt.terminal_transfer
                            == Some(WorkbenchTerminalTransfer::ReplyAccepted)
                    })
                    .count(),
                1,
                "exactly one native terminal transfer"
            );
            assert_eq!(
                forest
                    .environment
                    .requests
                    .observe_response(requester.identity(), request),
                Ok(ResponseObservation::Ready)
            );
            assert!(matches!(control.terminal_reply(), Some(Ok(_))));
            assert!(child.terminal().get().is_none(), "reply does not retire the actor");
            assert!(requester.terminal().get().is_none(), "requester remains live");
            let continued = execute(&child, "pure True", None)
                .await
                .expect("actor accepts work after request settlement");
            assert_eq!(continued.status, WorkbenchRunStatus::Committed, "{continued:?}");
            let outcomes = forest.shutdown().await;
            assert!(!outcomes.is_empty(), "the owning forest retires its roots");
            assert!(
                outcomes.iter().all(crate::ForestRootShutdown::is_confirmed),
                "hosted actor cleanup is independent of successful reply: {outcomes:?}"
            );
            return;
        }
        control.publication_decision().terminate();
        assert!(!control
            .native_cancel()
            .load(std::sync::atomic::Ordering::Acquire));
        let failure = execute(&child, "respond (42 :: Int)", Some(control.clone()))
            .await
            .expect_err("original private publication refuses after native acceptance");
        eprintln!(
            "native terminal fixture: reply publication refused elapsed_ms={}",
            fixture_started.elapsed().as_millis()
        );
        let KernelInvocationFailure::TerminalTransferFailed {
            actor: failed_actor,
            request: failed_request,
            source,
        } = failure
        else {
            panic!("expected typed consumed-terminal failure");
        };
        assert_eq!(failed_actor, target);
        assert_eq!(failed_request, request);
        let KernelInvocationFailure::Workbench(original) = *source else {
            panic!("original publication failure retains native receipts");
        };
        assert_eq!(original.actor, target);
        assert!(
            original.receipts.iter().any(|receipt| {
                receipt.terminal_transfer == Some(WorkbenchTerminalTransfer::ReplyAccepted)
            }),
            "native reply acceptance precedes publication refusal"
        );
        assert!(matches!(
            forest
                .environment
                .requests
                .observe_response(requester.identity(), request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::SettlementFailed(_)
            ))
        ));
        let terminal = child.terminal().wait().await;
        assert_eq!(terminal.kind, ActorExitKind::Failed);
        assert!(child
            .terminal()
            .cleanup()
            .expect("retained original cleanup")
            .is_confirmed());
        eprintln!(
            "native terminal fixture: actor retired with confirmed cleanup elapsed_ms={}",
            fixture_started.elapsed().as_millis()
        );
        assert_eq!(
            control.terminal_reply(),
            Some(Err(KernelInvocationFailure::TerminalTransferFailed {
                actor: target,
                request,
                source: Box::new(KernelInvocationFailure::Workbench(original)),
            }))
        );
        assert!(
            requester.terminal().get().is_none(),
            "requester remains live"
        );
        forest.shutdown().await;
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "native reply/publication/retirement did not complete after activation: {error}; graph={:?}; requests={:?}",
            forest.inspect_host_graph(),
            {
                let status = forest.environment.requests.status_for(requester.identity());
                (status.pending_responses, status.ready_responses, status.unavailable_responses)
            },
        );
    });
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
