# JIT memory lifetime follow-up

The resident JIT uses Cranelift's system memory provider and enforces long-range
function/data references at `CodegenPipeline::define_function`. Incremental
compilation can grow beyond a fixed arena while existing addresses remain valid.
Tests exercise 288 MiB of definitions and references spanning more than 2 GiB.

This removes the fixed allocation ceiling, not lifetime memory growth.
Production still retains compiled functions and literal data within each live
machine. The owning module releases its allocations on destruction. Binding-root expiry
does not reclaim old-space storage or executable code.

## Owning work

1. Design reclamation within a live machine around the actual reference graph:
   closures, thunks, literal data, compiled references, stack-map/debug metadata,
   and parked continuations. Retiring a lexical scope alone does not prove its
   code is unreachable. Preserve cross-scope captures and immutable fork tips.
2. Measure repeated compilation through the existing fragment/function/block
   counters. Reuse must respect resolved binding identities and compiler settings;
   identical source text alone is not a sound key. Extend compilation ownership
   rather than introducing a frontend cache.

`tidepool-codegen` owns executable allocation and JIT reference lifetimes.
`PersistentSession` and the session registry own machine lifetime and recovery.
Use deterministic ownership and address-validity checks; process RSS alone is
not proof of reclamation. Keep tests for old code remaining callable after growth
and for independent parked continuations surviving sibling work.

## Running-session boundary

Building a corrected binary does not update an already running host. The
shoal-repl report records typed requests that remained pending after compilation
failed; their recovery has not been established. Do not replay assignments or
effects to manufacture settlement, or claim `:recovery` restored live values.
Recovery must report which exact requests, bindings, and handles survive or are
lost through the existing owners. A host restart is a separate operational action.
