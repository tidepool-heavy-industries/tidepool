//! Error-reporting PARITY guard for the Stage 2 walker removal.
//!
//! The let-binding error-deferral path was removed (`rhs_is_error_call_in_group`,
//! `emit_error_binding`, and the poison-closure interception sites): a non-trivial
//! error RHS such as `error "m"`, `raise#`, or `case error of {}` is now
//! thunkified by lazy-default and forced on demand. This pins that a forced
//! error-THUNK reports the SAME message and class as the old poison closure did —
//! proven equal at the time of removal, since `runtime_error_dynamic` forwards to
//! the same `runtime_error_with_msg` the message poison used. The
//! conditional-position lowering (collapse_frame's `error` in a case-alt body to
//! `EmitFrame::Raise`) is UNTOUCHED.
use tidepool_repr::{CoreExpr, CoreFrame, Literal, TreeBuilder, VarId};

// error sentinel: high byte 0x45 (ERROR_SENTINEL_TAG), low byte = kind (2 = UserError).
const SENTINEL_USERERROR: u64 = 0x4500_0000_0000_0002;

// Mirror of tidepool-mcp `FailureClass::classify_error_text` markers — a forced
// error binding must classify as the user "haskell-error" class, NOT a
// signal-crash (case trap / bad pointer / non-closure) or runtime-yield
// (overflow). Keeping the check local avoids a tidepool-mcp dev-dep.
fn is_haskell_error_class(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const SIGNAL: &[&str] = &[
        "jit signal:",
        "case trap",
        "bad pointer",
        "null function pointer",
        "application of non-closure",
        "forced type metadata",
    ];
    const YIELD: &[&str] = &[
        "stack overflow",
        "heap overflow",
        "unbounded recursion",
        "blackhole",
    ];
    !SIGNAL.iter().any(|m| lower.contains(m)) && !YIELD.iter().any(|m| lower.contains(m))
}

fn run(expr: CoreExpr) -> String {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let table = tidepool_testing::proptest::build_table_for_expr(&expr);
            match tidepool_codegen::jit_machine::JitEffectMachine::compile(&expr, &table, 1 << 20) {
                Ok(mut m) => match m.run_pure() {
                    Ok(v) => format!("UNEXPECTED-OK {v:?}"),
                    Err(e) => format!("{e}"),
                },
                Err(e) => format!("COMPILE-ERR {e:?}"),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

fn error_boom_rhs(b: &mut TreeBuilder) -> usize {
    let sent = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    let msg = b.push(CoreFrame::Lit(Literal::LitString(b"boom".to_vec())));
    b.push(CoreFrame::App {
        fun: sent,
        arg: msg,
    })
}

fn assert_clean_boom(name: &str, text: &str) {
    assert!(
        text.to_ascii_lowercase().contains("boom"),
        "{name}: error message must preserve \"boom\", got: {text}"
    );
    assert!(
        is_haskell_error_class(text),
        "{name}: forced error binding must classify as haskell-error, got: {text}"
    );
}

/// `let x = error "boom" in x` (LetNonRec) — forced → clean HaskellError "boom".
#[test]
fn letnonrec_error_binding_reports_message() {
    let x = VarId(1);
    let mut b = TreeBuilder::new();
    let rhs = error_boom_rhs(&mut b);
    let body = b.push(CoreFrame::Var(x));
    b.push(CoreFrame::LetNonRec {
        binder: x,
        rhs,
        body,
    });
    assert_clean_boom("letnonrec", &run(b.build()));
}

/// `letrec { x = error "boom" } in x` (LetRec simple binding) — same.
#[test]
fn letrec_error_binding_reports_message() {
    let x = VarId(1);
    let mut b = TreeBuilder::new();
    let rhs = error_boom_rhs(&mut b);
    let body = b.push(CoreFrame::Var(x));
    b.push(CoreFrame::LetRec {
        bindings: vec![(x, rhs)],
        body,
    });
    assert_clean_boom("letrec", &run(b.build()));
}

/// `error "boom"` at the root — the conditional/direct lowering path that was
/// KEPT. Must stay identical (the reference the let-binding thunks now match).
#[test]
fn direct_error_reports_message() {
    let mut b = TreeBuilder::new();
    let _ = error_boom_rhs(&mut b);
    assert_clean_boom("direct", &run(b.build()));
}

/// `let f = error in f "boom"` — exercises `poison_trampoline_lazy` directly.
///
/// When `error_sentinel` appears as a plain Var (not in App head), the emit
/// stores a lazy poison closure as `f`. The subsequent `App(Var(f), "boom")`
/// is an ordinary closure call; `poison_trampoline_lazy` fires at runtime,
/// calls `materialize_message`, and routes through `runtime_error_with_msg`.
#[test]
fn lazy_poison_applied_at_runtime_reports_message() {
    let f = VarId(1);
    let mut b = TreeBuilder::new();
    // RHS: bare error_sentinel var (NOT in App position — no static-msg extraction)
    let sent = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    // Body: apply f (the lazy poison) to "boom" at a separate App site
    let boom = b.push(CoreFrame::Lit(Literal::LitString(b"boom".to_vec())));
    let f_var = b.push(CoreFrame::Var(f));
    let body = b.push(CoreFrame::App {
        fun: f_var,
        arg: boom,
    });
    b.push(CoreFrame::LetNonRec {
        binder: f,
        rhs: sent,
        body,
    });
    assert_clean_boom("lazy-poison-applied", &run(b.build()));
}
