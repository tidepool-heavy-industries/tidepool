//! Per-item hang watchdog for corpus-loop suites.
//!
//! A corpus suite is one `#[test]` iterating data files, so a single
//! non-terminating item spins the suite at 100% CPU indefinitely and is
//! indistinguishable from a long run. The watchdog aborts the process
//! (exit 101) with the current item's name on raw stderr — which survives
//! libtest output capture — when one item exceeds
//! `TIDEPOOL_FIXTURE_TIMEOUT_SECS` (default 120).
//!
//! Usage: call [`arm`] once at the top of the `#[test]`, then [`begin`] with
//! the item's name before processing each item — BIND the returned [`Guard`]
//! (`let _guard = begin(name);`) so it stays alive for exactly as long as
//! that item (or, for a multi-stage test, the whole test) is being watched.
//! Under nextest (process-per-test) this is cosmetic; it matters for the
//! documented `cargo test -- --test-threads=1` fallback, where multiple
//! `#[test]`s share one process and the watchdog thread lives for the whole
//! run: without a disarm signal, the epoch going stale after the LAST armed
//! test finishes reads as a hang and kills the process mid-way through some
//! later, unrelated (and possibly just slow) test — blaming a fixture that
//! already completed. The guard's `Drop` decrements a shared active-cases
//! counter; the watchdog thread only treats epoch staleness as a hang while
//! that counter is above zero, so once every guard has dropped the epoch
//! check goes quiet on its own.
//!
//! Residual limitation this does NOT fix: two `#[test]`s running
//! CONCURRENTLY under `--test-threads=1` (i.e. genuinely multi-threaded
//! within one process — not the file's own doc scenario, but a shared-process
//! run with more than one live watcher) still share one epoch, so one test's
//! hang can be masked by another test's `begin()` calls keeping the epoch
//! moving. Fixing that would need a per-test epoch, a bigger change than the
//! disarm gap this guard closes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};

static CURRENT: Mutex<String> = Mutex::new(String::new());
static EPOCH: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicU64 = AtomicU64::new(0);
static ARM: Once = Once::new();

/// RAII disarm handle for one watched item. Decrements the shared
/// active-cases counter on drop — hold it for exactly as long as the item
/// (or test) it was created for is being watched.
pub struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Record the item about to be processed (bumps the liveness epoch and the
/// active-cases counter). Bind the returned guard — see the module docs.
#[must_use = "dropping the guard immediately disarms the watchdog for this item; bind it (`let _guard = begin(...)`) for the duration you want watched"]
pub fn begin(name: &str) -> Guard {
    *CURRENT.lock().unwrap() = name.to_string();
    EPOCH.fetch_add(1, Ordering::Relaxed);
    ACTIVE.fetch_add(1, Ordering::Relaxed);
    Guard(())
}

/// Spawn the watchdog thread (idempotent). The thread dies with the process.
pub fn arm() {
    ARM.call_once(|| {
        let limit_secs: u64 = std::env::var("TIDEPOOL_FIXTURE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        std::thread::spawn(move || {
            let mut last_epoch = EPOCH.load(Ordering::Relaxed);
            let mut stuck_secs = 0u64;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(10));
                let epoch = EPOCH.load(Ordering::Relaxed);
                if epoch == last_epoch {
                    stuck_secs += 10;
                } else {
                    last_epoch = epoch;
                    stuck_secs = 0;
                }
                // Only treat staleness as a hang while at least one guard is
                // still held — once every watched item/test has finished and
                // dropped its guard, a quiet epoch just means nobody is
                // currently being watched, not that something is stuck.
                if stuck_secs >= limit_secs && ACTIVE.load(Ordering::Relaxed) > 0 {
                    let name = CURRENT.lock().unwrap().clone();
                    use std::io::Write;
                    let _ = writeln!(
                        std::io::stderr(),
                        "\n[FIXTURE WATCHDOG] item '{name}' exceeded {limit_secs}s — \
                         suspected non-termination; aborting suite"
                    );
                    std::process::exit(101);
                }
            }
        });
    });
}
