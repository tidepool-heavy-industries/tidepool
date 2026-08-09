# Plans

This directory describes the current Tidepool work. Completed plans, design
history, experiment receipts, and superseded handoffs are intentionally kept
in git history rather than here.

## Active work

- [One-spawn turn protocol](one-spawn-turn-protocol.md) + [Phase B
  contract](one-spawn-turn-protocol-phase-b.md): LANDED, both phases. One
  extract spawn per turn on every caller; `--emit-stmt-binders` and
  `--emit-binders` deleted (a WIRE BREAK — `scripts/redeploy.sh` must ship
  extract and servers together); the extract-side `classify` phase live and
  `classify_extract` retired. Read the Phase B contract for the classify
  lane's surviving per-BLOCK shape.
- [GHCi affordances](ghci-affordances-todo.md): deferred `:t` and multi-item
  turn support, revisited after Phase B.
- [Post-restart execution](post-restart/): the current implementation lanes,
  gates, and benchmark track.
- [Extract-side latency wave](post-restart/extract-wave.md): **folded
  2026-08-09 — read its CLOSING STATE section first.** C1, D1-A and E6 landed
  verified (E6 **moves the wire**); item 0 (both boot seeds deleted), item 0b
  and the `--targets` prerequisite landed **UNVERIFIED** — gate runs were
  stopped under the wrap-up directive and their legs run in the centralized
  pass. Item 0's headline is an *expected, unmeasured* 4 → 2, not 4 → 1;
  `PRE_MODEL_EXTRACT_COMPILES` deliberately stays at 4 so the drop must be
  measured rather than assumed. Wave 3 (render+loop fusion) and D2 are CUT and
  routed forward ready-to-spawn, D2 with a 212-line hand-off at
  `extract-wave/spawn-latency/03-d2-handoff.md`.
  The closing section also lists six standing hazards the wave established but
  did not fix — chief among them that `haskell_suite_differential` and
  `corpus_report` **never invoke the extractor** and cannot gate extractor
  changes, and that the "pinned id-stability" trio is three DataConId guards
  observing no VarIds.
  `extract-wave/OPERATIONAL.md` is the wave's operating doctrine — read it by
  ref, never from a worktree copy.
- [Generic askUser PRD](self-iterating-harness/14-generic-derived-askuser-prd.md)
  and [generic surface wave](self-iterating-harness/15-generic-surface-wave.md):
  the current typed interaction surface.
- [Typed subagent spawning PRD](self-iterating-harness/18-typed-subagent-spawning-prd.md):
  the next substrate/product direction.

The post-restart directory contains the operational source of truth for work
in flight. The numbered self-iterating-harness documents above are the current
forward-facing design documents; no chronology is implied by their numbers.

## Reference

- [Decision archive](decision-archive/README.md): doc-history asides
  extracted out of `CLAUDE.md` files (root + per-crate) during the
  2026-08-08 restructure, so those files stay working guides instead of
  accumulating backstory. Current architecture contracts are NOT here —
  they stay in each `CLAUDE.md` (root's Key Decisions Reference is
  authoritative and verbatim).
