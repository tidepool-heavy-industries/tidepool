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

## Smaller items

- `oracle.rs` `test_differential_identity` asserts only "both sides are
  closures" — differential in name only; compare via CBOR-eval on a
  ground-typed fixture or rename to the smoke test it is.
- `generator_reach_stats` prints Join/LetRec/Case frequencies but asserts only
  `nodes > 0`; asserting nonzero Join/LetRec/Case at the weighted depth-7
  setting turns the reach report into a regression gate for "deep cases
  unreachable".
- `redeploy.sh` preflight opportunity: after `nix profile upgrade`, run the
  installed `tidepool-extract` no-args and check the `Usage:` banner —
  catches a broken wrapper at deploy time instead of first eval.
- Cross-refs into other plans' test asks: plan 04 F4 (shadowing generator
  mode), plan 04 F6 (delete `run_oracle_eval_may_panic`), plan 02
  opportunity 1 (non-ASCII lane), plan 01 (array-GC red test + heap-verify
  corpus).

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

- [ ] F1 deep-force landed; local bug-reinjection goes red
- [ ] F2 preamble ↔ handlers ↔ production aligned; legacy preambles migrated
- [ ] F3 floor + timeout classification + 101 routing
- [ ] F4 RAII disarm; F5 battery hardening
- [ ] Smaller items triaged/filed
- [ ] `scripts/battery.sh` green end-to-end
