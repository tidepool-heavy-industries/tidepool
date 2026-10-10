//! Visible source uncertainty cannot become success when source refresh fails.

use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Condvar, Mutex, OnceLock,
};

#[derive(Default)]
struct UnconfirmedSource {
    refresh_failed: AtomicBool,
    reloads: AtomicUsize,
    freezes: AtomicUsize,
    source_gate: Mutex<()>,
    recovery_entered: tokio::sync::Notify,
    recovery_permit: Mutex<bool>,
    recovery_released: Condvar,
    publication: OnceLock<Arc<tidepool_runtime::session::PublicationDecision>>,
}

impl exomonad_actor::ActorSourceLayers for UnconfirmedSource {
    fn stage_spec_reload(
        self: Arc<Self>,
        _: tidepool_repr::PrincipalId,
        _: &[String],
    ) -> Result<Box<dyn exomonad_actor::StagedActorSourceReload>, exomonad_actor::SourceLayerReload>
    {
        Err(exomonad_actor::SourceLayerReload::Unavailable(
            "this source fixture installs no agent spec".into(),
        ))
    }

    fn layer_include_for(&self, _: &str) -> Result<Vec<std::path::PathBuf>, String> {
        Ok(Vec::new())
    }
    fn bind_for(&self, _: tidepool_repr::PrincipalId, _: &str) {}

    fn freeze_checkpoint_layer(
        &self,
        _: tidepool_repr::PrincipalId,
    ) -> Result<exomonad_actor::CheckpointSourceLayer, String> {
        self.freezes.fetch_add(1, Ordering::AcqRel);
        let _source = self.source_gate.lock().unwrap();
        if self.refresh_failed.load(Ordering::Acquire) {
            Err("published source cannot be frozen".into())
        } else {
            Ok(exomonad_actor::CheckpointSourceLayer::default())
        }
    }

    fn reload_helpers_with_publication(
        &self,
        _: tidepool_repr::PrincipalId,
        _: &[String],
        publication: &Arc<tidepool_runtime::session::PublicationDecision>,
    ) -> exomonad_actor::SourceLayerReload {
        let _source = self.source_gate.lock().unwrap();
        let ordinal = self.reloads.fetch_add(1, Ordering::AcqRel);
        if ordinal != 0 {
            self.recovery_entered.notify_one();
            let mut permit = self.recovery_permit.lock().unwrap();
            while !*permit {
                permit = self.recovery_released.wait(permit).unwrap();
            }
            drop(permit);
            let Some(claim) = publication.claim_commit() else {
                return exomonad_actor::SourceLayerReload::Cancelled;
            };
            self.refresh_failed.store(false, Ordering::Release);
            claim.published();
            return exomonad_actor::SourceLayerReload::Published {
                previous: "visible-revision".into(),
                revision: "recovered-revision".into(),
                changed: Vec::new(),
            };
        }
        let claim = publication.claim_commit().expect("source commit admitted");
        self.publication
            .set(publication.clone())
            .expect("original uncertain reload");
        // Model the source owner's visible rename followed by failed durable
        // confirmation. Its native claim must remain unconfirmed.
        self.refresh_failed.store(true, Ordering::Release);
        drop(claim);
        exomonad_actor::SourceLayerReload::PublicationUnconfirmed {
            revision: "visible-revision".into(),
            diagnostics: "active-record durability failed after rename".into(),
        }
    }
}

impl UnconfirmedSource {
    fn release_recovery(&self) {
        *self.recovery_permit.lock().unwrap() = true;
        self.recovery_released.notify_all();
    }
}

struct ReleaseRecovery(Arc<UnconfirmedSource>);
impl Drop for ReleaseRecovery {
    fn drop(&mut self) {
        self.0.release_recovery();
    }
}

#[tokio::test]
async fn visible_reload_uncertainty_and_failed_freeze_preserve_failure() {
    let layers = Arc::new(UnconfirmedSource::default());
    let _release_on_failure = ReleaseRecovery(layers.clone());
    let fixture = ConcurrentResident::new_with_source(193, &[], Some(layers.clone())).await;
    assert_eq!(
        layers.freezes.load(Ordering::Acquire),
        1,
        "workbench boot issues its initial retained source lease"
    );
    fixture
        .check_value("initial-workbench-source", "(42 :: Int) == 42")
        .await;
    let failed_value = ConcurrentResident::settle(fixture.spawn_cell(
        "false-value-control",
        "if (42 :: Int) == 43 then pure () else error \"fixture value changed\"".into(),
    ))
    .await
    .expect_err("an incorrect value is evaluated and fails");
    let exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Workbench(failure),
    ) = failed_value
    else {
        panic!("expected a workbench evaluation failure: {failed_value:?}");
    };
    assert!(
        failure.detail.contains("fixture value changed"),
        "{failure:?}"
    );
    assert_eq!(
        layers.freezes.load(Ordering::Acquire),
        1,
        "first authored cell uses its startup lease"
    );
    let context = ToolInvocationContext::external(
        "reload-uncertainty".into(),
        "reload-uncertain".into(),
        "reload-uncertain".into(),
        Some("reload-uncertain".into()),
        None,
    );
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        fixture.policy.dispatch_json_boxed(ToolInvocation {
            context: Some(context.clone()),
            name: "reload_helpers".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        }),
    )
    .await
    .expect("source uncertainty settles its original tool call")
    .expect_err("postvisibility uncertainty must not become a committed receipt");
    let detail = reply.to_string();
    assert!(
        detail.contains("visible-revision")
            && detail.contains("durability is unconfirmed")
            && detail.contains("frozen source unavailable"),
        "{detail}"
    );
    let decision = layers
        .publication
        .get()
        .expect("original source publication owner");
    assert!(matches!(
        decision.phase(),
        tidepool_runtime::session::PublicationPhase::CommitClaimed { .. }
    ));
    let reconciled = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.policy.cancel_workbench_boxed(context),
    )
    .await
    .expect("uncertainty inspection progresses")
    .expect("original control inspected");
    let exomonad_actor::WorkbenchCancellationOutcome::PublicationSettled {
        reply: Err(retained),
        ..
    } = reconciled
    else {
        panic!("completed cleanup retains the source publication failure: {reconciled:?}");
    };
    let exomonad_actor::ResidentToolError::Invocation(original) = reply else {
        panic!("source publication failure must retain its native invocation: {reply:?}");
    };
    assert_eq!(
        retained, original,
        "visible uncertainty must not authorize replay"
    );
    assert!(matches!(
        decision.phase(),
        tidepool_runtime::session::PublicationPhase::CommitClaimed { .. }
    ));
    let refused = ConcurrentResident::settle(
        fixture.spawn_cell("reload-after-uncertain", "(42 :: Int)".into()),
    )
    .await
    .expect_err("failed source refresh closes exact source admission");
    let exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Rejected {
            actor, receipts, ..
        },
    ) = refused
    else {
        panic!("unavailable installation must refuse authored admission: {refused:?}");
    };
    assert_eq!(actor, fixture.actor.identity());
    assert!(receipts.is_empty(), "no authored effects were admitted");
    assert!(matches!(
        fixture.policy.snapshot_for_request(),
        Err(exomonad_actor::ResidentToolError::Unavailable(_))
    ));
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.policy.dispatch_json_boxed(ToolInvocation {
            context: Some(ToolInvocationContext::external(
                "reload-uncertainty".into(),
                "uncertain-status".into(),
                "uncertain-status".into(),
                Some("uncertain-status".into()),
                None,
            )),
            name: "status".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"view": "detailed"})),
        }),
    )
    .await
    .expect("status does not depend on unavailable source refresh")
    .expect("status admission stays available");
    assert_eq!(status["status"], "committed", "{status:?}");
    let projection = status["items"][0]["output"]
        .as_str()
        .expect("status projection");
    assert!(projection.contains("source unavailable"), "{status:?}");
    assert!(!projection.contains("none installed"), "{status:?}");
    assert!(
        layers.refresh_failed.load(Ordering::Acquire),
        "status does not repair or re-freeze source"
    );
    let recovery = tokio::spawn(fixture.policy.dispatch_json_boxed(ToolInvocation {
        context: Some(ToolInvocationContext::external(
            "reload-uncertainty".into(),
            "reload-recovery".into(),
            "reload-recovery".into(),
            Some("reload-recovery".into()),
            None,
        )),
        name: "reload_helpers".into(),
        arguments: ToolArguments::Structured(serde_json::json!({})),
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        layers.recovery_entered.notified(),
    )
    .await
    .expect("recovery preparation holds the real source owner gate");
    let refused_during_recovery = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ConcurrentResident::settle(
            fixture.spawn_cell("reload-during-recovery", "(42 :: Int)".into()),
        ),
    )
    .await
    .expect("authored admission does not synchronously wait on the source gate")
    .expect_err("source-invalid authored input is refused until exact recovery publishes");
    let exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Rejected {
            actor, receipts, ..
        },
    ) = refused_during_recovery
    else {
        panic!("recovery must retain authored admission refusal: {refused_during_recovery:?}");
    };
    assert_eq!(actor, fixture.actor.identity());
    assert!(receipts.is_empty(), "recovery admitted no authored effects");
    let status_during_recovery = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.policy.dispatch_json_boxed(ToolInvocation {
            context: Some(ToolInvocationContext::external(
                "reload-uncertainty".into(),
                "recovery-status".into(),
                "recovery-status".into(),
                Some("recovery-status".into()),
                None,
            )),
            name: "status".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"view": "summary"})),
        }),
    )
    .await
    .expect("status progresses while source recovery is parked")
    .expect("status remains available");
    assert_eq!(status_during_recovery["status"], "committed");
    assert!(
        !recovery.is_finished(),
        "status and refusal precede source gate release"
    );
    layers.release_recovery();
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(5), recovery)
        .await
        .expect("recovery settles after its own source preparation")
        .expect("recovery task")
        .expect("source owner can repair the failed refresh");
    assert_eq!(recovered["status"], "committed", "{recovered:?}");
    assert!(recovered["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("recovered-revision"));
    assert_eq!(layers.reloads.load(Ordering::Acquire), 2);
    fixture
        .check_value("reload-recovered-read", "(42 :: Int) == 42")
        .await;
    assert_eq!(
        layers.freezes.load(Ordering::Acquire),
        3,
        "startup and both reload outcomes freeze once; authored admission never re-freezes"
    );
    assert!(
        matches!(
            decision.phase(),
            tidepool_runtime::session::PublicationPhase::CommitClaimed { .. }
        ),
        "later recovery cannot reconcile the original uncertain publication"
    );
    fixture.shutdown(&[]).await;
}
