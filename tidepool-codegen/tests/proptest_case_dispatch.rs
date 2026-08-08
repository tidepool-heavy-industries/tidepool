//! Lane: Case-dispatch edges — a JIT-vs-eval differential net.
//!
//! Complements the already-green N-way constructor-case coverage
//! (`proptest_ghc_idioms_widen.rs`, another agent's lane) with the *dispatch
//! shapes* that the synthetic generators in `tidepool-testing/strategy.rs`
//! never exercise:
//!
//!   (A) literal-alt cases on `Int#` — both DENSE (0,1,..,N consecutive) and
//!       SPARSE (scattered, including `i64::MIN`/`i64::MAX`), with and without a
//!       `Default`. The JIT lowers BOTH to a linear `icmp`-chain over the
//!       unboxed scrutinee (`emit_lit_dispatch`, tidepool-codegen/src/emit/
//!       case.rs); whether Cranelift turns a dense chain into a jump table is an
//!       internal decision we want to stress on both sides of the boundary.
//!   (B) literal-alt cases on `Char#` — dense ASCII runs and sparse Unicode
//!       codepoints (`emit_lit_dispatch` `LitChar` arm: compares `*c as i64`).
//!   (C) VERY-WIDE constructor cases — 10..16 distinct nullary `DataConId`s in
//!       one `Case`. Data dispatch is a linear `icmp`-chain over `con_tag`
//!       (== `DataConId.0`) (`emit_data_dispatch`); width stresses the chain
//!       length and the trailing trap/default fall-through.
//!   (D) NESTED case-of-case at depth — a chain where each `Case`'s scrutinee is
//!       the *result* of the previous `Case`, 3..8 deep. The GHC join-point
//!       factory shape, but here driven purely through nested literal dispatch.
//!   (E) PARTIAL constructor set + `Default` — a wide constructor `Case` that
//!       lists only a SUBSET of the type's constructors and relies on a trailing
//!       `Default` to catch the rest (both the matched-constructor and the
//!       fall-through-to-default dynamic paths are swept).
//!
//! Construction style (mirrors `proptest_ghc_idioms.rs`): hand-built
//! `RecursiveTree<CoreFrame<usize>>` via `TreeBuilder`, every program TOTAL and
//! GROUND by construction so ~100% of cases reach a JIT-vs-eval value compare.
//!
//! TOTALITY DISCIPLINE for literal/data cases WITHOUT a `Default`: the scrutinee
//! is a *constant chosen from the alt key set*, so it always matches some alt —
//! the JIT `runtime_shape_trap` / eval no-match-error path is never reached on a
//! no-default program. Programs WITH a `Default` additionally sweep
//! deliberately-out-of-range scrutinees to exercise the fall-through arm.
//!
//! Oracle: `tidepool_testing::differential` — one [`DiffConfig`] sweeping the
//! 64KiB/4KiB nursery pair with `repeat_count(2)` (determinism) and
//! `CrashContainment::ForkProbe` (a child killed by a signal is a reportable
//! divergence the parent shrinks). Every property drives `TestRunner`
//! directly so its `ReachCounter` lives and asserts in the SAME process as
//! the cases it counts.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serial_test::serial;

use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};

use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_testing::differential::{check, CrashContainment, DiffConfig, ReachCounter};
use tidepool_testing::proptest::values_equal;

// ---------------------------------------------------------------------------
// Shape instrumentation. Each property's local `ReachCounter` (see `dcfg`
// call sites below) carries reach; these counters carry shape frequency and
// are printed at the end of the property that bumps them — nextest gives
// every test its own process, so a counter is only meaningful within the
// test that both bumps and reads it.
// ---------------------------------------------------------------------------
static N_DENSE_INT: AtomicU64 = AtomicU64::new(0);
static N_SPARSE_INT: AtomicU64 = AtomicU64::new(0);
static N_CHAR: AtomicU64 = AtomicU64::new(0);
static N_WIDE_CON: AtomicU64 = AtomicU64::new(0);
static N_NESTED: AtomicU64 = AtomicU64::new(0);
static N_PARTIAL: AtomicU64 = AtomicU64::new(0);
static N_NO_DEFAULT: AtomicU64 = AtomicU64::new(0); // no-default programs (totality-safe)
static N_HIT_DEFAULT: AtomicU64 = AtomicU64::new(0); // scrutinee deliberately out-of-range

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Fresh VarId supply, per-builder.
// ---------------------------------------------------------------------------
thread_local! {
    static VAR_CTR: Cell<u64> = const { Cell::new(0) };
}
fn reset_ctrs() {
    VAR_CTR.with(|c| c.set(2000));
}
fn fresh_var() -> VarId {
    VAR_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        VarId(v)
    })
}

// ---------------------------------------------------------------------------
// Differential config: the nursery pair, determinism repeats, and fork probe
// that used to be reinvented per-lane now live in one DiffConfig. Nothing is
// tolerated — this generator is total and ground by construction, so an eval
// or JIT error is a failure, not a skip.
// ---------------------------------------------------------------------------
fn dcfg(label: &'static str) -> DiffConfig {
    DiffConfig::new(label)
        .nurseries(&[64 * 1024, 4 * 1024])
        .repeat_count(2)
        .crash_containment(CrashContainment::ForkProbe)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------
fn push_int(b: &mut TreeBuilder, n: i64) -> usize {
    b.push(CoreFrame::Lit(Literal::LitInt(n)))
}

/// Wrap `root` so it is the LAST node in the tree (eval/compile treat the final
/// node as the root). Mirrors `fixup_root` in proptest_ghc_idioms.rs.
fn fixup_root(tree: &mut CoreExpr, root: usize) -> CoreExpr {
    if root == tree.nodes.len() - 1 {
        return tree.clone();
    }
    let binder = fresh_var();
    let var_idx = tree.nodes.len();
    tree.nodes.push(CoreFrame::Var(binder));
    tree.nodes.push(CoreFrame::LetNonRec {
        binder,
        rhs: root,
        body: var_idx,
    });
    tree.clone()
}

// ===========================================================================
// (A) DENSE Int# literal cases.
//
//   case <scrut> of { 0# -> r0; 1# -> r1; ...; (N-1)# -> r_{N-1}; [_ -> rd] }
//
// `n_alts` in 2..25 consecutive keys 0..n_alts. The bodies are distinct Int#s
// so the value comparison actually witnesses which arm fired. Totality:
//   * has_default = false  -> scrutinee in 0..n_alts (always matches a key)
//   * has_default = true   -> scrutinee may be out-of-range (exercises default)
// ===========================================================================

#[derive(Clone, Debug)]
struct DenseIntSpec {
    n_alts: usize,
    bodies: Vec<i64>,
    default_body: i64,
    has_default: bool,
    /// 0..n_alts when no default; may exceed n_alts when there is one.
    scrut: i64,
}

fn arb_dense_int() -> impl Strategy<Value = DenseIntSpec> {
    (2usize..25, any::<bool>())
        .prop_flat_map(|(n_alts, has_default)| {
            let bodies = prop::collection::vec(-1000i64..1000, n_alts);
            // With a default we allow OOB scrutinees (0..n_alts+8); without, the
            // scrutinee must hit a key.
            let scrut_hi = if has_default {
                (n_alts + 8) as i64
            } else {
                n_alts as i64
            };
            (
                Just(n_alts),
                bodies,
                -1000i64..1000,
                Just(has_default),
                0i64..scrut_hi,
            )
        })
        .prop_map(
            |(n_alts, bodies, default_body, has_default, scrut)| DenseIntSpec {
                n_alts,
                bodies,
                default_body,
                has_default,
                scrut,
            },
        )
}

fn build_dense_int(spec: &DenseIntSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_DENSE_INT);
    if !spec.has_default {
        bump(&N_NO_DEFAULT);
    } else if spec.scrut >= spec.n_alts as i64 {
        bump(&N_HIT_DEFAULT);
    }
    let mut b = TreeBuilder::new();

    let scrut = push_int(&mut b, spec.scrut);
    let mut alts: Vec<Alt<usize>> = Vec::with_capacity(spec.n_alts + 1);
    for (k, &body) in spec.bodies.iter().enumerate().take(spec.n_alts) {
        let body_idx = push_int(&mut b, body);
        alts.push(Alt {
            con: AltCon::LitAlt(Literal::LitInt(k as i64)),
            binders: vec![],
            body: body_idx,
        });
    }
    if spec.has_default {
        let d = push_int(&mut b, spec.default_body);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body: d,
        });
    }
    let binder = fresh_var();
    let root = b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder,
        alts,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (B) SPARSE Int# literal cases.
//
// Same shape as (A) but the alt keys are SCATTERED, large-magnitude values
// including the i64 extremes. This is the case where Cranelift cannot build a
// dense jump table, so the chain stays a long `icmp` ladder. Distinct keys are
// guaranteed by construction (a sorted/deduped sample) so eval and the JIT see
// the same key set.
// ===========================================================================

/// A fixed pool of "interesting" sparse keys: i64 extremes, powers of two,
/// negatives, and a few small values. Sampling distinct keys from this pool
/// gives reproducible, boundary-heavy sparse cases.
const SPARSE_POOL: &[i64] = &[
    i64::MIN,
    i64::MIN + 1,
    -1_000_000_000_000,
    -65_536,
    -1024,
    -42,
    -1,
    0,
    1,
    7,
    255,
    256,
    65_535,
    65_536,
    1_000_000,
    2_147_483_647, // i32::MAX
    2_147_483_648,
    1_000_000_000_000,
    i64::MAX - 1,
    i64::MAX,
];

#[derive(Clone, Debug)]
struct SparseIntSpec {
    /// indices into SPARSE_POOL, deduped+sorted -> distinct keys.
    key_idxs: Vec<usize>,
    bodies: Vec<i64>,
    default_body: i64,
    has_default: bool,
    /// index into the active key set (when no default) or an OOB sentinel.
    scrut_pick: usize,
    /// when has_default: if true, use an OOB scrutinee not in the key set.
    scrut_oob: bool,
}

fn arb_sparse_int() -> impl Strategy<Value = SparseIntSpec> {
    (
        prop::collection::vec(0usize..SPARSE_POOL.len(), 3..12),
        any::<bool>(),
        any::<bool>(),
        0usize..64,
        -1000i64..1000,
        prop::collection::vec(-1000i64..1000, 12),
    )
        .prop_map(
            |(raw_idxs, has_default, scrut_oob, scrut_pick, default_body, bodies)| {
                let mut key_idxs: Vec<usize> = raw_idxs;
                key_idxs.sort_unstable();
                key_idxs.dedup();
                SparseIntSpec {
                    key_idxs,
                    bodies,
                    default_body,
                    has_default,
                    scrut_pick,
                    scrut_oob,
                }
            },
        )
}

fn build_sparse_int(spec: &SparseIntSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_SPARSE_INT);
    let keys: Vec<i64> = spec.key_idxs.iter().map(|&i| SPARSE_POOL[i]).collect();
    // key_idxs is always non-empty (collection min 3, dedup keeps >=1).
    let n = keys.len();

    // Choose a totality-safe scrutinee.
    let scrut_val = if spec.has_default && spec.scrut_oob {
        bump(&N_HIT_DEFAULT);
        // A value guaranteed NOT in `keys`: SPARSE_POOL has bounded entries; pick
        // a fresh sentinel far from any key by construction.
        let mut s = 123_456_789_i64;
        while keys.contains(&s) {
            s += 1;
        }
        s
    } else {
        keys[spec.scrut_pick % n]
    };
    if !spec.has_default {
        bump(&N_NO_DEFAULT);
    }

    let mut b = TreeBuilder::new();
    let scrut = push_int(&mut b, scrut_val);
    let mut alts: Vec<Alt<usize>> = Vec::with_capacity(n + 1);
    for (k, &key) in keys.iter().enumerate() {
        let body_val = spec.bodies[k % spec.bodies.len()];
        let body_idx = push_int(&mut b, body_val);
        alts.push(Alt {
            con: AltCon::LitAlt(Literal::LitInt(key)),
            binders: vec![],
            body: body_idx,
        });
    }
    if spec.has_default {
        let d = push_int(&mut b, spec.default_body);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body: d,
        });
    }
    let binder = fresh_var();
    let root = b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder,
        alts,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (C) Char# literal cases.
//
//   case <char-scrut> of { 'a'# -> r0; 'b'# -> r1; ...; [_ -> rd] }
//
// Two flavours: DENSE (a consecutive ASCII run) and SPARSE (scattered Unicode
// codepoints incl. astral-plane chars). The JIT compares `*c as i64` against the
// unboxed scrutinee; eval compares the LitChar directly. The scrutinee is a
// LitChar and the bodies are Int# (so results are ground & comparable).
// ===========================================================================

const SPARSE_CHARS: &[char] = &[
    '\0',
    '\t',
    ' ',
    '0',
    '9',
    'A',
    'Z',
    'a',
    'z',
    '~',
    'é',
    'λ',
    '中',
    '🦀',
    '\u{10FFFF}',
];

#[derive(Clone, Debug)]
struct CharSpec {
    dense: bool,
    /// dense: number of consecutive chars from 'a'; sparse: count from pool.
    n_alts: usize,
    bodies: Vec<i64>,
    default_body: i64,
    has_default: bool,
    scrut_pick: usize,
    scrut_oob: bool,
}

fn arb_char() -> impl Strategy<Value = CharSpec> {
    (
        any::<bool>(),
        2usize..14,
        prop::collection::vec(-1000i64..1000, 14),
        -1000i64..1000,
        any::<bool>(),
        0usize..64,
        any::<bool>(),
    )
        .prop_map(
            |(dense, n_alts, bodies, default_body, has_default, scrut_pick, scrut_oob)| CharSpec {
                dense,
                n_alts,
                bodies,
                default_body,
                has_default,
                scrut_pick,
                scrut_oob,
            },
        )
}

fn build_char(spec: &CharSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_CHAR);

    // Materialize the active key set.
    let keys: Vec<char> = if spec.dense {
        let n = spec.n_alts.min(26); // 'a'..'z'
        (0..n).map(|i| (b'a' + i as u8) as char).collect()
    } else {
        let n = spec.n_alts.min(SPARSE_CHARS.len());
        SPARSE_CHARS[..n].to_vec()
    };
    let n = keys.len();

    let scrut_char = if spec.has_default && spec.scrut_oob {
        bump(&N_HIT_DEFAULT);
        // A codepoint guaranteed not in `keys`.
        let mut candidate = 'Q'; // not 'a'..'z', not in SPARSE_CHARS
        if keys.contains(&candidate) {
            candidate = '\u{1F600}'; // grinning face, also OOB for both pools
        }
        candidate
    } else {
        keys[spec.scrut_pick % n]
    };
    if !spec.has_default {
        bump(&N_NO_DEFAULT);
    }

    let mut b = TreeBuilder::new();
    let scrut = b.push(CoreFrame::Lit(Literal::LitChar(scrut_char)));
    let mut alts: Vec<Alt<usize>> = Vec::with_capacity(n + 1);
    for (k, &key) in keys.iter().enumerate() {
        let body_val = spec.bodies[k % spec.bodies.len()];
        let body_idx = push_int(&mut b, body_val);
        alts.push(Alt {
            con: AltCon::LitAlt(Literal::LitChar(key)),
            binders: vec![],
            body: body_idx,
        });
    }
    if spec.has_default {
        let d = push_int(&mut b, spec.default_body);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body: d,
        });
    }
    let binder = fresh_var();
    let root = b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder,
        alts,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (D) VERY-WIDE constructor cases.
//
//   case (C_k) of { C_0 -> r0; C_1 -> r1; ...; C_{w-1} -> r_{w-1}; [_ -> rd] }
//
// `width` in 10..17 distinct nullary constructors. We synthesize fresh
// DataConIds well above the standard table (base 200) so they don't alias the
// pre-seeded Maybe/Bool/List/box cons; `build_table_for_expr` auto-registers
// them. The scrutinee is `Con C_k` (nullary). Data dispatch is a linear
// `icmp`-chain over `con_tag == DataConId.0`. Totality:
//   * no default  -> scrutinee is one of the listed cons.
//   * default     -> scrutinee may be a con OUTSIDE the listed subset.
// ===========================================================================

const WIDE_CON_BASE: u64 = 200;

#[derive(Clone, Debug)]
struct WideConSpec {
    width: usize,
    bodies: Vec<i64>,
    default_body: i64,
    has_default: bool,
    scrut_k: usize,
    /// when has_default: scrutinee is a con id ABOVE the listed subset.
    scrut_oob: bool,
}

fn arb_wide_con() -> impl Strategy<Value = WideConSpec> {
    (
        10usize..17,
        prop::collection::vec(-1000i64..1000, 17),
        -1000i64..1000,
        any::<bool>(),
        0usize..64,
        any::<bool>(),
    )
        .prop_map(
            |(width, bodies, default_body, has_default, scrut_k, scrut_oob)| WideConSpec {
                width,
                bodies,
                default_body,
                has_default,
                scrut_k,
                scrut_oob,
            },
        )
}

fn build_wide_con(spec: &WideConSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_WIDE_CON);
    let mut b = TreeBuilder::new();

    // Choose the scrutinee constructor.
    let scrut_id = if spec.has_default && spec.scrut_oob {
        bump(&N_HIT_DEFAULT);
        // A con id above the listed subset (subset is BASE..BASE+width).
        DataConId(WIDE_CON_BASE + spec.width as u64 + 1)
    } else {
        DataConId(WIDE_CON_BASE + (spec.scrut_k % spec.width) as u64)
    };
    if !spec.has_default {
        bump(&N_NO_DEFAULT);
    }

    let scrut = b.push(CoreFrame::Con {
        tag: scrut_id,
        fields: vec![],
    });

    let mut alts: Vec<Alt<usize>> = Vec::with_capacity(spec.width + 1);
    for k in 0..spec.width {
        let body_val = spec.bodies[k % spec.bodies.len()];
        let body_idx = push_int(&mut b, body_val);
        alts.push(Alt {
            con: AltCon::DataAlt(DataConId(WIDE_CON_BASE + k as u64)),
            binders: vec![],
            body: body_idx,
        });
    }
    if spec.has_default {
        let d = push_int(&mut b, spec.default_body);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body: d,
        });
    }
    let binder = fresh_var();
    let root = b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder,
        alts,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// (E) NESTED case-of-case at depth.
//
// A chain of `depth` (3..9) Int# cases where each Case's scrutinee is the RESULT
// of the previous Case. Each level: `case prev of { 0# -> a; 1# -> b; _ -> c }`
// producing an Int# that feeds the next level. The innermost scrutinee is a
// ground Int#. This is the join-point-factory shape (case-of-case) but driven
// purely through nested literal dispatch — stresses the merge-block + scrutinee
// threading at depth on both engines.
// ===========================================================================

#[derive(Clone, Debug)]
struct NestedSpec {
    depth: usize,
    seed: i64,
    /// per level: (key0_body, key1_body, default_body).
    levels: Vec<(i64, i64, i64)>,
}

fn arb_nested() -> impl Strategy<Value = NestedSpec> {
    (3usize..9, -4i64..4)
        .prop_flat_map(|(depth, seed)| {
            let levels = prop::collection::vec((-3i64..3, -3i64..3, -3i64..3), depth);
            (Just(depth), Just(seed), levels)
        })
        .prop_map(|(depth, seed, levels)| NestedSpec {
            depth,
            seed,
            levels,
        })
}

fn build_nested(spec: &NestedSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_NESTED);
    let mut b = TreeBuilder::new();

    // Innermost scrutinee.
    let mut cur = push_int(&mut b, spec.seed);
    for &(b0, b1, bd) in spec.levels.iter().take(spec.depth) {
        let a0 = push_int(&mut b, b0);
        let a1 = push_int(&mut b, b1);
        let ad = push_int(&mut b, bd);
        let binder = fresh_var();
        cur = b.push(CoreFrame::Case {
            scrutinee: cur,
            binder,
            alts: vec![
                Alt {
                    con: AltCon::LitAlt(Literal::LitInt(0)),
                    binders: vec![],
                    body: a0,
                },
                Alt {
                    con: AltCon::LitAlt(Literal::LitInt(1)),
                    binders: vec![],
                    body: a1,
                },
                Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: ad,
                },
            ],
        });
    }
    let mut tree = b.build();
    fixup_root(&mut tree, cur)
}

// ===========================================================================
// (F) PARTIAL constructor set + Default.
//
//   case (C_k) of { C_a -> ra; C_b -> rb; _ -> rd }
//
// A wide constructor type (width 10..17) where the Case lists only a SUBSET
// (1..width-1 alts) of the constructors plus a trailing Default. The scrutinee
// is chosen either INSIDE the listed subset (a matched-constructor path) or
// OUTSIDE it (the default fall-through path). This is the GHC "incomplete
// pattern with wildcard" shape — the partial-set-then-default ordering that the
// existing strategy.rs `gen_case` Maybe/Bool arms never widen.
// ===========================================================================

#[derive(Clone, Debug)]
struct PartialSpec {
    width: usize,
    /// distinct, sorted constructor offsets (subset of 0..width) that are listed.
    listed: Vec<usize>,
    bodies: Vec<i64>,
    default_body: i64,
    scrut_k: usize,
    /// pick the scrutinee from the listed subset (false) or outside it (true).
    scrut_outside: bool,
}

fn arb_partial() -> impl Strategy<Value = PartialSpec> {
    (10usize..17)
        .prop_flat_map(|width| {
            // list a proper subset: 1..width-1 of the offsets.
            let listed = prop::collection::vec(0usize..width, 1..width);
            (
                Just(width),
                listed,
                prop::collection::vec(-1000i64..1000, width),
                -1000i64..1000,
                0usize..64,
                any::<bool>(),
            )
        })
        .prop_map(
            |(width, raw_listed, bodies, default_body, scrut_k, scrut_outside)| {
                let mut listed = raw_listed;
                listed.sort_unstable();
                listed.dedup();
                PartialSpec {
                    width,
                    listed,
                    bodies,
                    default_body,
                    scrut_k,
                    scrut_outside,
                }
            },
        )
}

fn build_partial(spec: &PartialSpec) -> CoreExpr {
    reset_ctrs();
    bump(&N_PARTIAL);
    let mut b = TreeBuilder::new();

    // `listed` is a proper-or-full subset of 0..width; compute the "outside" set.
    let listed_set: std::collections::HashSet<usize> = spec.listed.iter().copied().collect();
    let outside: Vec<usize> = (0..spec.width)
        .filter(|k| !listed_set.contains(k))
        .collect();

    // Choose scrutinee constructor offset.
    let want_outside = spec.scrut_outside && !outside.is_empty();
    let scrut_off = if want_outside {
        bump(&N_HIT_DEFAULT);
        outside[spec.scrut_k % outside.len()]
    } else {
        // listed is non-empty by construction.
        spec.listed[spec.scrut_k % spec.listed.len()]
    };
    let scrut = b.push(CoreFrame::Con {
        tag: DataConId(WIDE_CON_BASE + scrut_off as u64),
        fields: vec![],
    });

    let mut alts: Vec<Alt<usize>> = Vec::with_capacity(spec.listed.len() + 1);
    for (i, &off) in spec.listed.iter().enumerate() {
        let body_val = spec.bodies[i % spec.bodies.len()];
        let body_idx = push_int(&mut b, body_val);
        alts.push(Alt {
            con: AltCon::DataAlt(DataConId(WIDE_CON_BASE + off as u64)),
            binders: vec![],
            body: body_idx,
        });
    }
    // Always trailing Default (the point of this shape).
    let d = push_int(&mut b, spec.default_body);
    alts.push(Alt {
        con: AltCon::Default,
        binders: vec![],
        body: d,
    });

    let binder = fresh_var();
    let root = b.push(CoreFrame::Case {
        scrutinee: scrut,
        binder,
        alts,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

// ===========================================================================
// Properties.
//
// 400 cases each. `serial` because the JIT effect machine + the fork guard are
// not safe to run concurrently. A NURSERY pair (64KB/4KB) is swept inside
// each property's `DiffConfig` so the small-nursery GC path is covered
// without a second harness. Each property drives `TestRunner` directly (not
// the `proptest!` macro) so its `ReachCounter` closes over the SAME process
// as the cases it counts and its floor assertion can actually fail.
// ===========================================================================

fn cfg() -> Config {
    let mut c = Config::with_cases(400);
    c.max_shrink_iters = 6000;
    // Counterexample persistence resolves against the calling source file. The
    // `proptest!` macro supplies it implicitly; a hand-built `Config` leaves it
    // `None`, and proptest then neither replays the checked-in seeds nor saves
    // a new one — a red run's seed would die with the process.
    c.source_file = Some(file!());
    c
}

#[test]
#[serial]
fn prop_dense_int() {
    let reach = ReachCounter::new("case-dispatch/dense_int");
    let mut runner = TestRunner::new(cfg());
    runner
        .run(&arb_dense_int(), |spec| {
            let expr = build_dense_int(&spec);
            prop_assert!(expr.nodes.len() <= 256);
            check(expr, &dcfg("case-dispatch/dense_int"), &reach)
        })
        .unwrap();
    reach.assert_floor(0.90);
    eprintln!(
        "SHAPE FREQ dense_int: n={} no_default={} hit_default={}",
        N_DENSE_INT.load(Ordering::Relaxed),
        N_NO_DEFAULT.load(Ordering::Relaxed),
        N_HIT_DEFAULT.load(Ordering::Relaxed),
    );
}

#[test]
#[serial]
fn prop_sparse_int() {
    let reach = ReachCounter::new("case-dispatch/sparse_int");
    let mut runner = TestRunner::new(cfg());
    runner
        .run(&arb_sparse_int(), |spec| {
            let expr = build_sparse_int(&spec);
            check(expr, &dcfg("case-dispatch/sparse_int"), &reach)
        })
        .unwrap();
    reach.assert_floor(0.90);
    eprintln!(
        "SHAPE FREQ sparse_int: n={} no_default={} hit_default={}",
        N_SPARSE_INT.load(Ordering::Relaxed),
        N_NO_DEFAULT.load(Ordering::Relaxed),
        N_HIT_DEFAULT.load(Ordering::Relaxed),
    );
}

#[test]
#[serial]
fn prop_char() {
    let reach = ReachCounter::new("case-dispatch/char");
    let mut runner = TestRunner::new(cfg());
    runner
        .run(&arb_char(), |spec| {
            let expr = build_char(&spec);
            check(expr, &dcfg("case-dispatch/char"), &reach)
        })
        .unwrap();
    reach.assert_floor(0.90);
    eprintln!(
        "SHAPE FREQ char: n={} no_default={} hit_default={}",
        N_CHAR.load(Ordering::Relaxed),
        N_NO_DEFAULT.load(Ordering::Relaxed),
        N_HIT_DEFAULT.load(Ordering::Relaxed),
    );
}

#[test]
#[serial]
fn prop_wide_con() {
    let reach = ReachCounter::new("case-dispatch/wide_con");
    let mut runner = TestRunner::new(cfg());
    runner
        .run(&arb_wide_con(), |spec| {
            let expr = build_wide_con(&spec);
            check(expr, &dcfg("case-dispatch/wide_con"), &reach)
        })
        .unwrap();
    reach.assert_floor(0.90);
    eprintln!(
        "SHAPE FREQ wide_con: n={} no_default={} hit_default={}",
        N_WIDE_CON.load(Ordering::Relaxed),
        N_NO_DEFAULT.load(Ordering::Relaxed),
        N_HIT_DEFAULT.load(Ordering::Relaxed),
    );
}

#[test]
#[serial]
fn prop_nested_case() {
    let reach = ReachCounter::new("case-dispatch/nested_case");
    let mut runner = TestRunner::new(cfg());
    runner
        .run(&arb_nested(), |spec| {
            let expr = build_nested(&spec);
            check(expr, &dcfg("case-dispatch/nested_case"), &reach)
        })
        .unwrap();
    reach.assert_floor(0.90);
    eprintln!("SHAPE FREQ nested: n={}", N_NESTED.load(Ordering::Relaxed));
}

#[test]
#[serial]
fn prop_partial_default() {
    let reach = ReachCounter::new("case-dispatch/partial_default");
    let mut runner = TestRunner::new(cfg());
    runner
        .run(&arb_partial(), |spec| {
            let expr = build_partial(&spec);
            check(expr, &dcfg("case-dispatch/partial_default"), &reach)
        })
        .unwrap();
    reach.assert_floor(0.90);
    eprintln!(
        "SHAPE FREQ partial: n={} hit_default={}",
        N_PARTIAL.load(Ordering::Relaxed),
        N_HIT_DEFAULT.load(Ordering::Relaxed),
    );
}

// ===========================================================================
// Deterministic edge fixtures: hand-built minimal programs at the exact
// boundaries the property strategies sweep. These run unconditionally (not
// proptest) so a regression at a specific edge is named, not just shrunk.
// ===========================================================================

/// Helper: run one hand-built program through both engines and assert agreement.
fn assert_jit_eq_eval(tree: &CoreExpr, what: &str) {
    use tidepool_eval::{env_from_datacon_table, eval, VecHeap};

    let table = tidepool_testing::proptest::build_table_for_expr(tree);
    let mut heap = VecHeap::new();
    let env = env_from_datacon_table(&table);
    let ev = eval(tree, &env, &mut heap).unwrap_or_else(|e| panic!("[{what}] eval failed: {e:?}"));

    let jit = JitEffectMachine::compile(tree, &table, 64 * 1024)
        .and_then(|mut m| m.run_pure())
        .unwrap_or_else(|e| panic!("[{what}] JIT failed but eval=Ok({ev:?}): {e:?}"));

    assert!(
        values_equal(&ev, &jit),
        "[{what}] JIT/eval divergence: eval={ev:?} jit={jit:?}"
    );
}

/// Build `case scrut of <int alts> [default]` returning the matched body.
fn case_int(scrut: i64, keys: &[(i64, i64)], default: Option<i64>) -> CoreExpr {
    reset_ctrs();
    let mut b = TreeBuilder::new();
    let s = push_int(&mut b, scrut);
    let mut alts = Vec::new();
    for &(k, body) in keys {
        let body_idx = push_int(&mut b, body);
        alts.push(Alt {
            con: AltCon::LitAlt(Literal::LitInt(k)),
            binders: vec![],
            body: body_idx,
        });
    }
    if let Some(d) = default {
        let di = push_int(&mut b, d);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body: di,
        });
    }
    let binder = fresh_var();
    let root = b.push(CoreFrame::Case {
        scrutinee: s,
        binder,
        alts,
    });
    let mut tree = b.build();
    fixup_root(&mut tree, root)
}

#[test]
#[serial]
fn edge_dense_first_last_default() {
    // dense 0..8; hit the first key, the last key, and the default.
    let keys: Vec<(i64, i64)> = (0..8).map(|k| (k, 100 + k)).collect();
    assert_jit_eq_eval(&case_int(0, &keys, Some(-1)), "dense_first");
    assert_jit_eq_eval(&case_int(7, &keys, Some(-1)), "dense_last");
    assert_jit_eq_eval(&case_int(99, &keys, Some(-1)), "dense_default");
}

#[test]
#[serial]
fn edge_sparse_i64_extremes() {
    // sparse with both i64 extremes as keys; hit each + default.
    let keys = [(i64::MIN, 1), (-1, 2), (0, 3), (i64::MAX, 4)];
    assert_jit_eq_eval(&case_int(i64::MIN, &keys, Some(-9)), "sparse_min");
    assert_jit_eq_eval(&case_int(i64::MAX, &keys, Some(-9)), "sparse_max");
    assert_jit_eq_eval(&case_int(0, &keys, Some(-9)), "sparse_zero");
    assert_jit_eq_eval(&case_int(42, &keys, Some(-9)), "sparse_default");
}

#[test]
#[serial]
fn edge_no_default_total() {
    // No default: scrutinee MUST match. First key, middle, last.
    let keys = [(0i64, 10i64), (5, 20), (100, 30)];
    assert_jit_eq_eval(&case_int(0, &keys, None), "nodef_first");
    assert_jit_eq_eval(&case_int(5, &keys, None), "nodef_mid");
    assert_jit_eq_eval(&case_int(100, &keys, None), "nodef_last");
}

#[test]
#[serial]
fn edge_char_dense_and_astral() {
    reset_ctrs();
    // case 'c' of { 'a' -> 1; 'b' -> 2; 'c' -> 3; _ -> 0 }
    let build = |scrut: char, keys: &[(char, i64)], default: i64| -> CoreExpr {
        reset_ctrs();
        let mut b = TreeBuilder::new();
        let s = b.push(CoreFrame::Lit(Literal::LitChar(scrut)));
        let mut alts = Vec::new();
        for &(k, body) in keys {
            let body_idx = push_int(&mut b, body);
            alts.push(Alt {
                con: AltCon::LitAlt(Literal::LitChar(k)),
                binders: vec![],
                body: body_idx,
            });
        }
        let di = push_int(&mut b, default);
        alts.push(Alt {
            con: AltCon::Default,
            binders: vec![],
            body: di,
        });
        let binder = fresh_var();
        let root = b.push(CoreFrame::Case {
            scrutinee: s,
            binder,
            alts,
        });
        let mut tree = b.build();
        fixup_root(&mut tree, root)
    };
    let keys = [('a', 1), ('b', 2), ('c', 3), ('🦀', 4), ('\u{10FFFF}', 5)];
    assert_jit_eq_eval(&build('c', &keys, 0), "char_ascii");
    assert_jit_eq_eval(&build('🦀', &keys, 0), "char_astral");
    assert_jit_eq_eval(&build('\u{10FFFF}', &keys, 0), "char_max");
    assert_jit_eq_eval(&build('Z', &keys, 0), "char_default");
}

#[test]
#[serial]
fn edge_wide_con_16() {
    reset_ctrs();
    // 16 nullary cons; scrutinize the first, last, and an out-of-subset con.
    let build = |scrut_off: u64, has_default: bool| -> CoreExpr {
        reset_ctrs();
        let mut b = TreeBuilder::new();
        let scrut = b.push(CoreFrame::Con {
            tag: DataConId(WIDE_CON_BASE + scrut_off),
            fields: vec![],
        });
        let mut alts = Vec::new();
        for k in 0..16u64 {
            let body_idx = push_int(&mut b, 1000 + k as i64);
            alts.push(Alt {
                con: AltCon::DataAlt(DataConId(WIDE_CON_BASE + k)),
                binders: vec![],
                body: body_idx,
            });
        }
        if has_default {
            let di = push_int(&mut b, -7);
            alts.push(Alt {
                con: AltCon::Default,
                binders: vec![],
                body: di,
            });
        }
        let binder = fresh_var();
        let root = b.push(CoreFrame::Case {
            scrutinee: scrut,
            binder,
            alts,
        });
        let mut tree = b.build();
        fixup_root(&mut tree, root)
    };
    assert_jit_eq_eval(&build(0, false), "wide_first");
    assert_jit_eq_eval(&build(15, false), "wide_last");
    // out-of-subset con with a default present (con 99 not listed).
    assert_jit_eq_eval(&build(99, true), "wide_default");
}
