# tidepool-harness — resident typed-yield harness

## Charter

This crate owns the resident harness: node lifecycle, machine-session checkout,
suspension routing, authored `render`/`loop` driving, agent sessions, recursive
forks, operator interaction, durable harness events, and restart handling.

It does not own the JIT (`tidepool-codegen`), the machine-session primitive
(`tidepool-runtime`), concrete base-effect handlers (`tidepool-handlers`), or
operator rendering (`tidepool-web`).

Read module documentation for local algorithms. This file records only the
cross-module constraints that are easy to violate.

## Machine-session ownership

`tidepool_runtime::session::registry` is the session-lifecycle mechanism.
This crate's `registry` module only fixes the hole identifier type.

- Access a resident machine only through checkout APIs. Never remove a machine,
  run it, and reinsert it manually.
- A checkout epoch fences stale settlement. Timeout, cancellation, and panic
  paths must settle the checkout exactly once.
- A suspended session may own several holes. Hole identity is
  `(SessionId, HoleId)`; never recover a suspension by “the current hole.”
- `pending_suspensions` owns domain metadata for parked holes. The machine
  registry owns the continuations themselves.
- A wedged run retires the harness node. This crate does not construct a
  recoverable `Slot::Wedged` session.

The shared outer machine session is registry-owned. Attached agent nodes borrow
it through runtime resource scopes and do not own or remove it.

## Compilation and cache

All Haskell compilation goes through
`tidepool_runtime::artifacts::compile_targets`, backed by the compiled-artifact
cache in `tidepool-toolchain`. Do not add a cache-free compile path or a second
extractor frontend here.

Cache keys must include every input that can alter emitted artifacts. Mutable
per-test state belongs in a temp directory; the content-addressed compile cache
may be shared.

## Suspension and replay

The harness classifies a suspended request by constructor and routes it as a
typed hole. A response must be decoded against the exact `DataConTable` and
answer contract captured when that hole was compiled.

Replay has two meanings only:

- `ReplayProvider` substitutes recorded model replies for provider calls.
- `fold_tree_state` reconstructs durable node state after a crash.

Handled effects are not replayed. Re-executing an effect from an old log would
repeat external actions and is forbidden.

## Names and runtime resource scopes

Declarations and bindings have different lifetimes:

- Top-level declarations live in the persistent declaration environment and
  can survive model rounds, loop iterations, and machine rotation.
- Heap bindings and parked frames belong to runtime resource scopes.
- Closing a scope retires its frames, handles, bindings, and roots without
  touching siblings.
- Closure-valued results cross between continuations through `ValueHandle`, not
  JSON or the tolerant bridge.

The GC-root accounting rules are owned by `tidepool-codegen`; scope retirement
must preserve their reported counts.

## Answer contracts

Every agent session compiles against `Finalize T`. The row pins the answer type;
do not accept an untyped or post-hoc converted final answer.

The type's defining imports are resolved from extractor metadata. Generated
turn modules and validation probes must receive the same session include set as
the turn body.

A declaration-only reply may persist declarations but does not advance a turn.
Corrective feedback should state the actual compile or contract failure and let
the next model round use the declarations already committed.

## Forking and failure boundaries

`fork` and `forkAll` create attached child agent sessions on the shared machine.
Children may fork recursively within configured depth, subtree, and per-session
budgets. A refused spawn returns a legible budget result without allocating a
session or compiling a turn.

For authored `runLLMTurnFork`/`runLLMTurnFanout`:

- a child agent's failure is data at that child's position:
  `Either InvocationExit T`;
- a scheduler, assembly, constructor-table, or bookkeeping failure is a harness
  error and must not be laundered into `InvocationExit`;
- results are restored to declaration order before assembly, so completion
  order is not observable.

## Operator interaction

`AskUser` forms, notes, continue gates, and steering all use the operator-gate
machinery. Do not create a second operator-input channel.

The answerer row and outer authored row are intentionally different. When
adding an effect, update the owning row and its suspension-routing service
together; declaration alone does not provide a handler.

## Machine rotation

The shared machine rotates only at a quiescent loop boundary after reaching the
fragment ceiling. Rotation keeps the same session identity, carries checkpointed
state and persistent declarations forward, and reports machine-local bindings
that cannot be reconstructed. Never rotate while parked holes remain.

## Durable state and process ownership

The run lease prevents two live selfharness processes from repeating external
effects and racing checkpoints in the same log directory. A lease held by a
different live PID is a hard refusal. `TIDEPOOL_SELFHARNESS_TAKEOVER=1` is the
explicit operator override; takeover archives the previous lease.

There are two durable event streams:

- `transcript.jsonl`: loop-level driver and operator events;
- `log-<epoch>.jsonl`: per-node turns, compiled source, holes, and answers.

Use `scripts/current-run.sh paths|tail` instead of reconstructing current file
names in tooling.

## Verification

Choose the smallest relevant integration test. Important families include:

- checkout, timeout, and stale-settlement tests;
- suspension identity and answer-type pinning;
- recursive fork and outer fanout behavior;
- declaration persistence and closure delivery;
- machine rotation and run-lease recovery;
- durable observability and replay reconstruction.

This crate is GHC-heavy and skipped by the quick nextest filter. Run a targeted
test with:

```bash
scripts/battery.sh -p tidepool-harness -E 'test(<name>)'
```

Use the binary groups in `scripts/battery-shard.sh` for broader coverage.
