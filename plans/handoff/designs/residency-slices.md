# Bounded residency: implementation slices

`lifetime-contract-v2.md` is the governing contract. This note fixes the order
its first slice lands in and the interfaces each step commits to, so a later
step never has to undo an earlier one.

## Slice 1a: stable ids and per-program root blocks (decisions 4, 5)

Landed. Every `CompiledProgram` owns a fixed-address `RootWords` root block
(top words then import words); generated code embeds its address
(`emit::root_slot_value`, `adapter::emit_adapter`). The shared top table,
`TopSlotBase`, the capacity option and the compile/install reservation are
gone. `ProgramId`s are minted monotonically per machine and never reused;
`PreparedMachine::empty` creates a programless machine.

What this buys the later slices: a program's roots are exactly its block's
words, so retiring a program deregisters one block; and an id can be reported
in a receipt without ever aliasing a later install.

## Slice 1b: liveness, retirement and the receipt (decisions 6, 3, 2 without compaction, 7, 8)

Landed on the machine side. Interfaces as built:

- `PreparedMachine::quiesce(&self) -> Result<Quiescent, ExecutionError>`
  mints the token only when the disposition is `Reusable`, `call_depth == 0`,
  `rust_roots_len() == 0`, no observation borrows old space and GC state is
  installed; otherwise `NotQuiescent` (the runtime classifies it as a
  rejection). `collect_major(Quiescent)` consumes the token, re-checks, runs
  ordinary collection, marks, retires. The allocation trigger cannot mint one.
- `InstalledProgram` records what install minted for the program: its static
  region, the descriptor headers it owns (`owned_headers`), its callable
  headers, and whether it has pinned byte storage. Ownership is per program;
  a header owned by two programs is not shared (each program owns its own
  enterable descriptors), so no header-to-owners map was needed. Interned
  constructors have no owner and are never retired.
- Mark: a worklist over tagged words seeded from every value handle, every
  parked frame's cell (a prepared frame's evidence owner and runner are live
  by construction), every pin, and the root blocks of programs found live.
  `ObservationHeap::trace_step` classifies a word as `Traced::Static
  { region }` (marks the region's program, stops) or `Traced::Object
  { header, children }` (marks the header's owner, continues). The mark is
  non-moving and forces nothing. Programs with byte storage are pinned and
  reported (edge (c) deferred). There is no census: a descriptor row is
  retired iff its owner retires, which is sound because owned descriptors are
  only ever reached through their owner's objects, and a reachable owned
  object keeps its owner live.
- Retirement follows decision 7's order: block roots and remembered ranges
  (block and static region), call/enter rows, owned descriptor rows and
  descriptor-space admission (`DescriptorSpace::retire_owner`), the stack-map
  registry by identity, the static region and literal pool; the block, the
  receipt entry and the code drop with the program.
- `RetirementReceipt { programs, block_words, pinned_by_bytes, old_bytes }`
  and `ResidencyCounts` (programs, block words, persistent roots, handles,
  parked, stack-map links, static regions, descriptor/callable/enter rows).
  `pin`/`unpin` hold a program across the install-to-bind gap.

Acceptance met: `repeated_installs_retire_and_keep_residency_flat` (2k in the
suite, `TIDEPOOL_RESIDENCY_ITERATIONS=10000` for the contract's loop) holds
every `ResidencyCounts` field flat on every iteration; old bytes are reported
until slice 1c.

Still open from this slice, for the runtime (S4): drain the receipt
synchronously after each major collection: remove `ProgramFacts`, release
that program's leases, and re-home or drop its site witnesses; call
`quiesce`/`collect_major` at the session's between-turn point; surface the
counts in the session receipt.

## Slice 1c: descriptor-arena compaction (decision 2 remainder)

Source facts (2026-09-16): every retention promotion (`OldSpace::
promote_prepared`, `old_space/prepared.rs`) mints one `DescriptorArena`
sized to the nursery's used bytes and copies the selected graph into it
through `promote_and_fixup_with_external`; arenas are never freed, so
`RetirementReceipt::old_bytes` (`OldSpace::prepared_bytes_used`) grows with
every bound value and the loop test only asserts it monotonic. The Core
compactor (`stage_compaction`/`commit_compaction`) reads the Core object
format and is not applicable to descriptor objects.

Design: compaction is the existing Cheney copy with its *source* generalized.
`raw::copy_prevalidated_descriptor_graph_with_external` today decides
membership by one contiguous range (`from_start`, `from_used`); the
compactor needs "any object admitted by one of the retiring arenas". Give the
raw copier a source admission (`&dyn DescriptorOldSpace`, the trait the
`previous` parameter already uses) beside the range form, so promotion and
compaction share one copy loop and one forwarding protocol (`Forwarded`
headers written into the source objects, exact-start admission on the
destination).

`OldSpace::compact_prepared(Quiescent, machine, vmctx, descriptors,
external)` then runs after ordinary collection under the token:

1. Roots are the complete root snapshot (`complete_root_snapshot`: run,
   persistent, stowed, remembered, tail slots) plus every root-block word;
   the nursery is the additional source of edges into old space, walked by
   exact-start scan from `active_start` to `alloc_ptr`.
2. Size the live set (a mark over the retiring arenas' objects only; nursery
   and static targets stop the walk), reserve one destination arena of that
   size with every descriptor the machine holds, copy, seal.
3. Fix up: roots, nursery object fields whose target forwarded, copied
   objects' fields, and boxed-array payload slots through the external
   owner; re-register remembered slots at their copied addresses
   (`clear_remembered_slots` then `register_remembered_slot` for each
   copied old→nursery edge, as `commit_compaction` does for Core).
4. Retire every old arena range from the machine (`retire_old_space_arena`),
   register the new one, replace `prepared_arenas` with the single arena.
   Nothing after the copy may fail; every fallible step precedes it, as in
   promotion.

Acceptance: the residency loop's `old_bytes` is flat after warm-up (it joins
the `ResidencyCounts` equality), a value graph shared between two handles
compacts to one copy, and a parked frame's continuation survives compaction
and resumes.

## Deferred beyond the first slice

Edge (c) retirement of byte-storage programs, the external-payload sweep
from old space, `PreparedRuntime` lease re-keying and the shadowed-binding
policy (decision 9), and the reservation-token fallback (moot after 1a).
