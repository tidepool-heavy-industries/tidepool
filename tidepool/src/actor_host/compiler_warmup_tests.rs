//! Measurement for the compiler warm-up ([`super::warm_compiler`]).
//!
//! Ignored by default, for two reasons that are both about honesty rather than
//! cost. It needs a live compiler daemon (`TIDEPOOL_EXTRACT_DAEMON_SOCKET`):
//! without one every compile spawns its own worker, there is no module memo to
//! warm, and the numbers would say nothing about the thing being measured. And
//! it needs a real workspace to compile (`TIDEPOOL_WARMUP_WORKSPACE`), because
//! the graph that costs a run its first minute is the workspace's graph — the
//! stdlib plus `[haskell] modules` plus `Jev.Operators`.
//!
//! `TIDEPOOL_WARMUP=1` turns the warm-up on. Running this test once against a
//! fresh daemon with it off and once against another fresh daemon with it on is
//! the A/B; the compiler daemon's own log carries the per-request
//! `tidepool-timing phase=lowering` and `tidepool-memo-miss` lines that say where
//! the difference came from.

use std::time::Instant;

/// The imports `compile_root` gives an actor's workbench for a workspace that
/// supplies `Jev.Operators`. The warm-up primes the graph a CELL reaches, not
/// the smaller one the driver alone reaches, so the measurement uses the same
/// list a cell would.
const WORKBENCH_IMPORTS: &str = "qualified Data.Set as Set\n\
     qualified Tidepool.Inspection as TidepoolInspection\n\
     Tidepool.Inspection (print, cellDisplay)\n\
     Tidepool.Actors.Shoal\n\
     qualified Tidepool.Actor.Record as R\n\
     qualified Tidepool.Command as Cmd\n\
     qualified Jev.Operators as J\n\
     qualified Jev.Core\n\
     qualified Jev.Core.Contract\n\
     qualified Jev.Core.Schema\n\
     qualified Jev.Core.Json\n\
     Tidepool.Command (bash, withMemory, Memory(..))\n\
     qualified Tidepool.Actor as Actor\n\
     Tidepool.QQ (fmt, j, patch, uri)";

#[test]
#[ignore = "needs a live compiler daemon and a workspace; reports timings rather than asserting"]
fn compiler_warmup_measurement() {
    assert!(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_some(),
        "set {} to a live compiler daemon socket; without one there is no module memo to warm",
        tidepool_extract_cmd::DAEMON_SOCKET_ENV
    );
    let workspace = std::path::PathBuf::from(
        std::env::var("TIDEPOOL_WARMUP_WORKSPACE")
            .expect("set TIDEPOOL_WARMUP_WORKSPACE to a Shoal workspace"),
    );
    let run = tempfile::tempdir().expect("scratch run root");
    let run_root = run.path().to_path_buf();
    let frozen = crate::shoal::workspace::FrozenWorkspace::load(&workspace, &run_root)
        .expect("workspace loads");
    let haskell_root = crate::haskell_sources::ensure_shoal_haskell().expect("shoal Haskell root");

    let started = Instant::now();
    super::compile_driver(&haskell_root, Some(&frozen), &run_root, None).expect("driver compiles");
    println!(
        "warmup-measure first-driver-compile-ms={}",
        started.elapsed().as_millis()
    );

    if std::env::var("TIDEPOOL_WARMUP").as_deref() == Ok("1") {
        let started = Instant::now();
        super::warm_compiler(&haskell_root, Some(&frozen), &run_root, WORKBENCH_IMPORTS)
            .expect("warm-up compiles");
        println!("warmup-measure warmup-ms={}", started.elapsed().as_millis());
    }

    // What a first cell pays: another compile over the same graph. A warm memo
    // makes this one cheap; a cold one makes it cost what the first did.
    let started = Instant::now();
    super::compile_driver(&haskell_root, Some(&frozen), &run_root, None).expect("driver compiles");
    println!(
        "warmup-measure next-compile-ms={}",
        started.elapsed().as_millis()
    );
}
