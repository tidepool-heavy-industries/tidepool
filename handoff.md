# Agent-to-agent handoff: alpha acceptance

## Mission / decisions

Continue toward publishing Tidepool as an alpha. Do not equate focused tests
with clean-user or live-provider acceptance. User wants successor in this SAME
repository, not a new worktree/session running concurrently against it.
Host path: `/home/inanna/dev/tidepool`; actor mount: `/tmp/tidepool-actor-workspace`.
Previous tmux run: `shoal-tidepool-followup`. Stop it before launching successor.
Live heap/bindings do not survive. All implementation workers were retired
with `StoppedNow`; no child obligations need transferring.

User-approved positioning (README draft already committed): harness AND
orchestration system built from ground up for rapid token-efficient RSI.
Reasoning LLMs = System 2; Jev = first-class System 1. Agents edit/hot-swap/share
System 1 programs live. Live Haskell notebook/shared heap plus xmonad.hs-style
on-disk tools/hooks; expressive typed DSLs and live values/functions across actors,
not JSON-only communication. Unfold work into trees, fold typed results through
review/integration; Erlang-style supervision, inherited/cache-conscious prefixes,
Bubblewrap/COW/resource budgets against Sorcerer's Apprentice. Be enthusiastic,
clear, personal, accurate; no recursion-schemes detour. Cache reuse is not
guaranteed by Bubblewrap; scope/authority constrain shared values.

## Integrated source and evidence

- After-turn implementation: 4eb47e36d, 10094d205, repair 6ed65780c;
  integrated via abd561c0a. Independent Sol Medium review ACCEPTED 6ed65780c.
- Review fixed real bug: Annotated text was discarded. Now retains bounded
  2000-character Unicode-safe text in detailed status, never provider context.
- Root integration check:
  `just test-lib tidepool 'test(after_turn_) | test(bound_retirement_cancels_and_joins_delivery_and_turn_observer)'`
  exited 0 with clean cleanup. Worker/reviewer independently ran five after_turn_
  tests and one Bound retirement test, all passing. No broad `just verify`.
- Tests prove typed payload, spec revision reload, baseline/cursor selection,
  multiple completions/poll, dedup/gaps, Pruned rejection, authored failure,
  suspended-effect timeout followed by a successful cell, real Bound retirement
  seam cancellation/join of both delivery and turn observer.
- Local watchdog baseline e59a63088; bounded escalation evidence a26962652.
  After-tool slot is CHILD-ONLY; root abstains. Explicit support+trigger Jev
  questions avoid interpreting unavailable history as misconduct. Experimental
  support floor 0.8 is not calibrated. Escalations include child-local handle,
  bounded JSON arguments and first 12 numbered displayed-output lines. Prefix
  may omit trigger; this is explicitly disclosed. This is not pre-action safety.
- Role profiles e9d445e65; context selection b69d9502b + repair 0fd86d8fa.
  New modules `.shoal/Project/{SupervisionProfiles,ContextSelection}.hs` plus
  shipped counterparts/checks. Root reload compiled both and fixtures (install 6).
  NOT globally enabled. Fixture recipes compiled but NOT executed; no live
  Jev context-selection proof. Context selector validates config before effects,
  preserves J.each identities, abstains on ambiguous/all-selected/missing context,
  and labels input as supplied/display-bounded ToolResult, not full stdout.
- README narrative a3a445a17 (earlier portions also mechanically checkpointed
  by fork admission); setup docs 30183a08b; current pre-handoff HEAD dbef8e8e7.
  Setup now documents pinned-client login, finite systemd slice, cache trust,
  minimal scaffold versus extended example; stale starter-watchdog comment fixed.

## Build ready for successor

Executed `just shoal-init -- --help` successfully on integrated source.
This builds matched Rust extractor frontend + Haskell worker, validates the
extractor/compiler endpoint, builds `target/debug/shoal`, then prints init help
WITHOUT starting a session. Build exit 0; Rust Shoal build reported 55.74s.
Toolchain: Rust 1.93.0, GHC 9.12.2.
Pinned client: `/nix/store/i7kchmwvj25j8jcdsngrvwbx9i5ifmnl-codex-rs-0.0.0-dev+fc8e158/bin/codex`.
Use repository bootstrap to relaunch so environment selects matched tools:
`just shoal-init -- --session shoal-tidepool-alpha --no-attach --model gpt-6-astra --effort high`.
Current live host predates afterTurn; Haskell hot reload cannot retrofit Rust
observer wiring. Successor is required. No production cache artifact was built
or published by the debug bootstrap.

## First successor task: actual live afterTurn acceptance

Read `.shoal/plans/turn-supervision.md`, owning contributor instructions, and
`tidepool/src/actor_host/agent_spec_tests.rs` for compiled examples.
Current `.shoal/AgentSpec.hs` deliberately still configures only afterTool.
Install an observational afterTurn hook in successor, preserving existing tools
and afterTool. Use defaultSpec record update; no new tool schema.

API: `afterTurn :: Maybe (TurnObservation -> Eff effects Annotation)`.
TurnObservation fields: `turnObservationThread :: Text`,
`turnObservationTurn :: ConversationTurn`; typed roles/items match reflect.
Dispatcher entry 2. Return Annotated marker/review; Pruned is invalid here.
Begin with deterministic markers to prove delivery/reload, then compose Jev.
Use actual compiled test patterns, not speculative snippets.

Acceptance sequence:
1. Read detailed status; verify observer exact thread and baseline.
2. Install marker A hook; reload. Complete a REAL text-only model turn.
3. Next user/operator interaction reads status: one observational after-turn
   record with exact marker A, thread/turn identity. No self-nudge/provider
   insertion. Distinguish real rollout event from manual endpoint test.
4. Change to marker B; reload; complete another real turn; assert B, not A.
5. Exercise Jev in hook; preserve errors and evidence; don't invent success.
6. Record operational cleanup evidence when stopping a test/successor as
   appropriate. Do not terminate your own host before saving observations.

Current semantics: first readable completed-turn snapshot establishes baseline;
only subsequent distinct completions eligible. Startup racing completion can
be intentionally skipped. Observer enumerates unseen completions in order,
records gaps, never blindly replays effects. Polls full conversation each second;
in-memory seen set is unbounded: explicit first-slice scalability limitation.
Failures/timeouts passive/status-visible. No automatic nudges, no assignment
provenance in TurnObservation yet. Parent reload does not upgrade live children.

## Alpha release gates still OPEN

1. Above live text-only Codex/TUI acceptance. Focused tests are not this proof.
2. Exact release Nix build, full closure cache publication/verification.
   `https://tidepool.cachix.org/nix-cache-info` publicly responds.
   flake URL/key configured; Cachix CLI installed. Earlier dirty-checkout outputs
   awpjigidqfzmijrq788r01idncbh40ci-shoal and
   qbphxn6c30m4lj8l21dhwvaz4v0cgddq-tidepool-extract both returned HTTP 404 narinfo.
   These are NOT current release output paths; reevaluate exact final revision.
   Dependencies may be cached. No repo cache-push workflow found.
   Do not claim endpoint existence proves closure coverage.
3. Clean-user smoke: no GitHub credentials/warm store, evaluate flake, substitute/
   build, pinned Codex authentication, finite swarm.slice, shoal new/check/init,
   simple Haskell+Jev, one read-only child/reply/cleanup.
   Pinned Codex archive fc8e158 publicly downloaded HTTP 200 without credentials;
   stale source comments call it private. Clean Nix evaluation remains distinct.
4. Execute new helper pure fixtures and live optional selector before enabling
   default pruning. Both workers hit new-module import visibility despite
   successful reload/typecheck; root has not re-proven that issue after successor.
5. Update docs/skills for afterTurn once live acceptance passes; README currently
   explicitly says integrated/focused-tested but live acceptance pending.
6. Review final diff, formatting/checks, exact release revision and publication
   permissions. No release tag/push/cache upload has occurred.

Setup docs evidence: systemd-analyze accepted slice example; selected client
help confirms login/status. Full nix develop authentication command not executed
to completion; no secrets read, no authentication performed. Do not change host
limits or trust settings without considering existing runs.

## Suggested work policy

Use Sol Medium implementation/review; Luna allowed for bounded independent work.
Keep cross-layer contracts/root integration owned centrally. Delegate through
Haskell unfold, inspect exact commits, retain useful evidence, stop done workers.
Use focused just checks, not routine two-hour verify. Build/publish readiness is
separate from code review acceptance. Record remaining uncertainty explicitly.
