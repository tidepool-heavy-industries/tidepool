# Plans

This directory describes current and pending Tidepool design work. A plan
doc is scaffolding for in-flight work, not a home for standing truth — once
work lands, load-bearing content is hoisted into the owning CLAUDE.md /
charter / glossary and the plan file is deleted (git is the archive).

## Active work

- [Persistence versioning design](persistence-versioning-design.md):
  design doc AWAITING OPERATOR REVIEW (2026-08-24) — version-stamped
  checkpoint + journal persistence mirroring repr's CBOR versioning;
  four open questions flagged for sign-off; no implementation until
  approved. Wire bytes (the log `Event` enum, `Checkpoint` struct, serde
  tags, journal kinds) stay frozen until this lands.
- [Resident-session kernel design](resident-session-kernel-design.md):
  design doc AWAITING OPERATOR REVIEW (2026-08-24) — unifying repl
  ask/suspend with harness suspension routing; recommends a
  tidepool-runtime module over a new crate (needs the operator's call),
  migration order conditioned on one-session.md Phase 6; four open
  questions.
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
- Resident compile daemon direction: viable in principle (every compile
  input is already explicit, no ambient interactive context to externalize),
  but blocked on the `localVarId` determinism gap — a long-running process's
  session-Unique counter would make two back-to-back compiles of identical
  source diverge on nested-Id `VarId`s. Cannot land before that gap closes.
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

## Deferred

- `self-iterating-harness/` and `post-restart/`: largely landed/superseded
  PRD 18/19/20/21/22 design and implementation-wave history, cross-referenced
  with each other and with `tidepool-harness/CLAUDE.md`. Retiring them
  requires hoisting their still-load-bearing content into
  `tidepool-harness/CLAUDE.md`, which a concurrent lane owns; deferred to
  that lane's fold rather than guessed at here. The one file in this cluster
  that is standing (not scaffolding): `post-restart/realm-lanes/
  continuation-parking-contract.md`, a frozen contract downstream consumers
  read directly — keep it wherever this cluster resettles.

## Reference

- [Decision archive](decision-archive/README.md): the narrow exception to
  "inline doc-history is deleted, git is the store" — backstory whose loss
  would invite re-tripping a hazard already fixed once. Current architecture
  contracts are NOT here; they stay in each `CLAUDE.md` (root's Key Decisions
  Reference is authoritative and verbatim).
