//! Turn-latency bench: drives real turns through the production compile/run
//! path (`Harness::drive_turn` / `Harness::run_to_hole_or_done`) with a
//! `replay::ReplayProvider` substituting the model, and reports per-stage
//! median/p90/total latency plus per-turn wall clock and the unattributed
//! residual (turn wall minus the sum of attributed stages).
//!
//! MEASUREMENT ONLY — see
//! `plans/self-iterating-harness/11-turn-latency-contract.md` for the
//! documented invocation and what the four scenarios below are for.
//!
//! Run with: `cargo run --release --example turn_latency_bench -p tidepool-harness`
//! (needs `TIDEPOOL_EXTRACT` + a with-packages GHC on `PATH`; see the
//! contract doc for the exact env).

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::timing::RUST_STAGES;
use tidepool_harness::{answerer_decls, Harness, NodeId, TurnOutcome};

// ---------------------------------------------------------------------------
// Tracing collector — matches the event shape `timing::record_stage` emits
// (target `tidepool_harness::timing`, fields `node`/`round`/`stage`/`ms`/
// `bytes`). Zero Rust-side call sites exist yet as of this bench's authoring
// (sibling branches land them after this one merges), so an empty `stages`
// table per scenario is EXPECTED today, not a bug — see `Meta::note` below.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct StageSample {
    node: u64,
    #[allow(dead_code)]
    round: u64,
    stage: String,
    ms: u64,
    #[allow(dead_code)]
    bytes: u64,
}

#[derive(Default)]
struct Collector {
    /// The active scenario label, or `None` to drop events between windows.
    window: Mutex<Option<String>>,
    samples: Mutex<Vec<(String, StageSample)>>,
}

impl Collector {
    fn start_window(&self, label: &str) {
        *self.window.lock().unwrap() = Some(label.to_string());
    }

    fn stop_window(&self) {
        *self.window.lock().unwrap() = None;
    }

    fn samples_for(&self, label: &str) -> Vec<StageSample> {
        self.samples
            .lock()
            .unwrap()
            .iter()
            .filter(|(l, _)| l == label)
            .map(|(_, s)| s.clone())
            .collect()
    }
}

#[derive(Default)]
struct StageVisitor {
    node: Option<u64>,
    round: Option<u64>,
    stage: Option<String>,
    ms: Option<u64>,
    bytes: Option<u64>,
}

impl Visit for StageVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "node" => self.node = Some(value),
            "round" => self.round = Some(value),
            "ms" => self.ms = Some(value),
            "bytes" => self.bytes = Some(value),
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if value >= 0 {
            self.record_u64(field, value as u64);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "stage" {
            self.stage = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {
        // `message` and any non-numeric/non-str field land here; the
        // `record_stage` shape never needs it (stage/ms/node/round/bytes are
        // all typed primitives, caught above).
    }
}

/// A `tracing_subscriber::Layer` that matches ONLY `target:
/// "tidepool_harness::timing"` events and files their `stage`/`ms`/`node`/
/// `round`/`bytes` fields into whichever scenario window is currently open.
/// Installed as the GLOBAL default subscriber (not composed with the
/// caller's `RUST_LOG`/`EnvFilter`) so a human who forgets to set `RUST_LOG`
/// still gets data — step 8 of the spec this bench implements.
#[derive(Clone)]
struct TimingLayer(Arc<Collector>);

impl<S: tracing::Subscriber> Layer<S> for TimingLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "tidepool_harness::timing" {
            return;
        }
        let mut visitor = StageVisitor::default();
        event.record(&mut visitor);
        let (Some(stage), Some(ms)) = (visitor.stage, visitor.ms) else {
            return;
        };
        let Some(label) = self.0.window.lock().unwrap().clone() else {
            return;
        };
        self.0.samples.lock().unwrap().push((
            label,
            StageSample {
                node: visitor.node.unwrap_or(u64::MAX),
                round: visitor.round.unwrap_or(u64::MAX),
                stage,
                ms,
                bytes: visitor.bytes.unwrap_or(0),
            },
        ));
    }
}

/// Install the collector as the process-global tracing subscriber. No
/// `EnvFilter` layer is composed in, so this does NOT depend on the caller's
/// `RUST_LOG` — `record_stage`'s DEBUG events reach this layer regardless.
fn install_collector() -> Arc<Collector> {
    let collector = Arc::new(Collector::default());
    let layer = TimingLayer(collector.clone());
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::set_global_default(subscriber)
        .expect("install turn-latency-bench tracing subscriber (called once, at process start)");
    collector
}

// ---------------------------------------------------------------------------
// Report shape
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StageStat {
    stage: String,
    n: usize,
    median_ms: f64,
    p90_ms: f64,
    total_ms: u64,
}

#[derive(Serialize)]
struct TurnRecord {
    index: usize,
    label: String,
    node: u64,
    outcome: String,
    wall_ms: u64,
    attributed_ms: u64,
    unattributed_ms: u64,
}

#[derive(Serialize)]
struct ScenarioReport {
    scenario: String,
    n_turns: usize,
    turns: Vec<TurnRecord>,
    stages: Vec<StageStat>,
    wall_ms_total: u64,
    wall_ms_median: f64,
}

#[derive(Serialize)]
struct Meta {
    percentile_definition: String,
    rust_stages: Vec<&'static str>,
    note: String,
}

#[derive(Serialize)]
struct BenchReport {
    pid: u32,
    n: usize,
    size_n: usize,
    retry_repeats: usize,
    overall_wall_ms: u64,
    output_path: String,
    scenarios: Vec<ScenarioReport>,
    meta: Meta,
}

// ---------------------------------------------------------------------------
// Harness construction — mirrors tests/acceptance_selfharness.rs /
// tests/selfharness_spine.rs's construction (real Harness over
// `answerer_decls()`, a `ReplayProvider` substituting the model). Each
// scenario gets its OWN `Harness` (its own boot compile), matching "cold" as
// this bench's first turn in a freshly booted harness.
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> PathBuf {
    repo_root().join("haskell/lib")
}

fn header(tag: &str) -> LogHeader {
    LogHeader {
        prelude_hash: format!("turn-latency-bench-{tag}"),
        extract_fingerprint: format!("turn-latency-bench-{tag}"),
        harness_version: "turn-latency-bench".to_string(),
    }
}

fn reply(content: String) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content,
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
        },
    }
}

fn fenced(block: &str) -> String {
    format!("```haskell\n{block}\n```")
}

fn build_harness(replies: Vec<RecordedReply>, tag: &str) -> Result<Harness, Box<dyn Error>> {
    let cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)?;
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let log_path = std::env::temp_dir().join(format!(
        "turn-latency-bench-{tag}-{}.jsonl",
        std::process::id()
    ));
    let writer = LogWriter::create(&log_path, &header(tag))?;
    Ok(Harness::new(writer, cfg, provider)?)
}

// ---------------------------------------------------------------------------
// Blocks — the fenced ```haskell content a replayed assistant turn "wrote".
// All finalize a plain `Int` (no author-defined ADT / project_lib needed).
// ---------------------------------------------------------------------------

/// `finalize @Int 1`-shaped: the smallest possible answerer turn.
fn small_block() -> String {
    "(finalize @Int 1 :: M ())".to_string()
}

/// Tens of lines: a chain of local helper bindings then a fold over them.
/// Uses an EXPLICIT `let { ...; ... }` brace list (not the whitespace-offside
/// form) so the generated source's exact indentation can't break layout.
fn large_block() -> String {
    let count = 60;
    let mut bindings = vec!["helper0 x = x + 1".to_string()];
    for i in 1..count {
        let prev = i - 1;
        bindings.push(format!("helper{i} x = helper{prev} x + 1"));
    }
    let last = count - 1;
    bindings.push(format!(
        "total = sum (map (\\i -> helper{last} i) [1 .. 40])"
    ));
    let body = bindings.join("\n     ; ");
    format!("(let {{ {body}\n     }} in finalize @Int total :: M ())")
}

/// Does not typecheck (`Int` expected, `String` given) — a real GHC
/// diagnostic, not a stub error.
fn bad_block() -> String {
    "(finalize @Int \"oops-not-an-int\" :: M ())".to_string()
}

/// The corrected reply a retry scenario replays after `bad_block`'s error.
fn good_block() -> String {
    "(finalize @Int 42 :: M ())".to_string()
}

// ---------------------------------------------------------------------------
// Scenario runners
// ---------------------------------------------------------------------------

type TurnRow = (usize, String, u64, Duration, String);

fn outcome_label(outcome: &TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

async fn drive_one(
    harness: &Harness,
    scenario: &str,
    index: usize,
) -> Result<(NodeId, Duration, TurnOutcome), Box<dyn Error>> {
    let node = harness.create_root(&format!("{scenario}-{index}"), "Begin.")?;
    harness.force(node, Actor::Operator)?;
    let start = Instant::now();
    let outcome = harness.drive_turn(node).await?;
    Ok((node, start.elapsed(), outcome))
}

/// COLD (first turn in this freshly-booted `Harness`) vs WARM (every turn
/// after) — the single most important axis: tells us whether anything is
/// actually amortized across turns in the same process.
async fn scenario_cold_warm(
    collector: &Collector,
    n: usize,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let scenario = "cold_vs_warm";
    let replies = (0..n).map(|_| reply(fenced(&small_block()))).collect();
    let harness = build_harness(replies, scenario)?;

    collector.start_window(scenario);
    let mut turns: Vec<TurnRow> = Vec::new();
    for i in 0..n {
        let label = if i == 0 { "cold" } else { "warm" };
        let (node, wall, outcome) = drive_one(&harness, scenario, i).await?;
        turns.push((
            i,
            label.to_string(),
            node.0,
            wall,
            outcome_label(&outcome).to_string(),
        ));
    }
    collector.stop_window();

    Ok(build_report(scenario, n, turns, collector))
}

/// SMALL vs LARGE block: does turn cost scale with source size, or is it a
/// flat floor dominated by fixed per-spawn cost?
async fn scenario_small_vs_large(
    collector: &Collector,
    n: usize,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let scenario = "small_vs_large_block";
    let mut replies: Vec<RecordedReply> = (0..n).map(|_| reply(fenced(&small_block()))).collect();
    replies.extend((0..n).map(|_| reply(fenced(&large_block()))));
    let harness = build_harness(replies, scenario)?;

    collector.start_window(scenario);
    let mut turns: Vec<TurnRow> = Vec::new();
    let mut idx = 0;
    for _ in 0..n {
        let (node, wall, outcome) = drive_one(&harness, scenario, idx).await?;
        turns.push((
            idx,
            "small".to_string(),
            node.0,
            wall,
            outcome_label(&outcome).to_string(),
        ));
        idx += 1;
    }
    for _ in 0..n {
        let (node, wall, outcome) = drive_one(&harness, scenario, idx).await?;
        turns.push((
            idx,
            "large".to_string(),
            node.0,
            wall,
            outcome_label(&outcome).to_string(),
        ));
        idx += 1;
    }
    collector.stop_window();

    Ok(build_report(scenario, 2 * n, turns, collector))
}

/// A turn whose block does not typecheck, followed by a corrected turn —
/// `Harness::run_to_hole_or_done`'s corrective-retry loop is the multiplier
/// we care about, so a failed compile's cost must appear in the per-node
/// stage sum (both attempts share ONE node, since the retry pushes the GHC
/// diagnostic back as a user turn on the SAME node rather than starting a
/// fresh one).
async fn scenario_retry(
    collector: &Collector,
    repeats: usize,
) -> Result<ScenarioReport, Box<dyn Error>> {
    let scenario = "compile_error_retry";
    let mut replies = Vec::new();
    for _ in 0..repeats {
        replies.push(reply(fenced(&bad_block())));
        replies.push(reply(fenced(&good_block())));
    }
    let harness = build_harness(replies, scenario)?;

    collector.start_window(scenario);
    let mut turns: Vec<TurnRow> = Vec::new();
    for i in 0..repeats {
        let node = harness.create_root(&format!("{scenario}-{i}"), "Begin.")?;
        harness.force(node, Actor::Operator)?;
        let start = Instant::now();
        let outcome = harness.run_to_hole_or_done(node).await?;
        let wall = start.elapsed();
        turns.push((
            i,
            "retry_roundtrip".to_string(),
            node.0,
            wall,
            outcome_label(&outcome).to_string(),
        ));
    }
    collector.stop_window();

    Ok(build_report(scenario, repeats, turns, collector))
}

// ---------------------------------------------------------------------------
// Summary math — n is small (single-digit), so this is a plain sorted-index
// nearest-rank percentile, documented (not silently implied precise): with
// n<=2 "p90" coincides with the max, a worst-observed marker rather than a
// true 90th-percentile estimate.
// ---------------------------------------------------------------------------

fn percentile(sorted_values: &[u64], p: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let idx = (((sorted_values.len() - 1) as f64) * p).round() as usize;
    sorted_values[idx.min(sorted_values.len() - 1)] as f64
}

fn summarize_stage(stage: &str, mut values: Vec<u64>) -> StageStat {
    values.sort_unstable();
    let total_ms = values.iter().sum();
    StageStat {
        stage: stage.to_string(),
        n: values.len(),
        median_ms: percentile(&values, 0.5),
        p90_ms: percentile(&values, 0.9),
        total_ms,
    }
}

fn build_report(
    scenario: &str,
    n: usize,
    turns: Vec<TurnRow>,
    collector: &Collector,
) -> ScenarioReport {
    let samples = collector.samples_for(scenario);

    let mut by_stage: HashMap<String, Vec<u64>> = HashMap::new();
    let mut by_node: HashMap<u64, u64> = HashMap::new();
    for s in &samples {
        by_stage.entry(s.stage.clone()).or_default().push(s.ms);
        *by_node.entry(s.node).or_default() += s.ms;
    }

    // Every known Rust-side stage first (fixed pipeline order), then any
    // `extract.*` phase stages present, sorted for stable output.
    let mut stages: Vec<StageStat> = RUST_STAGES
        .iter()
        .filter_map(|stage| {
            by_stage
                .get(*stage)
                .map(|v| summarize_stage(stage, v.clone()))
        })
        .collect();
    let mut extra: Vec<&String> = by_stage
        .keys()
        .filter(|k| !RUST_STAGES.contains(&k.as_str()))
        .collect();
    extra.sort();
    for k in extra {
        stages.push(summarize_stage(k, by_stage[k].clone()));
    }

    let turn_records: Vec<TurnRecord> = turns
        .into_iter()
        .map(|(index, label, node, wall, outcome)| {
            let wall_ms = wall.as_millis() as u64;
            let attributed_ms = *by_node.get(&node).unwrap_or(&0);
            TurnRecord {
                index,
                label,
                node,
                outcome,
                wall_ms,
                attributed_ms,
                unattributed_ms: wall_ms.saturating_sub(attributed_ms),
            }
        })
        .collect();

    let mut wall_values: Vec<u64> = turn_records.iter().map(|t| t.wall_ms).collect();
    let wall_ms_total = wall_values.iter().sum();
    wall_values.sort_unstable();
    let wall_ms_median = percentile(&wall_values, 0.5);

    ScenarioReport {
        scenario: scenario.to_string(),
        n_turns: n,
        turns: turn_records,
        stages,
        wall_ms_total,
        wall_ms_median,
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn default_output_path() -> PathBuf {
    repo_root().join("target").join("turn-latency-bench.json")
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error>> {
    let collector = install_collector();

    let n = env_usize("TURN_LATENCY_BENCH_N", 5);
    let size_n = env_usize("TURN_LATENCY_BENCH_SIZE_N", n.clamp(1, 3));
    let retry_repeats = env_usize("TURN_LATENCY_BENCH_RETRY_N", n.clamp(1, 3));

    let overall_start = Instant::now();

    let scenarios = vec![
        scenario_cold_warm(&collector, n).await?,
        scenario_small_vs_large(&collector, size_n).await?,
        scenario_retry(&collector, retry_repeats).await?,
    ];

    let overall_wall_ms = overall_start.elapsed().as_millis() as u64;

    let output_path = std::env::var("TURN_LATENCY_BENCH_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_output_path());
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let report = BenchReport {
        pid: std::process::id(),
        n,
        size_n,
        retry_repeats,
        overall_wall_ms,
        output_path: output_path.display().to_string(),
        scenarios,
        meta: Meta {
            percentile_definition: "sorted-index nearest-rank: idx = round((len-1)*p), 0-indexed \
                into the sorted sample. With n<=2 samples 'p90' coincides with the max — read it \
                as a worst-observed marker, not a precise 90th-percentile estimate."
                .to_string(),
            rust_stages: RUST_STAGES.to_vec(),
            note: "Stage samples come from tidepool_harness::timing::record_stage call sites. \
                As of this bench's authoring, ZERO Rust-side call sites and ZERO extract-side \
                TIDEPOOL_TIMING forwarding exist yet (two sibling branches land them, merging \
                after this one) — an empty `stages` array, or all-zero stage stats, is EXPECTED \
                here, not a bug. Each turn's `wall_ms` is measured independently by this bench \
                (wraps `Harness::drive_turn`/`run_to_hole_or_done`) and is always populated."
                .to_string(),
        },
    };

    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&output_path, &json)?;
    println!("{json}");
    Ok(())
}
