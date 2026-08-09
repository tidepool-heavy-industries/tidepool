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
- [Generic askUser PRD](self-iterating-harness/14-generic-derived-askuser-prd.md)
  and [generic surface wave](self-iterating-harness/15-generic-surface-wave.md):
  the current typed interaction surface.
- [Typed subagent spawning PRD](self-iterating-harness/18-typed-subagent-spawning-prd.md):
  the next substrate/product direction.

The post-restart directory contains the operational source of truth for work
in flight. The numbered self-iterating-harness documents above are the current
forward-facing design documents; no chronology is implied by their numbers.
