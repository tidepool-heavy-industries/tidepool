//! W6 — Differential property testing of the full Haskell -> Core -> JIT pipeline.
//!
//! This layer is *unreachable* by the Rust-side IR generators (proptest_jit_vs_eval
//! et al.) because it exercises `Translate.hs` and GHC's -O2 Core shapes: joinrec,
//! tagToEnum#, occurrence analysis, env save/restore, strict-let error hoisting, and
//! the other 17 documented translation gotchas (see CLAUDE.md).
//!
//! Strategy: generate small TOTAL Haskell source programs (an AST in Rust, pretty
//! printed), evaluate them with a Rust *reference interpreter* (the oracle, total by
//! construction), and compare against `compile_and_run_pure(...).to_json()`. Every
//! generated program is total by construction, so every mismatch is a real bug.
//!
//! Oracles:
//!   B1  JIT result != reference interpreter
//!   B2  compile or runtime error on a valid total program
//!   B3  fatal signal (SIGILL/SIGSEGV) — detected in the subprocess worker
//!   B4  compile-twice nondeterminism, or lazy-A/B (TIDEPOOL_LAZY_RESULTS) divergence
//!
//! Committed properties are capped at 30 cases (GHC compile ~1-2s/case). The real
//! hunting lives in `long_haul` (#[ignore]d, 300 cases) which is run by hand during
//! the authoring session. Run with `--test-threads=1` (the compile cache is a shared
//! filesystem resource).

#![allow(clippy::needless_range_loop)]

use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// PRNG — splitmix64. Self-contained so the generator needs no extra dep, and
// the proptest input is a single u64 seed (recorded verbatim in the regression
// file, which is all we need for reproduction — shrinking is done by hand).
// ============================================================================

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x123456789abcdef)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, n).
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    /// Uniform inclusive integer in [lo, hi].
    fn ri(&mut self, lo: i64, hi: i64) -> i64 {
        let span = (hi - lo + 1).max(1) as u64;
        lo + self.below(span) as i64
    }
    /// True with probability num/den.
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
    fn pick(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }
}

// ============================================================================
// AST. Total by construction: no division, no head/last on possibly-empty, no
// error/undefined/read. Result types are always ground: Int, Text, [Int],
// (Int, Text). [Text] appears only as an intermediate.
// ============================================================================

#[derive(Clone, Copy, PartialEq, Debug)]
enum Ty {
    Int,
    Text,
    ListInt,
    ListText,
    Pair, // (Int, Text)
}

/// Closed Int -> Int functions (lambda bodies for map/iterate). No free vars.
#[derive(Clone, Copy, Debug)]
enum IntFn {
    AddK(i64),
    SubK(i64),
    MulK(i64),
    Neg,
    AbsF,
    Id,
}

/// Closed Int -> Bool predicates (for filter).
#[derive(Clone, Copy, Debug)]
enum IntPred {
    Even,
    Odd,
    GtK(i64),
    LtK(i64),
    GeK(i64),
    NeqK(i64),
}

/// Closed Int -> [Int] functions (for concatMap).
#[derive(Clone, Copy, Debug)]
enum ListFn {
    Single, // \x -> [x]
    Dup,    // \x -> [x, x]
    Empty,  // \x -> []
    PairUp, // \x -> [x, x + 1]
}

#[derive(Clone, Copy, Debug)]
enum ArithOp {
    Add,
    Sub,
    Mul,
}

#[derive(Clone, Copy, Debug)]
enum CmpOp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

#[derive(Clone, Debug)]
enum HExpr {
    // --- Int ---
    IntLit(i64),
    Arith(ArithOp, Box<HExpr>, Box<HExpr>),
    Length(Box<HExpr>), // length of a list expr
    Sum(Box<HExpr>),    // sum of [Int]
    Product(Box<HExpr>),
    FoldlAdd(i64, Box<HExpr>), // foldl' (+) k xs
    NegateE(Box<HExpr>),       // negate via abs'/0-x at value level: 0 - e
    IfInt(Box<HExpr>, Box<HExpr>, Box<HExpr>),
    WhereGo {
        start: i64,
        op: ArithOp,
        with_sig: bool,
    },
    CaseOfCase(Box<HExpr>, Box<HExpr>, Box<HExpr>), // scrutinee, then-branch, else-branch
    Guard {
        arg: Box<HExpr>,
        a: i64,
        c: i64,
        b1: Box<HExpr>,
        b2: Box<HExpr>,
        b3: Box<HExpr>,
        with_sig: bool,
    },
    LetShadowInt {
        v0: u32,
        va: u32,
        e1: Box<HExpr>,
        e2: Box<HExpr>, // may reference outer v0
        e3: Box<HExpr>, // inner (shadowing) rhs; closed wrt v0/va
    },

    // --- Bool (conditions only) ---
    Cmp(CmpOp, Box<HExpr>, Box<HExpr>),
    EvenE(Box<HExpr>),
    OddE(Box<HExpr>),
    AndE(Box<HExpr>, Box<HExpr>),
    OrE(Box<HExpr>, Box<HExpr>),
    NotE(Box<HExpr>),

    // --- Text ---
    StrLit(String),
    Append(Box<HExpr>, Box<HExpr>),
    ShowInt(Box<HExpr>),
    ToUpper(Box<HExpr>),
    ToLower(Box<HExpr>),
    Strip(Box<HExpr>),
    TReverse(Box<HExpr>),
    Unwords(Box<HExpr>), // unwords [Text]
    IfText(Box<HExpr>, Box<HExpr>, Box<HExpr>),

    // --- [Int] ---
    ListIntLit(Vec<i64>),
    MapInt(IntFn, Box<HExpr>),
    FilterInt(IntPred, Box<HExpr>),
    TakeI(i64, Box<HExpr>),
    DropI(i64, Box<HExpr>),
    ReverseI(Box<HExpr>),
    ConcatMapI(ListFn, Box<HExpr>),
    EnumFromTo(i64, i64),
    TakeIterate(i64, IntFn, i64), // take n (iterate f start)
    AppendI(Box<HExpr>, Box<HExpr>),
    IfListInt(Box<HExpr>, Box<HExpr>, Box<HExpr>),

    // --- [Text] ---
    ListTextLit(Vec<String>),
    MapShow(Box<HExpr>), // map show [Int]
    Words(Box<HExpr>),
    MapToUpper(Box<HExpr>),
    FilterPrefix(String, Box<HExpr>),
    TakeT(i64, Box<HExpr>),

    // --- (Int, Text) ---
    PairLit(Box<HExpr>, Box<HExpr>),

    // --- variables ---
    Var(u32),
}

// ============================================================================
// Generator. Type-directed; `budget` is a depth budget that counts down.
// `scope` carries in-scope Int variables (most translation gotchas are
// Int-shaped). `next_var` mints fresh variable ids.
// ============================================================================

struct Gen {
    rng: Rng,
    next_var: u32,
    /// Per-construct enable flags. A construct that reliably triggers a known
    /// bug is disabled here so the committed/live generator stays GREEN; the
    /// bug is captured separately as an #[ignore]d repro. See findings doc.
    enable_wh001_unsigned_go: bool,
    /// When set (env PIPELINE_FORCE_UNSIGNED=1, hand-run hunting only), every
    /// `WhereGo`/`Guard` helper omits its type signature — maximally exercises
    /// the Integer-defaulting trap (gotcha #3). Off for committed runs.
    force_unsigned: bool,
}

impl Gen {
    fn new(seed: u64) -> Self {
        Gen {
            rng: Rng::new(seed),
            next_var: 0,
            enable_wh001_unsigned_go: true,
            force_unsigned: std::env::var("PIPELINE_FORCE_UNSIGNED").as_deref() == Ok("1"),
        }
    }

    fn fresh(&mut self) -> u32 {
        let v = self.next_var;
        self.next_var += 1;
        v
    }

    fn safe_str(&mut self, max_len: usize) -> String {
        // lowercase letters, digits, spaces; occasionally a newline. No quote
        // or backslash. Total/round-trippable.
        const CS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789   ";
        let len = self.rng.below(max_len as u64 + 1) as usize;
        let mut s = String::new();
        for _ in 0..len {
            if self.rng.chance(1, 12) {
                s.push('\n');
            } else {
                let c = CS[self.rng.pick(CS.len())] as char;
                s.push(c);
            }
        }
        s
    }

    fn int_fn(&mut self) -> IntFn {
        match self.rng.pick(6) {
            0 => IntFn::AddK(self.rng.ri(-5, 6)),
            1 => IntFn::SubK(self.rng.ri(-5, 6)),
            2 => IntFn::MulK(self.rng.ri(-3, 4)),
            3 => IntFn::Neg,
            4 => IntFn::AbsF,
            _ => IntFn::Id,
        }
    }

    fn int_pred(&mut self) -> IntPred {
        match self.rng.pick(6) {
            0 => IntPred::Even,
            1 => IntPred::Odd,
            2 => IntPred::GtK(self.rng.ri(-5, 5)),
            3 => IntPred::LtK(self.rng.ri(-5, 5)),
            4 => IntPred::GeK(self.rng.ri(-5, 5)),
            _ => IntPred::NeqK(self.rng.ri(-5, 5)),
        }
    }

    fn list_fn(&mut self) -> ListFn {
        match self.rng.pick(4) {
            0 => ListFn::Single,
            1 => ListFn::Dup,
            2 => ListFn::Empty,
            _ => ListFn::PairUp,
        }
    }

    fn arith_op(&mut self) -> ArithOp {
        match self.rng.pick(3) {
            0 => ArithOp::Add,
            1 => ArithOp::Sub,
            _ => ArithOp::Mul,
        }
    }

    fn cmp_op(&mut self) -> CmpOp {
        match self.rng.pick(6) {
            0 => CmpOp::Lt,
            1 => CmpOp::Gt,
            2 => CmpOp::Le,
            3 => CmpOp::Ge,
            4 => CmpOp::Eq,
            _ => CmpOp::Ne,
        }
    }

    /// An in-scope Int variable, if any.
    fn scope_int_var(&mut self, scope: &[(u32, Ty)]) -> Option<u32> {
        let ints: Vec<u32> = scope
            .iter()
            .filter(|(_, t)| *t == Ty::Int)
            .map(|(v, _)| *v)
            .collect();
        if ints.is_empty() {
            None
        } else {
            Some(ints[self.rng.pick(ints.len())])
        }
    }

    fn gen(&mut self, ty: Ty, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        match ty {
            Ty::Int => self.gen_int(budget, scope),
            Ty::Text => self.gen_text(budget, scope),
            Ty::ListInt => self.gen_list_int(budget, scope),
            Ty::ListText => self.gen_list_text(budget, scope),
            Ty::Pair => HExpr::PairLit(
                Box::new(self.gen_int(budget.saturating_sub(1), scope)),
                Box::new(self.gen_text(budget.saturating_sub(1), scope)),
            ),
        }
    }

    fn gen_int(&mut self, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        // Leaf
        if budget == 0 {
            if let Some(v) = self.scope_int_var(scope) {
                if self.rng.chance(1, 2) {
                    return HExpr::Var(v);
                }
            }
            return HExpr::IntLit(self.rng.ri(-20, 40));
        }
        let b = budget - 1;
        // >=40% of the time, reach for a join-point / recursion / shadowing shape.
        if self.rng.chance(1, 2) {
            match self.rng.pick(4) {
                0 => {
                    let with_sig = if self.force_unsigned {
                        false
                    } else {
                        !self.enable_wh001_unsigned_go || self.rng.chance(4, 5)
                    };
                    return HExpr::WhereGo {
                        start: self.rng.ri(0, 40),
                        op: self.arith_op(),
                        with_sig,
                    };
                }
                1 => {
                    return HExpr::CaseOfCase(
                        Box::new(self.gen_int(b, scope)),
                        Box::new(self.gen_int(b, scope)),
                        Box::new(self.gen_int(b, scope)),
                    );
                }
                2 => {
                    let with_sig = if self.force_unsigned {
                        false
                    } else {
                        self.rng.chance(4, 5)
                    };
                    return HExpr::Guard {
                        arg: Box::new(self.gen_int(b, scope)),
                        a: self.rng.ri(-5, 10),
                        c: self.rng.ri(-10, 5),
                        b1: Box::new(self.gen_int(b, scope)),
                        b2: Box::new(self.gen_int(b, scope)),
                        b3: Box::new(self.gen_int(b, scope)),
                        with_sig,
                    };
                }
                _ => return self.gen_let_shadow(b, scope),
            }
        }
        match self.rng.pick(8) {
            0 => HExpr::IntLit(self.rng.ri(-50, 80)),
            1 => HExpr::Arith(
                self.arith_op(),
                Box::new(self.gen_int(b, scope)),
                Box::new(self.gen_int(b, scope)),
            ),
            2 => HExpr::Length(Box::new(self.gen_any_list(b, scope))),
            3 => HExpr::Sum(Box::new(self.gen_list_int(b, scope))),
            4 => HExpr::Product(Box::new(self.gen_list_int(b, scope))),
            5 => HExpr::FoldlAdd(self.rng.ri(-3, 5), Box::new(self.gen_list_int(b, scope))),
            6 => HExpr::IfInt(
                Box::new(self.gen_bool(b, scope)),
                Box::new(self.gen_int(b, scope)),
                Box::new(self.gen_int(b, scope)),
            ),
            _ => HExpr::NegateE(Box::new(self.gen_int(b, scope))),
        }
    }

    fn gen_let_shadow(&mut self, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        let b = budget.saturating_sub(1);
        let v0 = self.fresh();
        let va = self.fresh();
        let e1 = self.gen_int(b, scope);
        // e2 may reference outer v0 (it is lexically in scope).
        scope.push((v0, Ty::Int));
        let e2 = self.gen_int(b, scope);
        scope.pop();
        // e3 is the inner (shadowing) rhs — generated WITHOUT v0/va in scope so
        // it never self-references (Haskell `let` is recursive → would loop).
        let e3 = self.gen_int(b, scope);
        HExpr::LetShadowInt {
            v0,
            va,
            e1: Box::new(e1),
            e2: Box::new(e2),
            e3: Box::new(e3),
        }
    }

    fn gen_bool(&mut self, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        if budget == 0 {
            return HExpr::Cmp(
                self.cmp_op(),
                Box::new(self.gen_int(0, scope)),
                Box::new(self.gen_int(0, scope)),
            );
        }
        let b = budget - 1;
        match self.rng.pick(6) {
            0 => HExpr::Cmp(
                self.cmp_op(),
                Box::new(self.gen_int(b, scope)),
                Box::new(self.gen_int(b, scope)),
            ),
            1 => HExpr::EvenE(Box::new(self.gen_int(b, scope))),
            2 => HExpr::OddE(Box::new(self.gen_int(b, scope))),
            3 => HExpr::AndE(
                Box::new(self.gen_bool(b, scope)),
                Box::new(self.gen_bool(b, scope)),
            ),
            4 => HExpr::OrE(
                Box::new(self.gen_bool(b, scope)),
                Box::new(self.gen_bool(b, scope)),
            ),
            _ => HExpr::NotE(Box::new(self.gen_bool(b, scope))),
        }
    }

    fn gen_text(&mut self, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        if budget == 0 {
            return HExpr::StrLit(self.safe_str(8));
        }
        let b = budget - 1;
        match self.rng.pick(9) {
            0 => HExpr::StrLit(self.safe_str(10)),
            1 => HExpr::Append(
                Box::new(self.gen_text(b, scope)),
                Box::new(self.gen_text(b, scope)),
            ),
            2 => HExpr::ShowInt(Box::new(self.gen_int(b, scope))),
            3 => HExpr::ToUpper(Box::new(self.gen_text(b, scope))),
            4 => HExpr::ToLower(Box::new(self.gen_text(b, scope))),
            5 => HExpr::Strip(Box::new(self.gen_text(b, scope))),
            6 => HExpr::TReverse(Box::new(self.gen_text(b, scope))),
            7 => HExpr::Unwords(Box::new(self.gen_list_text(b, scope))),
            _ => HExpr::IfText(
                Box::new(self.gen_bool(b, scope)),
                Box::new(self.gen_text(b, scope)),
                Box::new(self.gen_text(b, scope)),
            ),
        }
    }

    fn gen_any_list(&mut self, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        if self.rng.chance(1, 2) {
            self.gen_list_int(budget, scope)
        } else {
            self.gen_list_text(budget, scope)
        }
    }

    fn gen_list_int(&mut self, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        if budget == 0 {
            let len = self.rng.below(8) as usize;
            let v: Vec<i64> = (0..len).map(|_| self.rng.ri(-15, 25)).collect();
            return HExpr::ListIntLit(v);
        }
        let b = budget - 1;
        match self.rng.pick(11) {
            0 => {
                let len = self.rng.below(13) as usize;
                let v: Vec<i64> = (0..len).map(|_| self.rng.ri(-20, 40)).collect();
                HExpr::ListIntLit(v)
            }
            1 => HExpr::MapInt(self.int_fn(), Box::new(self.gen_list_int(b, scope))),
            2 => HExpr::FilterInt(self.int_pred(), Box::new(self.gen_list_int(b, scope))),
            3 => HExpr::TakeI(self.rng.ri(0, 14), Box::new(self.gen_list_int(b, scope))),
            4 => HExpr::DropI(self.rng.ri(0, 14), Box::new(self.gen_list_int(b, scope))),
            5 => HExpr::ReverseI(Box::new(self.gen_list_int(b, scope))),
            6 => HExpr::ConcatMapI(self.list_fn(), Box::new(self.gen_list_int(b, scope))),
            7 => {
                let a = self.rng.ri(-8, 12);
                let len = self.rng.ri(0, 14);
                HExpr::EnumFromTo(a, a + len - 1)
            }
            8 => HExpr::TakeIterate(self.rng.ri(0, 12), self.int_fn(), self.rng.ri(-10, 10)),
            9 => HExpr::AppendI(
                Box::new(self.gen_list_int(b, scope)),
                Box::new(self.gen_list_int(b, scope)),
            ),
            _ => HExpr::IfListInt(
                Box::new(self.gen_bool(b, scope)),
                Box::new(self.gen_list_int(b, scope)),
                Box::new(self.gen_list_int(b, scope)),
            ),
        }
    }

    fn gen_list_text(&mut self, budget: u32, scope: &mut Vec<(u32, Ty)>) -> HExpr {
        if budget == 0 {
            let len = self.rng.below(5) as usize;
            let v: Vec<String> = (0..len).map(|_| self.safe_str(6)).collect();
            return HExpr::ListTextLit(v);
        }
        let b = budget - 1;
        match self.rng.pick(6) {
            0 => {
                let len = self.rng.below(6) as usize;
                let v: Vec<String> = (0..len).map(|_| self.safe_str(7)).collect();
                HExpr::ListTextLit(v)
            }
            1 => HExpr::MapShow(Box::new(self.gen_list_int(b, scope))),
            2 => HExpr::Words(Box::new(self.gen_text(b, scope))),
            3 => HExpr::MapToUpper(Box::new(self.gen_list_text(b, scope))),
            4 => {
                let p = self.safe_str(2);
                HExpr::FilterPrefix(p, Box::new(self.gen_list_text(b, scope)))
            }
            _ => HExpr::TakeT(self.rng.ri(0, 8), Box::new(self.gen_list_text(b, scope))),
        }
    }
}

/// Generate a whole program: pick a result type and a body. ~45% Int (the
/// type where the join-point/recursion/shadow shapes concentrate).
fn gen_program(seed: u64) -> (Ty, HExpr) {
    let mut g = Gen::new(seed);
    let r = g.rng.below(100);
    let ty = if r < 45 {
        Ty::Int
    } else if r < 65 {
        Ty::Text
    } else if r < 85 {
        Ty::ListInt
    } else {
        Ty::Pair
    };
    // Depth 3..4 by default; env PIPELINE_DEPTH cranks it for hand-run hunting
    // (deeper trees = more nested join points / shadowing / recursion).
    let budget = std::env::var("PIPELINE_DEPTH")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(3 + (seed % 2) as u32);
    let mut scope = Vec::new();
    let body = g.gen(ty, budget, &mut scope);
    (ty, body)
}

// ============================================================================
// Pretty-printer. Every produced string is "atomic-or-parenthesized" so it can
// be dropped into any argument position without precedence hazards.
// ============================================================================

fn ppfn(f: IntFn) -> String {
    match f {
        IntFn::AddK(k) => format!("(\\x -> x + {})", lit(k)),
        IntFn::SubK(k) => format!("(\\x -> x - {})", lit(k)),
        IntFn::MulK(k) => format!("(\\x -> x * {})", lit(k)),
        IntFn::Neg => "(\\x -> 0 - x)".to_string(),
        IntFn::AbsF => "(\\x -> abs' x)".to_string(),
        IntFn::Id => "(\\x -> x)".to_string(),
    }
}

fn pppred(p: IntPred) -> String {
    match p {
        IntPred::Even => "even".to_string(),
        IntPred::Odd => "odd".to_string(),
        IntPred::GtK(k) => format!("(\\x -> x > {})", lit(k)),
        IntPred::LtK(k) => format!("(\\x -> x < {})", lit(k)),
        IntPred::GeK(k) => format!("(\\x -> x >= {})", lit(k)),
        IntPred::NeqK(k) => format!("(\\x -> x /= {})", lit(k)),
    }
}

fn pplistfn(f: ListFn) -> String {
    match f {
        ListFn::Single => "(\\x -> [x])".to_string(),
        ListFn::Dup => "(\\x -> [x, x])".to_string(),
        ListFn::Empty => "(\\x -> ([] :: [Int]))".to_string(),
        ListFn::PairUp => "(\\x -> [x, x + 1])".to_string(),
    }
}

/// A literal int, parenthesized if negative (so `take (-3)` is valid).
fn lit(n: i64) -> String {
    if n < 0 {
        format!("({})", n)
    } else {
        n.to_string()
    }
}

fn arith_sym(op: ArithOp) -> &'static str {
    match op {
        ArithOp::Add => "+",
        ArithOp::Sub => "-",
        ArithOp::Mul => "*",
    }
}

fn cmp_sym(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Lt => "<",
        CmpOp::Gt => ">",
        CmpOp::Le => "<=",
        CmpOp::Ge => ">=",
        CmpOp::Eq => "==",
        CmpOp::Ne => "/=",
    }
}

fn esc_str(s: &str) -> String {
    // Charset is safe (no quote/backslash) except possible '\n'.
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '\n' {
            out.push_str("\\n");
        } else {
            out.push(c);
        }
    }
    out.push('"');
    out
}

fn pp_list_int(v: &[i64]) -> String {
    let inner: Vec<String> = v.iter().map(|n| lit(*n)).collect();
    format!("([{}] :: [Int])", inner.join(", "))
}

fn pp_list_text(v: &[String]) -> String {
    let inner: Vec<String> = v.iter().map(|s| esc_str(s)).collect();
    format!("([{}] :: [Text])", inner.join(", "))
}

fn pp(e: &HExpr) -> String {
    match e {
        HExpr::IntLit(n) => lit(*n),
        HExpr::Arith(op, a, b) => format!("({} {} {})", pp(a), arith_sym(*op), pp(b)),
        HExpr::Length(l) => format!("(length {})", pp(l)),
        HExpr::Sum(l) => format!("(sum {})", pp(l)),
        HExpr::Product(l) => format!("(product {})", pp(l)),
        HExpr::FoldlAdd(k, l) => format!("(foldl' (+) ({} :: Int) {})", k, pp(l)),
        HExpr::NegateE(x) => format!("(0 - {})", pp(x)),
        HExpr::IfInt(c, t, f) | HExpr::IfText(c, t, f) | HExpr::IfListInt(c, t, f) => {
            format!("(if {} then {} else {})", pp(c), pp(t), pp(f))
        }
        HExpr::WhereGo {
            start,
            op,
            with_sig,
        } => {
            let sig = if *with_sig {
                "go :: Int -> Int -> Int; "
            } else {
                ""
            };
            // go n acc = go (n-1) (acc OP n); go 0 acc = acc
            format!(
                "(let {}go 0 acc = acc; go n acc = go (n - 1) (acc {} n) in go {} 0)",
                sig,
                arith_sym(*op),
                start
            )
        }
        HExpr::CaseOfCase(s, t, f) => format!(
            "(case (case {} of {{ 0 -> (1 :: Int); _ -> 2 }}) of {{ 1 -> {}; _ -> {} }})",
            pp(s),
            pp(t),
            pp(f)
        ),
        HExpr::Guard {
            arg,
            a,
            c,
            b1,
            b2,
            b3,
            with_sig,
        } => {
            let sig = if *with_sig { "f :: Int -> Int; " } else { "" };
            format!(
                "(let {}f x | x > {} = {} | x < {} = {} | otherwise = {} in f {})",
                sig,
                lit(*a),
                pp(b1),
                lit(*c),
                pp(b2),
                pp(b3),
                pp(arg)
            )
        }
        HExpr::LetShadowInt { v0, va, e1, e2, e3 } => format!(
            "(let v{v0} = {} in let v{va} = ({} + v{v0}) in let v{v0} = {} in (v{v0} + v{va}))",
            pp(e1),
            pp(e2),
            pp(e3)
        ),
        HExpr::Cmp(op, a, b) => format!("({} {} {})", pp(a), cmp_sym(*op), pp(b)),
        HExpr::EvenE(x) => format!("(even {})", pp(x)),
        HExpr::OddE(x) => format!("(odd {})", pp(x)),
        HExpr::AndE(a, b) => format!("({} && {})", pp(a), pp(b)),
        HExpr::OrE(a, b) => format!("({} || {})", pp(a), pp(b)),
        HExpr::NotE(x) => format!("(not {})", pp(x)),
        HExpr::StrLit(s) => esc_str(s),
        HExpr::Append(a, b) => format!("({} <> {})", pp(a), pp(b)),
        HExpr::ShowInt(x) => format!("(show ({} :: Int))", pp(x)),
        HExpr::ToUpper(x) => format!("(toUpper {})", pp(x)),
        HExpr::ToLower(x) => format!("(toLower {})", pp(x)),
        HExpr::Strip(x) => format!("(strip {})", pp(x)),
        HExpr::TReverse(x) => format!("(tReverse {})", pp(x)),
        HExpr::Unwords(x) => format!("(unwords {})", pp(x)),
        HExpr::ListIntLit(v) => pp_list_int(v),
        HExpr::MapInt(f, l) => format!("(map {} {})", ppfn(*f), pp(l)),
        HExpr::FilterInt(p, l) => format!("(filter {} {})", pppred(*p), pp(l)),
        HExpr::TakeI(n, l) => format!("(take {} {})", lit(*n), pp(l)),
        HExpr::DropI(n, l) => format!("(drop {} {})", lit(*n), pp(l)),
        HExpr::ReverseI(l) => format!("(reverse {})", pp(l)),
        HExpr::ConcatMapI(f, l) => format!("(concatMap {} {})", pplistfn(*f), pp(l)),
        HExpr::EnumFromTo(a, b) => format!("(enumFromTo ({} :: Int) {})", a, lit(*b)),
        HExpr::TakeIterate(n, f, start) => {
            format!(
                "(take {} (iterate {} ({} :: Int)))",
                lit(*n),
                ppfn(*f),
                start
            )
        }
        HExpr::AppendI(a, b) => format!("({} ++ {})", pp(a), pp(b)),
        HExpr::ListTextLit(v) => pp_list_text(v),
        HExpr::MapShow(l) => format!("(map (\\n -> show (n :: Int)) {})", pp(l)),
        HExpr::Words(x) => format!("(words {})", pp(x)),
        HExpr::MapToUpper(l) => format!("(map toUpper {})", pp(l)),
        HExpr::FilterPrefix(p, l) => {
            format!("(filter (\\s -> {} `isPrefixOf` s) {})", esc_str(p), pp(l))
        }
        HExpr::TakeT(n, l) => format!("(take {} {})", lit(*n), pp(l)),
        HExpr::PairLit(a, b) => format!("({}, {})", pp(a), pp(b)),
        HExpr::Var(v) => format!("v{}", v),
    }
}

fn ty_sig(ty: Ty) -> &'static str {
    match ty {
        Ty::Int => "Int",
        Ty::Text => "Text",
        Ty::ListInt => "[Int]",
        Ty::ListText => "[Text]",
        Ty::Pair => "(Int, Text)",
    }
}

/// Render a complete, compilable module.
fn render_module(ty: Ty, body: &HExpr, nonce: &str) -> String {
    format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, ScopedTypeVariables #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
default (Int, Text)
{nonce}
result :: {sig}
result = {body}
"#,
        nonce = if nonce.is_empty() {
            String::new()
        } else {
            format!("-- nonce {nonce}\n")
        },
        sig = ty_sig(ty),
        body = pp(body),
    )
}

// ============================================================================
// Reference interpreter — the ORACLE. Total by construction. Uses i64 WRAPPING
// arithmetic to match GHC's machine Int#.
// ============================================================================

#[derive(Clone, Debug, PartialEq)]
enum HVal {
    I(i64),
    S(String),
    LI(Vec<i64>),
    LS(Vec<String>),
    Pair(Box<HVal>, Box<HVal>),
    B(bool),
}

impl HVal {
    fn int(&self) -> i64 {
        match self {
            HVal::I(n) => *n,
            _ => panic!("type error in reference: expected Int, got {:?}", self),
        }
    }
    fn text(&self) -> &str {
        match self {
            HVal::S(s) => s,
            _ => panic!("type error in reference: expected Text, got {:?}", self),
        }
    }
    fn list_int(&self) -> &[i64] {
        match self {
            HVal::LI(v) => v,
            _ => panic!("type error in reference: expected [Int], got {:?}", self),
        }
    }
    fn list_text(&self) -> &[String] {
        match self {
            HVal::LS(v) => v,
            _ => panic!("type error in reference: expected [Text], got {:?}", self),
        }
    }
    fn boolean(&self) -> bool {
        match self {
            HVal::B(b) => *b,
            _ => panic!("type error in reference: expected Bool, got {:?}", self),
        }
    }
}

fn apply_int_fn(f: IntFn, x: i64) -> i64 {
    match f {
        IntFn::AddK(k) => x.wrapping_add(k),
        IntFn::SubK(k) => x.wrapping_sub(k),
        IntFn::MulK(k) => x.wrapping_mul(k),
        IntFn::Neg => 0i64.wrapping_sub(x),
        IntFn::AbsF => x.wrapping_abs(),
        IntFn::Id => x,
    }
}

fn apply_int_pred(p: IntPred, x: i64) -> bool {
    match p {
        IntPred::Even => x.rem_euclid(2) == 0,
        IntPred::Odd => x.rem_euclid(2) != 0,
        IntPred::GtK(k) => x > k,
        IntPred::LtK(k) => x < k,
        IntPred::GeK(k) => x >= k,
        IntPred::NeqK(k) => x != k,
    }
}

fn apply_list_fn(f: ListFn, x: i64) -> Vec<i64> {
    match f {
        ListFn::Single => vec![x],
        ListFn::Dup => vec![x, x],
        ListFn::Empty => vec![],
        ListFn::PairUp => vec![x, x.wrapping_add(1)],
    }
}

/// Haskell `show` for an Int.
fn show_int(n: i64) -> String {
    n.to_string()
}

/// Data.Text.words: split on whitespace, no empty fragments.
fn t_words(s: &str) -> Vec<String> {
    s.split_whitespace().map(|w| w.to_string()).collect()
}

/// Data.Text.unwords: intercalate a single space.
fn t_unwords(v: &[String]) -> String {
    v.join(" ")
}

/// Data.Text.strip: strip leading/trailing whitespace.
fn t_strip(s: &str) -> String {
    s.trim().to_string()
}

fn eval(e: &HExpr, env: &mut Vec<(u32, HVal)>) -> HVal {
    match e {
        HExpr::IntLit(n) => HVal::I(*n),
        HExpr::Arith(op, a, b) => {
            let x = eval(a, env).int();
            let y = eval(b, env).int();
            HVal::I(match op {
                ArithOp::Add => x.wrapping_add(y),
                ArithOp::Sub => x.wrapping_sub(y),
                ArithOp::Mul => x.wrapping_mul(y),
            })
        }
        HExpr::Length(l) => {
            let v = eval(l, env);
            let n = match &v {
                HVal::LI(xs) => xs.len(),
                HVal::LS(xs) => xs.len(),
                _ => panic!("length of non-list"),
            };
            HVal::I(n as i64)
        }
        HExpr::Sum(l) => HVal::I(
            eval(l, env)
                .list_int()
                .iter()
                .fold(0i64, |a, x| a.wrapping_add(*x)),
        ),
        HExpr::Product(l) => HVal::I(
            eval(l, env)
                .list_int()
                .iter()
                .fold(1i64, |a, x| a.wrapping_mul(*x)),
        ),
        HExpr::FoldlAdd(k, l) => HVal::I(
            eval(l, env)
                .list_int()
                .iter()
                .fold(*k, |a, x| a.wrapping_add(*x)),
        ),
        HExpr::NegateE(x) => HVal::I(0i64.wrapping_sub(eval(x, env).int())),
        HExpr::IfInt(c, t, f) | HExpr::IfText(c, t, f) | HExpr::IfListInt(c, t, f) => {
            if eval(c, env).boolean() {
                eval(t, env)
            } else {
                eval(f, env)
            }
        }
        HExpr::WhereGo { start, op, .. } => {
            let mut n = *start;
            let mut acc = 0i64;
            // go 0 acc = acc; go n acc = go (n-1) (acc OP n)
            while n != 0 {
                acc = match op {
                    ArithOp::Add => acc.wrapping_add(n),
                    ArithOp::Sub => acc.wrapping_sub(n),
                    ArithOp::Mul => acc.wrapping_mul(n),
                };
                n -= 1;
            }
            HVal::I(acc)
        }
        HExpr::CaseOfCase(s, t, f) => {
            let inner = if eval(s, env).int() == 0 { 1 } else { 2 };
            if inner == 1 {
                eval(t, env)
            } else {
                eval(f, env)
            }
        }
        HExpr::Guard {
            arg,
            a,
            c,
            b1,
            b2,
            b3,
            ..
        } => {
            let x = eval(arg, env).int();
            if x > *a {
                eval(b1, env)
            } else if x < *c {
                eval(b2, env)
            } else {
                eval(b3, env)
            }
        }
        HExpr::LetShadowInt { v0, va, e1, e2, e3 } => {
            // Mirror Haskell's lexical scoping with a save/restore on `env`.
            let val1 = eval(e1, env).int();
            env.push((*v0, HVal::I(val1))); // outer v0 in scope for e2
            let vaval = eval(e2, env).int().wrapping_add(val1);
            env.push((*va, HVal::I(vaval)));
            let val3 = eval(e3, env).int();
            env.push((*v0, HVal::I(val3))); // inner v0 shadows (last wins)
                                            // body = inner v0 + va
            let body = val3.wrapping_add(vaval);
            env.pop();
            env.pop();
            env.pop();
            HVal::I(body)
        }
        HExpr::Cmp(op, a, b) => {
            let x = eval(a, env).int();
            let y = eval(b, env).int();
            HVal::B(match op {
                CmpOp::Lt => x < y,
                CmpOp::Gt => x > y,
                CmpOp::Le => x <= y,
                CmpOp::Ge => x >= y,
                CmpOp::Eq => x == y,
                CmpOp::Ne => x != y,
            })
        }
        HExpr::EvenE(x) => HVal::B(eval(x, env).int().rem_euclid(2) == 0),
        HExpr::OddE(x) => HVal::B(eval(x, env).int().rem_euclid(2) != 0),
        HExpr::AndE(a, b) => HVal::B(eval(a, env).boolean() && eval(b, env).boolean()),
        HExpr::OrE(a, b) => HVal::B(eval(a, env).boolean() || eval(b, env).boolean()),
        HExpr::NotE(x) => HVal::B(!eval(x, env).boolean()),
        HExpr::StrLit(s) => HVal::S(s.clone()),
        HExpr::Append(a, b) => HVal::S(format!("{}{}", eval(a, env).text(), eval(b, env).text())),
        HExpr::ShowInt(x) => HVal::S(show_int(eval(x, env).int())),
        HExpr::ToUpper(x) => HVal::S(eval(x, env).text().to_uppercase()),
        HExpr::ToLower(x) => HVal::S(eval(x, env).text().to_lowercase()),
        HExpr::Strip(x) => HVal::S(t_strip(eval(x, env).text())),
        HExpr::TReverse(x) => HVal::S(eval(x, env).text().chars().rev().collect()),
        HExpr::Unwords(x) => HVal::S(t_unwords(eval(x, env).list_text())),
        HExpr::ListIntLit(v) => HVal::LI(v.clone()),
        HExpr::MapInt(f, l) => HVal::LI(
            eval(l, env)
                .list_int()
                .iter()
                .map(|x| apply_int_fn(*f, *x))
                .collect(),
        ),
        HExpr::FilterInt(p, l) => HVal::LI(
            eval(l, env)
                .list_int()
                .iter()
                .copied()
                .filter(|x| apply_int_pred(*p, *x))
                .collect(),
        ),
        HExpr::TakeI(n, l) => {
            let v = eval(l, env);
            let xs = v.list_int();
            let k = (*n).max(0) as usize;
            HVal::LI(xs.iter().take(k).copied().collect())
        }
        HExpr::DropI(n, l) => {
            let v = eval(l, env);
            let xs = v.list_int();
            let k = (*n).max(0) as usize;
            HVal::LI(xs.iter().skip(k).copied().collect())
        }
        HExpr::ReverseI(l) => {
            let v = eval(l, env);
            let mut xs = v.list_int().to_vec();
            xs.reverse();
            HVal::LI(xs)
        }
        HExpr::ConcatMapI(f, l) => {
            let v = eval(l, env);
            let mut out = Vec::new();
            for x in v.list_int() {
                out.extend(apply_list_fn(*f, *x));
            }
            HVal::LI(out)
        }
        HExpr::EnumFromTo(a, b) => {
            if a > b {
                HVal::LI(vec![])
            } else {
                HVal::LI((*a..=*b).collect())
            }
        }
        HExpr::TakeIterate(n, f, start) => {
            let k = (*n).max(0) as usize;
            let mut out = Vec::with_capacity(k);
            let mut cur = *start;
            for _ in 0..k {
                out.push(cur);
                cur = apply_int_fn(*f, cur);
            }
            HVal::LI(out)
        }
        HExpr::AppendI(a, b) => {
            let mut x = eval(a, env).list_int().to_vec();
            x.extend_from_slice(eval(b, env).list_int());
            HVal::LI(x)
        }
        HExpr::ListTextLit(v) => HVal::LS(v.clone()),
        HExpr::MapShow(l) => HVal::LS(
            eval(l, env)
                .list_int()
                .iter()
                .map(|n| show_int(*n))
                .collect(),
        ),
        HExpr::Words(x) => HVal::LS(t_words(eval(x, env).text())),
        HExpr::MapToUpper(l) => HVal::LS(
            eval(l, env)
                .list_text()
                .iter()
                .map(|s| s.to_uppercase())
                .collect(),
        ),
        HExpr::FilterPrefix(p, l) => HVal::LS(
            eval(l, env)
                .list_text()
                .iter()
                .filter(|s| s.starts_with(p.as_str()))
                .cloned()
                .collect(),
        ),
        HExpr::TakeT(n, l) => {
            let v = eval(l, env);
            let xs = v.list_text();
            let k = (*n).max(0) as usize;
            HVal::LS(xs.iter().take(k).cloned().collect())
        }
        HExpr::PairLit(a, b) => HVal::Pair(Box::new(eval(a, env)), Box::new(eval(b, env))),
        HExpr::Var(v) => {
            // last binding wins (shadowing)
            for (id, val) in env.iter().rev() {
                if id == v {
                    return val.clone();
                }
            }
            panic!("unbound var v{} in reference", v);
        }
    }
}

/// Reference value as the JSON the runtime's `to_json` is expected to produce.
fn hval_to_json(v: &HVal) -> serde_json::Value {
    match v {
        HVal::I(n) => serde_json::json!(n),
        HVal::S(s) => serde_json::json!(s),
        HVal::LI(xs) => serde_json::json!(xs),
        HVal::LS(xs) => serde_json::json!(xs),
        HVal::Pair(a, b) => serde_json::json!([hval_to_json(a), hval_to_json(b)]),
        HVal::B(b) => serde_json::json!(b),
    }
}

fn reference_json(body: &HExpr) -> serde_json::Value {
    let mut env = Vec::new();
    hval_to_json(&eval(body, &mut env))
}

// ============================================================================
// Pipeline drivers.
// ============================================================================

fn prelude_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell")
        .join("lib")
}

/// Compile + run a pure program; returns the JSON result or an error string.
fn run_pure(source: &str) -> Result<serde_json::Value, String> {
    let pp = prelude_path();
    let include = [pp.as_path()];
    tidepool_runtime::compile_and_run_pure(source, "result", &include)
        .map(|r| r.to_json())
        .map_err(|e| format!("{e}"))
}

/// Outcome classification of one pure case.
#[derive(Debug)]
enum Outcome {
    Match,
    Mismatch {
        expected: serde_json::Value,
        got: serde_json::Value,
    },
    Error(String),
}

fn run_pure_case(seed: u64, nonce: &str) -> (String, serde_json::Value, Outcome) {
    let (ty, body) = gen_program(seed);
    let source = render_module(ty, &body, nonce);
    let expected = reference_json(&body);
    let outcome = match run_pure(&source) {
        Ok(got) => {
            if got == expected {
                Outcome::Match
            } else {
                Outcome::Mismatch {
                    expected: expected.clone(),
                    got,
                }
            }
        }
        Err(e) => Outcome::Error(e),
    };
    (source, expected, outcome)
}

// ============================================================================
// Self-check: the reference interpreter must be total and internally
// consistent over many random ASTs (no GHC, instant). Also pins a handful of
// hand-computed text-op semantics so a reference bug can't masquerade as a
// pipeline bug.
// ============================================================================

#[test]
fn reference_self_check_1000() {
    for i in 0..1000u64 {
        let (_, body) = gen_program(i.wrapping_mul(2654435761));
        let mut env1 = Vec::new();
        let mut env2 = Vec::new();
        let a = eval(&body, &mut env1);
        let b = eval(&body, &mut env2);
        assert_eq!(
            a, b,
            "reference interpreter is nondeterministic at seed {i}"
        );
        // Pretty-printing must produce a non-empty body.
        assert!(!pp(&body).is_empty());
        // JSON projection must not panic.
        let _ = hval_to_json(&a);
    }
}

#[test]
fn reference_text_semantics_pinned() {
    // words
    assert_eq!(t_words("  a  b "), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(t_words(""), Vec::<String>::new());
    assert_eq!(t_words("solo"), vec!["solo".to_string()]);
    // unwords
    assert_eq!(t_unwords(&["a".into(), "b".into()]), "a b");
    assert_eq!(t_unwords(&[]), "");
    // strip
    assert_eq!(t_strip("  hi \n"), "hi");
    // show
    assert_eq!(show_int(-5), "-5");
    assert_eq!(show_int(0), "0");
}

#[test]
fn reference_algebraic_identities() {
    // reverse . reverse == id ; length (map f xs) == length xs
    for i in 0..400u64 {
        let mut g = Gen::new(i.wrapping_mul(40503));
        let mut scope = Vec::new();
        let list = g.gen_list_int(3, &mut scope);
        let mut env = Vec::new();
        let xs = eval(&list, &mut env).list_int().to_vec();
        let rr = HExpr::ReverseI(Box::new(HExpr::ReverseI(Box::new(list.clone()))));
        let mut env2 = Vec::new();
        assert_eq!(eval(&rr, &mut env2).list_int(), &xs[..]);
        let mapped = HExpr::MapInt(IntFn::AddK(1), Box::new(list.clone()));
        let mut env3 = Vec::new();
        assert_eq!(eval(&mapped, &mut env3).list_int().len(), xs.len());
    }
}

// ============================================================================
// COMMITTED PROPERTIES (30 cases each). Use proptest so the failing u64 seed is
// persisted to the regression file. --test-threads=1 required (shared cache).
// ============================================================================

mod committed {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    fn cfg() -> Config {
        Config {
            cases: 30,
            failure_persistence: Some(Box::new(
                proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"),
            )),
            ..Config::default()
        }
    }

    /// B1/B2: JIT pure result must equal the reference; no error on a total program.
    #[test]
    fn pure_reference_x30() {
        let mut runner = TestRunner::new(cfg());
        runner
            .run(&any::<u64>(), |seed| {
                let (source, expected, outcome) = run_pure_case(seed, "");
                match outcome {
                    Outcome::Match => Ok(()),
                    Outcome::Mismatch { got, .. } => Err(TestCaseError::fail(format!(
                        "B1 mismatch (seed {seed})\nexpected: {expected}\n     got: {got}\n--- source ---\n{source}"
                    ))),
                    Outcome::Error(e) => Err(TestCaseError::fail(format!(
                        "B2 error (seed {seed}): {e}\n--- source ---\n{source}"
                    ))),
                }
            })
            .unwrap();
    }

    /// B4: compiling the same program twice (different nonce comments → fresh
    /// compile, cache busted) yields equal results.
    #[test]
    fn determinism_x30() {
        let mut runner = TestRunner::new(cfg());
        runner
            .run(&any::<u64>(), |seed| {
                let (ty, body) = gen_program(seed);
                let src1 = render_module(ty, &body, &format!("a{seed}"));
                let src2 = render_module(ty, &body, &format!("b{seed}"));
                let r1 = run_pure(&src1);
                let r2 = run_pure(&src2);
                match (r1, r2) {
                    (Ok(a), Ok(b)) => {
                        prop_assert_eq!(
                            &a,
                            &b,
                            "B4 nondeterminism (seed {})\nrun1: {}\nrun2: {}\n--- source ---\n{}",
                            seed,
                            a,
                            b,
                            src1
                        );
                        Ok(())
                    }
                    // A program that errors on BOTH compiles is caught by the
                    // pure property; here we only assert agreement.
                    (Err(e1), Err(e2)) => {
                        prop_assert_eq!(e1, e2, "B4 nondeterministic error (seed {})", seed);
                        Ok(())
                    }
                    (a, b) => Err(TestCaseError::fail(format!(
                        "B4 one compile succeeded, one failed (seed {seed}): {a:?} vs {b:?}"
                    ))),
                }
            })
            .unwrap();
    }

    /// B1/B3/B4 over the effectful path: a program consuming a large handler
    /// response, run in a subprocess under both TIDEPOOL_LAZY_RESULTS=1 and =0,
    /// compared to each other and to the reference. List sizes straddle the
    /// 2000-element lazy threshold.
    ///
    /// Only 8 cases: each case spawns TWO subprocesses, and every subprocess
    /// JIT-compiles the full MCP effect preamble (~10-30s). 30 cases here cost
    /// ~33 min — untenable for a committed smoke property. Deep lazy hunting is
    /// the sibling W4 (proptest-lazy-consumption) workstream; this is a bonus
    /// A/B oracle on the effect path. Cap stays ≤30 per task boundary.
    #[test]
    fn effectful_lazy_ab_x8() {
        let mut runner = TestRunner::new(Config { cases: 8, ..cfg() });
        runner
            .run(&any::<u64>(), |seed| {
                let case = gen_effect_case(seed);
                let expected = case.reference_json();
                let lazy = run_effect_worker(&case, true);
                let eager = run_effect_worker(&case, false);
                match (&lazy, &eager) {
                    (WorkerResult::Ok(a), WorkerResult::Ok(b)) => {
                        prop_assert_eq!(
                            a, b,
                            "B4 lazy/eager divergence (seed {})\ncase: {:?}",
                            seed, case
                        );
                        prop_assert_eq!(
                            a, &expected,
                            "B1 effectful mismatch (seed {})\ncase: {:?}",
                            seed, case
                        );
                        Ok(())
                    }
                    // DOCUMENTED divergence, not a bug (cf. W4's
                    // cap_boundary_clean_error): over the 100k node cap the
                    // lazy path parks (streamed responses have no cap) while
                    // the eager kill-switch drains and returns a clean
                    // EffectResponseTooLarge. Lazy must still match the
                    // reference; eager must fail CLEANLY (no signal).
                    (WorkerResult::Ok(a), WorkerResult::Fail(msg)) if msg.contains("too large") => {
                        prop_assert_eq!(
                            a,
                            &expected,
                            "lazy result wrong in over-cap case (seed {})\ncase: {:?}",
                            seed,
                            case
                        );
                        Ok(())
                    }
                    (a, b) => Err(TestCaseError::fail(format!(
                        "B2/B3 effect worker failure (seed {seed})\ncase: {case:?}\nlazy: {a:?}\neager: {b:?}"
                    ))),
                }
            })
            .unwrap();
    }
}

// ============================================================================
// LONG-HAUL (#[ignore]d): 300 pure cases. This is where the bugs are. Run by
// hand during the authoring session, multiple rounds with fresh base seeds.
// Logs every failure to stderr (captured into the findings doc).
// ============================================================================

#[test]
#[ignore = "long-haul hunting property; run by hand with --ignored"]
fn long_haul() {
    let base: u64 = std::env::var("LONGHAUL_BASE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0xC0FFEE);
    let n: u64 = std::env::var("LONGHAUL_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let mut failures = 0usize;
    for i in 0..n {
        let seed = base.wrapping_add(i).wrapping_mul(0x9E3779B97F4A7C15);
        let (source, expected, outcome) = run_pure_case(seed, "");
        match outcome {
            Outcome::Match => {}
            Outcome::Mismatch { got, .. } => {
                failures += 1;
                eprintln!(
                    "\n=== LONGHAUL FAIL B1 (seed {seed}) ===\nexpected: {expected}\n     got: {got}\n--- source ---\n{source}\n"
                );
            }
            Outcome::Error(e) => {
                failures += 1;
                eprintln!(
                    "\n=== LONGHAUL FAIL B2 (seed {seed}) ===\nerror: {e}\n--- source ---\n{source}\n"
                );
            }
        }
        if i % 25 == 0 {
            eprintln!("[longhaul] {i}/{n} ({failures} failures so far)");
        }
    }
    eprintln!("[longhaul] DONE {n} cases, {failures} failures");
    assert_eq!(
        failures, 0,
        "long-haul found {failures} failing cases (see stderr)"
    );
}

// ============================================================================
// EFFECTFUL WORKER. The handler responds to every effect with a list of N
// "item-i" strings (BigListDispatcher analogue). The program consumes it. The
// driver runs the program in a SUBPROCESS so a fatal signal (SIGILL/SIGSEGV) is
// observable as ExitStatus::signal() rather than killing the test runner.
// ============================================================================

#[derive(Debug, Clone)]
struct EffectCase {
    /// Number of items the handler returns. Straddles the 2000 lazy threshold.
    n: usize,
    /// Which consumer expression to run.
    op: EffectOp,
}

#[derive(Debug, Clone)]
enum EffectOp {
    Length,
    LengthTake(usize),
    LengthDrop(usize),
    Take(usize),
    ReverseTake(usize),
    FilterPrefixLen(String),
    MapUpperTake(usize),
}

fn gen_effect_case(seed: u64) -> EffectCase {
    let mut r = Rng::new(seed ^ 0xEFFEC7);
    let sizes = [50usize, 1500, 1999, 2000, 2001, 5000, 30000];
    let n = sizes[r.pick(sizes.len())];
    let op = match r.pick(7) {
        0 => EffectOp::Length,
        1 => EffectOp::LengthTake(r.below(6) as usize),
        2 => EffectOp::LengthDrop(r.below(6) as usize),
        3 => EffectOp::Take(r.below(5) as usize),
        4 => EffectOp::ReverseTake(r.below(5) as usize),
        5 => {
            // prefix that selects a known subset of "item-i"
            let digit = r.below(3); // 0,1,2
            EffectOp::FilterPrefixLen(format!("item-{digit}"))
        }
        _ => EffectOp::MapUpperTake(r.below(4) as usize),
    };
    EffectCase { n, op }
}

impl EffectCase {
    /// The Haskell body that consumes `xs <- glob "**"`.
    fn body(&self) -> String {
        match &self.op {
            EffectOp::Length => "pure (length xs)".to_string(),
            EffectOp::LengthTake(k) => format!("pure (length (take {k} xs))"),
            EffectOp::LengthDrop(k) => format!("pure (length (drop {k} xs))"),
            EffectOp::Take(k) => format!("pure (take {k} xs)"),
            EffectOp::ReverseTake(k) => format!("pure (reverse (take {k} xs))"),
            EffectOp::FilterPrefixLen(p) => {
                format!("pure (length (filter (\\x -> \"{p}\" `isPrefixOf` x) xs))")
            }
            EffectOp::MapUpperTake(k) => format!("pure (map toUpper (take {k} xs))"),
        }
    }

    fn code(&self) -> String {
        format!("xs <- glob \"**\"\n{}", self.body())
    }

    /// The synthetic handler list: ["item-0", ..., "item-(n-1)"].
    fn items(&self) -> Vec<String> {
        (0..self.n).map(|i| format!("item-{i}")).collect()
    }

    fn reference_json(&self) -> serde_json::Value {
        let items = self.items();
        match &self.op {
            EffectOp::Length => serde_json::json!(items.len()),
            EffectOp::LengthTake(k) => serde_json::json!(items.iter().take(*k).count()),
            EffectOp::LengthDrop(k) => {
                serde_json::json!(items.iter().skip(*k).count())
            }
            EffectOp::Take(k) => {
                serde_json::json!(items.iter().take(*k).collect::<Vec<_>>())
            }
            EffectOp::ReverseTake(k) => {
                let mut v: Vec<String> = items.iter().take(*k).cloned().collect();
                v.reverse();
                serde_json::json!(v)
            }
            EffectOp::FilterPrefixLen(p) => {
                serde_json::json!(items.iter().filter(|s| s.starts_with(p.as_str())).count())
            }
            EffectOp::MapUpperTake(k) => {
                let v: Vec<String> = items.iter().take(*k).map(|s| s.to_uppercase()).collect();
                serde_json::json!(v)
            }
        }
    }
}

#[derive(Debug)]
enum WorkerResult {
    Ok(serde_json::Value),
    /// Process died from a fatal signal (SIGILL=4, SIGSEGV=11, etc).
    Signal(i32),
    /// Non-signal failure (panic / compile error / bad output).
    Fail(String),
}

impl PartialEq for WorkerResult {
    fn eq(&self, other: &Self) -> bool {
        matches!((self, other), (WorkerResult::Ok(a), WorkerResult::Ok(b)) if a == b)
    }
}

/// Spawn this test binary's effect worker in a subprocess.
fn run_effect_worker(case: &EffectCase, lazy: bool) -> WorkerResult {
    use std::os::unix::process::ExitStatusExt;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return WorkerResult::Fail(format!("current_exe: {e}")),
    };
    let output = Command::new(exe)
        .args([
            "--exact",
            "pipeline_effect_worker",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("TP_EFFECT_CODE", case.code())
        .env("TP_EFFECT_N", case.n.to_string())
        .env("TIDEPOOL_LAZY_RESULTS", if lazy { "1" } else { "0" })
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => return WorkerResult::Fail(format!("spawn: {e}")),
    };
    if let Some(sig) = output.status.signal() {
        return WorkerResult::Signal(sig);
    }
    // NOTE: libtest with --nocapture prints "test <name> ... " WITHOUT a
    // trailing newline before running, so our marker can land mid-line
    // (e.g. "test pipeline_effect_worker ... __EFFECT_JSON__5"). Search for the
    // marker as a SUBSTRING, not a line prefix, and take the rest of that line
    // (the JSON, which serde renders single-line). A trailing "ok" appears on
    // the next line and is ignored.
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(idx) = line.find("__EFFECT_JSON__") {
            let rest = &line[idx + "__EFFECT_JSON__".len()..];
            return match serde_json::from_str(rest) {
                Ok(v) => WorkerResult::Ok(v),
                Err(e) => WorkerResult::Fail(format!("bad json: {e}: {rest}")),
            };
        }
        if let Some(idx) = line.find("__EFFECT_ERR__") {
            let rest = &line[idx + "__EFFECT_ERR__".len()..];
            return WorkerResult::Fail(rest.to_string());
        }
    }
    WorkerResult::Fail(format!(
        "no result marker (exit {:?})\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// The subprocess body. #[ignore]d so it never runs in a normal `cargo test`;
/// invoked explicitly via `--exact pipeline_effect_worker --ignored`.
#[test]
#[ignore = "subprocess effect worker; invoked by run_effect_worker"]
fn pipeline_effect_worker() {
    let code = match std::env::var("TP_EFFECT_CODE") {
        Ok(c) => c,
        Err(_) => {
            // Not invoked as a worker — nothing to do.
            return;
        }
    };
    let n: usize = std::env::var("TP_EFFECT_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, &code, "", "", None, None);
    let effects_dir = match tidepool_mcp::ensure_effects_module(&decls) {
        Ok(p) => p,
        Err(e) => {
            println!("__EFFECT_ERR__effects module: {e}");
            return;
        }
    };
    let pp = prelude_path();
    let user_lib = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".tidepool/lib");
    let include = [pp.as_path(), user_lib.as_path(), effects_dir.as_path()];

    struct BigList {
        n: usize,
    }
    impl tidepool_effect::DispatchEffect<()> for BigList {
        fn dispatch(
            &mut self,
            _tag: u64,
            _request: &tidepool_eval::value::Value,
            cx: &tidepool_effect::EffectContext<'_, ()>,
        ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
            let items: Vec<String> = (0..self.n).map(|i| format!("item-{i}")).collect();
            cx.respond(items)
        }
    }

    let mut d = BigList { n };
    // Leading newline so the marker starts its own line even under libtest's
    // newline-less "test <name> ... " preamble (parser also substring-matches).
    match tidepool_runtime::compile_and_run(&source, "result", &include, &mut d, &()) {
        Ok(r) => println!("\n__EFFECT_JSON__{}", r.to_json()),
        Err(e) => println!("\n__EFFECT_ERR__{e}"),
    }
}

// ============================================================================
// Coverage census. Recursively tally construct occurrences over many generated
// programs so the findings doc carries REAL idiom counters (not estimates) and
// can prove the join-point / recursion / shadow shapes are actually emitted.
// #[ignore]d (prints to stdout; run by hand with --ignored --nocapture).
// ============================================================================

fn count_constructs(e: &HExpr, t: &mut std::collections::BTreeMap<&'static str, usize>) {
    use HExpr::*;
    let name: &'static str = match e {
        IntLit(_) => "IntLit",
        Arith(..) => "Arith",
        Length(_) => "Length",
        Sum(_) => "Sum",
        Product(_) => "Product",
        FoldlAdd(..) => "FoldlAdd",
        NegateE(_) => "NegateE",
        IfInt(..) => "IfInt",
        WhereGo { with_sig: true, .. } => "WhereGo(sig)",
        WhereGo {
            with_sig: false, ..
        } => "WhereGo(unsigned)",
        CaseOfCase(..) => "CaseOfCase",
        Guard { with_sig: true, .. } => "Guard(sig)",
        Guard {
            with_sig: false, ..
        } => "Guard(unsigned)",
        LetShadowInt { .. } => "LetShadowInt",
        Cmp(..) => "Cmp",
        EvenE(_) => "EvenE",
        OddE(_) => "OddE",
        AndE(..) => "AndE",
        OrE(..) => "OrE",
        NotE(_) => "NotE",
        StrLit(_) => "StrLit",
        Append(..) => "Append",
        ShowInt(_) => "ShowInt",
        ToUpper(_) => "ToUpper",
        ToLower(_) => "ToLower",
        Strip(_) => "Strip",
        TReverse(_) => "TReverse",
        Unwords(_) => "Unwords",
        IfText(..) => "IfText",
        ListIntLit(_) => "ListIntLit",
        MapInt(..) => "MapInt",
        FilterInt(..) => "FilterInt",
        TakeI(..) => "TakeI",
        DropI(..) => "DropI",
        ReverseI(_) => "ReverseI",
        ConcatMapI(..) => "ConcatMapI",
        EnumFromTo(..) => "EnumFromTo",
        TakeIterate(..) => "TakeIterate",
        AppendI(..) => "AppendI",
        IfListInt(..) => "IfListInt",
        ListTextLit(_) => "ListTextLit",
        MapShow(_) => "MapShow",
        Words(_) => "Words",
        MapToUpper(_) => "MapToUpper",
        FilterPrefix(..) => "FilterPrefix",
        TakeT(..) => "TakeT",
        PairLit(..) => "PairLit",
        Var(_) => "Var",
    };
    *t.entry(name).or_insert(0) += 1;
    // Recurse into children.
    match e {
        Arith(_, a, b) | AndE(a, b) | OrE(a, b) | Cmp(_, a, b) | Append(a, b) | AppendI(a, b) => {
            count_constructs(a, t);
            count_constructs(b, t);
        }
        Length(a)
        | Sum(a)
        | Product(a)
        | FoldlAdd(_, a)
        | NegateE(a)
        | EvenE(a)
        | OddE(a)
        | NotE(a)
        | ShowInt(a)
        | ToUpper(a)
        | ToLower(a)
        | Strip(a)
        | TReverse(a)
        | Unwords(a)
        | MapInt(_, a)
        | FilterInt(_, a)
        | TakeI(_, a)
        | DropI(_, a)
        | ReverseI(a)
        | ConcatMapI(_, a)
        | MapShow(a)
        | Words(a)
        | MapToUpper(a)
        | FilterPrefix(_, a)
        | TakeT(_, a) => count_constructs(a, t),
        IfInt(a, b, c) | IfText(a, b, c) | IfListInt(a, b, c) | CaseOfCase(a, b, c) => {
            count_constructs(a, t);
            count_constructs(b, t);
            count_constructs(c, t);
        }
        Guard {
            arg, b1, b2, b3, ..
        } => {
            count_constructs(arg, t);
            count_constructs(b1, t);
            count_constructs(b2, t);
            count_constructs(b3, t);
        }
        LetShadowInt { e1, e2, e3, .. } => {
            count_constructs(e1, t);
            count_constructs(e2, t);
            count_constructs(e3, t);
        }
        PairLit(a, b) => {
            count_constructs(a, t);
            count_constructs(b, t);
        }
        _ => {}
    }
}

#[test]
#[ignore = "coverage census; run by hand with --ignored --nocapture"]
fn coverage_census() {
    let depth: u32 = std::env::var("PIPELINE_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let n: u64 = 5000;
    let mut totals = std::collections::BTreeMap::new();
    let mut result_ty = std::collections::BTreeMap::new();
    let mut programs_with_joinshape = 0usize;
    for i in 0..n {
        let seed = (i.wrapping_add(1)).wrapping_mul(0x9E3779B97F4A7C15);
        if depth > 0 {
            std::env::set_var("PIPELINE_DEPTH", depth.to_string());
        }
        let (ty, body) = gen_program(seed);
        *result_ty.entry(format!("{ty:?}")).or_insert(0usize) += 1;
        let mut t = std::collections::BTreeMap::new();
        count_constructs(&body, &mut t);
        let has_join = t.keys().any(|k| {
            k.starts_with("WhereGo")
                || *k == "CaseOfCase"
                || k.starts_with("Guard")
                || *k == "LetShadowInt"
        });
        if has_join {
            programs_with_joinshape += 1;
        }
        for (k, v) in t {
            *totals.entry(k).or_insert(0usize) += v;
        }
    }
    println!("\n=== COVERAGE CENSUS ({n} programs) ===");
    println!(
        "programs containing a join-point/recursion/shadow shape: {programs_with_joinshape}/{n} ({:.1}%)",
        100.0 * programs_with_joinshape as f64 / n as f64
    );
    println!("\nresult types:");
    for (k, v) in &result_ty {
        println!("  {k:<10} {v}");
    }
    println!("\nconstruct occurrences:");
    for (k, v) in &totals {
        println!("  {k:<20} {v}");
    }
}
