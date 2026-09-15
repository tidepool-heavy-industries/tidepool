# Safety and compiler milestone implementation

This update supersedes the handoff's open-decision list and fix status. The
approved milestone closes safety/compiler blockers and bounded adjacent
defects; it does not complete the production STG cutover.

Review of this milestone's commits, with the blocking fixes that precede
acceptance: [review-2026-09-15.md](review-2026-09-15.md).

**Gate outcome at `d8a91a7e8` (stopped, not passed).** The `just verify` below
was stopped at 2,256/3,019 tests after 61 minutes, by decision, in favour of
focused per-fix runs. Lint passed. 2,249 passed; 3 failed, all tokio `Elapsed`
timeouts inside `tidepool` actor-host tests that ran 150–500 s each under the
four-slot `ghc-heavy` group (`active_update_keeps_original_request_and_fences_terminal_delivery`,
`preview_and_explicit_research_budget_match_without_spawning_during_preview`,
`lifecycle_sources_follow_replacement_and_capture_retained_exit`); 4 were
terminated by the stop and 763 never ran. Suite registration, fixture
freshness and the full prepared corpus were not reached. Artifacts:
`target/tidepool-test-runs/20260915T195052Z-2446621-check`. The three timeouts
are unclassified until each reruns alone.

**Full prepared corpus, measured after the review fixes** (`just fixtures-check`
with the typed-site change applied, committed as `e50831879`; report
`target/prepared-corpus/suite.yv9lfv/results.json`): all cohorts pass their
gates. Suite: 812 rows projected, validated, admitted and compiled; execution
702 passed, 0 failed, 110 classified (104 not closed, 3 no finite observation,
3 function valued, the expected rise from 2 to 3 after `5d3b2d5d9`);
comparison 234 passed, 0 failed, 4 missing source oracles (the allowlisted
four), 464 compiler-introduced rows without oracles (exactly the new ceiling
from `0870da4ff`). The oracle check reports "fingerprint and payload seal are
current". This is the first complete corpus pass for the milestone.

## Start here after switching subscriptions

Snapshot: **2026-09-15 19:52 UTC (12:52 PDT)**. Branch
`engine/stg-production-cutover`; implementation/test HEAD **`d8a91a7e8`**.
Nothing has been pushed. All implementation changes are committed. The
handoff commit following that revision changes documentation only.

Preserve the pre-existing untracked `examples/guess/` and
`haskell/dist-newstyle-wave4-haskell/`. No agents have unfinished edits.

**`just verify` is running, not yet passed.** Workspace lint passed; its
default-tier nextest run has started 3,019 tests across 110 binaries. It
reports 71 tests and 43 binaries skipped (including the default filter).

- Log: `/tmp/tidepool-final-verify.log`
- `just` PID: `2446324`; verifier shell PID: `2446346`
- Current nextest PID: `2446803`
- Nextest run ID: `17c52df0-f352-44c1-9daa-d393e9949b36`
- Codex execution session ID: `85244` (use OS PID/log from another client)

First check the existing job, **do not start a second battery**:

```sh
ps -p 2446324,2446346,2446803 -o pid,etime,stat,comm
tail -80 /tmp/tidepool-final-verify.log
```

`verify: all steps passed` is the terminal success marker. If the process has
ended without a complete terminal summary, or switching clients killed it,
rerun `just verify` quietly and alone. The gate independently runs lint,
default-tier tests, suite registration, and fixture freshness/full prepared
corpus. Do not infer success from the lint or nextest summary alone.

No more implementation changes were planned before this gate. Finish its
triage, record final results here, then plan the next production-turn slice.

## Approved decisions and scope

- A handler failure after drain should end the actor as Failed. Exit kind
  stays separate from cleanup confirmation. Implementation belongs to the
  next actor-exit milestone.
- Prompts render `active_children=3` and `active_children=unbounded`.
- Shoal exports the public types named by its signatures.
- Binary artifact compatibility is not required: schema 9 rejects schema 8;
  prepared fixtures must be regenerated together with the new codec.
- Typed resume, scoped binding authority, retirement/major collection,
  production routing and Core deletion remain later milestones.

## Committed fixes

| Commit | Result | Focused evidence |
|---|---|---|
| `01a0058bc` | Bounded daemon requests, owned endpoint cleanup, explicit rejection during rotation, worker crash recovery | 54 extractor-command library tests; integration target compiled |
| `502af70ab` | Join continuation validation and codegen scrutinee isolation | 47 validator and 13 codec tests |
| `186d8d12c` | Transactional prepared installs, root-realm protection, bounded Address observation and ledger-backed string primitives | Prepared codegen regressions and 18 runtime prepared tests |
| `b7287f8ce` | GHC-compatible double formatting | 11 unit tests; pinned GHC oracle over 4,888 binary64 inputs |
| `43587d64d` | Shared typed-site classifier, sibling reachability, current-sibling cache precedence and CPP invalidation | Prepared pipeline tests, real legacy lowering, Core lint and CPP memo regression |
| `e69d28717` | Boxed resident-machine custody through checkout and settlement | 18 registry tests; previously crashing actor-message test and operator disconnect test pass |
| `f1aac6960` | Prepared formatter coverage at GHC rounding boundaries | Included in 290 prepared-codegen tests |
| `fb9697a51` | CallerResult projection, specialization, schema 9 and seven regenerated fixtures | 21 schema/codec tests, 290 prepared-codegen tests, 15 prepared-runtime tests including native GHC comparison |
| `56f030122` | Test grouping, stale fixtures, Shoal signature types, prompt wording and deterministic disconnect coverage | 16 targeted behavior tests passed; interactive target compiled and deliberately ignored |
| `5d3b2d5d9` | Permit same-closure nonreturning joins across case scrutinees | 48 validator tests, 291 prepared-codegen tests; backedge cancellation regression and cross-closure rejection |
| `b764ef023`, `d8a91a7e8` | Generated native oracle, honest corpus classifications, crash settlement, exact decimal decoding and tightened gates | 32 corpus-harness tests, native oracle regeneration, initial Suite measurements below |

## Verified integration and current corpus evidence

CallerResult is committed with Rust and Haskell coverage for boxed/unboxed
results, polymorphic forwarding and joins, PAP completion and cross-program
dispatch. The native GHC runtime comparison and all seven regenerated fixtures
pass. The import consumers became smaller because stale copied producer bodies
were pruned; both retained producer identities and generation 11 are unchanged.

Actual production pure-display templates using Prelude, generated Effects,
Orchestrate and Control.Lens project successfully for both `42` and
`((41 :: Int, ()) ^. _1) + 1`. This checks an empty effect row and projection,
not effectful prepared-turn dispatch.

The corpus driver now distinguishes source tops missing an oracle from
compiler-introduced bindings with no source-level GHC name. Nonclosed entries
are classified from the engine's typed argument-admission errors. Typed
classifications are counted separately from passes; watchdog and cleanup
failures cannot satisfy a language-error oracle. The native GHC oracle has 18
additional value expectations and two explicit cyclic classifications, with
no old expectation removed. Five double oracles use zero tolerance; String
values use lists of Char rather than Text. Decimal decoding enables
`serde_json/float_roundtrip`; zero tolerance compares finite bits, including
signed zero.

The initial full Suite report is
`target/prepared-corpus/suite.dbaSpY/results.json`, with executable provenance
in `target/prepared-corpus/run.XyaWMv/provenance.json`. **It is not an all-green
report:**

| Stage | Measured result |
|---|---|
| Projection | 812 passed |
| Validation | 811 passed, 1 failed (`thunk_blackhole`) |
| Admission / compilation | 811 passed each; 1 not reached |
| Execution | 702 passed, 0 failed; 104 not closed, 2 nonfinite, 3 function-valued; 1 not reached |
| Comparison | 234 passed, 0 failed, 4 missing source oracles, 464 compiler-introduced rows without source oracles |

This confirms all 74 former Address execution failures cleared. The one
validation failure exposed an overly strict case barrier in our first fix:
GHC emits a recursive nonreturning join inside an empty case. Commit
`5d3b2d5d9` permits only declared `NoSuccess` joins from the same heap closure
across case scrutinees; returning and CallerResult joins stay hidden, and all
joins stay hidden across heap closures. The regression cancels on the third
backedge twice, verifies reusable settlement, and retains returning-jump
rejection. A fresh complete corpus run after that fix is pending in the live
gate; expected nonfinite classifications rise from 2 to 3.

The initial corpus driver stopped after writing the Suite report because I
edited its script while it was running, shifting Bash's read position. The
current script passes syntax checks and is frozen. The priority Project.Work
and actor awaitSettled cohorts passed before Suite; later cohorts were not
reached in that run. Do not cite it as a complete corpus pass.

The committed Suite gate now requires 812 structurally valid/compiled rows,
at least 702 execution passes, **zero execution failures**, at most 110 typed
classifications, at least 234 comparison passes and zero mismatches. Missing
source oracles are capped at four and restricted by identity to `FmtKInt`,
`qq_j_anti`, `qq_j_build`, `qq_j_scalar`. Improvements cannot license a newly
missing source oracle. Classifications and absent compiler-level oracles are
never counted as comparison passes.

Solo reruns of the six previous timeout tests reached terminal outcomes:
five passed; the sixth reached a stale receipt assertion, then passed after
the assertion was updated. No timeout or blocked checkout was observed.
Fixture testing separately reproduced a host stack overflow twice in the
actor-message test. Its faulting instruction is a stack probe in
`SessionRegistry::take_machine`, outside JIT protection. Boxed custody passes
the focused registry checks and the original actor-message test (40.92 seconds).

The final schema-9 facade test passed in 112.758 seconds, hosted lookup in
23.211 seconds, and the operator test in 7.10 seconds. The operator test now
proves both preparation failure (`NotRun`) and a committed let prefix before
runtime failure, plus disconnect behavior while a real Sleep effect is
pending. The pinned interactive TUI test was compiled and its ignored
registration checked; it was not executed.

`scripts/fixtures.sh update` regenerated the legacy Suite corpus: bytes were
unchanged and only `.source-fingerprint` changed. Full workspace lint passed
after a test-only Vec-to-Box cleanup. The running `just verify` repeated lint
successfully before starting tests.

## Next actions, in order

1. Finish/read the live gate. Triage any failures against the owning source;
   do not lower corpus floors or weaken receipts to make it green. Keep
   Haskell-compiling work and broad batteries serialized. Record exact final
   gate revision, totals, skips and corpus report/provenance paths.
2. Once the gate is settled, update this handoff and the governing completion
   plan. No push was requested.
3. Implement the first real prepared notebook turn through the production
   owners. The old handoff's **Extractor turn mode** file map remains the
   starting plan: typed prepared-turn request, existing workbench templates,
   retained generation derivation and the session-owned install path. The
   Prelude/Effects/Lens projection risk is now cleared for pure templates;
   effectful routing and retained heap custody remain open.
4. Complete typed effect resume, scoped binding authority and the approved
   actor-exit/cleanup contract; then retirement/collection, parity, default
   routing and Core removal. Do not skip these by switching the default now.

## Reproduction notes and useful logs

- Use `just` or `bash scripts/dev-shell.sh`; do not use ambient Cargo for
  extractor-backed tests. Do not modify a running verification script.
- `/tmp/tidepool-final-preflight.log`: 48 validator and 291 codegen passes;
  its first lint attempt found the subsequently fixed test-only `useless_vec`.
- `/tmp/tidepool-final-lint.log`: all workspace formatting/clippy checks pass.
- `/tmp/tidepool-prepared-schema9.log`: initial 290 codegen and 15 runtime
  prepared tests pass, including the real `RepPoly.hs` GHC differential test.
- `/tmp/tidepool-corpus-integration.log`: 32 harness tests, fixture update,
  and the initial partial corpus run described above.
- `/tmp/tidepool-suite-oracle.log`: successful native GHC oracle generation.
- `/tmp/tidepool-production-projection/{baseline,lens}/output/manifest.json`:
  both real production pure-display template entries project successfully.
- `/tmp/tidepool-facade-probe/message-actor-crash.txt`: original host stack
  overflow backtrace (fixed); private facade signature probe found zero
  remaining named-type export leaks. None of its instrumentation was shipped.
- Schema-9 fixture regeneration was verified before copying: Freer retention
  is `freerRequest`, manifest artifact `0.prepared.cbor`; Freer resume is
  `freerResumeEntries`, artifact `2.prepared.cbor`. The checked recipe is now
  in `haskell/test-prepared-stg/FreerRetention.md`; the original ordered
  seven-fixture checklist remains in README.
- `just fixtures-update` updates legacy Suite bytes/fingerprint. It does not
  regenerate the seven prepared fixtures or the native oracle automatically.
  If oracle inputs change, run through Nix:
  `scripts/prepared-corpus-oracle.sh update <fresh Suite manifest>`.
  The required `SuiteOracleNonterminating.txt` is intentionally tracked despite
  the repository's generic txt ignore rule (commit `d8a91a7e8`).

## Coverage limits to retain

- Generic prepared functions offer a finite set of concrete result instances
  from their artifact plus lifted results. An independently compiled demand
  absent from that offer returns reusable `UnresolvedCallee`; there is no
  dynamic specialization. Generic joins inside generic owners are covered;
  generic joins directly inside concrete owners remain unsupported.
- The Haskell retained-import probe currently receives `LFUnknown` from the
  home-source pipeline. It checks generation and concrete demand, not a
  separately packaged `LFReEntrant` interface. Known generic imports are
  covered by Rust artifacts.
- Nonliteral byte-array Address observations are unauthenticated. The ledger
  serves bounded string primitives but observation does not read arbitrary
  external payloads.
- Daemon crash recovery is proven with a synthetic crashing worker, not a
  successful real-GHC request following a GHC crash.
- The former display-test diagnostic was not conclusively diagnosed. The
  corrected fixture passed after rebuilding the classifier worker; that is
  the evidence, not proof that it shared the notification test's cause.
- The integrated broad gate is in progress. Its final outcome and the full
  corrected corpus result remain unverified at this snapshot.
