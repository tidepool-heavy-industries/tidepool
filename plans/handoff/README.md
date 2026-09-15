# STG cutover handoff (2026-09-15)

For the next LLM session taking over `engine/stg-production-cutover`. Read
this, then `plans/stg-completion.md` (the governing plan), then `CLAUDE.md`.

## State

- Branch `engine/stg-production-cutover`, clean tree at the commit that adds
  this file. Nothing is pushed. `examples/guess/` and
  `haskell/dist-newstyle-wave4-haskell/` are untracked and must not be touched.
- Goal: finish the prepared-STG engine cutover (completion plan steps 1–5):
  close cross-program application, connect one real notebook turn through the
  production owners, retirement and collection, parity then default routing,
  delete the Core engine.
- The broad gate is `just verify`, now independent steps (lint, default-tier
  tests, suite registration, fixtures); every step runs and all failures are
  listed. Last full gate: `ff73dfde0`, 2913 passed, 55 failed, 2 timed out.
  Most of those failures are now fixed or classified (below). No gate has run
  since; run one quietly before claiming step 1 closed.

## Commits this session (oldest first)

| Commit | What |
|---|---|
| `7bbbe155d` | Gate: lint and tests independent; `scripts/lib-steps.sh`, `just lint` |
| `ff73dfde0` | Workspace clippy backlog hidden by fail-fast targets |
| `373f19abc` | Stale schema contract pins (fork args 15, 29 effects) |
| `177d8435d` | Protocol generator owns the decode enums' lint policy |
| `76718e6c7` | Core: compiled literals are ledger-owned byte arrays (raw-bash `BadPointer`) |
| `3f5a5ee28` | Prepared `_hs_text_measure_off` intrinsic, `TextUnitAuthority` |
| `e3a7d8e97` | Typed-site failures deferred to executable reachability |
| `da5a53d14` | Raise contract test checks semantics, not node shape |
| `ddb9d2746` | Plan: typed-resume and lifetime contracts, gate evidence |
| `a43ec1971` | Record actor protocols indexed by record type; `Message` exported |
| `052efa6ed` | Core: major collection keeps retained payload slots remembered (drain corruption) |
| `8d684b1e5` | Request fixtures pass an `Assignment` |
| `52bc630c5` | Three actor-host tests updated to designed behavior changes |

## Open defects (evidence in `plans/stg-completion.md` step 1 and below)

1. **Typed-site elaboration (both elaborators)** — highest priority compiler
   defect. Site literals are placed by counting value arguments from the
   right, so an eta-reduced `Member`-constrained verb gets the literal in its
   dictionary slot. GHC wraps calls under an open-tail effect row
   (`Member V effs => Eff (Other ': effs) a`) in `nospec`, which survives to
   `elaboratePreparedSites` (removed only by CorePrep); today such a fully
   applied `runLLMTurn`/`fork`/`finalize` site silently goes out as site 0, and
   `request`/`child`/`receive`/`serve`/`forkMap`/`forkCata` get a false
   rejection. Implementation plan: `designs/site-classifier.md`. Fix: one shared classifier (strip `nospec`, classify the whole
   call once, literal index from the verb's type, rewrite or defer-reject,
   delete `vsMisShapeIsError`). Test with open-tail and eta-reduced fixtures.
   Probe evidence: `/home/inanna/.claude/jobs/4940a626/tmp/nospec/rep/` (may be
   deleted with the job; the conclusions are here).
2. **Caller-chosen result representation** — projection rejects
   `$mActorDefinition` (pattern-synonym matcher, reached from
   `startActor`/`startActorFork`) as "runtime-polymorphic representation".
   Design: `ResultContract::CallerResult` (proposed fixes in the completion
   plan). Blocks `shoal_exports_persistent_agents_and_hides_turn_lifecycle_operations`.
   Commit plan: `designs/caller-result-first-slice.md`.
3. **`Address` host values** — the only engine gap among the 184 corpus
   execution failures (74 rows). First slice designed (observation of pinned
   literal bytes; typed refusal otherwise). Implementation plan:
   `designs/address-first-slice.md`.
4. **Actor exit contract** (needs the user's decision, `designs/actor-exit-contract.md`):
   a handler failure during drain pauses forever; unconfirmed cleanup rewrites
   the exit kind; four exit writers. `stateful_replacement_preserves_owned_children`
   still fails (no heap corruption in its run) and is attributed here.
5. **Remaining stale tests** — patches in `patches/stale-fixtures/`, applied
   and run once, NOT committed: 02 (steering, load timeout only), 10 (custody
   shutdown settlement arm; load timeout only), 11 (research escalation
   `assignment`; load timeout only) — re-run each ALONE. 03, 04e and 04f failed
   for real reasons; use the corrected `patches/stale-fixtures/v2/` versions
   (diagnoses under "Investigations folded in after handoff"; the earlier
   "structural display wins" diagnosis for 04f was wrong).
   `notification_admission_and_poll_preserve_typed_request_bindings` is a stale
   test (the designed settlement notice arrives first; fix described below).
   04a is a product prompt change awaiting sign-off.
6. **Cell pins** — `acknowledgeCancellation` names
   `Tidepool.Agent.Reply.Internal.Reply`; patches and a pin-surface probe in
   `patches/pin-exports/` (not compiled). `sleep` names an unexported
   `Duration` on the eval surface.
7. **Model-facing `Option` Debug output** — `active_children=Some(3)`
   (`actor_host.rs` ~4875), also `runtime_observation.rs:164`,
   `resident_actor.rs:1028`.
8. **Corpus harness** — 183 of 184 execution failures are harness-driven; ~560
   of 595 missing expectations are GHC-generated tops no oracle can name.
   Patches in `patches/corpus-driver/` (not built).

Broader read-only audit findings (bugs, coverage gaps, idioms) are collected in
`audit-wave.md`.

## Designs (proposed, not reviewed by the user)

`designs/`: `resume-contract-v2.md` (typed resume after adversarial review:
delivery mode per site, structural type evidence, constructor closure,
continuation ledger, first slice `runLLMTurn @Bool` answered by JSON),
`lifetime-contract-v2.md` (per-program root blocks, mark phase, owner sets,
stable program ids, quiescence token), `cell-compiles.md` (one compile per
expression via two internal pauses: 39 → 21 compiles for a large cell),
`actor-exit-contract.md` (decision memo), `corpus-driver.md`. The completion
plan's step 2/3 sections still hold the v1 contracts plus review findings;
replace them with the v2 drafts after peer review.

## Decisions waiting on the user

1. Drain-while-paused: recommend handler failure after a drain request ends the
   actor `Failed`.
2. Exit kind: recommend keeping the requested kind, cleanup confirmation as a
   separate fact, one shutdown deadline, one exit-publication owner.
3. Prompt text `active_children=3`/`unbounded` (patch 04a).
4. Whether Shoal-only imports must re-export every type its signatures name.

## Recommended next order

1. Quiet broad gate on the handoff commit; update step 1 evidence.
2. Typed-site classifier (defect 1).
3. `Address` first slice; `CallerResult` first slice.
4. Remaining stale tests (defect 5), pin re-exports (6).
5. Peer review of the v2 contracts, then the production notebook slice
   (completion plan step 2), folding in `cell-compiles.md`.

## Working rules and gotchas

- User preferences: always look for the structural fix; stop for a peer-LLM
  review at natural points; subagents as interns for typing-heavy work, the
  lead handles the engine core; Opus investigators allowed for read-only
  investigation. Peer-authored code is committed naming the peer, without a
  Claude trailer. Commit per verified fix, `git add <files>` (never `-A`),
  exact commands and counts in the body; trailer
  `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
- Serialize CPU-heavy runs. Actor-host tests are load-sensitive: running
  several together produces `Elapsed(())` timeouts that look like failures;
  rerun alone before classifying. Never run them alongside the corpus.
- `just fixtures-check` fingerprints `haskell/src`, `haskell/app`,
  `haskell/lib`, `Suite.hs`, cabal files: any library change needs
  `just fixtures-update` first. The corpus step compiles codegen and the
  extractor from the tree, so do not edit those while it runs.
- `python3` may be missing from the tool shell's PATH; use
  `/home/inanna/.nix-profile/bin/python3`. zsh does not word-split `$var`
  lists (use `xargs`). `git rebase -i` is unavailable; rebuild unpushed commits
  with `git commit-tree` if needed. A shell `&` job is not tracked; use the
  tool's background mode.
- Clippy must run with `--keep-going` (now the default in `scripts/lint.sh`).
- Prepared corpus results: `target/prepared-corpus/suite.*/results.json`.

## Investigations folded in after handoff


### Notification test "expected never-assigned recipient policy" — stale test
`notification_admission_and_poll_preserve_typed_request_bindings`
(`tidepool/src/actor_host.rs` ~7227-7240) requires the next deployment after
`startAgent (readonlyAgent ...)` to be `PolicyInstalled`. `assignment`
defaults to `NotifyOwner` (`haskell/lib/Tidepool/Agent/Assignment.hs:62`), so
when the child replies `reevaluate_watches` (`tidepool-actor/src/request.rs`
~1635-1660) queues a `SettlementNotification` for root, published as
`SettlementChanged` (`resident_actor.rs` ~847-852) before `PolicyInstalled`;
`pollResponse` is not a watch, so it does not suppress the notice. The notice
was added deliberately in bbf724ae1; the assertion predates it (b4fab4757).
Fix (test only): right after the answer (~7221) receive and assert
`SettlementChanged` for root, label `"notification-original"`, transition
`Ready`; keep the `PolicyInstalled` match and make its `_` arm print the event.
Unverified: whether later parts of the test pass once the notice is drained.

### Extractor prepared turn mode (first unit of the production slice) — map
- Request: nullary `PreparedTurn` field, tag 39 (`RetainedGeneration` is 38).
  Rust `tidepool-extract-cmd/src/request.rs` (`enum Field`, setter beside
  `turn()`, `encode_field`, decoder tag match, CLI rendering or exclusion like
  `retained_generation_is_absent_from_the_cli_flag_parser`); consider bumping
  `MAGIC` (`TPREQ007`). Haskell `ExtractRequest.hs` (`RequestField`,
  `requestPreparedTurn`, fold, encoder, decoder). Round-trip test beside
  `retained_generation_round_trips_through_the_typed_protocol`.
- `haskell/app/Main.hs` `runTurnMode`/`compileVariants`: when set, compile with
  `PreparedStg` and the request's retained generations (as `processFile` does)
  and call `writePreparedArtifacts ... [targetName]`. Project the single
  `__result` entry (tuple for multi-binder binds), not one artifact per binder.
  Every live session binder a turn references must be a retained generation,
  identified exactly as `mkBoundBinders`/`sessionBinderName` mint it; session
  value modules are thin interfaces with no unfoldings.
- Rust: `CompiledTurn.prepared: Option<PreparedProgram>` read from the
  `__result.prepared.cbor` sidecar in `read_compiled_turn`;
  `TurnRequest.{retained, prepared}` forwarded in `run_turn_with_pin`; callers
  (`resident_workbench.rs`, harness `lifecycle.rs`, `hosted_lifecycle_tests.rs`)
  pass `prepared: false` until cutover.
- `prepared_turn.rs`: fold the retained-set derivation, identity extraction,
  artifact parse and install sequence into `session/prepared.rs`; delete
  `SessionTurns`, `TurnForm`, `project()`, `turn::prepared_turn_module`.
- Test: rewrite `tidepool-runtime/tests/prepared_turn.rs` on
  `resident_workbench_templates` + `eval_import_lines` (model:
  `turn_classification_corpus_old_and_new_path_agree`), three `run_turn`
  calls plus a two-name bind; register in `tests/suites/`.
- Risks: `Tidepool.Prelude`/generated `Tidepool.Effects`/`Control.Lens`
  visibility under prepared projection is unproven (no corpus imports them;
  `lens` needs fat-interface bodies) — try a one-line production-template turn
  first; heap custody for retained imports is not settled (avoid GC across
  turns); asks and Core `result.cbor` are still written in parallel.

### Corrected stale-fixture patches (`patches/stale-fixtures/v2/`, not compiled)
Supersede 03, 04e and 04f:
- 03 `hosted_lookup_and_status_use_actor_owned_views`: nothing replaced
  `available`; `lookup_tool.rs::render_text` now prefixes value lines with an
  availability label (`  [available] awaitSettled :: …`), so the test's name
  parse took the label too. Keep the doc-sentence swap; take the last token of
  the name part.
- 04e `unix_http_live_workbench_and_graph`: the runtime-failure cell must use
  the proven committed-prefix shape (`notebook_prefix_failure.hs`:
  `x <- pure ...` then a failing pattern bind), not `let` + `if error`. Open
  question worth checking: whether a `let` prefix before a runtime failure
  should stay committed (possible product gap vs the "earlier bindings remain
  committed" contract).
- 04f `failed_command_display_retains_result_without_reexecution`: the earlier
  diagnosis was wrong. `Right (result)` is the Either `Display` instance
  rendering `Cmd.stdout`; the failing assertion is most likely the stale needle
  `"Right result"` (l.651), not the display-failure path, which can still fail
  at runtime. Keep the explicit erroring `Display` instance, change the needle
  to `Right (result)`, bind with `<-`. The same stale needle is at l.993 in
  `command_presentation_is_automatic_scoped_and_retains_quiet_results`.

### `let` prefix before a runtime failure — most likely a fixture artefact
A notebook `let x = e` is `TurnKind::Bind` (`turn.rs:65`, `:297`), compiled and
run as its own item (`resident_workbench.rs` `begin_ready_block` 2458-2465,
`settle_fragment` 2552-2588), and a later runtime rejection does not roll back
earlier receipts or bindings (`resident_actor.rs:4786-4825`). The `<-` and
`let` paths are identical there. The 04e cell's missing `committedPrefix` is
therefore most likely because the `if error ...` item was rejected at
preparation (whole cell `NotRun`, `resident_actor.rs:4521-4550`) or surfaced
as a hard workbench failure rather than `ResidentError::Run`. The contract
(`plans/notebook-cells/README.md:8-9`) says runtime failure retains the
committed prefix, `let` included. Resolve with one run of the original 04e
cell: item 0 `NotRun` means the fixture needs a genuinely runtime-only failure;
item 0 `Committed` with the binding gone afterwards is a real `tidepool-actor`
gap (add a `let` variant beside `notebook_prefix_failure.hs`).

### Remaining lifecycle/drain timeouts — load, with a likely config root cause
None of the seven shows a product defect (inferred from code and timings, not
solo reruns). Six Haskell-backed tests (`authored_seal_survives_lost_waiter_and_rejects_late_work`,
`notebook_cell_cancellation_stops_at_item_boundaries`,
`resident_sleep_waits_fifteen_minutes_without_blocking_a_sibling`,
`http_actual_resident_seal_identity_late_dispatch_and_completion`,
`lifecycle_sources_follow_replacement_and_capture_retained_exit`,
`root_recovery_replays_lost_workbench_reply_without_repeating_effects`) ran
out of time on waits that include a Haskell compile, before reaching their
product-specific steps. Caveat: workbench dispatch checks out the single forest
machine through `checkout_queued` (`registry.rs:328`), which has no timeout; a
leaked checkout would look like a slow compile.
- **Config root cause:** `.config/nextest.toml`'s `ghc-heavy` cap exempts
  `package(tidepool)` (so all `actor_host::tests`, `command_jobs_tests`,
  `actual_seal`) and `package(tidepool-actor) & !kind(test)` (so
  `lifecycle_tests`), although they compile Haskell; they ran at full core
  count. Put `actor_host::`, `host_dynamic_tools::drain_tests::actual_seal`
  and `resident_interactive::lifecycle_tests` under `ghc-heavy`.
- Raise the 30s `wait_until_sleeping` windows (`hosted_lifecycle_tests.rs`
  ~257 and ~579) to at least 90s; rerun the six alone at HEAD
  (`lifecycle_sources` also exercises 052efa6ed at its stage 3).
- `pinned_full_tui_binds_and_accepts_exactly_one_owned_input` fails in 0.017s
  on a missing `TIDEPOOL_INTERACTIVE_CODEX_BIN`: mark it
  `#[ignore = "requires TIDEPOOL_INTERACTIVE_CODEX_BIN (pinned Codex) and tmux"]`,
  matching `tidepool/tests/shoal_namespace_entry.rs:122`.

### Shared constructor enter rows after retirement — yes, with conditions
Any surviving owner's `prepared_enter` can serve a shared evaluated-constructor
header: the constructor branch (`entry.rs:130-145`) only compares the header
with descriptor addresses and returns the reference, with no program-local
state, and `enter_owned_headers` (`prepared_program.rs:917`) is built from the
same `enter_evaluated` list, so every registered owner has the compare.
Conditions for `lifetime-contract-v2.md`: rows share only identical
descriptor addresses (interner keeps the first); re-point the row to a
surviving owner before freeing the retiring pipeline; "any owner" holds only
for evaluated rows (thunk rows run one program's body) — store a kind flag;
the ownerless-but-live state needs a machine-owned evaluated-identity enter
stub or the enter row removed (non-declaring programs then hit
`BadThunkState`). Rows must store the owner set plus per-owner code (or a
`ProgramId` lookup). Also check: `owns_prepared_entry` returns true for
constructor headers, so a call miss on a constructor is a reusable
`UnresolvedCallee` — contrary to its doc comment.

### Prepared fixture regeneration checklist (for any schema version bump)
Bump `tidepool-repr/src/execution_schema.rs:9` and
`haskell/src/Tidepool/ExecutionSchema.hs:24`, rebuild the extractor, then
(steps 1-4 from `haskell/` inside `bash scripts/dev-shell.sh`):
1. `test-execution-schema-encode/fixtures/schema6-intrinsic.cbor`:
   `cabal test execution-schema-encode --test-options='--write-schema6-fixture test-execution-schema-encode/fixtures/schema6-intrinsic.cbor'`.
2. `test-prepared-stg/fixtures/m3-vertical.cbor`:
   `cabal run execution-schema-projection -- test-prepared-stg/fixtures/m3-vertical.cbor`.
3. `freer-resume.cbor`: `cabal build tidepool-extract-bin execution-corpus-projection`;
   `$(cabal list-bin execution-corpus-projection) test-prepared-stg/FreerResume.hs FreerResume test-prepared-stg/FreerResumeTargets <out> lib`;
   copy `<out>/2.prepared.cbor`.
4. `freer-retention.cbor`: no recorded generator; likely the same probe with
   `FreerRetention.hs FreerRetention test-prepared-stg/FreerRetentionTargets`,
   copying `<out>/0.prepared.cbor` — verify the manifest row is `freerRequest`.
5. Import fixtures (all three), from the repo root:
   `source scripts/lib-extract.sh && resolve_tidepool_extract` then
   `cargo test --config 'build.rustc-wrapper=""' -p tidepool-extract-cmd --test import_fixtures -- --ignored --nocapture`.
Then `just fixtures-check`, the `tidepool-repr` `execution_schema_contract`
test, and `cargo nextest run -p tidepool-runtime --test prepared_execution --ignore-default-filter`.
Core CBOR fixtures are unaffected. Five of the seven prepared fixtures have
never been regenerated through a bump before.

### `*Sited` siblings — all consistent
All siblings match their surface verbs' foralls and constraints in order, with
the `Int` right after the constraints (index 3 for `runLLMTurn*`/`fork*`,
4 for `finalize`, 2 for `forkMap`/`forkCata`). Core siblings are generated from
`tidepool-protocol/src/effects/{run_llm_turn,fork}.rs` via
`tidepool-mcp/src/generated/*` and written by `ensure_effects_module`;
`forkMapSited`/`forkCataSited` live in `haskell/lib/Tidepool/Answerer/Fork.hs`.
Cached effects folders without siblings are stale content-hashed leftovers.
The classifier plan's literal-index rule is consistent with every sibling.

