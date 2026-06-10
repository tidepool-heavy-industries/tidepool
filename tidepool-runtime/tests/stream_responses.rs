//! respond_stream: handlers park an iterator; elements convert per-pull at
//! chunk-materialization time. No Value spine ever exists on the response
//! path — `take k` of a huge listing converts ~one chunk, and an infinite
//! iterator is a legitimate infinite Haskell list.
//!
//! Lazy results are DEFAULT-ON (no env var needed); the kill-switch drain
//! is covered separately in lazy_eager_fallback.rs (own process).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tidepool_bridge::{BridgeError, ToCore};
use tidepool_bridge_derive::FromCore;
use tidepool_effect::{
    EffectContext, EffectError, EffectHandler, Response, ValueSource, ValueStream,
};
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::compile_and_run;

#[derive(FromCore)]
enum ListingReq {
    #[core(name = "GetList")]
    GetList,
}

/// An indexed source whose element CONVERSIONS are observable — the
/// stage-3a probe. (ToCore is sealed, so counting happens at the
/// ValueSource layer, through the `from_source` escape hatch.)
struct CountingSource {
    items: Vec<String>,
    pos: usize,
    conversions: Arc<AtomicUsize>,
}

impl ValueSource for CountingSource {
    fn next_value(&mut self, table: &DataConTable) -> Option<Result<Value, BridgeError>> {
        let item = self.items.get(self.pos)?;
        self.pos += 1;
        self.conversions.fetch_add(1, Ordering::Relaxed);
        Some(item.to_value(table))
    }

    fn len(&self) -> Option<usize> {
        Some(self.items.len())
    }

    fn get(&self, idx: usize, table: &DataConTable) -> Option<Result<Value, BridgeError>> {
        self.items.get(idx).map(|x| {
            self.conversions.fetch_add(1, Ordering::Relaxed);
            x.to_value(table)
        })
    }
}

/// What the handler streams, per test scenario.
enum Source {
    /// `item-0..item-{n-1}`, counting every pull through the shared counter.
    Counted { n: usize, pulls: Arc<AtomicUsize> },
    /// `item-0, item-1, ...` forever.
    Infinite,
    /// Panics when element `at` is pulled.
    PanicsAt { at: usize },
    /// respond_list (indexed; element-thunk heads), counting CONVERSIONS.
    IndexedList {
        n: usize,
        conversions: Arc<AtomicUsize>,
    },
}

struct StreamListing {
    source: Source,
}

impl EffectHandler for StreamListing {
    type Request = ListingReq;
    fn handle(&mut self, req: ListingReq, cx: &EffectContext) -> Result<Response, EffectError> {
        match req {
            ListingReq::GetList => match &self.source {
                Source::Counted { n, pulls } => {
                    let pulls = pulls.clone();
                    let n = *n;
                    cx.respond_stream((0..n).map(move |i| {
                        pulls.fetch_add(1, Ordering::Relaxed);
                        format!("item-{i}")
                    }))
                }
                Source::Infinite => cx.respond_stream((0..).map(|i| format!("item-{i}"))),
                Source::PanicsAt { at } => {
                    let at = *at;
                    cx.respond_stream((0..usize::MAX).map(move |i| {
                        assert!(i != at, "producer exploded at element {at}");
                        format!("item-{i}")
                    }))
                }
                Source::IndexedList { n, conversions } => {
                    let items: Vec<String> = (0..*n).map(|i| format!("item-{i}")).collect();
                    let cons =
                        tidepool_bridge::get_resilient(cx.table(), ":", 2).expect("cons in table");
                    let nil =
                        tidepool_bridge::get_resilient(cx.table(), "[]", 0).expect("nil in table");
                    Ok(Response::Stream(ValueStream::from_source(
                        Box::new(CountingSource {
                            items,
                            pos: 0,
                            conversions: conversions.clone(),
                        }),
                        cons,
                        nil,
                    )))
                }
            },
        }
    }
}

fn run_stream(body: &str, source: Source) -> Result<serde_json::Value, String> {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds,
             TypeOperators, GADTs, FlexibleContexts, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import Control.Monad.Freer hiding (run)
default (Int, Text)

data Listing a where
    GetList :: Listing [Text]

result :: Eff '[Listing] _
result = do
{body}
"#
    );
    let pp = common::prelude_path();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![StreamListing { source }];
            compile_and_run(&src, "result", &include, &mut handlers, &())
                .map(|v| v.to_json())
                .map_err(|e| format!("{e}"))
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn take_converts_one_chunk() {
    // The load-bearing stage-2 claim: take 3 of 12k converts exactly one
    // chunk's worth of elements (256), not 12,000 — observable through the
    // pull counter.
    let pulls = Arc::new(AtomicUsize::new(0));
    let r = run_stream(
        "  xs <- send GetList\n  pure (take 3 xs)",
        Source::Counted {
            n: 12_000,
            pulls: pulls.clone(),
        },
    );
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );
    let pulled = pulls.load(Ordering::Relaxed);
    assert!(
        pulled <= 256,
        "expected at most one chunk (256) pulled, got {pulled}"
    );
}

#[test]
fn full_traversal_streams() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let r = run_stream(
        "  xs <- send GetList\n  pure (length xs)",
        Source::Counted {
            n: 12_000,
            pulls: pulls.clone(),
        },
    );
    assert_eq!(r.ok(), Some(serde_json::json!(12_000)));
    assert_eq!(pulls.load(Ordering::Relaxed), 12_000);
}

#[test]
fn infinite_stream_take() {
    // An infinite producer is a legitimate infinite Haskell list.
    let r = run_stream("  xs <- send GetList\n  pure (take 5 xs)", Source::Infinite);
    assert_eq!(
        r.ok(),
        Some(serde_json::json!([
            "item-0", "item-1", "item-2", "item-3", "item-4"
        ]))
    );
}

#[test]
fn empty_stream_is_nil() {
    let pulls = Arc::new(AtomicUsize::new(0));
    let r = run_stream(
        "  xs <- send GetList\n  pure xs",
        Source::Counted { n: 0, pulls },
    );
    assert_eq!(r.ok(), Some(serde_json::json!([])));
}

#[test]
fn producer_panic_is_clean_error() {
    // A panic in producer code must NOT unwind across JIT frames (UB) —
    // it surfaces as a runtime error. Unreached panics are fine.
    let ok = run_stream(
        "  xs <- send GetList\n  pure (take 3 xs)",
        Source::PanicsAt { at: 5_000 },
    );
    assert_eq!(
        ok.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );

    let err = run_stream(
        "  xs <- send GetList\n  pure (length (take 6000 xs))",
        Source::PanicsAt { at: 5_000 },
    )
    .expect_err("panicking producer must error");
    assert!(
        err.contains("panicked") || err.contains("exploded"),
        "expected producer panic surfaced, got: {err}"
    );
}

// ===== stage 3a: element-level laziness (respond_list / indexed sources) ==

#[test]
fn list_take_converts_exactly_three() {
    // Element thunks: take 3 of 12k forces three HEADS — three conversions,
    // not a 256-element chunk, not 12,000.
    let conversions = Arc::new(AtomicUsize::new(0));
    let r = run_stream(
        "  xs <- send GetList\n  pure (take 3 xs)",
        Source::IndexedList {
            n: 12_000,
            conversions: conversions.clone(),
        },
    );
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );
    assert_eq!(conversions.load(Ordering::Relaxed), 3);
}

#[test]
fn list_length_converts_nothing() {
    // length walks the spine without ever forcing a head: ZERO element
    // conversions for a 12k listing.
    let conversions = Arc::new(AtomicUsize::new(0));
    let r = run_stream(
        "  xs <- send GetList\n  pure (length xs)",
        Source::IndexedList {
            n: 12_000,
            conversions: conversions.clone(),
        },
    );
    assert_eq!(r.ok(), Some(serde_json::json!(12_000)));
    assert_eq!(conversions.load(Ordering::Relaxed), 0);
}

#[test]
fn list_filter_forces_all() {
    // Contrast: a filter inspects every head — all elements convert.
    let conversions = Arc::new(AtomicUsize::new(0));
    let r = run_stream(
        "  xs <- send GetList\n  pure (length (filter (\\x -> \"item-1\" `isPrefixOf` x) xs))",
        Source::IndexedList {
            n: 1_000,
            conversions: conversions.clone(),
        },
    );
    // decimal-starts-with-1 in 0..1000: 1+10+100
    assert_eq!(r.ok(), Some(serde_json::json!(111)));
    assert_eq!(conversions.load(Ordering::Relaxed), 1_000);
}

#[test]
fn list_whole_result_round_trips() {
    // The whole indexed list as the program result: every element thunk is
    // forced by the result bridge; renderer truncates at 10k + "...".
    let conversions = Arc::new(AtomicUsize::new(0));
    let r = run_stream(
        "  xs <- send GetList\n  pure xs",
        Source::IndexedList {
            n: 12_000,
            conversions: conversions.clone(),
        },
    );
    let arr = r.expect("whole-list result must succeed");
    let arr = arr.as_array().expect("expected JSON array");
    assert_eq!(arr.len(), 10_001);
    assert_eq!(arr[0], serde_json::json!("item-0"));
    assert_eq!(arr[10_000], serde_json::json!("..."));
    assert_eq!(conversions.load(Ordering::Relaxed), 12_000);
}

#[test]
fn interleaved_streams_via_zip() {
    // Two lazy streams forced alternately (zip) — two independent parked
    // sources advancing in lockstep.
    let r = run_stream(
        "  xs <- send GetList\n  ys <- send GetList\n  pure (take 2 (zip xs ys))",
        Source::Infinite,
    );
    assert_eq!(
        r.ok(),
        Some(serde_json::json!([
            ["item-0", "item-0"],
            ["item-1", "item-1"]
        ]))
    );
}
