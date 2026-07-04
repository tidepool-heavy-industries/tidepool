//! #336: a self-referential binding (`let x = x in x` — GHC emits this Core
//! for a genuine `<<loop>>`) must surface as a runtime error on BOTH engines,
//! never spin. Eval detects it via its `BlackHole` thunk state; these tests
//! pin the JIT to the same contract. The corpus fixture `thunk_blackhole.cbor`
//! covers the real-GHC shape; these synthetic forms cover the minimal one the
//! gc-pressure proptest generator produced (see #336).

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{env_from_datacon_table, eval, VecHeap};
use tidepool_repr::builder::TreeBuilder;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::{DataConTable, VarId};

fn self_letrec() -> tidepool_repr::CoreExpr {
    // LetRec { bindings: [(x, Var(x))], body: Var(x) }
    let mut b = TreeBuilder::new();
    let x = VarId(1);
    let rhs = b.push(CoreFrame::Var(x));
    let body = b.push(CoreFrame::Var(x));
    let root = b.push(CoreFrame::LetRec {
        bindings: vec![(x, rhs)],
        body,
    });
    let _ = root;
    b.build()
}

fn mutual_alias_letrec() -> tidepool_repr::CoreExpr {
    // LetRec { bindings: [(x, Var(y)), (y, Var(x))], body: Var(x) } — the
    // memoized indirections form an EVALUATED 2-cycle with no blackhole state
    // in it; only the follow-limit can catch it.
    let mut b = TreeBuilder::new();
    let x = VarId(1);
    let y = VarId(2);
    let rhs_x = b.push(CoreFrame::Var(y));
    let rhs_y = b.push(CoreFrame::Var(x));
    let body = b.push(CoreFrame::Var(x));
    b.push(CoreFrame::LetRec {
        bindings: vec![(x, rhs_x), (y, rhs_y)],
        body,
    });
    b.build()
}

#[test]
fn mutual_alias_letrec_errors_both_engines() {
    tidepool_testing::watchdog::arm();
    tidepool_testing::watchdog::begin("blackhole: let x = y; y = x in x");

    let expr = mutual_alias_letrec();
    let table = DataConTable::new();

    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let eval_res = eval(&expr, &env, &mut heap);
    assert!(eval_res.is_err(), "eval must reject the alias cycle");

    let jit_res = match JitEffectMachine::compile(&expr, &table, 1 << 20) {
        Ok(mut machine) => machine
            .run_pure()
            .map(|v| format!("{v:?}"))
            .map_err(|e| format!("{e:?}")),
        Err(e) => Err(format!("compile: {e:?}")),
    };
    assert!(
        jit_res.is_err(),
        "JIT must reject the alias cycle, got {jit_res:?}"
    );
}

#[test]
fn self_referential_letrec_errors_both_engines() {
    tidepool_testing::watchdog::arm();

    let expr = self_letrec();
    let table = DataConTable::new();

    // Eval: must error (BlackHole / unresolved), not spin.
    tidepool_testing::watchdog::begin("blackhole EVAL stage: let x = x in x");
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let eval_res = eval(&expr, &env, &mut heap);
    eprintln!("eval stage done: {:?}", eval_res.as_ref().map(|_| "Ok"));
    tidepool_testing::watchdog::begin("blackhole JIT stage: let x = x in x");
    assert!(
        eval_res.is_err(),
        "eval must reject let x = x in x, got {eval_res:?}"
    );

    // JIT: must error, not spin. Compile failure is acceptable too — any
    // terminating rejection satisfies the contract.
    let jit_res = match JitEffectMachine::compile(&expr, &table, 1 << 20) {
        Ok(mut machine) => {
            eprintln!("jit compile done");
            tidepool_testing::watchdog::begin("blackhole JIT RUN stage: let x = x in x");
            machine
                .run_pure()
                .map(|v| format!("{v:?}"))
                .map_err(|e| format!("{e:?}"))
        }
        Err(e) => Err(format!("compile: {e:?}")),
    };
    assert!(
        jit_res.is_err(),
        "JIT must reject let x = x in x, got {jit_res:?}"
    );
}
