//! Pins the timing module's emitted event shape: tracing target, message,
//! and field set/order — the wire contract that
//! `tidepool-runtime/src/session/{mod.rs,turn.rs}` used to hand-mirror
//! across the crate boundary because a back-dependency on `tidepool-harness`
//! would cycle. This test must stay green, UNCHANGED, whether `timing.rs`
//! lives in `tidepool-harness` or is re-exported from `tidepool-runtime` —
//! it exercises only `tidepool_harness::timing`'s public surface.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

use tidepool_harness::timing::{
    self, ExtractTiming, NO_NODE, NO_ROUND, STAGE_TEMPLATE,
};

#[derive(Default, Debug, Clone)]
struct CapturedEvent {
    target: String,
    field_names: Vec<String>,
    node: Option<String>,
    round: Option<String>,
    stage: Option<String>,
    ms: Option<u64>,
    bytes: Option<u64>,
}

#[derive(Default)]
struct Visitor(CapturedEvent);

impl Visit for Visitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "ms" => self.0.ms = Some(value),
            "bytes" => self.0.bytes = Some(value),
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if value >= 0 {
            self.record_u64(field, value as u64);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "node" => self.0.node = Some(value.to_string()),
            "round" => self.0.round = Some(value.to_string()),
            "stage" => self.0.stage = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

#[derive(Clone, Default)]
struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        // The declared field set/order, minus the implicit `message` field
        // the macro's trailing string-literal arm adds — the contract table
        // in `timing.rs`'s module doc names exactly these five, in order.
        let field_names: Vec<String> = event
            .metadata()
            .fields()
            .iter()
            .map(|f| f.name().to_string())
            .filter(|n| n != "message")
            .collect();

        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        let mut captured = visitor.0;
        captured.target = event.metadata().target().to_string();
        captured.field_names = field_names;
        self.events.lock().unwrap().push(captured);
    }
}

fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<CapturedEvent>) {
    let layer = CaptureLayer::default();
    let events = layer.events.clone();
    let subscriber = tracing_subscriber::registry().with(layer);
    let result = tracing::subscriber::with_default(subscriber, f);
    let events = events.lock().unwrap().clone();
    (result, events)
}

#[test]
fn record_stage_emits_the_pinned_target_message_and_field_shape() {
    let (_, events) = capture(|| {
        timing::record_stage(NO_NODE, NO_ROUND, STAGE_TEMPLATE, Duration::from_millis(42), 7);
    });

    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.target, "tidepool_harness::timing");
    assert_eq!(
        e.field_names,
        vec!["node", "round", "stage", "ms", "bytes"],
        "field set/order is the collector-facing contract — must not shift"
    );
    assert_eq!(e.node.as_deref(), Some("bootstrap"));
    assert_eq!(e.round.as_deref(), Some("-"));
    assert_eq!(e.stage.as_deref(), Some(STAGE_TEMPLATE));
    assert_eq!(e.ms, Some(42));
    assert_eq!(e.bytes, Some(7));
}

#[test]
fn record_stage_renders_real_node_and_round_as_decimal() {
    let (_, events) = capture(|| {
        timing::record_stage(3, 5, STAGE_TEMPLATE, Duration::from_millis(1), 0);
    });

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].node.as_deref(), Some("3"));
    assert_eq!(events[0].round.as_deref(), Some("5"));
}

#[test]
fn record_extract_phases_forwards_each_phase_under_the_extract_prefix() {
    let stderr = "\
tidepool-timing phase=ghc_setup ms=142\n\
tidepool-timing phase=ghc_load ms=4533\n\
tidepool-timing phase=total ms=4700\n";
    let parsed = ExtractTiming::parse(stderr);

    let (_, events) = capture(|| {
        timing::record_extract_phases(NO_NODE, NO_ROUND, &parsed);
    });

    let stages: Vec<Option<String>> = events.iter().map(|e| e.stage.clone()).collect();
    assert_eq!(
        stages,
        vec![
            Some("extract.ghc_setup".to_string()),
            Some("extract.ghc_load".to_string()),
            Some("extract.total".to_string()),
        ]
    );
    for e in &events {
        assert_eq!(e.target, "tidepool_harness::timing");
        assert_eq!(e.field_names, vec!["node", "round", "stage", "ms", "bytes"]);
        assert_eq!(e.node.as_deref(), Some("bootstrap"));
        assert_eq!(e.round.as_deref(), Some("-"));
        assert_eq!(e.bytes, Some(0));
    }
    assert_eq!(events[0].ms, Some(142));
    assert_eq!(events[1].ms, Some(4533));
    assert_eq!(events[2].ms, Some(4700));
}
