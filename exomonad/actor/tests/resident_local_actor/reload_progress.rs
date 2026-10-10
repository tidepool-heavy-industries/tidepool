//! Real parked resident cells remain independent of owned reload preparation.

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};

struct ReloadGate {
    entered: tokio::sync::mpsc::UnboundedSender<usize>,
    calls: AtomicUsize,
    decisions: Mutex<Vec<Arc<tidepool_runtime::session::PublicationDecision>>>,
    reject_ordinal: Option<usize>,
    permits: Mutex<usize>,
    released: Condvar,
}

impl ReloadGate {
    fn release(&self) {
        *self.permits.lock().unwrap() += 1;
        self.released.notify_one();
    }
}

struct ReleaseReloads(Arc<ReloadGate>);
impl Drop for ReleaseReloads {
    fn drop(&mut self) {
        *self.0.permits.lock().unwrap() = usize::MAX;
        self.0.released.notify_all();
    }
}

impl exomonad_actor::ActorSourceLayers for ReloadGate {
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

    fn reload_helpers_with_publication(
        &self,
        _: tidepool_repr::PrincipalId,
        _: &[String],
        publication: &Arc<tidepool_runtime::session::PublicationDecision>,
    ) -> exomonad_actor::SourceLayerReload {
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst);
        self.decisions.lock().unwrap().push(publication.clone());
        self.entered
            .send(ordinal)
            .expect("reload progress observer");
        let mut permits = self.permits.lock().unwrap();
        while *permits == 0 {
            permits = self.released.wait(permits).unwrap();
        }
        *permits -= 1;
        drop(permits);
        if self.reject_ordinal == Some(ordinal) {
            return exomonad_actor::SourceLayerReload::Rejected {
                active: "last-valid".into(),
                rejected: "invalid-draft".into(),
                diagnostics: "draft refused".into(),
            };
        }
        let Some(claim) = publication.claim_commit() else {
            return exomonad_actor::SourceLayerReload::Cancelled;
        };
        claim.published();
        exomonad_actor::SourceLayerReload::Published {
            previous: format!("reload-{ordinal}"),
            revision: format!("reload-{}", ordinal + 1),
            changed: Vec::new(),
        }
    }
}

async fn hosted_tool(
    fixture: &ConcurrentResident,
    key: &str,
    name: &str,
    arguments: serde_json::Value,
) -> ResidentCellCall {
    let policy = fixture.policy.clone();
    let context = reload_context(key);
    let name = name.to_owned();
    let mut invocation = policy.dispatch_json_boxed(ToolInvocation {
        context: Some(context),
        name,
        arguments: ToolArguments::Structured(arguments),
    });
    // The local endpoint submits its workbench message synchronously on first poll.
    // Submit this call before the caller sends the next control or cell request.
    let initial = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(invocation.as_mut(), cx))
    })
    .await;
    tokio::spawn(async move {
        match initial {
            std::task::Poll::Ready(reply) => reply,
            std::task::Poll::Pending => invocation.await,
        }
    })
}

fn reload_context(key: &str) -> ToolInvocationContext {
    ToolInvocationContext::external(
        "reload-progress".into(),
        key.into(),
        key.into(),
        Some(key.into()),
        None,
    )
}

#[tokio::test]
async fn two_parked_cells_reload_and_control_progress_independently() {
    let markers = ["reload-A", "reload-B"];
    let (entered, mut progress) = tokio::sync::mpsc::unbounded_channel();
    let layers = Arc::new(ReloadGate {
        entered,
        calls: AtomicUsize::new(0),
        decisions: Mutex::new(Vec::new()),
        reject_ordinal: None,
        permits: Mutex::new(0),
        released: Condvar::new(),
    });
    let _release_on_failure = ReleaseReloads(layers.clone());
    let mut fixture =
        ConcurrentResident::new_with_source(191, &markers, Some(layers.clone())).await;
    fixture.read("reload-seed", "x <- pure (0 :: Int)").await;
    let source = include_str!("shadow_cell.hs");
    let mut a = fixture.spawn_cell(
        markers[0],
        source
            .replace("OLD_BINDING", "oldA")
            .replace("MARKER", markers[0])
            .replace("RESULT_VALUE", "11"),
    );
    let a_execution = fixture.wait_started(markers[0], &mut a).await;
    let mut b = fixture.spawn_cell(
        markers[1],
        source
            .replace("OLD_BINDING", "oldB")
            .replace("MARKER", markers[1])
            .replace("RESULT_VALUE", "22"),
    );
    let b_execution = fixture.wait_started(markers[1], &mut b).await;
    let reload = hosted_tool(
        &fixture,
        "reload-first",
        "reload_helpers",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), progress.recv())
            .await
            .expect("reload starts while A/B park"),
        Some(0)
    );
    let next_reload = hosted_tool(
        &fixture,
        "reload-second",
        "reload_helpers",
        serde_json::json!({}),
    )
    .await;
    fixture
        .read(
            "reload-C",
            "c <- pure (42 :: Int)\nif c == 42 then pure () else error \"C binding changed\"",
        )
        .await;
    let status = ConcurrentResident::settle(
        hosted_tool(
            &fixture,
            "reload-status",
            "status",
            serde_json::json!({"view": "summary"}),
        )
        .await,
    )
    .await
    .expect("status progresses during reload");
    assert_eq!(status["status"], "committed", "{status:?}");
    assert!(
        !a.is_finished() && !b.is_finished() && !reload.is_finished() && !next_reload.is_finished()
    );
    assert_eq!(
        layers.calls.load(Ordering::SeqCst),
        1,
        "administrative publication remains serialized"
    );
    layers.release();
    assert_eq!(
        ConcurrentResident::settle(reload)
            .await
            .expect("first reload settles")["status"],
        "committed"
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), progress.recv())
            .await
            .expect("queued reload starts with A/B still parked"),
        Some(1)
    );
    fixture.check_value("reload-D", "c == 42").await;
    assert!(!next_reload.is_finished() && !a.is_finished() && !b.is_finished());
    layers.release();
    assert_eq!(
        ConcurrentResident::settle(next_reload)
            .await
            .expect("second reload settles")["status"],
        "committed"
    );
    assert!(!a.is_finished() && !b.is_finished());
    fixture.release(markers[0]);
    assert_eq!(
        committed_execution(&ConcurrentResident::settle(a).await.expect("A resumes")),
        a_execution
    );
    fixture.release(markers[1]);
    assert_eq!(
        committed_execution(&ConcurrentResident::settle(b).await.expect("B resumes")),
        b_execution
    );
    fixture
        .check_value(
            "reload-joined",
            "x == 22 && c == 42 && oldA == 0 && oldB == 0",
        )
        .await;
    fixture.shutdown(&markers).await;
}

#[tokio::test]
async fn cancelled_and_rejected_reload_release_the_administrative_owner() {
    use tidepool_runtime::session::PublicationPhase;
    let (entered, mut progress) = tokio::sync::mpsc::unbounded_channel();
    let layers = Arc::new(ReloadGate {
        entered,
        calls: AtomicUsize::new(0),
        decisions: Mutex::new(Vec::new()),
        reject_ordinal: Some(1),
        permits: Mutex::new(0),
        released: Condvar::new(),
    });
    let _release_on_failure = ReleaseReloads(layers.clone());
    let fixture = ConcurrentResident::new_with_source(192, &[], Some(layers.clone())).await;
    fixture
        .read("reload-cancel-seed", "kept <- pure (42 :: Int)")
        .await;
    let cancelled = hosted_tool(
        &fixture,
        "reload-cancel",
        "reload_helpers",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), progress.recv())
            .await
            .expect("reload starts"),
        Some(0)
    );
    let context = reload_context("reload-cancel");
    assert!(
        !cancelled.is_finished(),
        "the original source preparation still owns its unpublished result"
    );
    let cancellation = tokio::spawn(fixture.policy.cancel_workbench_boxed(context.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while layers.decisions.lock().unwrap()[0].phase() != PublicationPhase::CancellationRequested
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancellation reaches original reload control");
    assert!(
        !cancelled.is_finished() && !cancellation.is_finished(),
        "cancellation retains actual source settlement"
    );
    let rejected = hosted_tool(
        &fixture,
        "reload-refused",
        "reload_helpers",
        serde_json::json!({}),
    )
    .await;
    fixture.read("reload-cancel-control", "kept").await;
    layers.release();
    let cancelled_reply = ConcurrentResident::settle(cancelled)
        .await
        .expect("cancelled reload receipt");
    let exomonad_actor::WorkbenchCancellationOutcome::Cancelled {
        execution: cancelled_execution,
        reply: Ok(retained),
    } = tokio::time::timeout(std::time::Duration::from_secs(5), cancellation)
        .await
        .expect("actual cancellation settles")
        .expect("cancellation task")
        .expect("cancellation result")
    else {
        panic!("reload did not retain proven cancellation");
    };
    assert_eq!(serde_json::to_value(retained).unwrap(), cancelled_reply);
    let exomonad_actor::WorkbenchCancellationOutcome::Cancelled {
        execution: reconciled_execution,
        reply: Ok(reconciled_reply),
    } = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.policy.cancel_workbench_boxed(context),
    )
    .await
    .expect("the original cancellation owner remains inspectable")
    .expect("retained cancellation reconciliation")
    else {
        panic!("original reload cancellation lost its retained owner");
    };
    assert_eq!(reconciled_execution, cancelled_execution);
    assert_eq!(
        serde_json::to_value(reconciled_reply).unwrap(),
        cancelled_reply
    );
    assert!(cancelled_reply["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("cancelled before source publication"));
    assert_eq!(
        layers.decisions.lock().unwrap()[0].phase(),
        PublicationPhase::Terminated
    );
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), progress.recv())
            .await
            .expect("queued reload starts after cancellation"),
        Some(1)
    );
    layers.release();
    let refusal = ConcurrentResident::settle(rejected)
        .await
        .expect("refusal receipt");
    assert_eq!(refusal["status"], "committed");
    assert!(refusal["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("rejected"));
    assert!(refusal["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("last-valid remains active"));
    assert!(refusal["items"][0]["output"]
        .as_str()
        .unwrap()
        .contains("draft refused"));
    assert_eq!(
        layers.decisions.lock().unwrap()[1].phase(),
        PublicationPhase::Terminated
    );
    let valid = hosted_tool(
        &fixture,
        "reload-after-refusal",
        "reload_helpers",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), progress.recv())
            .await
            .expect("new reload starts after refusal"),
        Some(2)
    );
    layers.release();
    assert_eq!(
        ConcurrentResident::settle(valid)
            .await
            .expect("later reload commits")["status"],
        "committed"
    );
    assert_eq!(layers.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        layers.decisions.lock().unwrap()[2].phase(),
        PublicationPhase::Published
    );
    fixture
        .check_value("reload-cancel-kept", "kept == 42")
        .await;
    fixture.shutdown(&[]).await;
}
