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
   rejection. Fix: one shared classifier (strip `nospec`, classify the whole
   call once, literal index from the verb's type, rewrite or defer-reject,
   delete `vsMisShapeIsError`). Test with open-tail and eta-reduced fixtures.
   Probe evidence: `/home/inanna/.claude/jobs/4940a626/tmp/nospec/rep/` (may be
   deleted with the job; the conclusions are here).
2. **Caller-chosen result representation** — projection rejects
   `$mActorDefinition` (pattern-synonym matcher, reached from
   `startActor`/`startActorFork`) as "runtime-polymorphic representation".
   Design: `ResultContract::CallerResult` (proposed fixes in the completion
   plan). Blocks `shoal_exports_persistent_agents_and_hides_turn_lifecycle_operations`.
3. **`Address` host values** — the only engine gap among the 184 corpus
   execution failures (74 rows). First slice designed (observation of pinned
   literal bytes; typed refusal otherwise).
4. **Actor exit contract** (needs the user's decision, `designs/actor-exit-contract.md`):
   a handler failure during drain pauses forever; unconfirmed cleanup rewrites
   the exit kind; four exit writers. `stateful_replacement_preserves_owned_children`
   still fails (no heap corruption in its run) and is attributed here.
5. **Remaining stale tests** — patches in `patches/stale-fixtures/`, applied
   and run once, NOT committed: 02 (steering, load timeout only), 03 (doc text;
   the new cell still fails: `Variable not in scope: available`), 04e (operator;
   still references `committedPrefix`), 04f (display failure; still renders
   `Right (result)` — the structural display wins, patch premise wrong), 10
   (custody shutdown settlement arm; load timeout only), 11 (research
   escalation `assignment`; load timeout only). Re-run 02, 10, 11 ALONE; fix 03,
   04e, 04f. `notification_admission_and_poll_preserve_typed_request_bindings`
   now fails "expected never-assigned recipient policy" (investigate). 04a is a
   product prompt change awaiting sign-off.
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

(Filled in as read-only investigators finish.)
