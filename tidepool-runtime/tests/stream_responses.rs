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
use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler, Response};
use tidepool_runtime::compile_and_run;

#[derive(FromCore)]
enum ListingReq {
    #[core(name = "GetList")]
    GetList,
}

/// What the handler streams, per test scenario.
enum Source {
    /// `item-0..item-{n-1}`, counting every pull through the shared counter.
    Counted { n: usize, pulls: Arc<AtomicUsize> },
    /// `item-0, item-1, ...` forever.
    Infinite,
    /// Panics when element `at` is pulled.
    PanicsAt { at: usize },
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
