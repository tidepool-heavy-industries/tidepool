# The observation budget rejects committed work

Two cells in the Astra run of 2026-09-17 failed with
`prepared execution failed: observation budget 100000 exhausted`. Both were
binds. The exact cells and the exact tool output the model saw are preserved
beside this file:

| Call | Cell | What failed |
| --- | --- | --- |
| `call_vTAtgwrR7TAGootopFBJMJ9F` | `semanticAudits <- traverse auditSemanticModule semanticModulePaths` | unit 3 of 4, after **45 committed operations** (10 Jev calls, 35 command jobs) |
| `call_g86sEV140DlL2hxVW1jqhrvf` | `editorialContext <- reflect 3` | unit 4 of 7, after **one** operation |

## Where the limit applies

Not where it looks. The three candidate boundaries, in order:

**Computing — succeeds.** Every effect ran and committed. The first cell's
receipt lists 45 operations, all `Committed`. That work is done, paid for, and
unrepeatable without re-running it.

**Retaining the binding — succeeds, and never consults the budget.**
`PreparedMachine::run_entry_retained`
(`tidepool/codegen/src/prepared_program/machine.rs:2442`) promotes the result
into old space and issues a `PreparedHandle`. It takes `PreparedCallOptions`
(which carries `observation_budget`) and never reads it. A binding is stored as
`BoundValue::Prepared { handle }` — a root, not a materialized value. **Retention
is materialization-free by construction.**

**Rendering — fails here.** `tidepool/runtime/src/session/resident.rs:851`:

```rust
match plan {
    SettlePlan::Observe | SettlePlan::Bind(ValueTier::Tier0Data) => {
        let value = observe(engine, handle)?;
        Ok(PreparedRun::Done { handle, value })
    }
    SettlePlan::Bind(ValueTier::Tier1Closure) => Ok(PreparedRun::Done {
        handle,
        value: HaskellValue::Con(CLOSURE_SENTINEL, Vec::new()),
    }),
```

`observe` forces and fully materializes the value into a host `Value`
(`prepared_program/forcing.rs::observe_results`), bounded by
`RunOptions::default().observation_budget` = 100_000. The closure tier directly
above already proves a bind can complete without materializing anything.

## Why this is a defect and not a tight budget

For a bind, the materialized value **is not used**:

- The binding comes from the handle:
  `bind_prepared(program, scope, generation, &[(binder, handle)])`
  (`resident.rs:2237`).
- The receipt comes from the binder names:
  `WorkbenchDisplay::Binding(names) => format!("[bound {}]", names.join(", "))`
  (`exomonad/actor/src/resident_workbench.rs:2803`). The failing receipts show
  exactly this — `[bound semanticModulePaths]`, no value.
- For a *projected* pattern bind the observation's result is discarded outright;
  `resident.rs:869` calls `observe` only to check for an error.

So the observation on a bind is a **forcing step** whose materialized product is
thrown away — and whose budget failure is propagated with `?`, which releases
the already-good handle and rejects the whole unit.

The error is also raised *underneath* the layer designed to handle large values.
`WorkbenchDisplay::Observation { budget, .. }` renders through
`render_cell_observation` with a display budget (8192 characters in
`resident_actor.rs:4800`) and offers `cellDisplay.more` for the rest. The value
is materialized in full only to be truncated to 8 KiB — and when full
materialization is impossible, the graceful layer never runs.

## How small the budget really is

`ObservationBudget::charge_bytes` (`observe.rs:79`): *"Materialization costs one
unit per value node and per copied payload byte."* 100_000 units is therefore
roughly **100 KB of reachable payload**, not 100k values. A cell that reads a
handful of source files and binds the result exceeds it. `reflect 3` over a
Shoal session — three turns carrying cell sources and outputs — exceeds it on
its own, which makes Reflect unusable at its documented default.

## Required behaviour

A large result stays usable by Haskell and recoverable through bounded
inspection. A display limit must not force replay of committed effects.

## The fix, reusing what exists

On `SettlePlan::Bind` and `SettlePlan::Project`, an exhausted observation budget
is not an error: the handle is retained, the binding is sound, and the
materialized value was going to be discarded. Keep the handle and complete the
unit — the same shape the `Tier1Closure` arm already uses. Bounded inspection of
the retained binding is the existing `cellDisplay` / `more` path, which needs no
new mechanism.

`SettlePlan::Observe` — a bare expression whose value really is displayed — is a
separate question and is not addressed by this change.
