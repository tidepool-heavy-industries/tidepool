# Wave 6A handoff

This is a temporary handoff for the next STG session-integration pass. Verify
the source below rather than treating this file as architecture.

## Checkpoint

Head after this handoff: `80470f4a0` on `engine/stg-production-cutover`.

The prepared engine now has a persistent, same-program Rust value boundary:

- `abf777364` adds a real `freer-simple` `send` probe and generated artifact.
- `f4fbe14b6` adds descriptor-only outer-constructor inspection to
  `PreparedMachine`. A managed child is retained before the nursery borrow
  ends; constructors identify through descriptor metadata; callable and live
  thunk children stay opaque; inspection refuses terminal machines.
- `3ffa5912e` makes that boundary available from
  `tidepool-runtime/src/session/prepared.rs` without exposing codegen roots.
  `PreparedValue` is opaque and linear; managed arguments and inspection borrow
  it, while `release` consumes it.
- `80470f4a0` records the intentional fixture regeneration after the producer
  changes.

The real Freer test executes an extracted `E` request twice, collects between
entries, then inspects the first request and retains its continuation as data.
It does not force or resume that continuation.

## Evidence

Focused checks reported from the committed slices:

- `cargo test -p tidepool-codegen prepared_program::machine::tests --lib -- --test-threads=1`: 11 passed.
- `cargo test -p tidepool-runtime --test prepared_execution -- --test-threads=1`: 4 passed.
- `cargo test -p tidepool-runtime session::prepared::tests --lib -- --test-threads=1`: 6 passed.

The canonical fixture flow was run after `80470f4a0`'s generated fingerprint:

```text
env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER just fixtures-update
env -u TIDEPOOL_EXTRACT -u TIDEPOOL_EXTRACT_WORKER just fixtures-check
```

The long corpus child completed and wrote
`target/prepared-corpus/suite.6NWUxF/results.json` with 812 projected,
validated, admitted, and compiled; 628 executed; 216 comparison matches; and
zero comparison mismatches. The parent command's exit code was unavailable
after the tooling yielded while its child continued, so do not call the
canonical gate green without re-running it if that distinction matters.

The 184 execution non-successes are existing harness limits, predominantly
managed host arguments and non-materializable addresses; they are not test
mismatches. The result file has the per-top reasons.

## Next semantic owner: executable imports

Do not start resident/workbench cutover or continuation resumption first.
Prepared schema linking already validates `GlobalId`, representation, entry
signature, evaluatedness, and required generation. But prepared admission and
emission reject every global, so `MachineImports` currently validates metadata
without providing an executable root.

The first next scaffold should be one private import-resolution contract between
prepared runtime/codegen, keyed by `GlobalId` and backed by a *leased stable*
`BindingTable` root slot plus the exact `ImportedValue` generation. Reuse the
existing `PersistentSession`/`BindingTable` owner; do not add a pointer cache or
registry. Keep global admission rejected until generated code can load that
slot. The first acceptance is one extracted artifact with an evaluated lifted
global that executes through the generated load across GC; stale generation and
released lease must fail before native entry.

Only after that boundary exists should a prepared Freer suspension be made
parkable. Its token must remain runtime-owned data holding request and
continuation `PreparedValue`s plus realm, principal, effect policy, and cancel
authority. It cannot reuse Core's `ContinuationFrame` or authorize by ambient
session state.

## Ownership map consulted

- `tidepool-runtime/src/session/prepared.rs`: prepared artifact, lazy compiled
  owner, cancellation, error classification, retained values.
- `tidepool-runtime/src/session/persistent.rs`: source/value generation and
  `BindingTable` custody.
- `tidepool-runtime/src/session/resident.rs`: production binding leases across
  suspension; leave untouched until imports work.
- `tidepool-codegen/src/prepared_program/{admission,emit}.rs`: globals remain
  deliberately rejected today.
- `tidepool-repr/src/execution_schema/link.rs`: exact import/generation
  metadata validation already exists.

## Dirty-tree boundary

Do not absorb these into the next commit without reviewing their owner:

- `tidepool-codegen/src/prepared_program/admission.rs`
- `tidepool-codegen/src/prepared_program/floating.rs`

They are pre-existing unrelated rustfmt-only diffs. Also preserve the untracked
`examples/guess/` and `haskell/dist-newstyle-wave4-haskell/` directories.
