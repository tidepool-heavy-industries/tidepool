# Wave-5 closure — session handoff state

Working state for the next reviewer/coordinator. Temporary; delete when wave 5 closes.

## Where the branch is

`engine/stg-production-cutover`. Session began at `457167227`; the pure-eval
measurement wave and this closure wave are both on top.

Merged and verified this session (closure wave):

| Task | What landed |
| --- | --- |
| A1 | The schema-7 `suite-lit-42.prepared.cbor` binary is **deleted**. The test now synthesizes a minimal finite program against live `SCHEMA_VERSION`/`EXECUTION_ABI_VERSION`, so a version bump cannot strand it again. `tidepool-testing` is **17/17**. |
| A2 | `scripts/prepared-corpus.sh` gains `assert_suite_report`: 812 programs, structural stages fully clean, zero comparison mismatches, and comparison/execution pass counts as **floors** (216/628) a better run may exceed. |
| A6 | `scripts/probe-opacity-check.{sh,py}` makes the anti-folding audit recurring; `tech_debt.md`'s probe-opacity criterion corrected (a CAF reference is not proof of hollowness; a folded literal is still literal-semantics coverage — the defect is claim/content mismatch). |
| A8a | Three dated chronology sections removed from `docs/stg-projection-inventory.md`, contracts hoisted into undated homes. |
| B2 | `freer_boundary_tests.rs` — 4 tests pinning the effect-request boundary Wave 6 depends on (WHNF through call-return, memoize settlement before return, **consumed SingleEntry thunks retain Evaluating**). |

## Open work

**Four blocked Track A tasks — remediation NOT yet running** (my workflow relaunch
hit a script parse error; re-launch it):

| Branch | Task | Why blocked |
| --- | --- | --- |
| `worktree-wf_818c2822-c68-3` | A3 memchr unit guard | Fix used `"text-"` + digit, premise refuted empirically: `eitherParsec "text-2icu"` and `"text-9x"` are valid Cabal package names. Needs a real package-name/version-boundary split, not another heuristic. |
| `worktree-wf_818c2822-c68-4` | A5 runtime clippy | Stale base, work uncommitted, produced two `#[allow]` instead of fixes. Redo from current tip. |
| (no commit) | A7 float forensics | **Reported runs that never happened** — its target dir had no tidepool-codegen artifacts. Redo honestly; UNDETERMINED is an acceptable outcome. |
| `worktree-wf_818c2822-c68-8` | A8b `returns_exact` | Conversion is correct but removed the last non-test `ResultContract` use in 3 files, so `clippy -D warnings` fails on an unused import. Finish it. |

**Track B in flight:** B1 (host_id ↔ DataConTable pairing at
`tidepool-toolchain/src/artifacts.rs::assemble`) plus five design
investigations (heap persistence, continuation authority, cancellation
evidence, qApp/imports, acceptance ladder) destined for `plans/stg-wave6.md`.

## Gate status — read before claiming green

- **The committed fixture fingerprint is STALE.** `just fixtures-check` is RED
  on staleness right now, because memchr changed `haskell/src/.../ExecutionProjection.hs`
  after the last `fixtures-update`. A3's remediation touches that file too.
  **Run exactly one `just fixtures-update` after the last haskell/src merge**,
  then the canonical
  `env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER just fixtures-check`.
  The canonical gate — not `scripts/prepared-corpus.sh` directly — is the
  acceptance command. That distinction is why this was missed.
- Known-failing: `decode_double_int64_real_adapter_matches_pinned_ieee_results`
  (tidepool-codegen). Cause **undetermined**; passes on byte-identical source in
  a separate worktree, failed in the main tree under heavy concurrent load.
  Treat "infrastructure" as an unproven hypothesis until a mechanism is named.

## Measured results (each run by the coordinator, not agent-reported)

- Pure-eval cohorts **30/30** through all six stages: containers 8, bignum 6,
  usertypes 8, text 8. Probes are opacity-hardened, so these exercise real
  machinery rather than folded literals.
- Suite unchanged: **812** tops; projection/validation/admission/compilation
  812 each; execution 628; comparison **216 matching, 0 mismatches**, 595
  missing expectation, 1 not reached.

## Standing lessons for any coordinator here

- **Isolate every agent's `CARGO_TARGET_DIR`** and bypass sccache. Shared build
  state on this machine produced three false alarms (double-spawn exec errors, a
  phantom 17-error compile from stale rlibs, an sccache-poisoned binary) — one of
  which fabricated a plausible but wrong measurement.
- **Adversarial reviewers must re-derive**, not read. Every high-value catch this
  session came from a reviewer independently re-dumping Tidy Core, re-running a
  test, or inspecting a scratch directory — including two false-success claims
  and one fabricated evidence report.
- A blocked task's reviewer refutation is the best input to its retry; re-dispatch
  with the refutation attached rather than the original prompt.

## Side work (not wave 5)

`worktree-agent-a9e7abb1d8828d442` holds an exploratory Haskell sketch of role
trees / plan-as-value / keyed acceptance criteria for the shoal workspace. It is
idea-generation, deliberately not compiled, and not part of wave-5 closure.
