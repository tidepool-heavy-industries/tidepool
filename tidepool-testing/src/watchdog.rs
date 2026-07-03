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
//! the item's name before processing each item.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};

static CURRENT: Mutex<String> = Mutex::new(String::new());
static EPOCH: AtomicU64 = AtomicU64::new(0);
static ARM: Once = Once::new();

/// Record the item about to be processed (bumps the liveness epoch).
pub fn begin(name: &str) {
    *CURRENT.lock().unwrap() = name.to_string();
    EPOCH.fetch_add(1, Ordering::Relaxed);
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
                if stuck_secs >= limit_secs && epoch > 0 {
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
