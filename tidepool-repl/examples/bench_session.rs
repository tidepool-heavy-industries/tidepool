//! Repl-session turn-latency probe for `scripts/bench-turn.sh`'s session and
//! block rows. Drives [`tidepool_repl::session::Session`]'s PUBLIC API only
//! (`Session::open` + `run_turn`) — no internals, since the session's
//! internals are actively being reworked by a sibling branch.
//!
//! Two independent scenarios, each its own fresh `Session` (its own boot):
//!
//! - `session`: turn 0 (a decl) then three subsequent one-item `Block` turns.
//! - `block5`: ONE `SessionCommand::Block` turn of 5 independent items — the
//!   before/after instrument for the in-flight batch-turns lane: what one
//!   call carrying 5 items costs today, vs `session`'s per-item turns.
//!
//! Per-turn wall clock is always measured directly (`Instant`). Per-stage
//! breakdown (extract/JIT phases) is captured BEST-EFFORT via a tracing
//! collector matching `tidepool_harness::timing::record_stage`'s event shape
//! — the same collection idiom `tidepool-harness/examples/turn_latency_bench.rs`
//! uses, trimmed to this bin's single-process, sequential-turns needs (no
//! percentile math: `scripts/bench-turn.sh` takes the median ACROSS repeated
//! process runs of this bin, not within one).
//!
//! Output: flat `key=value` lines to stdout, one per (turn, metric). Run with
//! (needs `TIDEPOOL_EXTRACT` + a with-packages GHC on `PATH`):
//!   `cargo run --release --example bench_session -p tidepool-repl`

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use std::time::Instant;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

use tidepool_mcp::EffectRoster;
use tidepool_repl::command::BlockItem;
use tidepool_repl::session::TurnStep;
use tidepool_repl::{
    BoxedStack, DeclText, ExprText, Session, SessionCommand, SessionConfig, TurnOutcome,
};
use tidepool_repr::SessionId;

// ---------------------------------------------------------------------------
// Tracing collector — matches `tidepool_harness::timing::record_stage`'s event
// shape (target `tidepool_harness::timing`, fields node/round/stage/ms/bytes).
// Attribution is by an in-flight "current turn label" window, not by `node`
// (both `jit_codegen`/`run_exec` and every forwarded extract phase carry the
// `NO_NODE` sentinel here — see `timing.rs`'s module doc).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Collector {
    current: Mutex<Option<String>>,
    // (turn label, stage) -> summed ms. A label is used at most once per
    // process (each scenario's turns are individually labelled), so summing
    // rather than overwriting is equivalent to "the one value" while still
    // being correct if a stage is ever emitted twice for the same turn.
    sums: Mutex<HashMap<(String, String), u64>>,
}

impl Collector {
    fn begin(&self, label: &str) {
        *self.current.lock() = Some(label.to_string());
    }

    fn end(&self) {
        *self.current.lock() = None;
    }

    fn drain_for(&self, label: &str) -> Vec<(String, u64)> {
        let sums = self.sums.lock();
        let mut out: Vec<(String, u64)> = sums
            .iter()
            .filter(|((l, _), _)| l == label)
            .map(|((_, stage), ms)| (stage.clone(), *ms))
            .collect();
        out.sort();
        out
    }
}

#[derive(Default)]
struct StageVisitor {
    stage: Option<String>,
    ms: Option<u64>,
}

impl Visit for StageVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "ms" {
            self.ms = Some(value);
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

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

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
        let Some(label) = self.0.current.lock().clone() else {
            return;
        };
        *self.0.sums.lock().entry((label, stage)).or_insert(0) += ms;
    }
}

fn install_collector() -> Arc<Collector> {
    let collector = Arc::new(Collector::default());
    let layer = TimingLayer(collector.clone());
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::set_global_default(subscriber)
        .expect("install bench_session tracing subscriber (called once, at process start)");
    collector
}

// ---------------------------------------------------------------------------
// Session construction — mirrors `session.rs`'s
// `reset_clears_stale_cancel_handle_from_slot` test (the documented recipe
// for a real, non-mock `Session` at the bare API level).
// ---------------------------------------------------------------------------

fn open_session(root: &std::path::Path, tag: &str) -> Session {
    let stack = tidepool_handlers::build_minimal_stack();
    let roster = EffectRoster::from_handlers(&stack);
    let effects_dir =
        tidepool_mcp::ensure_effects_module(roster.decls()).expect("write Tidepool.Effects module");
    let prelude_dir = tidepool_testing::eval_harness::prelude_path();
    let module_env = tidepool_mcp::session_decl_module_env(roster.decls(), false);
    let preamble = tidepool_mcp::build_preamble_non_interactive_mode(
        roster.decls(),
        false,
        tidepool_mcp::PaginateMode::Passthrough,
    );
    let effect_stack = tidepool_mcp::build_effect_stack_type(roster.decls());

    let mut base_include = effects_dir.include_paths().to_vec();
    base_include.push(prelude_dir);
    let cfg = SessionConfig {
        id: SessionId(1),
        root: root.join(tag),
        base_include,
        roster,
        preamble,
        effect_stack,
        module_env,
        nursery_size: tidepool_repl::DEFAULT_NURSERY_SIZE,
    };
    Session::open(cfg, Box::new(move || Box::new(stack.clone()) as BoxedStack))
        .expect("session opens")
}

fn outcome_label(step: &TurnStep) -> &'static str {
    match step {
        TurnStep::Completed(TurnOutcome::Value { .. }) => "Value",
        TurnStep::Completed(TurnOutcome::Bound { .. }) => "Bound",
        TurnStep::Completed(TurnOutcome::MultiBound { .. }) => "MultiBound",
        TurnStep::Completed(TurnOutcome::Defined { .. }) => "Defined",
        TurnStep::Completed(TurnOutcome::Meta(_)) => "Meta",
        TurnStep::Completed(TurnOutcome::Block { .. }) => "Block",
        TurnStep::Completed(TurnOutcome::Error(_)) => "Error",
        TurnStep::Suspended(_) => "Suspended",
    }
}

fn one_item_block(item: BlockItem) -> SessionCommand {
    SessionCommand::Block {
        items: vec![item],
        verbose: false,
    }
}

fn run_one(
    session: &mut Session,
    collector: &Collector,
    label: &str,
    cmd: &SessionCommand,
) -> (std::time::Duration, &'static str) {
    let gate = tidepool_effect::pause::PauseGate::new();
    let captured = tidepool_mcp::CapturedOutput::new();
    collector.begin(label);
    let start = Instant::now();
    let step = session.run_turn(cmd, gate, &captured);
    let wall = start.elapsed();
    collector.end();
    let outcome = outcome_label(&step);
    if matches!(step, TurnStep::Suspended(_)) {
        panic!("bench_session: turn '{label}' unexpectedly suspended on an ask");
    }
    (wall, outcome)
}

fn report_turn(collector: &Collector, label: &str, wall: std::time::Duration, outcome: &str) {
    println!("{label}.wall_ms={}", wall.as_millis());
    println!("{label}.outcome={outcome}");
    for (stage, ms) in collector.drain_for(label) {
        println!("{label}.{stage}_ms={ms}");
    }
}

fn scenario_session(root: &std::path::Path) {
    let mut session = open_session(root, "session");
    let collector = install_collector_once();

    let turn0 = one_item_block(BlockItem::Decl(DeclText(
        "helper x = x + (1 :: Int)".to_string(),
    )));
    let (wall, outcome) = run_one(&mut session, &collector, "session.turn0", &turn0);
    report_turn(&collector, "session.turn0", wall, outcome);

    // A bind (`vN <- pure (...)`), not a bare final expression — see
    // `scenario_block5`'s comment: a bare final expression routes through
    // the value-render path, which costs an extra ambiguous-defaulting
    // compile retry under this effect stack, independent of this bench.
    for (i, arg) in [1, 2, 3].into_iter().enumerate() {
        let label = format!("session.turn{}", i + 1);
        let cmd = one_item_block(BlockItem::Stmt(ExprText(format!(
            "v{arg} <- pure (helper {arg})"
        ))));
        let (wall, outcome) = run_one(&mut session, &collector, &label, &cmd);
        report_turn(&collector, &label, wall, outcome);
    }
}

fn scenario_block5(root: &std::path::Path) {
    let mut session = open_session(root, "block5");
    let collector = install_collector_once();

    // Every item is a BIND (`vN <- pure (...)`), not a bare final expression:
    // a block's final bare expression routes through the session's
    // value-render path (`it`/`__it_render`), which — independent of this
    // bench — needs more than a type annotation to resolve cleanly for a
    // plain numeric literal, and would cost a wasted ambiguous-defaulting
    // retry (one extra full extract compile) that this bench shouldn't pay
    // for. A bind sidesteps that path entirely while still doing 5
    // independent items of real work.
    let items: Vec<BlockItem> = (1..=5)
        .map(|n| BlockItem::Stmt(ExprText(format!("v{n} <- pure (({n} + {n}) :: Int)"))))
        .collect();
    let cmd = SessionCommand::Block {
        items,
        verbose: false,
    };
    let (wall, outcome) = run_one(&mut session, &collector, "block5.turn0", &cmd);
    report_turn(&collector, "block5.turn0", wall, outcome);
}

// The subscriber can only be installed once per process; both scenarios share
// it (labels are namespaced per-scenario, so there's no cross-talk).
fn install_collector_once() -> Arc<Collector> {
    static COLLECTOR: std::sync::OnceLock<Arc<Collector>> = std::sync::OnceLock::new();
    COLLECTOR.get_or_init(install_collector).clone()
}

fn main() {
    tidepool_testing::eval_harness::require_extract();
    let dir = tempfile::tempdir().expect("tempdir");

    scenario_session(dir.path());
    scenario_block5(dir.path());
}
