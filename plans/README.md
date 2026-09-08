# Plans

This directory describes current and pending Tidepool design work. A plan
doc is scaffolding for in-flight work, not a home for standing truth — once
work lands, load-bearing content is hoisted into the owning `AGENTS.md`, source
contract, or focused design reference and the plan file is deleted (git is the
archive).

## Active work

- [Shoal implementation handoff](../NEXT.md): complete the programmable foundation
  and a usable workspace orchestration package. Model/context selection, frozen
  customization, routes, scoped observations, launch previews and the executable
  planned Sol recipes have landed. The [workbench curation pass](next/package-curation.md)
  supplies substantive owners, complete contexts/handoffs and a fluent resident
  Haskell toolbox with portable candidate checks. Its [review](next/evidence/workbench-curation.md)
  records implementation readiness and the separate live-acceptance boundary.
  Ordinary human-requested RSI improves the next swarm.
  The [plan-understanding follow-up](next/plan-readback.md) adds Sol-authored branch
  interpretations, questions and original Astra planner review before implementation
  fan-out; the current application wave is exercising the checkpoint through steering.
  The [vision](next/planned-swarm.md), [workspace design](next/workspace-pilot.md)
  and [Haskell reference](next/sol-worker-routing.md) define the chosen direction.
  Live acceptance uses fresh actors on `shoal-repl` (the standalone TUI app) or
  another non-self-hosting project, using authored guidance rather than knowledge
  inherited from implementing Shoal. Keep normal Codex TUIs for worker interaction.
- [Supervised interactive Codex applications](interactive-applications/README.md):
  complete option A implementation plans for full worker TUIs, exact native
  session binding, durable delivery, process custody, hosted completion and
  recovery, with staged integration and acceptance across Tidepool and its Codex fork.
- [Typed file tools](typed-file-tools.md): composable resident Haskell reads,
  edit previews and stale-source-checked mutations over existing tool owners.
- [Small typed agents](small-agents.md): parent-pane, shared-worktree workers
  with selected typed context and reusable Haskell-defined tool interfaces.
  These broader capabilities and typed file tools are complementary designs;
  they do not gate the current orchestration package without a concrete consumer.
- [JIT memory lifetime](actor-model/jit-memory-lifetime.md): executable-memory
  reclamation and honest recovery after removing the fixed JIT arena ceiling.
- [Haskell engine through prepared STG](haskell-engine-stg.md): reuse GHC
  CorePrep and STG preparation throughout the Rust/Cranelift engine: typed
  calls, compact layouts, full collection, explicit optimization choices,
  and required code deletion with semantic and performance gates.
- [Test-time cut](test-time-cut.md): diagnosis landed; family bundling
  merged, turn-count top-5 lane in flight.

## Supporting actor designs and separate harness work

These references do not replace NEXT.md or prescribe the current swarm topology.
Verify mechanics against owning source; old dispatch and acceptance instructions
are not current assignments.

- [Self-writing Haskell actors](actor-model/README.md) and
  [persistent applications, replies and watches](actor-model/persistent-applications-replies-and-watches.md):
  actor, request and residency design references.
- [Context unfold](actor-model/cache-preserving-context-unfold.md) and
  [live follow-ups](actor-model/live-context-unfold-dogfood-followups.md):
  earlier interaction design and runtime findings.
- [Context-tree surface](actor-model/context-tree-emergent-haskell-ux.md),
  [scaffold campaign](actor-model/recursive-scaffold-campaign.md) and
  [recursive collaboration](actor-model/recursive-context-collaboration.md):
  supporting context, ownership and integration ideas. The planned Astra/Sol
  vision owns current model placement, decomposition and guidance.
- [DevSwarm](devswarm-haskell-dsl.md) and
  [flight campaign](flight-dogfood-campaign.md): separate self-harness designs;
  their migration proposals and dogfood loops are not current Shoal gates.

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

## Reference

- [Decision archive](decision-archive/README.md): the narrow exception to
  "inline doc-history is deleted, git is the store" — backstory whose loss
  would invite re-tripping a hazard already fixed once. Current architecture
  contracts belong in owning source, `AGENTS.md`, and focused design references,
  not historical charters.
