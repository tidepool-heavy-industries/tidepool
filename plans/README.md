# Plans

This directory describes current and pending Tidepool design work. A plan
doc is scaffolding for in-flight work, not a home for standing truth — once
work lands, load-bearing content is hoisted into the owning CLAUDE.md /
charter / glossary and the plan file is deleted (git is the archive).

## Active work

- [Persistence versioning design](persistence-versioning-design.md):
  LANDED 2026-08-24 — all six persistence kinds carry version stamps via
  `tidepool_repr::version_ladder`; legacy files migrate as v0, future
  versions refuse loudly; old-corpus replay test in place. Doc awaits its
  hoist-and-delete at wave end.
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
- [Session test review](session-test-review.md): read-only catalog of
  ~285 session-compile-driving tests feeding the turn-count cuts;
  retires with them.
- [Flight dogfood campaign](flight-dogfood-campaign.md): live process doc
  for autonomous fresh-session dogfood rounds driven by the root + native
  subagents while the operator is offline; robot-operator form answering,
  per-round analysis reports, scenario battery.
- [One session](one-session.md): Phases 0–5 landed (2026-08-12) — the
  self-harness runs collapsed on one resident session. Phase 6 (repl/
  one-shot conversion + slot deletion) stays deliberately parked behind a
  production-soak gate, not currently in flight.

## Carried-forward one-liners

Small still-open items whose originating plan doc has been retired:

- `:t` (a type-answer turn classification arm) remains unbuilt; the interim
  mitigation (signatures folded into the answerer prompt) covers the
  near-term need. Revisit at the next spawn that wants it.
- Held pending an explicit operator ping: moving the hand-carried Haskell
  turn/agent decls (`typed_request_agent_decls` et al.) into
  tidepool-protocol's generator; schema support for polymorphic verbs whose
  response type is bound at the invocation site (`fork @T`, `runLLMTurn @T`);
  and, after both land, the standing one-home rule for what the generator
  owns vs. the stdlib vs. verb libraries.
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
