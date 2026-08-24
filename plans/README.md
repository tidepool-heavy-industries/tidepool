# Plans

This directory describes the current Tidepool work. Completed plans, design
history, experiment receipts, and superseded handoffs are intentionally kept
in git history rather than narrated here — see "Landed" below for pointers
to plan files that stay in this directory but no longer describe active work.

## Active work

- [Harness architecture wave](harness-architecture-wave.md): **the current
  orchestration runbook** (2026-08-24) — prewritten, operator-approved
  specs for the remaining architecture lanes (fork concurrency, suspension
  schema step 1, composition-root move, lsp deletion fold, persistence
  versioning + resident-session kernel design briefs), with execution
  order, fold gates, and hard rules. An orchestrating session should read
  this first and execute it as written.
- [Flight dogfood campaign](flight-dogfood-campaign.md): autonomous
  fresh-session dogfood rounds driven by the root + native subagents while
  the operator is offline (~7h, 2026-08-22); robot-operator form answering,
  per-round analysis reports, scenario battery over the new fork/batch/
  session capabilities.

- [Fork subsumes split](fork-subsumes-split.md): direction locked by the
  operator (2026-08-22) — the companion tree emerges from model-authored
  `async (fork @T …)` calls; fork children inherit the full node lifecycle;
  gates drop in favor of ask-idiom policies. Steps 1–4 are landed (children
  on the agent-session pump, recursive rows + spawn-time budgets, fork
  lifecycle unification onto the branch spine, and the companion collapse
  itself, `bdfc334a`) — see that plan's own status line for what remains.
- [Session-ownership capstone](registry-capstone.md): human-approved wave
  (2026-08-22), consolidating dup-c-survey items 1 (resident-session
  checkout/lifecycle) and 5 (harness suspension state) into one promoted
  registry primitive in `tidepool-runtime` and one suspension-metadata map
  in the harness. LANDED (merged 2026-08-22).
- [Turn latency: state injection + compile incrementalism](turn-latency-state-injection.md):
  narrow-first design approved by the operator (2026-08-20): prove the
  mechanism on the one compile that dominates turn wall time (the fused
  outer `render`/`loop` compile, ~67% of a companion turn) before
  generalizing. In flight: a spike has proven the injection mechanism sound
  and shipped inert build-products-dir plumbing; the state-injection compile
  path itself is not yet wired end to end.
- [The Effect Protocol](self-iterating-harness/22-effect-protocol-prd.md):
  proposed (2026-08-17), operator-approved direction. One data-only schema
  crate (`tidepool-protocol`) generating every artifact the effect contract
  currently maintains by hand — byte-compatible golden migration, one effect
  at a time, smallest first. `Exec`, `Journal`, `Worktree`, and `RepoEvent`
  are already generated from `tidepool-protocol` (see
  `tidepool-protocol/src/effects/`); the remaining legacy effects still go
  through `tidepool-mcp/src/effect_defs.rs`. See
  [22-p1-protocol-scaffold.md](self-iterating-harness/22-p1-protocol-scaffold.md)
  for the scaffold and [tidepool-protocol/README.md](../tidepool-protocol/README.md)
  for the crate itself.
- [Recursive Companion](self-iterating-harness/21-recursive-companion-prd.md):
  proposed (2026-08-17). The companion track's successor: reasoning as a
  recursively discovered typed program. Lanes C0–C6; C1 (mounting a
  function-bearing value into an agent session, the de-risk spike) **landed
  2026-08-17** — see
  [21-c1-mount-seam.md](self-iterating-harness/21-c1-mount-seam.md). C2
  (scope trees) and the exit verb are also landed — see
  [21-c2-scope-trees.md](self-iterating-harness/21-c2-scope-trees.md) and
  [21-c3-exit-verb.md](self-iterating-harness/21-c3-exit-verb.md). The
  original C3 vertical slice
  ([21-c3-recursive-companion-slice.md](self-iterating-harness/21-c3-recursive-companion-slice.md))
  — a `ThoughtF` coalgebra/algebra layer walk with gate-bounded budgets — is
  SUPERSEDED, not built as designed:
  [Fork subsumes split](fork-subsumes-split.md) step 4 collapsed the
  companion around `fork` instead — see
  [`harness-dogfooding/recursive-companion/README.md`](../harness-dogfooding/recursive-companion/README.md).
  C4–C6 status is not reassessed here.
- [Exomonad v3 — the typed swarm](self-iterating-harness/20-exomonad-v3-prd.md):
  swarm coordination as a compiled resident Haskell program. Stage 1 lanes
  S1-L1 through S1-L5 are landed in code: row servicing for
  Console/Worktree/RepoEvent/Exec (`tidepool-harness/src/engine.rs`,
  `src/selfharness/driver.rs`), the lifted agent cycle table
  (`SubagentHandler`, superseding one-agent-at-a-time), the hylo swarm
  substrate (`Tidepool.Swarm`'s `PlanF`/`hyloM`, which
  `harness-dogfooding/dev-tree/Harness.hs` v2 now uses), green threads +
  capability mailboxes (merged 2026-08-17), and resume/journal hardening
  (`tidepool-handlers/src/handlers/journal.rs`, journal-segment + crash-resume
  commits). dev-tree v2 **typechecks** against the full landed row
  (`tidepool-harness/tests/dogfood_harness_typecheck.rs`) but that probe is
  typecheck-only — it does not drive a model, spawn an agent, or touch a
  repository, so "runs end to end" is not yet verified. S1-L6 (the operator
  surface: a live outcome-tree pane with per-node cost/triage/fold-receipt
  views) has no confirmed implementation as of this audit — `tidepool-web`'s
  new d3 tree view (`tidepool-web/src/tree.rs`) is a general session-tree
  GUI, not confirmed as the swarm-specific S1-L6 surface. Stage 2 (the
  long-running repository coordinator) is chartered but not started.
- [GHCi affordances](ghci-affordances-todo.md): multi-item turns landed
  (`Harness::run_multi_item_block`); `:t` (a type-answer classification arm)
  remains unbuilt — the interim mitigation (signatures folded into the
  prompt) is landed and covers the near-term need.
- [Generic surface wave](self-iterating-harness/15-generic-surface-wave.md)
  (approved direction, 2026-08-08): `askUser`'s `Generic`-derived typed forms
  are already live (see `docs/harness-capabilities.md`); the wave's
  `GTypeDoc` interpreter — a full typed declaration synopsis for typed request prompts,
  superseding the current constructor/selector-names-only shallow synopsis —
  is not yet built. See
  [14-generic-derived-askuser-prd.md](self-iterating-harness/14-generic-derived-askuser-prd.md)
  for the originating PRD.
- [Companion memory](companion-memory.md): Phase 1 (outer-row `Subagent`
  servicing) and Phase 2 (the memory epic — store bootstrap, curator wiring)
  are landed (2026-08-14) and green. Phase 3 (recall verb, async spawn,
  digest work) remains, friction-driven — no fixed schedule.
- [Post-restart execution](post-restart/): the current implementation lanes,
  gates, and benchmark track. `extract-wave/OPERATIONAL.md` is the wave's
  operating doctrine — read it by ref, never from a worktree copy.

## Landed (pointers only)

These plan files describe work that has since landed or been superseded.
They stay in this directory as pointers into the design history (per-crate
`CLAUDE.md` files hold the current contracts); the files themselves are not
narrated here — read them directly, or `git log`, for the history.

- [One-spawn turn protocol](one-spawn-turn-protocol.md) +
  [Phase B contract](one-spawn-turn-protocol-phase-b.md): both phases
  complete. One extract spawn per turn on every caller; the extract-side
  `classify` phase is live.
- [One session](one-session.md): Phases 0–5 landed (2026-08-12) — the
  self-harness runs collapsed on one resident session. Phase 6 (repl/one-shot
  conversion + slot deletion) stays deliberately parked behind a
  production-soak gate, not currently in flight. Substrate contract
  (amended): [the parking contract](post-restart/realm-lanes/continuation-parking-contract.md).
- [Extract-side latency wave](post-restart/extract-wave.md): folded. Two
  standing hazards it left unfixed, tracked elsewhere: `haskell_suite_differential`
  and `corpus_report` never invoke the extractor and cannot gate extractor
  changes; the "pinned id-stability" trio observes no `VarId`s.

## Reference

- [Decision archive](decision-archive/README.md): the narrow exception to
  "inline doc-history is deleted, git is the store" — backstory whose loss
  would invite re-tripping a hazard already fixed once. Current architecture
  contracts are NOT here; they stay in each `CLAUDE.md` (root's Key Decisions
  Reference is authoritative and verbatim).
