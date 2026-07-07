# 08 — Test infrastructure (tidepool-testing + scripts)

Tests that structurally cannot catch what they claim to cover. **Soft
prerequisite for plans 01/04/05:** several suites that would verify those
fixes currently mask the exact failure modes being fixed. Read this before
trusting a green run as evidence.

## ANTI-PATTERNS

- Do NOT weaken deep-force-before-compare to make suites pass — a new red
  after F1 lands is a REAL pass bug (that's the point).
- Do NOT "fix" the watchdog by removing it — nextest process-per-test still
  wants a hang backstop; add disarm, don't delete.

## READ FIRST

- `.config/nextest.toml` (hazard-audit note) + `scripts/battery.sh`
- `tidepool-testing/src/proptest.rs` — `values_equal` (:29-57),
  `check_jit_vs_eval` (:132-135, deep-forces per #336),
  `check_pass_preserves_eval` (:248-263, doesn't)
- `tidepool-testing/src/eval_harness.rs` — module doc + preamble (:386-447)
  vs mock handlers (:548-552, :623-628)

---

## F1 (MEDIUM): `check_pass_preserves_eval` compares un-forced WHNF with a thunk-skipping comparator — pass bugs under lazy fields are invisible

**Where:** `proptest.rs:248-263` (no `deep_force`) + `:29-57` (`values_equal`
SKIPS any pair where either side is a ThunkRef/closure).

**Failure:** original `let x = 1+1 in Just x` evaluates to
`Con(Just, [ThunkRef])`; a buggy pass rewrites it to `Just 4`. The comparator
reaches the `(ThunkRef, Lit 4)` pair, classifies it incomparable, SKIPS it,
property passes. Any pass bug corrupting a value under a lazy constructor
field is invisible — and let-bound fields are the common case.
`check_jit_vs_eval` deep-forces for exactly this reason (#336); the pass
oracle never got the same treatment. This is why plan 04's PartialEval bugs
needed shadowing to surface at the top level at all.

**Fix:** deep-force both results before comparing — skip when BOTH deep-forces
fail, fail when only one does (same policy as `cbor_roundtrip_preserves_eval`,
`gen/strategy.rs:1164-1193`).
**Verify:** temporarily re-introduce the plan 04 Lam bug locally → suite must
go red under a shadowing-capable generator.

## F2 (MEDIUM): `eval_harness::mock` preamble GADT return types contradict its own mock handlers (and production)

**Where:** `eval_harness.rs:386-447` (preamble) vs `:623-628` (MockExec),
`:548-552` (MockFs). Module doc claims decls and handlers are "kept in
lockstep on purpose," but:

- Preamble `Run :: Text -> Exec (Int, Text, Text)` — MockExec responds
  `Ok::<Proc, String>(Proc{…})`, the PRODUCTION shape
  (`effect_defs.rs:983`: `Run :: Text -> Exec (Either ExecError Proc)`).
- Preamble `FsMetadata :: Text -> Fs (Int, Bool, Bool)` — MockFs responds
  `Some(FileMeta{…})` (production: `Fs (Maybe FileMeta)`).
- Preamble `HttpGet :: Http Value` / `GitShow :: Git Value` /
  `LlmStructured :: Llm Value` vs production `Either …` shapes; `MockAsk`
  responds a bare `String` where the decl promises `Value`.

**Failure:** first new test writing `(code, out, err) <- send (Run "x")`
typechecks against the preamble but receives a `Right (Proc …)` con at
runtime → case-trap or garbage fields; conversely `Right p <- send (Run "x")`
won't typecheck. The Exec/Fs-metadata arms of the "canonical" harness are
unusable-by-construction; tests written against it validate non-production
semantics. (No current test calls these verbs — inherited drift from the
legacy per-test preambles — landmine, not live fire.)

**Fix:** update the preamble GADT strings to the production shapes from
`effect_defs.rs` (the Exec/Fs handler responses already match production);
make MockAsk/MockHttp/MockGit/MockLlm respond the declared shapes. Then
migrate the legacy 10-effect preamble copies (`show_double_10effect.rs`,
`value_case_match.rs`, `text_spliton.rs`, `fixtures/mcp_showdouble_repro.hs`)
onto the fixed harness — removes four more drift copies.

**Done:** preamble now declares `ExecError`/`HttpError`/`GitError`/`LlmError`
inline (production generates these via codegen; this static preamble has no
codegen step, so they're hand-declared to match the exact generated strings
verified by `effect_defs.rs`'s own tests) and fixes `Run`/`RunIn` (`Either
ExecError Proc`), `FsMetadata` (`Maybe FileMeta`), `HttpGet`/`HttpPost`
(`Either HttpError Value`), `GitShow` (`Either GitError Commit` — NOT `Either
GitError Value`; production's `GitShow` returns the typed `Commit` record,
which is already in scope via `Tidepool.Prelude` → `Tidepool.Records` →
`Tidepool.Records.Bridged`), `LlmStructured` (`Either LlmError Value`).
MockHttp/MockGit/MockAsk/MockLlm updated to respond the matching shapes
(`Ok::<_, String>(_)`, following the same `String`-as-stand-in-Left-type
convention already used by MockExec). Migrated all four named legacy
preamble copies onto the corrected types (`show_double_10effect.rs` already
used `mock::mcp_module`/`min_stack()` for most tests per its own header
comment — only its one Library-import special case had a stale inline
preamble, now fixed; same for `value_case_match.rs`'s two non-Library tests,
fully swapped to `mock::mcp_module`; `text_spliton.rs`'s `FsMetadata` type
fixed in place — none of these fixtures actually dispatch the effects whose
types were wrong, so this was a landmine-defusal, not a live-bug fix).

**Discrepancy note:** production's effect surface has drifted further than
this finding described since the 2026-07-06 review: the `Ask` verb was
renamed to `AskWith :: Text -> Value -> Ask Value` (the `schema` object is
now a second argument, wrapped by a Haskell-level `ask` helper), the `SG`
effect was removed/replaced by `Lsp`, and a `Time` effect was added — none
present in this mock preamble. Left out of this fix: renaming `Ask`→`AskWith`
or adding `Lsp`/`Time` would be a full effect-stack redesign of the mock
harness, not the bounded type-signature fix this finding asked for, and nothing
currently depends on it (same "landmine, not live fire" status). A future
pass reconciling the mock preamble's effect *set* (not just existing verbs'
types) with `effect_defs.rs` would need to touch this.

## F3 (MEDIUM-LOW): deep-diff harness can't fail on JIT non-termination and has no comparison floor

**Where:** `tidepool-testing/tests/proptest_deep_differential.rs:347-376,
408-443`.

1. `Outcome::Timeout` (20s child kill) is tallied/printed but NEVER fails the
   property — and eval runs before JIT in the same worker, so a timeout can't
   distinguish "synthetic program loops in eval" (benign skip) from "eval
   returned Ok, JIT loops forever" (real divergence class).
2. No minimum-compared floor (unlike `cbor_roundtrip_preserves_eval`'s
   `compared >= 25`) — a generator/whitelist regression pushing every case
   into `Skipped` turns all four deep-diff suites permanently green with ZERO
   comparisons.
3. `status.code() == 101` (any worker panic, incl. real harness/eval panics)
   maps to `Skipped` (:366-367).

**Fix:** worker marks eval-phase completion (distinct exit path or phase
file) so eval-done-then-timeout is reportable as divergence; add
`assert!(compared.get() >= N)` after `result.unwrap()` in `drive`; route
code-101 to failure with the panic captured.

## F4 (LOW-MED): watchdog never disarms

**Where:** `tidepool-testing/src/watchdog.rs:27-58`. The thread lives forever
and fires whenever the shared epoch stalls 120s. Fine under nextest
(process-per-test), but root CLAUDE.md documents
`cargo test --workspace -- --test-threads=1` as a supported fallback, where
multiple `#[test]`s share one process: after the last armed proptest finishes,
any OTHER test still running 120s later gets the whole process killed
(`exit(101)`) blaming an already-completed fixture (e.g.
`proptest_jit_vs_eval.rs` mixes armed and unarmed tests). Conversely two
concurrently-armed tests share the epoch, so one test's hang is masked while
the other keeps calling `begin`.
**Fix:** RAII disarm guard decrementing an active-cases counter; check the
epoch only while count > 0.

## F5 (LOW): battery.sh — export-masking + no validation of a pre-set TIDEPOOL_EXTRACT

**Where:** `scripts/battery.sh:17-22`.
1. `export TIDEPOOL_EXTRACT="$(cd haskell && cabal list-bin …)"` — `export`'s
   exit status masks the substitution failure (SC2155); under
   `set -euo pipefail` a failing `list-bin` proceeds with an EMPTY var.
2. A pre-set `TIDEPOOL_EXTRACT` is echoed and trusted with no existence/exec
   probe — and `eval_harness::extract_env` (:90-135) will SILENTLY SWAP to a
   `cabal list-bin` binary if the announced one doesn't run, so the banner can
   name a binary the tests never used; if no fallback resolves, GHC-guarded
   suites "skip cleanly" and the battery reports green with zero GHC coverage.
**Fix:** split declaration/assignment; when pre-set, assert `[ -x ]` + run the
same `Usage:` probe extract_env uses; print skipped-suite counts at the end.

**BLOCKED (boundary conflict):** this fix touches `scripts/battery.sh`, but the
test-infra work session fixing F1–F4 was explicitly instructed NOT to touch
`scripts/` (owned by a different worker in the parallel review-fix pass).
F5 is otherwise unstarted — the analysis above is still accurate as of this
note; a worker with `scripts/` in scope should apply the fix described.

## Discrepancy notes (from step-0 fixture work)

- `cross_mode_existing::pure_nested_value_case_cross_mode_equivalent` — after
  fixing the `Number 42.0` fixture-drift (Scientific has no `Fractional`
  instance, `Number 42.0` → `Number 42`), the test still failed the *structural*
  half of `assert_cross_mode_equivalent` with `LetRec binding index mismatch`
  deep in the shared top-level letrec (~57 bindings). Root-caused: importing
  `Tidepool.Aeson.Value` pulls in `Scientific`'s hand-written `Eq`/`Ord`/`Num`/
  `Show` instances; GHC's cross-module SCC tie-breaking (Unique-order
  dependent) reorders that region of the letrec differently between the
  single-module and split-module compiles. Confirmed via
  `structural_eq::assert_value_equivalent` alone (bypassing the structural
  check): the two modes' **runtime values agree** — this is a benign Core-shape
  difference, not an observable-behavior bug. Already-established precedent in
  the same file: `pure_typeclass_dispatch_ord`/`pure_primitive_boxing_*` use
  `assert_cross_mode_pure_equivalent` (runtime-only) for the same reason
  (typeclass-dictionary shape sensitivity across module boundaries). Switched
  this fixture to the same runtime-only check; not a fixture bug, not fixed at
  the harness level (would need `tidepool-codegen`/extractor work, out of
  test-infra's remit).

## Smaller items

- [x] `oracle.rs` `test_differential_identity` asserts only "both sides are
  closures" — differential in name only; compare via CBOR-eval on a
  ground-typed fixture or rename to the smoke test it is.
  **Done:** switched to `(\x -> x) 42` on both sides (Rust-constructed vs
  `haskell/test/suite_cbor/app_identity.cbor`, an existing tracked fixture
  compiled from `Suite.hs`'s `app_identity = (\x -> x) 42`) and compare the
  actual ground result (42), unboxing GHC's `I#` wrapper — a real
  cross-engine comparison instead of a shape-only smoke check.
- [x] `generator_reach_stats` prints Join/LetRec/Case frequencies but asserts
  only `nodes > 0`; asserting nonzero Join/LetRec/Case at the weighted
  depth-7 setting turns the reach report into a regression gate for "deep
  cases unreachable". **Done:** added the assertion.
- [ ] `redeploy.sh` preflight opportunity: after `nix profile upgrade`, run
  the installed `tidepool-extract` no-args and check the `Usage:` banner —
  catches a broken wrapper at deploy time instead of first eval. **BLOCKED**:
  same `scripts/` boundary conflict as F5 — unstarted.
- Cross-refs into other plans' test asks: plan 04 F4 (shadowing generator
  mode), plan 04 F6 (delete `run_oracle_eval_may_panic`), plan 02
  opportunity 1 (non-ASCII lane), plan 01 (array-GC red test + heap-verify
  corpus). Informational only — not actionable from this file.

## Verified clean — do NOT re-audit

`compare.rs` worklist comparator + heap reader (depth/field caps fail LOUD;
ByteArray `Arc::ptr_eq` deadlock guard correct; #334 regression tests real).
`gen/strategy.rs` depth/weights plumbing sound; depth-0 leaf-fallback
recursion bound correct; BUG-1/2/3 regression tests active in
`proptest_infra_selftest.rs` with the 2d+2 depth bound characterized.
`extract_env`'s `Usage:` probe matches reality (`haskell/app/Main.hs:48`).
`.config/nextest.toml` hazard audit matches code. `normalize_semantics.rs`
honestly documents identity-only coverage. No stale `#[ignore]`s referencing
closed issues in this slice (the stale panic-TOLERANCE is plan 04 F6).
`watchdog.rs` has no race/leak beyond the disarm gap above.

## DONE CRITERIA

- [x] F1 deep-force landed; local bug-reinjection goes red
- [x] F2 preamble ↔ handlers ↔ production aligned; legacy preambles migrated
- [x] F3 floor + timeout classification + 101 routing
- [x] F4 RAII disarm
- [ ] F5 battery hardening — BLOCKED: requires editing `scripts/`, out of
      this worker's boundary (see F5 note above); unstarted, needs a
      `scripts/`-scoped worker
- [x] Smaller items triaged/filed (2 fixed — `test_differential_identity`,
      `generator_reach_stats`; 1 blocked on the `scripts/` boundary —
      `redeploy.sh` preflight; cross-refs into other plans are informational)
- [x] `scripts/battery.sh` green end-to-end — full run: 2720 passed, 0 failed,
      31 skipped (expected `#[ignore]`d tests), ~124 min wall-clock
