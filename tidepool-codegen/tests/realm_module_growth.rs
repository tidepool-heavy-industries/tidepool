//! Throwaway measurement scaffolding for the realm-lifetime spike, COST B
//! (compiled-function lifetime). See
//! `plans/post-restart/spike-notes/realm-lifetime.md` for the receipts this
//! feeds and the static-analysis half of the finding (in particular: reading
//! `cranelift-jit` 0.129.1's own source shows `ArenaMemoryProvider::drop`
//! deliberately LEAKS its arena once anything has been finalized, and
//! nothing in this repo calls the escape-hatch `JITModule::free_memory`).
//!
//! No Cranelift-side allocated-bytes accessor is reachable from
//! `JitEffectMachine` (`ArenaMemoryProvider` exposes none publicly, and
//! `CodegenPipeline::module`/`JITModule` doesn't expose its private
//! `memory: Box<dyn JITMemoryProvider>` either) — so the memory proxy here is
//! process RSS read from `/proc/self/status`'s `VmRSS` line. That is a noisy
//! signal (allocator behavior, other threads, page cache) but it is the only
//! number reachable without vendoring Cranelift.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::DataConTable;

use serial_test::serial;

#[path = "support/session_scaffold.rs"]
mod session_scaffold;
use session_scaffold::{build_value_fragment, C1};

fn table_with_c1() -> DataConTable {
    let mut table = DataConTable::new();
    table.insert(DataCon {
        id: C1,
        name: "C1".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
        type_name: String::new(),
    });
    table
}

/// Process RSS in bytes, read from `/proc/self/status`'s `VmRSS:` line
/// (Linux-only — this whole environment is Linux, see repo `CLAUDE.md`).
/// See the module doc for why this is the fallback rather than a Cranelift-
/// side byte accessor.
fn rss_bytes() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: usize = rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .expect("parse VmRSS kB field");
            return kb * 1024;
        }
    }
    panic!("VmRSS not found in /proc/self/status");
}

/// ONE session machine, growing by `add_function` at checkpoints N in
/// {1, 16, 128}. Reports `functions_defined()` (the exact Cranelift compile
/// count — a real number, not a proxy) and RSS at each checkpoint.
#[test]
#[serial]
fn realm_module_growth_single_machine() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = table_with_c1();
            let dummy = build_value_fragment(0);
            let mut machine = JitEffectMachine::compile_session(&dummy, &table, 1 << 20)
                .expect("compile_session");

            let checkpoints = [1usize, 16, 128];
            let mut round = 0usize;
            let mut samples: Vec<(usize, u64, usize)> = Vec::new();
            for target in checkpoints {
                while round < target {
                    round += 1;
                    machine
                        .add_function(
                            &format!("frag_{round}"),
                            &build_value_fragment(round as i64),
                            &table,
                            &ExternalEnv::new(),
                        )
                        .unwrap_or_else(|e| panic!("add_function frag_{round}: {e}"));
                }
                let funcs = machine.functions_defined();
                let rss = rss_bytes();
                println!(
                    "[realm_module_growth] N={round} functions_defined={funcs} rss_bytes={rss}"
                );
                samples.push((round, funcs, rss));
            }

            // functions_defined must grow strictly with N (one dummy + N adds,
            // never fewer — nothing in add_function/CodegenPipeline ever
            // decrements it, matching the "no function removal" static
            // finding in the doc).
            for w in samples.windows(2) {
                assert!(
                    w[1].1 > w[0].1,
                    "functions_defined must grow strictly: {:?} -> {:?}",
                    w[0],
                    w[1]
                );
            }

            drop(machine);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Create-and-drop 32 whole session machines (~16 fragments each). Reports
/// RSS at baseline (before the loop), peak (max observed across the loop,
/// sampled after each machine is populated but before it drops), and after
/// the loop (all 32 machines dropped) — the reclamation-by-drop question a
/// cycle-scoped design lives or dies on: does dropping a machine give
/// Cranelift's code memory back, or does RSS keep climbing / never return
/// toward baseline?
///
/// A NO-GO finding here (RSS never comes back down) is a successful result
/// for this test, not a failure — it would corroborate the static finding
/// that `ArenaMemoryProvider::drop` leaks a finalized arena.
#[test]
#[serial]
fn realm_module_growth_create_drop_32_machines() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = table_with_c1();
            let baseline = rss_bytes();
            let mut peak = baseline;

            for m in 0..32 {
                let dummy = build_value_fragment(0);
                let mut machine = JitEffectMachine::compile_session(&dummy, &table, 1 << 16)
                    .expect("compile_session");
                for f in 0..16 {
                    machine
                        .add_function(
                            &format!("m{m}_frag_{f}"),
                            &build_value_fragment(f as i64),
                            &table,
                            &ExternalEnv::new(),
                        )
                        .unwrap_or_else(|e| panic!("add_function m{m}_frag_{f}: {e}"));
                }
                peak = peak.max(rss_bytes());
                drop(machine);
            }

            let after = rss_bytes();
            println!(
                "[realm_module_growth] baseline_bytes={baseline} peak_bytes={peak} \
                 after_32_drops_bytes={after}"
            );

            // Report only — no assertion on the direction of `after` relative
            // to `baseline`/`peak`. Per the task's own DONE criteria: a
            // NO-GO (RSS stays near peak, doesn't return toward baseline) is
            // a valid, successful measurement, not a test failure. The
            // findings doc interprets the printed numbers.
        })
        .unwrap()
        .join()
        .unwrap();
}
