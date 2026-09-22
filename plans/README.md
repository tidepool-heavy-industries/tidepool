# Plans

This directory describes current and pending Tidepool design work. A plan
doc is scaffolding for in-flight work, not a home for standing truth — once
work lands, load-bearing content is hoisted into the owning `AGENTS.md`, source
contract, or focused design reference and the plan file is deleted (git is the
archive).

## Active work

- [Agent spec and System 1 slots](jev-lab/agent-spec-design.md): one Haskell
  module per checkout names an agent's tools and the slot applied after every
  tool call; edited with file tools and reloaded live. After-tool is the first
  slot. [Alpha smoke run](jev-lab/alpha-smoke-brief.md) is the brief that
  exercises it with Jev in every layer.
- [Jev lab](jev-lab/README.md): the live measurement lab behind the
  `shoal-jev` skill. [Results](jev-lab/RESULTS.md) and the
  [intent experiment](jev-lab/INTENT-EXPERIMENT.md) cover programs that turn
  failed builds into source investigations; [wording A/B](jev-lab/wording-ab.md)
  has the before/after numbers behind the skill's wording rule. The breadth
  survey's [recognizing fit](jev-lab/breadth/RECOGNIZING-FIT.md) and
  [unrun ideas](jev-lab/breadth/NOT-RUN.md) are read from `NEXT.md`. The
  [observation-budget finding](jev-lab/observation-limit/FINDING.md) is fixed,
  kept as regression evidence for `observation_budget_tests.rs`.
- [Shared execution server](interactive-applications/shared-server.md):
  direction for one native execution server per swarm shared across ordinary
  interactive TUIs, reusing the existing app-server/client boundary.
- [Shoal implementation handoff](../NEXT.md): the current programmable
  workspace direction — model/context selection, frozen customization,
  routes, scoped observations, launch previews and executable planned Sol
  recipes.
- [Haskell command workbench](next/haskell-command-workbench.md): in-progress
  design for inspectable bash values, memory-weighted execution, typed process
  interaction and gradual migration from native shell orchestration.
  [Implementation checkpoints](next/command-jobs-implementation.md) (with its
  [wave delivery contract](next/WaveContract.hs)) track the matched
  native/resident acceptance and release.
- [Shoal commit-and-fork operating model](next/shoal-commit-forks.md): a
  proposed operating model from user interview, not an implementation
  commitment — when delegation pays, keeping planning infodense with the
  parent, and Luna's recursive-delegation shape.
- [Small typed workers](next/small-workers.md): a later, optional application
  of a typed numeric-discrepancy worker as a small read-only Sol worker,
  reusing existing fixtures rather than a comparative evaluation campaign.
- [Typed file tools](typed-file-tools.md): composable resident Haskell reads,
  edit previews and stale-source-checked mutations over existing tool owners.
- [Small typed agents](small-agents.md): parent-pane, shared-worktree workers
  with selected typed context and reusable Haskell-defined tool interfaces.
  These broader capabilities and typed file tools are complementary designs;
  they do not gate the current orchestration package without a concrete consumer.
- [JIT memory lifetime](actor-model/jit-memory-lifetime.md): executable-memory
  reclamation and honest recovery after removing the fixed JIT arena ceiling.
- [Test-time cut](test-time-cut.md): diagnosis landed; family bundling
  merged, turn-count top-5 lane in flight.

## Supporting actor designs

These references do not replace NEXT.md or prescribe the current swarm topology.
Verify mechanics against owning source; old dispatch and acceptance instructions
are not current assignments.

- [Self-writing Haskell actors](actor-model/README.md),
  [architecture](actor-model/architecture.md) and
  [implementation status](actor-model/implementation.md): the canonical
  actor, request and residency design references, and the maintained
  landed-versus-pending inventory. The request/reply/watch contract and
  cache-preserving context unfold they once specified have landed; their
  stable user contracts are in `SHOAL.md` and `tidepool-actor/CLAUDE.md`.
- [Live values and authority](actor-model/live-values-and-authority.md):
  same-machine value transfer, caller identity, launch grants, and the
  distinction between invoking a closure and calling an actor.
- [JSON optics in the resident workbench](actor-model/json-lens.md): future
  direction, not an implementation commitment — schema-aware discovery,
  retained JSON data, and composable lens/Aeson-style investigations.
- [Context-tree human acceptance](actor-model/context-tree-human-acceptance.md):
  the human-run acceptance guide for inhabiting a tree of work; the campaign
  itself is not yet recorded.
- [Astra UX exploration field report](actor-model/astra-ux-exploration-field-report.md):
  a computation-driven exploration session, still cited as source evidence by
  other active plans.
- [Other useful uses of resident Haskell](actor-model/resident-haskell-side-quests.md):
  a brainstorm of resident-Haskell experiments to try in a campaign, not a
  proposal to ship another DSL.
- [DevSwarm](devswarm-haskell-dsl.md): the earlier self-harness design for
  `harness-dogfooding/devswarm/`; `NEXT.md` owns the current direction.

## Carried-forward one-liners

Small still-open items whose originating plan doc has been retired:

- The one-session collapse (Phases 0–5, landed 2026-08-12): the self-harness
  runs collapsed on one resident session. Phase 6 (repl/one-shot conversion +
  slot deletion) stays deliberately parked behind a production-soak gate, not
  currently in flight.
- Beyond the landed top-5 turn cuts, ~215 further prunable session-turns
  were cataloged per-test (git: plans/session-test-review.md, retired
  2026-08-24). Largely mooted if the compile daemon serves battery
  compiles (a warm request is ~196ms vs ~5.3s); revisit only if session
  legs still dominate post-daemon-phase-1 measurements.
- `:t` (a type-answer turn classification arm) remains unbuilt; the interim
  mitigation (signatures folded into the answerer prompt) covers the
  near-term need. Revisit at the next spawn that wants it.
- **#24 (stdlib-vs-generator ownership, the standing one-home rule for what
  the generator owns vs. the stdlib vs. verb libraries) — first act landed,
  the rest still open.** `Ask`'s pure `isOpt`/`innerSchema`/`schemaToValue`/
  `data Schema` (`tidepool-protocol/src/effects/ask.rs`'s motivating case)
  moved from `effect_defs.rs`'s decl `type_defs`/`helpers` into
  `haskell/lib/Tidepool/Form/Schema.hs`, auto-imported via
  `extra_imports_for!(Ask)`; `Ask`'s decl block is now just `ask`'s own thin
  verb wrapper. The generator flip itself did NOT happen and still can't:
  `ask` calls `schemaToValue` directly rather than a bare `send (Ctor …)`,
  so it stays outside `HelperBody`'s reviewed shapes and hand-carried in
  `effect_defs.rs` — see `ask.rs`'s module doc for the detail. What #24 as a
  whole still owes: a stated general rule (not just this one instance) for
  when a decl helper belongs in the stdlib vs. the generator vs. a verb
  library, and an audit of whether any other hand-carried effect has the
  same stdlib-shaped-helper smell `Ask` had.
- No end-to-end test coverage of the timeout → grace-expiry → `Wedged`
  session transition (the reclaim paths OUT of `Wedged` are pinned; entry
  INTO it needs a JIT-cancel-resistant runaway or a seam to simulate one).
- Companion memory Phase 3 (recall verb, async spawn, digest work) remains,
  friction-driven — no fixed schedule. Phases 1–2 (outer-row `Subagent`
  servicing, store bootstrap, curator wiring) are landed.
- Workbench friction still open as of the 2026-09-17 lab: no `T.decimal`
  (or a `readInt :: Text -> Maybe Int`) in the Prelude; a declaration in a
  cell cannot see a value bound earlier in the same cell (declarations
  compile in a separate plane from statements); an authored project module
  is invisible to the agent's own `lookup`/`doc`, so a human must name the
  file path by hand; a Shoal session's workspace binding lock is keyed on
  the worktree path, not the session name, so several distinctly-named
  sessions cannot launch concurrently against one shared `--workspace`
  (`tidepool-worktree/src/binding.rs`) — the second session gets a hard
  storage-failure error, not a queue.
- `doc <topic>` now names the covering skill on a refusal (fixed), but a
  few 2026-09-17 lab findings were not reverified before this prune:
  `renderGitOid` (exported at `haskell/lib/Tidepool/Worktree.hs:131`)
  reportedly came back `no match` from `lookup`; `R.start`/`R.client` came
  back tagged `[unknown]` because their `Derive`/`Generic`/`GActor`
  constraints don't fit the `Member X effs` shape lookup classifies by; and
  no child was ever told in its task template to commit its work, which was
  the root cause of at least one child reporting success over an uncommitted
  change.
- Across 57 harness transcripts and 207 rejections (2026-09-17 measurement),
  "a value of the wrong type or arity" was the largest failure class in
  every run and had not fallen after two fix waves, while fixing one
  constructor's ergonomics (`IsString GitRef`) removed its whole failure
  class outright. The lever that measurement pointed at: advice naming the
  smart constructor for the expected type, plus one worked example per shape.
- Gated successor work from the live context-unfold hardening wave, still
  open: reattaching a failed external child application to its still-live
  actor/machine incarnation needs a durable provider idle/reattachment
  contract that makes duplicate turns impossible; generalizing root
  source-only successor recovery to independently-owned child machines needs
  Shoal's machine-boundary work first; a durable actor/request/watch/fork
  transition ledger (with replay tests at every commit point) should be
  owner-emitted, not inferred from composition-root observations; and
  handle-filtered `:trace`/campaign renderers are worth adding only once a
  live campaign shows the existing `observeCampaign`/`:lineage`/`:status!`/
  `:trace` views are insufficient.

## Reference

- [Decision archive](decision-archive/README.md): the narrow exception to
  "inline doc-history is deleted, git is the store" — backstory whose loss
  would invite re-tripping a hazard already fixed once. Current architecture
  contracts belong in owning source, `AGENTS.md`, and focused design references,
  not historical charters.
- [Notebook cell compile-count reduction](handoff/designs/cell-compiles.md)
  and [descriptor dispatch](handoff/designs/descriptor-dispatch.md): the two
  still-live designs from a since-retired handoff bundle, cited by
  `STG_KNOWN_ISSUES.md` for open cost and perf issues.
