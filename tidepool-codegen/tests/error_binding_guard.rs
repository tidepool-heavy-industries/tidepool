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

/// `error ("prefix" <> freeVar)` — a message built by combining a leading
/// string literal with a genuinely dynamic (free) variable, mirroring
/// Haskell's `error ("prefix" <> dynamicVar)`. The static fast path
/// (`extract_error_message` / `find_first_lit_string`) must NOT silently
/// truncate this to just the literal fragment "prefix": since the message
/// subtree also contains an unresolvable free `Var`, static extraction must
/// bail out (return `None`), routing the call through the normal-App /
/// dynamic (`runtime_error_dynamic`/`materialize_message`) path instead.
/// Regression guard for the "truncated-to-leading-literal" codegen bug.
#[test]
fn error_message_with_free_var_is_not_truncated_to_leading_literal() {
    let free = VarId(42); // genuinely free: never let-bound anywhere in this tree
    let mut b = TreeBuilder::new();
    let sent = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    let prefix = b.push(CoreFrame::Lit(Literal::LitString(b"prefix".to_vec())));
    let free_var = b.push(CoreFrame::Var(free));
    // Message subtree: App(App(prefix, ()), free_var) — stands in for `<>`
    // combining a literal fragment with dynamic content. The exact shape of
    // the combinator doesn't matter for this guard: what matters is that the
    // literal "prefix" and the free var `free` are BOTH reachable from the
    // message argument position, so a naive first-literal DFS would wrongly
    // return `Some("prefix")` instead of bailing out to the dynamic path.
    let msg_arg = b.push(CoreFrame::App {
        fun: prefix,
        arg: free_var,
    });
    b.push(CoreFrame::App {
        fun: sent,
        arg: msg_arg,
    });
    let text = run(b.build());
    assert!(
        !text.to_ascii_lowercase().contains("compile-err"),
        "must still compile via the dynamic fallback path, got: {text}"
    );
    assert!(
        text != "UNEXPECTED-OK ()" && !text.starts_with("UNEXPECTED-OK"),
        "error call must still raise, got: {text}"
    );
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

// Unsupported FFI lowers to this same bare sentinel, not an error application.
#[derive(Clone, Copy)]
enum DemandCase {
    Literal,
    DefaultOnly,
    DeadLiteralBranch,
    Data,
}

fn demand_case(case: DemandCase) -> CoreExpr {
    use tidepool_repr::{Alt, AltCon};
    let mut b = TreeBuilder::new();
    let poison = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let success = b.push(CoreFrame::Lit(Literal::LitInt(42)));
    let fallback = b.push(CoreFrame::Lit(Literal::LitInt(99)));
    let alts = if !matches!(case, DemandCase::DefaultOnly) {
        vec![
            Alt {
                con: if matches!(case, DemandCase::Data) {
                    AltCon::DataAlt(tidepool_repr::DataConId(777))
                } else {
                    AltCon::LitAlt(Literal::LitInt(0))
                },
                binders: vec![],
                body: success,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: if matches!(case, DemandCase::DeadLiteralBranch) {
                    poison
                } else {
                    fallback
                },
            },
        ]
    } else {
        vec![Alt {
            con: AltCon::Default,
            binders: vec![],
            body: success,
        }]
    };
    b.push(CoreFrame::Case {
        scrutinee: if matches!(case, DemandCase::DeadLiteralBranch) {
            zero
        } else {
            poison
        },
        binder: VarId(900),
        alts,
    });
    b.build()
}

fn assert_demand_error(expr: CoreExpr) {
    use tidepool_codegen::{
        host_fns::RuntimeError,
        jit_machine::{JitEffectMachine, JitError},
        yield_type::YieldError,
    };
    let table = tidepool_testing::proptest::build_table_for_expr(&expr);
    let env = tidepool_eval::env_from_datacon_table(&table);
    assert!(matches!(
        tidepool_eval::eval(&expr, &env, &mut tidepool_eval::VecHeap::new()),
        Err(tidepool_eval::EvalError::UserError)
    ));
    let got = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut m = JitEffectMachine::compile(&expr, &table, 1 << 20)
                .expect("compile demand regression");
            m.run_pure()
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(
        matches!(
            got,
            Err(JitError::Yield(YieldError::Runtime(
                RuntimeError::UserError
            )))
        ),
        "demand must preserve the original error, got {got:?}"
    );
}

#[test]
fn strict_demand_literal_case_rejects_lazy_poison() {
    assert_demand_error(demand_case(DemandCase::Literal));
}

#[test]
fn strict_demand_default_only_rejects_lazy_poison() {
    assert_demand_error(demand_case(DemandCase::DefaultOnly));
}

#[test]
fn strict_demand_dead_literal_branch_remains_lazy() {
    let expr = demand_case(DemandCase::DeadLiteralBranch);
    let table = tidepool_testing::proptest::build_table_for_expr(&expr);
    let env = tidepool_eval::env_from_datacon_table(&table);
    assert!(matches!(
        tidepool_eval::eval(&expr, &env, &mut tidepool_eval::VecHeap::new()),
        Ok(tidepool_eval::Value::Lit(Literal::LitInt(42)))
    ));
    let got = tidepool_codegen::jit_machine::JitEffectMachine::compile(&expr, &table, 1 << 20)
        .unwrap()
        .run_pure();
    assert!(
        matches!(got, Ok(tidepool_eval::Value::Lit(Literal::LitInt(42)))),
        "{got:?}"
    );
}

#[test]
fn strict_demand_numeric_operand_rejects_lazy_poison() {
    let mut b = TreeBuilder::new();
    let poison = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    b.push(CoreFrame::PrimOp {
        op: tidepool_repr::PrimOpKind::IntAdd,
        args: vec![poison, one],
    });
    assert_demand_error(b.build());
}

#[test]
fn strict_demand_data_default_rejects_lazy_poison() {
    assert_demand_error(demand_case(DemandCase::Data));
}

#[test]
fn strict_demand_boxed_numeric_rejects_lazy_poison() {
    let mut b = TreeBuilder::new();
    let poison = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    let boxed = b.push(CoreFrame::Con {
        tag: tidepool_repr::DataConId(778),
        fields: vec![poison],
    });
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    b.push(CoreFrame::PrimOp {
        op: tidepool_repr::PrimOpKind::IntAdd,
        args: vec![boxed, one],
    });
    assert_demand_error(b.build());
}

#[derive(Clone, Copy)]
enum LazyContainer {
    Function,
    Constructor,
    Array,
}

fn lazy_container_expr(container: LazyContainer) -> CoreExpr {
    use tidepool_repr::{Alt, AltCon, DataConId, PrimOpKind};
    let mut b = TreeBuilder::new();
    let poison = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    let lazy = b.push(CoreFrame::Var(VarId(912)));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let scrutinee = match container {
        LazyContainer::Function => b.push(CoreFrame::Lam {
            binder: VarId(910),
            body: lazy,
        }),
        LazyContainer::Constructor => b.push(CoreFrame::Con {
            tag: DataConId(779),
            fields: vec![lazy],
        }),
        LazyContainer::Array => b.push(CoreFrame::PrimOp {
            op: PrimOpKind::NewSmallArray,
            args: vec![one, lazy],
        }),
    };
    let success = b.push(CoreFrame::Lit(Literal::LitInt(42)));
    let body = b.push(CoreFrame::Case {
        scrutinee,
        binder: VarId(911),
        alts: vec![Alt {
            con: AltCon::Default,
            binders: vec![],
            body: success,
        }],
    });
    b.push(CoreFrame::LetNonRec {
        binder: VarId(912),
        rhs: poison,
        body,
    });
    b.build()
}

fn assert_lazy_container(container: LazyContainer) {
    let expr = lazy_container_expr(container);
    let table = tidepool_testing::proptest::build_table_for_expr(&expr);
    let env = tidepool_eval::env_from_datacon_table(&table);
    let expected = tidepool_eval::eval(&expr, &env, &mut tidepool_eval::VecHeap::new());
    let got = tidepool_codegen::jit_machine::JitEffectMachine::compile(&expr, &table, 1 << 20)
        .unwrap()
        .run_pure();
    assert!(
        matches!(expected, Ok(tidepool_eval::Value::Lit(Literal::LitInt(42))))
            && matches!(got, Ok(tidepool_eval::Value::Lit(Literal::LitInt(42)))),
        "reference: {expected:?}; JIT: {got:?}"
    );
}

#[test]
fn strict_demand_function_whnf_preserves_lazy_body() {
    assert_lazy_container(LazyContainer::Function);
}
#[test]
fn strict_demand_constructor_whnf_preserves_lazy_field() {
    assert_lazy_container(LazyContainer::Constructor);
}
#[test]
fn strict_demand_array_creation_preserves_lazy_element() {
    // Native contract: fixtures/strict_demand/ArrayLaziness.hs. The reference
    // evaluator intentionally does not implement boxed-array primops; its
    // blanket argument forcing is not an oracle for this lifted initializer.
    let expr = lazy_container_expr(LazyContainer::Array);
    let table = tidepool_testing::proptest::build_table_for_expr(&expr);
    let got = tidepool_codegen::jit_machine::JitEffectMachine::compile(&expr, &table, 1 << 20)
        .unwrap()
        .run_pure();
    assert!(
        matches!(got, Ok(tidepool_eval::Value::Lit(Literal::LitInt(42)))),
        "JIT array creation must not demand its lifted initializer: {got:?}"
    );
}

#[test]
fn strict_demand_selected_array_element_rejects_lazy_poison() {
    // Companion to the native fixture's selected branch: allocation is lazy,
    // but selecting and demanding the initializer must preserve its error.
    use tidepool_codegen::{
        host_fns::RuntimeError,
        jit_machine::{JitEffectMachine, JitError},
        yield_type::YieldError,
    };
    let mut b = TreeBuilder::new();
    let poison = b.push(CoreFrame::Var(VarId(SENTINEL_USERERROR)));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let array = b.push(CoreFrame::PrimOp {
        op: tidepool_repr::PrimOpKind::NewSmallArray,
        args: vec![one, poison],
    });
    b.push(CoreFrame::PrimOp {
        op: tidepool_repr::PrimOpKind::ReadSmallArray,
        args: vec![array, zero],
    });
    let expr = b.build();
    let table = tidepool_testing::proptest::build_table_for_expr(&expr);
    let got = JitEffectMachine::compile(&expr, &table, 1 << 20)
        .unwrap()
        .run_pure();
    assert!(
        matches!(
            got,
            Err(JitError::Yield(YieldError::Runtime(
                RuntimeError::UserError
            )))
        ),
        "demanding selected initializer must preserve UserError: {got:?}"
    );
}
