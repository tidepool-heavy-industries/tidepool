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
`target/tidepool-test-runs/20260915T195052Z-2446621-check`. Solo reruns at
`aca28b7bb` classify the three timeouts: `preview_and_explicit_research_budget_match_without_spawning_during_preview`
(pass, 64.6 s) and `lifecycle_sources_follow_replacement_and_capture_retained_exit`
(pass, 58.7 s) were load starvation. `active_update_keeps_original_request_and_fences_terminal_delivery`
fails deterministically at `actor_host.rs:5478`: the cell renders
`Right (UpdateQueued)` because `Tidepool.Inspection`'s `displayTree` for
`Either`/`Maybe` and the reply-state instances parenthesizes every payload
(introduced in `57def9e0a`; `56f030122` accepted the same drift in two
command-job needles). This is a display defect, not an STG defect, fixed in
`534d7c8ea` with three stale fixtures it exposed (all five affected tests pass
solo; `just fixtures-check` passes). The next wave is in
[next-wave-2026-09-15b.md](next-wave-2026-09-15b.md) (revision b supersedes
[revision a](next-wave-2026-09-15.md)'s A2–A7 shape after a Fable survey of the
runtime owners). A
further full `just verify` is deferred to the Wave A exit.

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

## Wave A, step 2: the engine seam (F1, F2)

Per `next-wave-2026-09-15b.md` Part 3/4, following the F0 probe
(`tidepool-actor/tests/prepared_render_probe.rs`).

**F1 (committed as `b8d25637f`), "the engine route is a field of the resident
session".** `PersistentSession` holds a `ResidentEngine {Core(JitEffectMachine),
Prepared(PreparedEngine)}` behind an `EngineKind` fixed at construction
(`ResidentSession::unbootstrapped_on`); the composition roots read
`TIDEPOOL_ENGINE=prepared` once per session. The run methods take a `TurnCode`
(Core expr + table + sites + optional prepared program) rather than
`(expr, table)`. Every template now defines `__prepared = TidepoolResume.settle
__result` beside `__result`; the worker projects that entry
(`preparedScaffoldTargetName`) and the host reads one
`Tidepool.Internal.Resume.Settled` layer (`Done`/`Suspended`) instead of
walking freer data. A completed value is observed through
`PreparedMachine::observe_handle`, adopted into the machine's ROOT scope via
`adopt_handle`, and bound as `BoundValue::Prepared` with identity `{unit,
module: Val.G<g>, namespace: "value", occurrence: name}`. `PreparedEngine`
(bootstrap/install/run_settled/observe/adopt/release) replaces the deleted
`SessionTurns`/`TurnForm`/`prepared_turn_module`/`session/prepared_turn.rs`;
`PreparedRuntime` stays for the composite tests until a later wave removes it.
Tests: `tidepool-runtime/tests/prepared_turn.rs` dual-run
(`notebook_turns_run_on_core`, `notebook_turns_run_on_prepared_stg`,
registered in `tests/suites/session.rs`); `tidepool-actor/tests/prepared_render_probe.rs`
(the F0 gate: render bind, dialect expression, and opaque fallback all
project; shared constructors agree on id — the prepared closure is not a
subset of the Core table, it declares more).

**F2 (landing in the next commit after `b8d25637f`), "multi-binder bind and
cell render on the prepared route".** `run_projected_bind_with_sites` on the
Prepared route runs the settled scaffold, reads the projected tuple's fields
with `PreparedEngine::fields` (one retained handle per field via
`inspect_outer`, count checked against the GHC binders, otherwise
`PreparedRuntimeError::ProjectionShape`), releases the tuple, and binds every
field through one `bind_prepared` routine (scope validated before any
adoption; failure releases every unbound handle) — outcome is
`ResidentOutcome::BindingsCommitted` as on Core. `publish_captured_alias_in`
re-mints a prepared alias's import identity to the alias's own module/name
(shared root and handle; `release_binding_roots`'s aliased-root guard covers
lifetime). The actor workbench (`start_fragment_settlement`) reports a
prepared-route Haskell failure (`PreparedFailureKind::Language`) as the same
`<cell item N>: runtime error: …` rejection Core reports for
`ResidentError::Run`; integrity/infrastructure failures stay infrastructure
errors. Tests (pending the gate run): `prepared_turn.rs` gains a pattern-bind
turn (`(lo, hi) <- pure (x - 19, x + 80)` then `hi - lo`);
`tidepool-actor/src/resident_workbench.rs` unit tests
`notebook_cells_run_on_core` / `notebook_cells_run_on_prepared_stg` drive
`begin_fragment` on a bare session: pattern bind, expression cell rendered
through `render_cell_observation`/`cellDisplay`, a failing pattern bind
rejected with the prefix intact, and a later cell importing the prefix.

**F4 slice 1 (committed as `97df2d711`), "prepared suspensions park in the
machine ledger".** A prepared turn that requests a typed effect now parks
instead of being refused. `ContinuationFrame` carries per-engine evidence:
`FrameEvidence::Core` keeps the session constructor snapshot as before;
`FrameEvidence::Prepared(PreparedFrameEvidence)` records the site's evidence
owner, the site id, the runner program and its admitted `__resume` entry.
`FrameCell` is either the Core `Box` cell or, for a prepared frame, the
continuation handle's own `OldSpace` root slot moved from the persistent-root
list to the stowed-root list for the park; the rooting receipt
(`stowed_roots_count() == parked_count()`) holds on both machines, and the
prepared collector traces stowed roots through the one root snapshot both
engines share. `PreparedMachine::{park, parked, parked_ids, parked_realm,
parked_count, take_parked}` land beside the existing `close_realm`, which now
drops a realm's parked frames (deregistering their stowed roots first) and
reports them. `abort` retires a prepared frame with Core's abort error
without entering it.
On the engine side, `ProgramFacts` keeps each installed program's sites, type
graph, constructor identities and admitted `__resume` entry; a machine-owned
site index maps site id to evidence owner, and installation checks a
duplicate id for structural equivalence (delivery, wire and input type graphs
by family/constructor identity with ordered arguments, cycles compared
coinductively) before anything compiles, refusing a conflict with
`SiteConflict` and keeping the existing owner canonical. One settled-layer
decoder (`decode_settled`) serves both the initial scaffold and
`resume_parked`; `finish_prepared` is the one completion routine for a
settled layer whichever entry produced it. `park_suspension` reads the
`Union` layer, observes its payload through the machine observe path,
reads the protocol's `typedSite` field, resolves the witness and parks; a
runner without a resume entry, a suspension under `HandleOrError`, an
untyped request or an unknown site releases both handles and parks nothing.
`reenter` dispatches on the engine, and hole reconciliation reads
`PersistentSession::parked_ids`, which answers for either engine, as do the
value-handle, stowed-root and parked-count accounting accessors. On the
producer side, `ProjectionContext.projectionAuxiliaryRoots` seeds reachability
for `__resume` beside the entry, and `Tidepool.Internal.Resume.Settled` is now
strict in the request (`send` builds an unevaluated `inj x`; the host reads a
constructor layer without forcing, and the payload is forced through the
machine's observation path instead).
Along the way, the new codegen test exposed a Core root leak:
`resume_continuation` dropped an unclaimed live-payload `RootSlot` without
deregistering it, leaking one persistent root per resumed or aborted frame;
it now releases the root as `close_realm` does.
`a2_live_payload_requires_an_explicit_run_policy` parked under ROOT (not
closable, since the ledger refused it) and asserted a `(1, 0)` closure
receipt; it now parks under a fresh realm.
Tests: `tidepool-codegen`
`a_parked_frame_roots_its_continuation_until_taken_or_its_realm_closes`
(park moves the handle out of the ledger and the persistent-root class, the
value survives a forced collection while parked, `take_parked` hands back a
realm-owned handle observing to the original value, a taken id is unknown,
`close_realm` drops a parked frame); `tidepool-runtime/tests/prepared_turn.rs`
`notebook_ask_parks_and_aborts_on_{core,prepared_stg}` (`b <- runLLMTurn
@Bool "q"` suspends on both engines; the request names one of the turn's
declared sites; the prepared artifact admits `__resume`; an unrelated turn
runs while the frame is parked; a host answer on the prepared route is
refused with the hole intact; `abort` reports Core's error on both routes,
retires the hole, and returns handle and root counts to where the turn found
them — prepared program tops stay rooted until program retirement, which the
residency wave owns).
Not in this slice: host-built and handle answers are refused
(`PreparedRuntimeError::NotYetSupported`) before the frame is touched until
F5 lands the validator and builder; ordinary handled effects (a request
without a typed site) are refused as `UntypedRequest` with temporaries
released; there is no live-payload custody on prepared parks
(`receive`/`serve` park nothing yet); `HandleOrError` refuses every prepared
suspension, since nothing is handled on this route.
Verification: the codegen unit test above passed solo; `just test-target
tidepool-runtime session 'test(prepared_turn)'` — all five tests pass on both
engines; codegen tests filtered `payload|realm|rooting|resume` pass (58, plus
the fixed `a2` test); `cargo clippy --all-targets -D warnings` clean for
`tidepool-codegen` and `tidepool-runtime`; `just fixtures-update` regenerated
only the source fingerprint (no corpus output changed). No broad gate has
run.
