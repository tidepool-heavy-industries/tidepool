# Gotcha Audit — Layer 0 (2026-06-11)

Empirical classification of every documented dangerous pattern / JIT
limitation. Every verdict is backed by an executable probe (eval-server
smoke test, `tidepool-runtime` example, or a `haskell_verified` template).
Server baseline: tidepool MCP build 2026-06-10 15:55 running from this
worktree; codegen fixes on this branch noted per-item.

Verdicts: **FIXED** (works now — docs updated), **FIXABLE** (fixed on this
branch — commit cited), **STANDING** (still real — precise boundary given).

| # | Item | Probe | Verdict | Evidence |
|---|------|-------|---------|----------|
| 1 | `read`/`reads` crash (SIGILL) | `read "42" :: Int`, `:: Double` | STANDING (by design), UX fixed | Fails at COMPILE time now: "Unsupported FFI call: ghc-bignum:__gmpn_add_1" — no runtime SIGILL. GMP hint added to `mapFfiCall` (this branch). `parseInt`/`parseDouble` remain the answer. |
| 2 | `last`/`init`/`head`/`tail` partial errors on computed lists | `last (filter even (enumFromTo 1 10))` | FIXED | Correct values; empty-list case yields clean "Haskell error: last: empty list" (error-sentinel family, 4273c51). |
| 3 | `maximum`/`minimum`/`sum`/`product` | computed-list probes | FIXED | Correct; clean errors on empty (4273c51 + lazy poison closures). |
| 4 | `foldr1` | computed-list probe | FIXED | 52 on non-empty; clean error empty. |
| 5 | `foldl1` | computed-list probe | FIXABLE → fixed | Was: "Haskell error: foldl1" even on NON-empty lists. Root cause: `$fFoldableList19 = lvl' callStackValue` error-CAF reaches the sentinel through a sibling lambda — eager setup evaluation. Fix: `rhs_is_error_call_in_group` follows Var heads through LetRec siblings (commit 0d6de28). Probe: non-empty=52, empty=clean "foldl1" error; foldr1/maximum unchanged. |
| 6 | `cycle` | `take 5 (cycle [1,2])` | STANDING | "unresolved variable VarId [tag=' ']" — recursive loop-breaker with no unfolding; fat-iface miss. Workaround: manual recursion or `concat (replicate n xs)`. |
| 7 | `round` shadow needed | `(round 2.5, round 3.5, round -2.5)` | FIXED upstream | (2,4,-2) banker's rounding — rintDouble→Cranelift `nearest` (7bb112c). Shadow retained only as a type pin (`round = P.round`). |
| 8 | Floating/Fractional (`sqrt`/`sin`/`exp`/`logBase`, `/`) | direct probes | FIXED | Lazy poison closures defer dictionary error branches. |
| 9 | Integer defaulting (gotcha 3) | untyped recursive `addUp`/`fib` | RESOLVED | Correct via load-bearing integerAdd/integerSub shims. Explicit `Int` sigs now perf-only. Multi-limb GMP (beyond add/sub) still compile-errors cleanly — see #1. |
| 10 | Recursion depth "keep 10-20" (gotcha 5) | `sumTo 5000` = 12502500; deep probes | STANDING, boundary moved 3 orders | Non-tail recursion safe past 10K frames; clean yield error ("stack overflow (likely infinite list or unbounded recursion)") between ~10K-20K. Tail recursion unbounded (TCO, PR #154). The 10-20 rule was for the long-gone tree-walker. eval_at ceiling NOT touched (per spec). |
| 11 | Eager argument position (memory: JIT eager-arg) | `concatMap`/`filter` over infinite lists; user-shaped `badConcat` | SPLIT | Prelude functions: FIXED (laziness work). User code that puts unguarded recursion in eager argument position: STANDING by design — but dies with the CLEAN stack-overflow yield error, not SIGSEGV. |
| 12 | >30KB file kills `lines`/`filter`/`map` chains | 124KB file through full chain | FIXED | Retired by `lines`/`words` delegation to `T.lines`/`T.words` (82e3f43) + host-stack work. The "cap reads at 30KB" advice is stale. |
| 13 | "String is expensive" | 20K-char String round-trips | SOFTENED | Works fine at moderate scale (parallel-node verdict, cited). Text still preferred; String no longer a landmine. |
| 14 | `T.takeWhile`/`T.dropWhile` under partial application | `map (T.takeWhile p) ts` | **FIXED** (2026-06-11) — bug dead | Was: PARTIAL application returned inputs unmodified (takeWhile) / empties (dropWhile). Fixed in passing by the EPS unpoison (9a827a3): interfaces now load unfoldings, so GHC lifts the PAP into a top-level worker (`pap_… = takeWhile isDig`) whose `takeWhile` reference inlines the real Data.Text fused worker — identical under PAP and saturation (Core verified). Probe matrix (section/named/eta/composition PAP × equality+range predicates × take/drop) all correct on production AND worktree binaries. Pinned by `tidepool-runtime/tests/repro_takewhile_pap.rs` (14 cases). `takeWhileT`/`dropWhileT` shadows kept for now (user code references the names); retirement is a separate follow-up — see `plans/takewhile-shadow-retirement.md`. |
| 15 | #313 join-wiring (t9/t10 class) | Probe.hs t1-t10 | FIXED | repro313 gate exit 0. |
| 16 | #313 t11 survivor (double `breakOn`) | `Probe.occ2`; **`Patch.patchFile`** | FIXED (0317fe5) | Root cause: TailCtx leaked through the emit hylo into value positions (App args, Case scrutinees, …) — a tail-NApp at the bottom of a Case-in-App-arg returned null and the trampoline delivered the breakOn remainder as the fn's return. Fix: hylo is hard-NonTail; tail-ness owned by the emit_node spine (tail App/Case/Join dispatch, alt TCO preserved). Guards: `repro_313` (occ2, asserts 2/FORCE→3) + `repro_313_patch_class` (patchFile success path) in tidepool-runtime. Patch.hs comments trued up; pure cross-module helpers of this shape are safe again. |
| 17 | `nub`/`sort` at scale | nub/sort 2000, Map 3000 | FIXED | Clean. |
| 18 | errorEmptyList family templates | haskell_verified templates | FIXABLE → leaf | Gemini leaf `error-family-templates` (in flight at audit time) adds the family to the proptest harness so regressions are caught structurally. |

## Meta-finding (root-confirmed)

The failure-mode **landscape** changed more than the failure **set**:
everything that still dies now dies with a useful, clean error — compile-time
FFI errors name the symbol, runtime errors carry the Haskell message, stack
overflow is a yield error, not SIGSEGV. The CLAUDE.md "Dangerous Patterns
(silent crash → SIGILL/SIGSEGV)" framing is obsolete; rewritten as "Known
Limits (clean errors)". As of 2026-06-11 the last standing SILENT failure,
#14 (takeWhile/dropWhile PAP), is also dead — fixed in passing by the EPS
unpoison. The only conceivable remaining silent class is a not-yet-mapped FFI
path; everything documented is now loud or correct.

## Standing list (the short, true version)

1. **t11 double-breakOn** (cross-module, pure or M) — case-trap / garbage
   con_tag. Root's fix in flight. Inline the shape as workaround.
2. ~~**takeWhile/dropWhile PAP**~~ — FIXED 2026-06-11 (EPS unpoison); `T.takeWhile`
   partially applied is now correct. `takeWhileT`/`dropWhileT` shadows remain only
   for source compatibility (retirement tracked in `plans/takewhile-shadow-retirement.md`).
3. **cycle** — unresolved external; manual recursion instead.
4. **read/Integer beyond add/sub** — clean compile error with GMP hint;
   `parseInt`/`parseDouble`.
5. **Unguarded recursion in eager argument position** — by-design strictness
   boundary; clean stack-overflow error at ~10-20K frames (tail calls
   unbounded).
