//! `compile_and_run_cancellable` surfaces the running machine's `CancelHandle`
//! via `on_ready`, so a watchdog thread can abort a pure runaway at a JIT
//! safepoint — the exact shape the eval/repl servers use to turn a turn timeout
//! into an actual abort (freeing the thread and the resources it pins).
//!
//! If the abort never landed, this test would hang; the assertion is that it
//! returns a `Cancelled` error instead of looping forever.
//!
//! Requires `TIDEPOOL_EXTRACT` (GHC→Core extractor); skips cleanly otherwise.

mod common;

use std::path::Path;
use std::time::Duration;

use tidepool_runtime::{compile_and_run_cancellable, DEFAULT_NURSERY_SIZE};

/// `boom` forces a non-terminating tail loop INSIDE the Eff computation (via
/// `$!`), so the runaway executes during `machine.run` — where the cancel flag
/// is installed and polled at the tail-call trampoline safepoint. (A lazily
/// returned thunk would instead loop during result rendering, outside the
/// cancel-polled region; the eval template likewise forces inside the do-block.)
const BOOM: &str = r#"{-# LANGUAGE DataKinds #-}
module Boom where
import Control.Monad.Freer (Eff)
boom :: Eff '[] Int
boom = pure $! go (0 :: Int)
  where go n = go (n + 1)
"#;

fn extract_available() -> bool {
    let bin = std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".into());
    std::process::Command::new(bin)
        .arg("--numeric-version")
        .output()
        .is_ok()
}

#[test]
fn watchdog_cancels_pure_runaway_via_on_ready_handle() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let pp = common::prelude_path();
    let include: Vec<&Path> = vec![pp.as_path()];
    let mut handlers = frunk::hlist![];

    // `on_ready` hands the machine's `CancelHandle` to a watchdog thread that
    // flips it after a short delay — exactly how a server turns a turn timeout
    // into an abort. The handle is `Send + Sync + Clone`, so this is safe.
    let result = compile_and_run_cancellable(
        BOOM,
        "boom",
        &include,
        &mut handlers,
        &(),
        DEFAULT_NURSERY_SIZE,
        |handle| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                handle.cancel();
            });
        },
    );

    let err = result.expect_err("a cancelled runaway must return Err, not a value");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Cancel"),
        "expected a Cancelled error after the watchdog abort, got: {msg}"
    );
}
