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

Interfaces:

- `PreparedMachine::quiesce(&mut self) -> Result<Quiescent<'_>, ExecutionError>`
  returns a token only when `call_depth == 0`, `rust_roots_len() == 0`,
  `prepared_old_space().is_none()`, GC state is installed, and the disposition
  is `Reusable`. `collect_major`, `retire` and install-time `collect_on` take
  the token. The allocation trigger cannot construct one.
- `InstalledProgram` keeps, beside its code custody, the program's static
  region, the descriptor headers it owns (thunk, function and PAP layouts:
  its `enter_owned_headers` plus its PAP layouts) and its byte ranges.
- Owner sets: `owners: BTreeMap<usize /* header */, BTreeSet<ProgramId>>`
  for enterable headers; the interner's constructor descriptors stay
  ownerless and are kept alive by the census.
- Mark: a worklist over tagged words seeded from every persistent root
  (handles, bindings), every stowed root (parked frames) and every pin,
  classified through the same three-way test `ObservationHeap::object` uses
  (static region, old-space arena, nursery exact start) and traced through
  `ObjectDescriptor::for_each_trace_slot`. A static hit marks the region's
  program and stops. A traced object's header marks its owners and joins the
  census. `Updated` thunks trace only their target. Programs with byte
  storage stay pinned and are reported, not retired (edge (c) deferred).
- Retirement, per unmarked program, in the contract's order: deregister its
  block roots and purge remembered slots inside its block, static region and
  byte ranges; remove its owner from call and enter rows (a row whose owner
  set empties and whose header the census did not see is deleted, and a
  surviving owner's code pointer replaces a retired owner's); remove its
  descriptors from the machine-wide sets unless the census saw them; unlink
  its stack-map registry by identity; drop its static region; free the
  block; push the receipt; drop the `CompiledProgram`.
- `RetirementReceipt { programs, handles, block_words, old_bytes,
  external_bytes, code_owners_freed }` is returned from `collect_major`, and
  the runtime drains it synchronously: remove `ProgramFacts`, release that
  program's leases, drop its site witnesses (a canonical witness whose owner
  retires moves to a surviving equivalent owner or is deleted).

Acceptance for 1b: the contract's 10k loop with every counter flat except
old and external bytes, which are reported.

## Slice 1c: descriptor-arena compaction (decision 2 remainder)

`OldSpace::stage_compaction`/`commit_compaction` learn the descriptor object
format (header word to descriptor, `for_each_trace_slot` for edges,
`allocation_extent` for size) so prepared arenas compact under the same token
after the mark. Old bytes then join the flat counters.

## Deferred beyond the first slice

Edge (c) retirement of byte-storage programs, the external-payload sweep
from old space, `PreparedRuntime` lease re-keying and the shadowed-binding
policy (decision 9), and the reservation-token fallback (moot after 1a).
