//! Measurement for what one notebook cell costs in compiler requests.
//!
//! Ignored by default: it reports timings rather than asserting them, and the
//! numbers only mean something against a live compiler daemon
//! (`TIDEPOOL_EXTRACT_DAEMON_SOCKET`). Without one every request spawns its own
//! worker, so nothing is warm and a "steady state" does not exist.
//!
//! What it reports, per dispatch of the same fixed cell: the number of logical
//! extractor invocations ([`tidepool_extract_cmd::extract_spawn_count`], which
//! counts daemon-served requests too) and the wall time of the tool call. Each
//! observation is one JSON object so matched runs can also compare generated
//! native code, heap residency, and installed-program ownership without
//! scraping human prose.
//!
//! To run it: start one persistent daemon with phase timing on, then point
//! this test at it.
//!
//! ```text
//! tidepool-extract --daemon --persistent --socket /tmp/p/extract.sock \
//!   --log-path /tmp/p/compiler.log &
//! TIDEPOOL_TIMING=1 TIDEPOOL_EXTRACT_DAEMON_SOCKET=/tmp/p/extract.sock \
//!   scripts/battery.sh -p tidepool --lib --run-ignored all --no-capture \
//!   -E 'test(cell_compile_cost_measurement)'
//! ```
//!
//! `compiler.log` then carries one `tidepool-timing phase=… ms=…` line per
//! phase per request — the per-request split this test's counts do not have.

use std::time::Instant;

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CellCostObservation<'a> {
    phase: &'a str,
    round: Option<u64>,
    statements: Option<u64>,
    compiler_requests: u64,
    wall_ms: u128,
    machine: Option<tidepool_actor::ResidentMachineMeasurement>,
}

fn report(
    campaign: &super::test_campaign::TestCampaign,
    phase: &str,
    round: Option<u64>,
    statements: Option<u64>,
    requests_before: u64,
    started: Instant,
) {
    let observation = CellCostObservation {
        phase,
        round,
        statements,
        compiler_requests: tidepool_extract_cmd::extract_spawn_count() - requests_before,
        wall_ms: started.elapsed().as_millis(),
        machine: campaign.forest.measurement_snapshot(),
    };
    println!(
        "cell-cost {}",
        serde_json::to_string(&observation).expect("measurement observation serializes")
    );
}

/// Six statements, no declaration: two pure `let`s, a bind whose value a later
/// statement reads, another `let`, a second bind, and a final expression. This
/// is the shape a Shoal cell has — the statements are cheap, so what the
/// dispatch costs is almost entirely per-request compiler overhead.
const SIX_STATEMENT_CELL: &str = "let xs = [1 .. 10 :: Int]\n\
     let ys = map (* 2) xs\n\
     total <- pure (sum ys)\n\
     let label = \"total \" ++ show total\n\
     shown <- pure (length label)\n\
     (total, shown)\n";

/// One statement, for the per-request fixed cost the six-statement cell pays
/// six times over.
const ONE_STATEMENT_CELL: &str = "sum [1 .. 10 :: Int]\n";

#[tokio::test]
#[ignore = "reports compile-request counts and timings; wants a live compiler daemon"]
async fn cell_compile_cost_measurement() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tidepool_codegen::prepared_compile=info,tidepool_runtime::prepared_install=info,tidepool_harness::timing=debug,tidepool_extract_cmd::endpoint=debug,tidepool_actor::resident_workbench=debug")
        .without_time()
        .try_init();
    let daemon = std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_some();
    println!("cell-cost daemon={daemon}");
    let campaign = super::test_campaign::TestCampaign::start().await;
    let policy = campaign.root_installation.policy.clone();

    // Report the first cell separately because it pays the cold compile.
    let started = Instant::now();
    let before = tidepool_extract_cmd::extract_spawn_count();
    super::tests::dispatch_haskell_script(policy.as_ref(), ONE_STATEMENT_CELL).await;
    report(&campaign, "firstCell", None, Some(1), before, started);

    for round in 0..2 {
        for (statements, cell) in [(1, ONE_STATEMENT_CELL), (6, SIX_STATEMENT_CELL)] {
            let before = tidepool_extract_cmd::extract_spawn_count();
            let started = Instant::now();
            super::tests::dispatch_haskell_script(policy.as_ref(), cell).await;
            report(
                &campaign,
                "recurringCell",
                Some(round),
                Some(statements),
                before,
                started,
            );
        }
    }

    // `lookup` and the after-tool slot are separate request sources; report
    // what a bare lookup adds on its own.
    let before = tidepool_extract_cmd::extract_spawn_count();
    let started = Instant::now();
    let _ = super::tests::dispatch_structured_tool(
        policy.as_ref(),
        "lookup",
        serde_json::json!({"queries": ["map"]}),
    )
    .await;
    report(&campaign, "lookup", None, None, before, started);

    campaign.hosted.abort();
}
