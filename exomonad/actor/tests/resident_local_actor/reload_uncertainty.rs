//! Unavailable configuration reload does not replace an issued generic source.
use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
pub(super) struct SourceObservation {
    failed: AtomicBool,
    freezes: AtomicUsize,
}
impl exomonad_actor::SourceInspection for SourceObservation {}

impl SourceObservation {
    pub(super) fn observe_freeze(&self) -> Result<(), String> {
        self.freezes.fetch_add(1, Ordering::AcqRel);
        if self.failed.load(Ordering::Acquire) {
            Err("mutable observation failed".into())
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
#[ignore = "requires the dedicated rooted notebook contract execution group"]
async fn unavailable_generation_does_not_recapture_source_or_discard_bindings() {
    let source = Arc::new(SourceObservation::default());
    let fixture = ConcurrentResident::new_with_source(193, &[], Some(source.clone())).await;
    fixture
        .read("source-kept", "kept <- pure (42 :: Int)")
        .await;
    let issued = fixture.policy.snapshot_for_request().unwrap();
    let captured = source.freezes.load(Ordering::Acquire);
    source.failed.store(true, Ordering::Release);
    let refused = issued
        .dispatch_json_boxed(ToolInvocation {
            context: Some(ToolInvocationContext::external(
                "source-observation".into(),
                "refused-reload".into(),
                "refused-reload".into(),
                Some("refused-reload".into()),
                None,
            )),
            name: "reload".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .unwrap();
    assert_eq!(refused["status"], "committed");
    assert_eq!(
        refused["items"][0]["value"]["Left"]["tag"],
        "ReloadUnavailable"
    );
    fixture.check_value("source-retained", "kept == 42").await;
    assert_eq!(
        source.freezes.load(Ordering::Acquire),
        captured,
        "dispatch uses retained source rather than a new mutable observation"
    );
    fixture.shutdown(&[]).await;
}
