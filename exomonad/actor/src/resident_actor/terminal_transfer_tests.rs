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

#[tokio::test]
async fn resumed_reply_requires_publication_and_cannot_succeed_after_retirement() {
    use crate::request::{ResponseFailure, ResponseObservation};

    for retired in [false, true] {
        let fixture = invocation_work::tests::Fixture::start().await;
        let target = fixture.actor.identity();
        let owner = ActorRef::first(crate::ActorId(target.id.0 + 100));
        let requests = &fixture.environment.requests;
        let request = requests.reserve(owner, target);
        crate::request::test_support::admit_destination(requests, owner, request);
        requests.mark_queued(owner, target, request).unwrap();
        requests.present(target, request).unwrap();
        let claim = requests.begin_reply(target, request).unwrap();
        let publication = if retired {
            AcceptedReplyPublication::Published(crate::request::test_support::snapshot(&claim))
        } else {
            AcceptedReplyPublication::Awaiting
        };
        let mut behavior = ResidentKernelBehavior::with_boot(
            ActorDescriptor::new(
                "settlement-resume",
                crate::ActorPlacement {
                    session: tidepool_repr::SessionId(0xD501),
                    resource_scope: RealmId::ROOT,
                    lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
                },
            ),
            fixture.environment.clone(),
            ResidentBoot::Workbench,
            Vec::new(),
        );
        behavior.boot = None;
        behavior.accepted_settlement = Some(AcceptedRequestSettlement {
            transfer: None,
            kind: AcceptedRequestKind::Reply(AcceptedReply {
                claim,
                preview: None,
                publication,
            }),
        });
        behavior.pending_program = Some(PendingActorProgram {
            transfer: None,
            resumption: ActorResumption::Program {
                outcome: ResidentOutcome::BindingsCommitted { output: Vec::new() },
                cleanup: None,
            },
        });
        if retired {
            requests.actor_stopped(
                target,
                &ActorTerminal::new(
                    ActorExitKind::Failed,
                    "owner lost after response publication",
                ),
            );
        }
        let resumed = behavior.resume(&fixture.kernel).await;
        let observation = requests.observe_response(owner, request).unwrap();
        if retired {
            assert!(resumed.is_ok(), "a retired claim must settle inertly");
            assert_eq!(
                observation,
                ResponseObservation::Unavailable(ResponseFailure::TargetFailed(
                    "owner lost after response publication".into()
                ))
            );
        } else {
            assert!(resumed
                .unwrap_err()
                .detail
                .contains("without publishing its response"));
            assert!(
                matches!(observation, ResponseObservation::Unavailable(ResponseFailure::SettlementFailed(ref detail)) if detail.contains("without publishing its response"))
            );
        }
        assert!(behavior.accepted_settlement.is_none());
        assert!(
            requests.begin_reply(target, request).is_err(),
            "neither history may later issue success authority"
        );
        fixture.finish().await;
    }
}

#[tokio::test]
async fn response_publication_refuses_missing_wrong_and_duplicate_settlement() {
    let fixture = invocation_work::tests::Fixture::start().await;
    let (mut resident, mut context, _source, mut parked, _root) =
        crate::resident_workbench::request_tests::activation_session_fixture(|_| {});
    context.actor = fixture.actor.identity();
    let owner = ActorRef::first(crate::ActorId(context.actor.id.0 + 100));
    let requests = &fixture.environment.requests;
    let request = requests.reserve(owner, context.actor);
    crate::request::test_support::admit_destination(requests, owner, request);
    requests.mark_queued(owner, context.actor, request).unwrap();
    requests.present(context.actor, request).unwrap();
    let claim = requests.begin_reply(context.actor, request).unwrap();
    let snapshot = crate::request::test_support::snapshot(&claim);
    let mut admitted = Some(claim);
    let mut behavior = ResidentKernelBehavior::with_boot(
        ActorDescriptor::new("publication-refusal", context.placement),
        fixture.environment.clone(),
        ResidentBoot::Workbench,
        Vec::new(),
    );
    let (continuation, input) = parked.remove(0);
    let ancestry = crate::CallAncestry::begin(context.actor);
    for history in 0..3 {
        if history == 1 {
            behavior.accepted_settlement = Some(AcceptedRequestSettlement {
                transfer: None,
                kind: AcceptedRequestKind::Reply(AcceptedReply {
                    claim: admitted.take().unwrap(),
                    preview: None,
                    publication: AcceptedReplyPublication::Awaiting,
                }),
            });
        }
        if history == 2 {
            let AcceptedRequestKind::Reply(reply) =
                &mut behavior.accepted_settlement.as_mut().unwrap().kind
            else {
                unreachable!();
            };
            reply.publication = AcceptedReplyPublication::Published(snapshot.clone());
        }
        let effect_owner = behavior.actor_effect_owner(context.actor);
        let error = behavior
            .prepare_actor_effect(
                &fixture.kernel,
                &context,
                effect_owner,
                &ancestry,
                ResidentActorBoundary::ResponsePublication {
                    continuation: continuation.clone(),
                    request: if history == 1 {
                        crate::RequestId(request.0 + 100)
                    } else {
                        request
                    },
                    value: crate::request::test_support::publication(request),
                },
            )
            .await
            .unwrap_err();
        let expected = if history == 2 {
            "published its response twice"
        } else {
            "does not belong to the accepted reply"
        };
        assert!(error.to_string().contains(expected), "{error}");
    }
    let AcceptedRequestKind::Reply(reply) = behavior.accepted_settlement.take().unwrap().kind
    else {
        unreachable!()
    };
    assert!(
        matches!(reply.publication, AcceptedReplyPublication::Published(ref retained) if Arc::ptr_eq(retained, &snapshot)),
        "refused publication must preserve the original snapshot"
    );
    requests.fail_reply_settlement(reply.claim, "publication refusal fixture complete");
    drop(input);
    let _ = resident.abort(
        continuation.cont_id(),
        "publication refusal fixture complete".into(),
    );
    for (hole, input) in parked {
        drop(input);
        let _ = resident.abort(hole.cont_id(), "unused publication fixture".into());
    }
    drop(behavior);
    fixture.finish().await;
}
