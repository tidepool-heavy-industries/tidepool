# Plans

This directory describes current and pending Tidepool design work. A plan
doc is scaffolding for in-flight work, not a home for standing truth — once
work lands, load-bearing content is hoisted into the owning CLAUDE.md /
charter / glossary and the plan file is deleted (git is the archive).

## Active work

- [Resident-session kernel design](resident-session-kernel-design.md):
  decision-complete (operator answers recorded inline 2026-08-24);
  implementation lane in flight against it.
- [Compile daemon design](compile-daemon-design.md): decision-complete
  (operator picks in the Decisions section, 2026-08-24); phase 0
  (opt-in daemon mode + `ExtractCmd` socket transport + warm spike
  measurement) in flight.
- [Test-time cut](test-time-cut.md): diagnosis landed; family bundling
  merged, turn-count top-5 lane in flight; nextest setup-scripts pilot
  DEFERRED pending compile-daemon phase 1 (still experimental upstream).
- [Flight dogfood campaign](flight-dogfood-campaign.md): live process doc
  for autonomous fresh-session dogfood rounds driven by the root + native
  subagents while the operator is offline; robot-operator form answering,
  per-round analysis reports, scenario battery.
- [Session crate design](session-crate-design.md): design-only, awaiting
  operator picks (crate name, mounting model, multi-mount sequencing).
  Proposes promoting `tidepool-runtime/src/session/*` into its own
  `tidepool-session` crate, with multi-frontend session mounting as a
  first-class design input; plans to retire
  [resident-session-kernel-design.md](resident-session-kernel-design.md)
  once the promotion lands.

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
  the generator owns vs. the stdlib vs. verb libraries) — precondition now
  MET, still held pending an explicit operator ping; do not start it from
  this note alone.** Both prior preconditions landed: schema support for
  polymorphic verbs whose response type binds at the invocation site
  (`Polymorphism::ArgBound`/`ResultBound`, schema-plane-decls lane) and the
  OPAQUE+`*Sited`+`unsafeCoerce` `HelperBody` shapes that let
  Fork/Finalize/RunLLMTurn/Green's decl text flip onto the generator
  (helperbody-flip lane). `Ask` (`tidepool-protocol/src/effects/ask.rs`) is
  the concrete motivating case #24 should resolve: its helpers
  (`isOpt`/`innerSchema`/`schemaToValue`) are ordinary pure Haskell functions
  over the `Schema` sum — stdlib-shaped code, not decl-shaped — and stay
  hand-carried in `tidepool-mcp/src/effect_defs.rs` because representing an
  arbitrary function body as schema data would be a new general-purpose
  mechanism, not a bounded extension. The likely #24 resolution is that
  helpers like these migrate to `haskell/lib` (imported via the preamble,
  the same relocation Worktree's/RepoEvent's own non-representable helpers
  already took) and `Ask`'s decl block shrinks to `ask`'s own thin verb
  wrapper — but that is a design call for #24 itself, not decided here.
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
