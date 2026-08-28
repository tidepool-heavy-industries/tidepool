# Continuation parking and resume

This is the public contract for consumers of the parked-continuation registry
in `JitEffectMachine`. Private frame layout and registry implementation may
change without notice.

## Public model

The machine mints `ContinuationId` and `ValueHandle` values; callers assign
`RealmId` values to identify their runtime resource scopes. Continuation
identifiers are never reused: resuspension creates a fresh identifier in the
same scope. Unknown or spent continuation and value-handle identifiers are
rejected rather than aliased to live resources.

`ParkKind` selects one of the four completion policies:

- `Plain`: return the bridged value.
- `Binding { forced }`: return the value and its persistent root.
- `Project { n_fields }`: return persistent roots for the projected fields.
- `Render { field0_forced }`: return field zero's root and field one's bridged
  rendering.

`ParkedOutcome` reports the corresponding completion or a suspension carrying
its `ContinuationId`, request value, and finalized-closure flag.

## Guarantees

- A parked continuation is a registered GC root from parking until resume or
  scope closure. At every quiescent point,
  `stowed_roots_count() == parked_count()`.
- `resume_continuation` takes an identifier and `ResumeInput`. The frame supplies its
  own constructor table, suspension tag, materialization policy, cancellation
  flag, and handled prefix, so a caller cannot resume it against a foreign
  effect row.
- Parked continuations may be resumed in any order.
- `parked_ids()` is sorted for deterministic enumeration.
- An unknown or spent continuation identifier is a typed error.
- A bridged answer containing bottom is rejected before the frame is consumed,
  leaving it parked and available for retry.
- `ValueHandle` refers to a machine-side persistent root. Observation and
  delivery borrow the handle; they do not consume it. `ResumeInput::Handle`
  delivers the heap value directly, allowing closures to cross between
  continuations on the same machine.
- `close_realm(realm)` removes that scope's parked frames, value handles, and
  cancellation entry without affecting siblings. It returns
  `(frames_dropped, handles_released)` and is idempotent.

## Consumer obligations

### Handled-prefix compatibility

All non-empty handled prefixes used on one machine must be exactly equal in
length, names, and positions. An empty prefix is compatible with any prefix.
The machine checks this before executing the incoming fragment and returns
`JitError::IncompatibleHandledPrefix` without mutating the machine.

The check compares callers with each other; the concrete handler stack `H` is
not runtime data and cannot be inspected. Callers construct one
`EffectBoundary` from the effect-list value used to construct `H` and pass
that boundary in `SuspensionRun`. Capacity limits, such as a session that
permits one outstanding turn, belong above the JIT and retain the returned
`ContinuationId` explicitly.

### Bounded lifetime

Runtime resource scopes are closed when their work retires. Long-lived shared
machines are bounded by fragment-count rotation at a quiescent loop boundary.
Rotation preserves checkpointed durable state and reports machine-local state
that cannot be reconstructed.

Cranelift executable arenas are not reclaimed by ordinary machine drop, so a
machine must not grow without a rotation bound.

## Main entry points

All methods are on `JitEffectMachine`:

```rust
run_until_suspension(run: SuspensionRun, handlers, user)
resume_continuation(id, handlers, user, input)

parked_count()
parked_ids()
parked_realm(id)
stowed_roots_count()
realm_cancel_handle(realm)

handle_from_finalized(id)
observe_handle(handle)
close_realm(realm)
```

Cancellation is scoped to a runtime resource scope rather than a machine or a
single continuation identifier. Reset a cancellation handle before retrying
work in that scope.

The definitive signatures and outcome fields live beside their definitions in
`tidepool-codegen/src/jit_machine.rs`. Contract coverage lives in the parked
continuation, handled-prefix, and realm-handle tests under
`tidepool-codegen/tests/`.
