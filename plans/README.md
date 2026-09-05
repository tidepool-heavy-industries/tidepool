# Plans

This directory describes current and pending Tidepool design work. A plan
doc is scaffolding for in-flight work, not a home for standing truth — once
work lands, load-bearing content is hoisted into the owning CLAUDE.md /
charter / glossary and the plan file is deleted (git is the archive).

## Active work

- [Self-writing Haskell actors](actor-model/README.md): Rust-owned actor
  mechanics around typed Haskell programs with resident model contexts and
  resident GHCi-style environments; live function-valued messages, caller-
  checked capabilities, program-image reuse, fresh and cache-preserving fork
  construction, and a staged migration away from global
  `State`/`render`/`loop` orchestration. Its current request/activation contract
  is [persistent applications, typed replies, and watches](actor-model/persistent-applications-replies-and-watches.md),
  and the canonical root plan, implementation handoff, and todo checklist for the
  accepted next interaction surface is
  [cache-preserving context unfold](actor-model/cache-preserving-context-unfold.md).
  Live recursive dogfood findings and the linear implementation handoff for
  resident-root UX, fault containment, observability, resource stability, and
  honest recovery are in
  [live context-unfold dogfood follow-ups](actor-model/live-context-unfold-dogfood-followups.md).
  The prompt and LLM-efficacy direction is
  [context trees and an emergent resident Haskell surface](actor-model/context-tree-emergent-haskell-ux.md):
  a shared scaffold/unfold/fold/refine practice with task-specific Haskell
  discovered during use, retained specialists, and effort-aware context reuse.
- [DevSwarm Haskell DSL](devswarm-haskell-dsl.md): clean-slate successor to
  `harness-dogfooding/dev-tree`; dynamic recursive owner sessions plus a typed
  candidate/review/revision interpreter. The current runnable slice uses a
  thin compatibility `State` while node-scoped workspace/store, durable owner
  re-entry, and a no-State entrypoint remain the platform gaps.
- [Test-time cut](test-time-cut.md): diagnosis landed; family bundling
  merged, turn-count top-5 lane in flight.
- [Flight dogfood campaign](flight-dogfood-campaign.md): live process doc
  for autonomous fresh-session dogfood rounds driven by the root + native
  subagents while the operator is offline; robot-operator form answering,
  per-round analysis reports, scenario battery.

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
  contracts are NOT here; they stay in each `CLAUDE.md` (root's Key Decisions
  Reference is authoritative and verbatim).
