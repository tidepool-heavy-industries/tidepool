# Plan — Lazy-default Let/LetRec spine (Pattern A consolidation)

## Context
Five bugs this wave were one root cause wearing different hats: **the JIT's Let/LetRec
spine evaluates simple bindings EAGERLY, but GHC Core `let` is NON-STRICT** (strictness
in Core is expressed via `case`, never `let`). The patches:

| Facet | What it patched | Where |
|---|---|---|
| #1 (`5ff287e`) | defer `case error of {}` CAF | error-deferral walker (`emit/expr.rs`) |
| follow-on (`2538afa`) | defer `raise# exc` binding | error-deferral walker |
| C (in progress) | thunkify non-trivial `LetNonRec` RHS | `LetNonRec` work-stack arm |
| Gap A (`fix.support-gaps d6bd81d`) | defer `LetRec` Var-alias to a pending sibling | `emit_letrec_phases` Phase 2.5 |

All four are "the spine is too eager." The unifying fix is **lazy-default**: thunk every
not-trivially-resolvable simple binding, force on demand — exactly GHC's `let` semantics.
It subsumes all four AND retires the eager-ordering machinery they work around.

## The unifying rule
A **simple binding** (RHS is neither `Lam` nor `Con` — i.e. App-CAF, Var-alias, `case`,
`error`/`raise#`) in `LetNonRec`/`LetRec` is emitted:
- **Eager** iff *trivially resolvable now*: a `Lit`, or a `Var`/`Con` referencing only
  values already in `env`.
- **Else a thunk** (`emit_thunk`), forced on demand.

`Lam`/`Con` bindings keep their pre-allocation + knot-tie fill (recursive closures / cyclic
data — `fibs`, `xs = 1:xs` — genuinely need the pointer pre-allocated). Only the **simple**
bindings change.

## What it subsumes (the payoff)
- **C** *is* this rule for `LetNonRec` (Stage 0).
- **Gap A**: a Var-alias to a still-pending sibling → a thunk referencing the sibling's
  thunk → resolves on force. No eager `emit_subtree` → no `UnresolvedVar` trap. (The 3-line
  Gap-A patch becomes unnecessary.)
- **#1 / follow-on**: `let x = error … / raise# …` → a thunk; forcing it evaluates the
  error → throws, matching eval's laziness. The 4 duplicated error-deferral walkers
  (`rhs_is_error_call`, `…_in_group`, `extract_error_kind`, `extract_error_message`) +
  `emit_error_binding` become removable — *provided* a forced error-thunk produces the same
  message/kind/`FailureClass` as the poison closure (verify before removing).
- **The 5-phase eager-ordering complexity**: Phase 2.5 (eager Var-alias) + Phase 3c
  (eager deferred-simple eval + incremental Con-fill + topo-sort) largely **retire** —
  thunks resolve lazily, so there's no dependency-order to compute. (Phases 1/3a/3b/3d, the
  Lam/Con knot-tie, stay.)

## Infrastructure already present (low-friction)
- `emit_thunk` (`expr.rs:1197`) — captures free vars, declares a thunk entry fn. Already used.
- `is_trivial_field` (`:925`) — the eager/thunk discriminator. Already used for Con fields.
- The deferred-simple region (`:2840+`) and `letrec_post_simple_step` ALREADY choose
  thunk-vs-eager per binding and already store thunks for non-trivial Con fields. So the
  change is mostly **flipping the default + tightening the eager fast-path to
  "resolvable-now"**, not new machinery.

## Stages (each independently green + landable)
- **Stage 0 — `LetNonRec` (C): DONE + landed `0617c60`.** Thunkify non-trivial RHS;
  trivial fast-path eager (`is_trivial_field`). Arm = `error→poison | trivial→eager | else→thunk`.
  Read class CLOSED (DIVERGENCE BUGS 3→0). **Perf measured: <1%** (worst-case App-let used
  strictly +0.7%; control +0.9% — both noise). New `lazy_let_guard.rs` (corecursive-let
  terminates; trivial-let eager). Walker-redundancy CONFIRMED: the LetNonRec error-walker is
  now redundant, but the **LetRec-phase walkers (#1/follow-on live there) stay load-bearing
  until Stage 1.**
- **Stage 1 — `LetRec` simple bindings:** apply the unifying rule in `emit_letrec_phases`.
  Replace Phase 2.5's eager Var-alias + the eager deferred-simple eval with: eager iff
  resolvable-now, else thunk. Subsumes Gap A; keep the Lam/Con knot-tie phases. Net code
  *reduction* (the topo-order/incremental-fill complexity shrinks).
- **Stage 2 — retire the error-deferral patches: DONE + landed `c5397fc` (on main 2026-06-21).**
  Parity PROVEN before removal: `runtime_error_dynamic` (thunk-force path) forwards to the SAME
  `runtime_error_with_msg` the message-poison uses → byte-identical by construction; empirically
  identical `"yield error: Haskell error: boom"` across LetNonRec / LetRec / direct-force; and a
  route-ALL-error-RHS-through-lazy-default stress run kept the net 0-divergence (incl. the `foldl1`
  floated-error-CAF the walker existed for). **Precision correction to "retire the 4 walkers":**
  only **2** were let-site-exclusive — `emit_error_binding` + `rhs_is_error_call_in_group` (REMOVED,
  with the 3 let-interception sites: LetNonRec arm + LetRec all-simple + Phase 2.5). The other **3**
  — `rhs_is_error_call`, `extract_error_kind`, `extract_error_message` — are SHARED with the
  conditional-position inline `EmitFrame::Raise` lowering (`collapse_frame`) and were KEPT. Guard:
  `error_binding_guard.rs` (3 tests, fixture-independent). Net Stage 1 −52 + Stage 2 −158 = **−210
  lines** while closing the eager-eval class at the root. Carried flag: 2 gotcha `loud_fail_*_overflow`
  probes fail on a stale system extract binary (pre-existing, isolated against reverted main) —
  recheck at deploy after the binary refresh.
- **Stage 3 — strictness-analysis pass: NOT NEEDED (settled by C's perf data).** GHC -O2's
  strictness analysis already lowers strict-used `let`s to `case` (which lazy-default never
  touches), so what survives as `let` in Core is genuinely non-strict — thunkifying it is
  correct AND nearly free (<1%). The eager-`let` "optimization" was redundant with GHC's pass
  and unsound for the lazy lets. No follow-up unless Stage 1's broader thunking surprises the
  benchmark (re-measure; not expected).

## Risks
- **Perf:** more thunks (alloc + force). Mitigated by the eager fast-path and Stage 3.
  *Measure* — C's hot-loop benchmark is the first data point; re-measure after Stage 1's
  broader thunking.
- **Knot-tie correctness:** the Lam/Con pre-alloc + fill (Phases 1/3a/3b/3d) must keep
  working when simple-binding references are now thunks. The fill paths already store thunks
  for non-trivial fields, so likely fine — but the documented "SIGSEGV when a later simple
  binding calls a closure that pattern-matches a not-yet-filled Con" case (Phase 3c's
  incremental fill) must be re-verified under lazy eval.
- **Error-reporting parity (Stage 2):** don't remove `emit_error_binding` until a forced
  error-thunk demonstrably matches the poison closure's message/kind/`FailureClass`.

## Verification net (build BEFORE refactoring — the "well-tested first" gate)
Fixture-independent synthetic guards (TreeBuilder → `check_jit_vs_eval`), kept permanently:
1. corecursive `let x = F k` consumed boundedly → eval==JIT terminate (C / lazy-let).
2. trivial `let x = Lit/Var-resolved` stays eager (fast-path; assert no thunk via coverage/trace).
3. `LetRec` Var-alias to a pending sibling resolves (Gap A class).
4. bottoming `let e = error/raise#` off the live path → deferred; forced → throws with msg
   (Stage 2 parity).
Plus the full corpus (0 unexpected), `haskell_suite` 217, differentials, bignum, captured —
green at every stage; dev-shell fmt + clippy per stage.

## Sequencing vs current work
Land the small *verified* fixes first (user's "working+tested then consolidate"): **C**
(Stage 0) → **Gap A + Gap B** (`fix.support-gaps`). THEN the consolidation: **Stage 1**
(subsumes/retires Gap A) → **Stage 2** (retires the walkers). Stage 3 only if perf demands.
Each stage is a behavior-preserving refactor validated against the net above.
