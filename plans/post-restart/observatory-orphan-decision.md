# Decision: retire the observatory-orphan API cluster

Queued architectural call from `plans/post-restart/README.md` and this lane's
spec: `Harness::first_operator_hole` / `live_turn` / `pending_dialog_ui` /
`tree_snapshot` / `tree_snapshot_page` — `pub`, zero callers. Retire or wire,
with reasons.

**Decision: RETIRE**, along with everything that is write-only once they go.

Sequenced after `reasoning-continuity` folds — the cut lands in `harness.rs`.

## What the evidence actually shows

The earlier framing ("five `pub` functions with no callers") understates it.
Verified on the post-`registry-unify` tree:

| symbol | real callers |
|---|---|
| `first_operator_hole` | 0 |
| `pending_dialog_ui` | 0 |
| `live_turn` | 0 |
| `node_heap_summary` | 0 |
| `tree_snapshot` | 0 — its only reference is `tree_snapshot_page`'s doc comment |
| `tree_snapshot_page` | 0 — its only reference is `tree_snapshot`'s doc comment |
| `set_notifier` | 0 — its only reference is its own doc comment |

Two of them reference each other and nothing else, which is why a naive grep
reads as "has callers."

**The consequence is the real finding: this is a subsystem fully wired on the
WRITE side with no read side at all.**

- Nobody calls `set_notifier`, so the `notifier` `OnceLock` is never
  populated, so `notify()` is a permanent no-op — fired three times on the
  streaming path (`harness.rs` ~524, ~534, ~545) for nothing.
- `live_turns` is written on every streaming delta and cleared twice more, and
  its ONLY reader is `live_turn`, which nobody calls. It is a write-only map.

So every streaming delta currently takes a mutex and clones into a buffer that
cannot be read, then fires a callback that was never installed. This is not
merely unused API surface; it is unused work on a live path.

## Why retire rather than wire

- **The consumer is gone, not pending.** These were the seven-pane observatory,
  replaced by the minimal operator gate. `tidepool-web` imports only
  `selfharness::operator` — the gate — and nothing else from this cluster.
- **The live GUI direction does not want them.** The approved direction is a
  typed-form GUI effect over the existing web shell, and the generic-derived
  surface (`plans/self-iterating-harness/15-generic-surface-wave.md`). Neither
  needs a tree pane, a heap pane, or a live-token buffer. Wiring these would be
  building consumers for a UI nobody asked for — speculative design, which the
  repo's own rule forbids.
- **Nothing is lost.** `NodeTree` plus the durable event log carry everything
  these rendered. `tree_snapshot` is ~20 lines of DFS over `node_ids_after`; if
  a tree pane returns, it is re-derivable in an afternoon from data that never
  goes away. Deleting a view over preserved data is cheap; keeping dead
  machinery next to live lifecycle code is not.
- **It has already cost correctness once, today.** `registry-unify`'s submit
  note justified keeping a session alive past cancellation "for observatory
  display" — a real behavior deviation defended by an API with no callers. Dead
  code does not sit inertly; it gets cited. That is the concrete argument
  against keeping it "just in case."

Removal ships on the correctness gates alone — this is wrong work being
deleted, not a simple mechanism replacing a complex one, so no benchmark
receipt is owed.

## Scope of the cut

Retire: `first_operator_hole`, `pending_dialog_ui`, `live_turn`,
`tree_snapshot`, `tree_snapshot_page`, `node_heap_summary`, `set_notifier`.

Then everything that becomes write-only or unreachable with them — verify each
by grep before deleting, do not assume this list is exhaustive or correct:

- the `notifier` field, `notify()`, and its three call sites;
- the `live_turns` field and its writes;
- `LiveTurn`, `NodeSummary`, `HeapSummary`, and `node_summary()`.

**Keep** the escalation machinery (`escalation_of`, `resolve_escalation`,
`Escalation`, `OperatorDecision`, the `escalations` map). It has real test
callers in `acceptance_fanout` and is the rung-2 operator path — only
`NodeSummary.awaiting_operator`, the dead *view* of it, goes.

**Do not** remove `StreamSink` or the provider's streaming plumbing in this
cut. `drive_model_turn` takes a sink and `harness.rs` passes one; whether
streaming has a future consumer is a wave-1.5 question, not this decision's.
Cut the dead buffer, leave the pipe.

## Interaction with wave-1.5 observability — read before implementing

Wave 1.5 wants live visibility into what the harness is doing, so there is an
apparent tension. There is not a real one: that work delivers `tracing` INFO
lines and durable jsonl events, not an in-memory per-node buffer for an HTTP
pane.

If wave 1.5 wants live token visibility, the answer is to log deltas through
`tracing` on the streaming path — **not** to resurrect `live_turns`. A
write-only map is not a head start on a feature; it is the thing that made
this cluster invisible for so long.

If the observability dev lands first and genuinely consumes one of these, this
decision yields to that fact — check before cutting.

## Verify

Deletion-only, so the gates are the proof: `cargo check --workspace
--all-targets`, `cargo fmt --all -- --check`, `cargo clippy --workspace` (three
pre-existing warnings are not ours; a NEW dead-code warning after the cut means
something else just became unreachable — chase it rather than silencing it).
Quick tier with the tests-RUN count, plus `acceptance_fanout` (the escalation
path this must not disturb) and `golden_path`.

Standing environment rules apply verbatim: shared read-only
`TIDEPOOL_EXTRACT`, mandatory per-worktree `XDG_CACHE_HOME`, GHC-heavy runs
through `scripts/ghc-slots.sh run --`, one test per invocation, gate on
tests-RUN counts never exit codes.
