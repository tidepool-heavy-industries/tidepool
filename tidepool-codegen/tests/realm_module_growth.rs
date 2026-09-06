//! Measure compiled-function growth within one machine and across dropped machines.
//! Cranelift's SystemMemoryProvider retains finalized allocations on drop;
//! production does not call JITModule::free_memory. RSS is a noisy process-level
//! proxy because JITModule does not expose allocated-byte counters.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::DataConTable;

use serial_test::serial;

use crate::session_scaffold;
use crate::session_scaffold_value;
use session_scaffold::C1;
use session_scaffold_value::build_value_fragment;

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
/// (Linux-only). See the module doc for why this is the fallback rather than
/// a Cranelift-side byte accessor.
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
/// that `SystemMemoryProvider` retains finalized allocations on drop.
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

            // Report only — see the fn doc: a NO-GO reading here (RSS stays
            // near peak) is a valid measurement, not a test failure.
        })
        .unwrap()
        .join()
        .unwrap();
}
