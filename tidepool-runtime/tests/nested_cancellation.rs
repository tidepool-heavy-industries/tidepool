//! External cancellation observed *between* effect handler invocations.
//!
//! The unit-level external_cancellation suite covers cancelling a top-level
//! tail-recursive tickLoop with no effects. This test covers the more realistic
//! shape: a Haskell program that yields effect requests in a tickLoop, with the
//! cancellation flag flipped from inside an effect handler. The JIT must
//! observe the flag at the next safepoint after `machine.resume()` returns
//! from handling the request, surfacing `YieldError::Cancelled` through the
//! same error path as a top-level cancel.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tidepool_bridge_derive::FromCore;
use tidepool_codegen::jit_machine::{CancelHandle, JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;
use tidepool_runtime::compile_haskell;

#[derive(FromCore)]
enum TickReq {
    #[core(name = "Tick")]
    Tick,
}

/// Counts handler invocations and flips the JIT's cancel flag on the
/// `cancel_at`'th call. The handler always responds normally with `()` —
/// cancellation is a side channel observed by the JIT, not a return value.
/// `count` is an Arc so the test can read the final value after the
/// handler is moved into the hlist.
struct CancellingHandler {
    count: Arc<AtomicUsize>,
    cancel_at: usize,
    handle: CancelHandle,
}

impl EffectHandler for CancellingHandler {
    type Request = TickReq;
    fn handle(
        &mut self,
        req: TickReq,
        cx: &EffectContext,
    ) -> Result<tidepool_effect::Response, EffectError> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.cancel_at {
            self.handle.cancel();
        }
        match req {
            TickReq::Tick => cx.respond(()),
        }
    }
}

// Concrete effect list (matches the cross-module fixture style) and a
// recursive loop. The polymorphic `Member TickEff effs => ...` form
// can fail to JIT-elaborate in this harness; the concrete `'[TickEff]`
// alias is the working pattern we know.
const LOOP_SOURCE: &str = r#"{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module TickLoop where
import Control.Monad.Freer (Eff, send)
data TickEff a where Tick :: TickEff ()
tickLoop :: Eff '[TickEff] ()
tickLoop = do
  send Tick
  tickLoop
"#;

#[test]
fn cancel_from_inside_effect_handler_unwinds() {
    let pp = common::prelude_path();
    let include: Vec<&Path> = vec![pp.as_path()];

    let (expr, mut table, _warnings) =
        compile_haskell(LOOP_SOURCE, "tickLoop", &include).expect("compile tickLoop fixture");
    table.populate_siblings_from_expr(&expr);

    let mut machine =
        JitEffectMachine::compile(&expr, &table, 1 << 20).expect("compile JIT machine");
    let handle = machine.cancel_handle();
    assert!(!handle.is_cancelled());

    let count = Arc::new(AtomicUsize::new(0));
    let handler = CancellingHandler {
        count: count.clone(),
        cancel_at: 5,
        handle: handle.clone(),
    };
    let mut handlers = frunk::hlist![handler];

    let result = machine.run(&table, &mut handlers, &());
    let calls = count.load(Ordering::SeqCst);

    // The handler reached the cancel point...
    assert!(
        calls >= 5,
        "handler must have reached the cancel point; got count={} (result was {:?})",
        calls,
        result,
    );

    // ...and the dispatch loop's cancel safepoint must observe it
    // *promptly* (next iteration), not after thousands of additional
    // yields. The exact bound is loose to allow for the cancel firing on
    // call N where N >= cancel_at; we just guard against the regression
    // where cancel observation requires JIT-side gc_trigger or trampoline
    // safepoints (which freer-simple effect loops don't reliably hit).
    const PROMPT_CANCEL_LIMIT: usize = 50;
    assert!(
        calls < PROMPT_CANCEL_LIMIT,
        "cancel was not observed promptly: handler ran {} times after cancel was set on call 5 \
         (limit {} additional calls); the dispatch-loop safepoint may have regressed",
        calls,
        PROMPT_CANCEL_LIMIT,
    );

    match result {
        Err(JitError::Yield(YieldError::Cancelled)) => {}
        Err(other) => panic!(
            "expected JitError::Yield(YieldError::Cancelled) after handler-driven cancel, \
             got {:?} (handler ran {} times)",
            other, calls
        ),
        Ok(v) => panic!(
            "expected the tickLoop to be cancelled, got Ok({:?}) after {} handler calls",
            v, calls
        ),
    }

    assert!(
        handle.is_cancelled(),
        "the shared CancelHandle must still report cancelled after run() returns"
    );
}
