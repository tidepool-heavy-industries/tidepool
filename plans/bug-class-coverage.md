# Bug-Class Coverage Ledger

**Goal:** rule out JIT bug *classes* principledly — not just find instances. The
captured-real-Core corpus + coverage metric is the instrument; this ledger is the
index of which classes exist, how each is netted, how well covered, and whether
ruled out.

## Rule-out hierarchy (how principled is a green?)
1. **Instance** — a hand-written test passes (rules out nothing).
2. **Coverage-measured** — a corpus exercises the class AND the metric proves the
   span → "ruled out to X%".
3. **Generator-covers-structure** — proptest over a generator that provably spans
   the class's shapes.
4. **Structural** — the bug made unrepresentable (typed invariant) or the input
   space is finite + enumerated.

Climb each class as high as feasible. A green at level 1 is luck; level 2+ is evidence.

## Nets (instruments)
- **captured-real-Core differential** — real GHC `-O2` Core (`--all-closed` → CBOR)
  through `check_jit_vs_eval_captured` (real `meta.cbor` table; explicit
  `CapturedOutcome`, no silent skips). The ONLY net that sees the real Core
  distribution. *[double-literal: `captured_real_core.rs` + corpus harness LANDED
  `64726b6`: `Corpus.hs` ~50 bindings, `regen-corpus.sh` (fails on SKIP),
  `real_core_corpus.rs` — 58 programs, 44 MATCH, 14 KNOWN, 0 unexpected]*
- **synthetic value-repr proptest** — hand-built `CoreExpr` over a grammar; generic
  dispatch/closure shapes. *[proptest-widen: `proptest_ghc_idioms_widen.rs`,
  `bc66e36`+`3045c04`]* Proven: cannot reach #1/#2 (synthetic table has no real
  Integer/BigNat repr).
- **joinrec differential** — recursive-join eval (trampoline) un-blinds the oracle
  for `joinrec`. *[eval-joinrec: `9f93273`]*
- **golden oracle** — eval-vs-expected, for SHARED-frontend bugs the differential is
  blind to (both engines wrong).
- **emit-path coverage metric** — instruments which JIT decision-points / `PrimOpKind`s
  the corpus exercises → turns "found none" into "% covered". *[in progress, double-literal]*
- **capture-time loud-fail** — unsupported FFI/external = compile error / `SKIP`,
  surfaced by name.

## Bug-class taxonomy
| Class | Best net | Level | Status |
|---|---|---|---|
| Constructor-repr (tag/field, non-uniform fields) | captured-real-Core + synthetic value-repr | 2 | **#1 FIXED + landed `5ff287e`.** CORRECTED diagnosis: NOT a tag misread (tag read correctly as IS) — it was **eager-eval of a bottoming `case error "…" of {}` CAF** (roundingMode#'s unreachable `IN→error` lifted to an unlifted Int# CAF); the error-deferral walker only checked the App-spine head, missing the `Case` RHS. Fix: the 4 error-walkers **follow the case scrutinee** (not alt bodies → branch-local errors stay un-poisoned). All `fromIntegral` Integer→Double now MATCH; DIVERGENCE BUGS 10→6. Follow-on below. |
| Eager-eval of bottoming LetRec bindings | captured-real-Core (big_double / Rational) | 2 | **FIXED + landed `2538afa` — closes the ORIGINAL `1.0e308` bug.** Same class as #1, different shape: a `raise# exc` LetRec binding (rationalToDouble's overflow throw) the deferral walker missed (handled `Var`/`App`/`Case`, not `PrimOp Raise`) → strict spine threw eagerly. Fix: walkers treat `PrimOp Raise` RHS as bottoming → deferred (inline conditional `raise#` unchanged). big_double_* un-ignored; DIVERGENCE BUGS 6→4. **The whole GHC.Float Double-conversion class (fromIntegral + Double-literal + Rational) is now correct** — two related root causes, both eager-eval of bottoming bindings the deferral check didn't recognize. |
| GADT equality-evidence arity | captured-real-Core (`gadtEval`) | 2 | **FIXED + landed `ef237a4`.** Root: `translateAlt`'s binder filter dropped `TyVar`s but not `CoVar`s, so a GADT alt bound the equality-evidence coercion var as an extra field — off-by-one vs the Con build (which drops it via `valueRepArity`) → eval `ArityMismatch`, JIT read past the Con → SIGSEGV. Fix (+1 line): `filter (not isTyVar && not isCoVar)`. `gadtEval` BOTH-BUG → MATCH; DIVERGENCE BUGS 4→3. (suite_cbor intentionally NOT regen'd — fix only affects CoVar-binding alts; the GHC-Unique drift would be orthogonal churn.) |
| Case-dispatch (n-alts/default/mixed/nested) | synthetic + corpus | 2–3 | green so far (widen: 0 divergence) |
| Join points | joinrec differential + proptest | 2–3 | **joinrec RULED OUT** (no divergence; dual-impl + JIT agree). Eval fix = **trampoline** (`9f93273`/`f222fc0`, O(1) stack, 200k jumps @2MB) — the naive knot (`793b82f`) is non-TCO, SIGSEGVs `prop_joinrec` @2MB. `join-crosses-lambda` OPEN (`bug1_join_crosses_lambda`, `#[ignore]`'d) |
| Translation: unboxed-1-tuple build (#2 shared half) | captured-real-Core (reads) | 2 | **FIXED + landed `7faab58`.** CORRECTED (not `~R#` — serializer strips newtype casts): GHC wraps ReadP's CPS fn in `MkSolo#` `(# f #)` (no runtime rep = its field); Translate's Con-BUILD boxed it while the case side treats `(# x #)` as identity → boxed Con in function position. Fix: erase saturated 1-elem unboxed-tuple builds to their field (fixes EVERY 1-tuple program). **Eval 100% correct on reads.** + robustness `1e82fe1` (`try_borrow_mut` — a Drop must not panic). |
| JIT-CPS runaway recursion (#2 JIT half) | captured-real-Core (reads) | 2 | **OPEN — last DIVERGENCE residual.** Post-FIX#1, reads flipped BothFail → **JitOnlyFailure**: the JIT doesn't TCO ReadP's `(a→P b)→P b` continuation chains, so `read "42"` blows `MAX_CALL_DEPTH=20_000` → StackOverflow/SIGSEGV; eval is bounded. Distinct JIT-codegen root cause (bigger). DIVERGENCE BUGS = 3 (all reads, JIT-only). |
| FFI/extractor primops (`sqrtFloat#` / Float family) | captured-real-Core | 2 | **FIXED on `fix.support-gaps` `1d562da` (Gap B, pending land).** Native `FloatSqrt`/`FloatFabs` PrimOpKinds (cranelift f32 sqrt/fabs, bit-exact) + Float transcendentals desugared in Translate to the Double libm path. `floatVal` un-SKIPs + 2 new fixtures MATCH; corpus 112→115. Independent of C. |
| LetRec Var-alias eager-emit (was mis-filed "unresolved external") | captured-real-Core | 2 | **FIXED on `fix.support-gaps` `d6bd81d` (Gap A, pending land). CORRECTED diagnosis (6th):** NOT an unresolved external — the yielded VarId is a LOCAL Rec binder. After resolveExternals merges to one Rec, `result = Var(start); body = Var(result)` where `start` is a still-PENDING sibling; `emit_letrec_phases` Phase 2.5 eagerly emit_subtree'd the bare-Var alias → lookup miss → UnresolvedVar trap. Fix (3 lines): fast-path a Var-alias only when its target is already in env, else defer (topo-sort orders it after). `sum`/`properFraction`/`realToFrac` → MATCH; SUPPORT GAPS 4→1. **Another facet of the eager-spine class** (Pattern A). `cycleTake` stays — genuinely different (cycle's knot-tied self-referential CAF, needs thunk back-patch; deferred). Boxed-array eval (Gap C): codegen has all ~25 ops; eval lacks a `Value::BoxedArray` variant — ~1 day, biggest+rarest, DEFER. |
| GADT equality-evidence arity | captured-real-Core | 2 | **OPEN — NEW, severe.** `gadtEval` (`AddE :: Expr Int -> Expr Int -> Expr Int`): eval `ArityMismatch` (eqspec arg, CLAUDE.md #8 `valueRepArity`), JIT **SIGSEGV**. Both engines |
| Oracle completeness (eval primop gaps) | — (eval is the oracle) | — | **bit-counts (PopCnt/Ctz ×10) DONE** (`f222fc0`, to spec, eval==JIT) → Double-literal/Rational paths un-blinded. Audit: 37 eval gaps total; **remaining = boxed `Array#`/`SmallArray#`/`IndexArray` (25)** — need a new boxed-array `Value` variant (separate scoped task; those paths stay oracle-blind till then). `TagToEnum` desugars to `case` in Translate (never reaches eval). |
| Laziness/strictness/thunk | captured-real-Core corpus | 2 | **RULED OUT** — lazy Con fields, bang-let, seq chains, bounded infinite consumption (take n [0..], fibs, repeat, iterate), shared thunks: all MATCH |
| GC/heap (copying collector, frame walker) | captured-real-Core corpus | 2 | **RULED OUT (≤~1k allocs)** — map/filter/concat/reverse/tree-build/string-alloc churn the JIT copying GC; eval VecHeap == JIT GC on the value. (Cap ~1k: eval's recursive Drop overflows the host stack at 10k+ — host-stack-overflow-class, not a JIT bug) |
| TCO/stack-safety | stack-safety work + 200k-jump test | 2 | mostly addressed |
| Effect machine (Union, continuation queue, lazy results) | lazy-results tests | 1–2 | partial |
| Robustness: eval timeout | — | 0 | **OPEN** — timeout is cooperative (checked at effect-yield boundaries); a pure-CPU spin never yields → never interrupted. Observed: a 6h-spinning eval. |

## Next actions
**COVERAGE SNAPSHOT (P2, `74fdeae`, `TIDEPOOL_EMIT_COVERAGE`):**
- structural (EmitFrame/case/con/let): **24/24 = 100%** — corpus saturates the taxonomy; curation plateaued here.
- primops: **107/230 = 47% — at the surface-Haskell CEILING** (curation plateaued). The unhit
  53% are opcodes real programs CAN'T emit: GHC rewrites them away before Core (`x-c`→`x+negate c`
  ⇒ DoubleSub unreachable; `x/=y`→`not(x==y)`; Int64/Word64 collapse on 64-bit; `fromIntegral::Word8`
  →`and# 0xFF` not narrow8; TagToEnum/SeqOp→`case`), OR are the boxed-array eval-gap, OR need Data.Text.
  **⇒ the corpus covers the REAL-program (surface-Haskell) distribution. The unhit tail is
  IMPLEMENTED-BUT-UNTESTED, NOT dead** — "our patterns didn't reach it" is EMPIRICAL, not a proof
  the opcode is never emitted (other phrasings / GHC versions / flags may; the JIT implements all
  230). Untested codegen is exactly where latent bugs hide — precedent: the dormant `timesInt2#`
  multi-output slot-order bug found during bignum bring-up. So the tail is a **real coverage gap to
  close**, not low-value: (1) harder surface patterns for opcodes a different phrasing can reach
  (also confirms they're live), (2) the parametric generator (emit Core directly) for the
  genuinely-surface-unreachable residual. Three incompleteness layers: extractor (`sqrtFloat#`),
  eval (boxed arrays), JIT (the divergence bugs).
- **COVERAGE PHASE COMPLETE** (108 corpus programs, green): structural saturated, primops at the
  real-distribution ceiling, laziness/GC/joinrec ruled out, effect-machine flagged for a separate net.

**IN FLIGHT:**
- `double-literal`: corpus harness + coverage metric DONE (`64726b6`, `74fdeae`). Now:
  (a) **close the 37% primop gap via targeted curation** (enumerable opcodes → deterministic;
  generator stays reserved for shape-COMBINATIONS, not opcode coverage), (b) laziness/GC
  admitted-hole families. Pulls eval-joinrec `f222fc0` (trampoline + bit-counts) for the oracle.

**NEEDED (queued):**
- **Coverage-enabling eval-oracle completion** (in-scope, like the joinrec fix —
  these are oracle gaps, not product fixes): implement eval `PopCnt` so the
  Double-literal/Rational→Double paths stop both-failing and the JIT side becomes
  visible. Each eval gap fixed un-blinds a slice of the differential.
- **Merge checkpoint: DONE — LANDED on `origin/main` @ `a68c4a7`** (clean zero-conflict
  3-way merge of all keepers: native-bignum backend + corpus + coverage + joinrec trampoline
  + PopCnt/Ctz + captured net + proptest-widen/eval-joinrec test nets). Verified: all
  merge-validators green; `bignum_native` proven environmental (passes with a freshly-built
  native binary); `T.strip` overflow confirmed pre-existing (identical on base). Pre-push gate
  (dev-shell fmt + clippy -D warnings) passed.
  - **DEPLOY PENDING (coordinate with concurrent LLM):** the live `~/.local/bin/tidepool-extract-bin`
    is still the GMP binary; native goes live only after rebuild+install + cache clear — don't
    yank it mid-session.
  - **Gotcha:** the pre-push hook runs `nix develop` cargo fmt --check + clippy -D warnings against
    the **cwd's worktree** (not the pushed commit); dev-shell rustfmt ≠ rustup rustfmt → format
    with `nix develop --command cargo fmt --all` and push from the fmt-clean worktree.
- **Net the admitted holes:** laziness/strictness, GC/heap, effect-machine — add
  corpus families + (where possible) differential coverage.
- **Fix backlog (gated on user go), corpus-prioritized:**
  1. `sum [1..100]` / `properFraction` / `realToFrac` → JIT `UnresolvedVar` (NEW;
     common idiom, high-impact resolver gap).
  2. GADT eqspec arity → **SIGSEGV** (NEW; hard crash; `valueRepArity` #8 territory).
  3. #1 `roundingMode#:IN` (ready, ~30-node target; now known magnitude-independent).
  4. #2 ReadP `~R#` coercion lowering; `join-crosses-lambda`; timeout-preemption.
- **Deferred:** parametric-template generator — only if the coverage metric shows
  curation plateaued.

**Killed:** the synthetic worktree workflow (`w4rhnlius`) — stale base + secondary
(synthetic provably can't reach the key bugs). Superseded by the captured-real-Core corpus.
