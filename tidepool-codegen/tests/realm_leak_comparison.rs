//! Controlled two-arm measurement for the realm-spike leak-comparison lane.
//! Builds on the sibling realm-lifetime finding: dropping a
//! `JitEffectMachine` does not reclaim its compiled code, because
//! `cranelift-jit` 0.129.1's `ArenaMemoryProvider::drop` deliberately leaks
//! once any segment has been finalized (true of every real machine).
//!
//! That finding was measured with a fixed machine count (32). This test asks
//! the question the sibling lane's task explicitly left open: for the SAME
//! total compiled-function workload (512 fragments), does spreading that
//! work across ONE machine instead of 32 separate machines change how much
//! is retained after everything drops? Both arms run back-to-back in one
//! process from one shared baseline, so the two retained deltas are directly
//! comparable — no cross-test baseline drift.
//!
//! ARM A: 32 session machines, 16 fragments each (today's architecture — one
//! machine per answerer session), each machine dropped before the next is
//! created.
//! ARM B: ONE session machine, 512 fragments (the unified/realm shape),
//! dropped once at the end.
//!
//! Fragment bodies are identical between arms: global fragment index `n` in
//! {0..512} is `build_value_fragment(n)` in both arms, so the only variable
//! is how many machines the same 512 compiled bodies are spread across.
//!
//! Also reports `VmSize` (virtual) alongside `VmRSS` (resident) in arm A, to
//! characterize the 256 MiB `ArenaMemoryProvider` reservation
//! (`ArenaMemoryProvider::new_with_size` in `src/pipeline.rs`) as reserved-virtual vs
//! committed-physical — see the findings doc for the reading of these
//! numbers against the vendored `cranelift-jit` source.

use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::datacon::DataCon;
use tidepool_repr::DataConTable;

use serial_test::serial;

use crate::session_scaffold;
use crate::session_scaffold_value;
use session_scaffold::C1;
use session_scaffold_value::build_value_fragment;

const MACHINES: usize = 32;
const FRAGMENTS_PER_MACHINE: usize = 16;
const TOTAL_FRAGMENTS: usize = MACHINES * FRAGMENTS_PER_MACHINE; // 512

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
/// (Linux-only).
fn rss_bytes() -> usize {
    proc_status_kb_field("VmRSS:") * 1024
}

/// Process virtual memory size in bytes, read from `/proc/self/status`'s
/// `VmSize:` line. This is the address-space-reservation side of the
/// arena-provider question (VmRSS is the physically-committed side).
fn vmsize_bytes() -> usize {
    proc_status_kb_field("VmSize:") * 1024
}

fn proc_status_kb_field(prefix: &str) -> usize {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .unwrap_or_else(|e| panic!("parse {prefix} field {rest:?}: {e}"));
        }
    }
    panic!("{prefix} not found in /proc/self/status");
}

/// One baseline, two arms, same 512-fragment workload. Reports RSS + VmSize
/// at 5 checkpoints: baseline, after arm A's last machine is populated
/// (pre-drop), after arm A's last machine drops (all 32 machines are now
/// dropped — the 31 before it were already dropped mid-loop), after arm B's
/// one machine is populated with all 512 fragments (pre-drop), and after arm
/// B's machine drops.
///
/// No assertion on RSS/VmSize direction anywhere — this is a measurement,
/// not a gate, so it cannot go red on a noisy box (per the sibling lane's
/// precedent). Assertions are structural only: exact function counts.
#[test]
#[serial]
fn realm_leak_comparison_one_vs_many_machines() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let table = table_with_c1();

            let baseline_rss = rss_bytes();
            let baseline_vsz = vmsize_bytes();

            // ---- ARM A: 32 machines x 16 fragments, drop-before-next-create ----
            let mut arm_a_last_functions_defined = 0u64;
            let mut after_arm_a_rss = baseline_rss;
            let mut after_arm_a_vsz = baseline_vsz;
            let mut after_arm_a_drop_rss = baseline_rss;
            let mut after_arm_a_drop_vsz = baseline_vsz;

            for m in 0..MACHINES {
                let dummy = build_value_fragment(0);
                let mut machine = JitEffectMachine::compile_session(&dummy, &table, 1 << 16)
                    .unwrap_or_else(|e| panic!("arm A compile_session m{m}: {e}"));
                for f in 0..FRAGMENTS_PER_MACHINE {
                    let global = m * FRAGMENTS_PER_MACHINE + f;
                    machine
                        .add_function(
                            &format!("a_m{m}_f{f}"),
                            &build_value_fragment(global as i64),
                            &table,
                            &ExternalEnv::new(),
                        )
                        .unwrap_or_else(|e| panic!("arm A add_function m{m}_f{f}: {e}"));
                }
                if m == MACHINES - 1 {
                    arm_a_last_functions_defined = machine.functions_defined();
                    after_arm_a_rss = rss_bytes();
                    after_arm_a_vsz = vmsize_bytes();
                }
                drop(machine);
                if m == MACHINES - 1 {
                    after_arm_a_drop_rss = rss_bytes();
                    after_arm_a_drop_vsz = vmsize_bytes();
                }
            }

            // ---- ARM B: ONE machine, 512 fragments, identical bodies ----
            let dummy = build_value_fragment(0);
            let mut machine = JitEffectMachine::compile_session(&dummy, &table, 1 << 20)
                .expect("arm B compile_session");
            for n in 0..TOTAL_FRAGMENTS {
                machine
                    .add_function(
                        &format!("b_f{n}"),
                        &build_value_fragment(n as i64),
                        &table,
                        &ExternalEnv::new(),
                    )
                    .unwrap_or_else(|e| panic!("arm B add_function f{n}: {e}"));
            }
            let arm_b_functions_defined = machine.functions_defined();
            let after_arm_b_rss = rss_bytes();
            let after_arm_b_vsz = vmsize_bytes();
            drop(machine);
            let after_arm_b_drop_rss = rss_bytes();
            let after_arm_b_drop_vsz = vmsize_bytes();

            println!(
                "[realm_leak_comparison] baseline rss_bytes={baseline_rss} vmsize_bytes={baseline_vsz}"
            );
            println!(
                "[realm_leak_comparison] after_arm_a (32nd machine populated, pre-drop) \
                 rss_bytes={after_arm_a_rss} vmsize_bytes={after_arm_a_vsz} \
                 last_machine_functions_defined={arm_a_last_functions_defined}"
            );
            println!(
                "[realm_leak_comparison] after_arm_a_drop (all 32 machines dropped) \
                 rss_bytes={after_arm_a_drop_rss} vmsize_bytes={after_arm_a_drop_vsz}"
            );
            println!(
                "[realm_leak_comparison] after_arm_b (1 machine, 512 fragments, pre-drop) \
                 rss_bytes={after_arm_b_rss} vmsize_bytes={after_arm_b_vsz} \
                 functions_defined={arm_b_functions_defined}"
            );
            println!(
                "[realm_leak_comparison] after_arm_b_drop (machine dropped) \
                 rss_bytes={after_arm_b_drop_rss} vmsize_bytes={after_arm_b_drop_vsz}"
            );

            let arm_a_retained_rss = after_arm_a_drop_rss as i64 - baseline_rss as i64;
            let arm_b_retained_rss = after_arm_b_drop_rss as i64 - after_arm_a_drop_rss as i64;
            let arm_a_retained_vsz = after_arm_a_drop_vsz as i64 - baseline_vsz as i64;
            let arm_b_retained_vsz = after_arm_b_drop_vsz as i64 - after_arm_a_drop_vsz as i64;
            println!(
                "[realm_leak_comparison] retained_delta arm_a_rss={arm_a_retained_rss} \
                 arm_b_rss={arm_b_retained_rss} arm_a_vmsize={arm_a_retained_vsz} \
                 arm_b_vmsize={arm_b_retained_vsz}"
            );

            // Structural facts only — these must hold regardless of RSS noise.
            // Arm A: each of the 32 machines compiles 1 dummy + 16 fragments = 17.
            assert_eq!(
                arm_a_last_functions_defined, 17,
                "arm A's last machine must report exactly 1 dummy + 16 fragments compiled"
            );
            // Arm B: 1 dummy + 512 fragments = 513, all on one machine.
            assert_eq!(
                arm_b_functions_defined,
                (TOTAL_FRAGMENTS + 1) as u64,
                "arm B's single machine must report 1 dummy + {TOTAL_FRAGMENTS} fragments compiled"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
