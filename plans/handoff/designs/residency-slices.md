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

Landed (`ffa904bef`). Every retention promotion mints one `DescriptorArena`;
`collect_major` now compacts all of them into one arena holding exactly the
live descriptor objects, so `RetirementReceipt::old_bytes` is flat across
install/bind/release/retire cycles and `compacted_bytes` reports what was
reclaimed.

- One copy loop. The raw Cheney copier takes its source membership from a
  `DescriptorSourceSpace` (`descriptor_region.rs`): the nursery range for
  promotion, the set of old arenas for compaction. It is a separate trait
  from `DescriptorOldSpace` because a source space must still recognise an
  object the copy has already forwarded, which target admission rejects.
- Roots: the complete root snapshot minus remembered slots inside the old
  arenas, plus every program root-block word, plus every nursery field and
  boxed-array payload slot that points into old space. Remembered slots
  outside the arenas stay roots, as in promotion: a retained boxed array
  registers all its slots and payloads are not yet swept from old space.
- The destination is sized to the old arenas' total bytes; only live objects
  are copied, so `old_bytes` is exact and the slack is freed at the next
  compaction. With nothing live, no arena is kept. Every fallible step
  precedes the copy.
- Order: compaction runs before `retire`, because the nursery walk resolves
  headers in the installed descriptor space and a retiring program's nursery
  objects still carry its headers. A retiring program's old-space objects are
  therefore reclaimed one collection later. The follow-up that detaches a
  retiring program's roots and runs a minor collection before removing its
  descriptors (decision 7) may move this.
- Install no longer re-appends shared constructor descriptors to the
  machine's descriptor list, which had grown by one copy per install and was
  walked by every promotion and compaction.

Evidence: the residency loop at 10,000 iterations holds every
`ResidencyCounts` field and `old_bytes` flat; a graph shared by two handles
compacts to one copy; a parked heap closure survives compaction and resumes;
boxed-array edges in both directions (old to nursery, nursery to old) are
followed.

## Deferred beyond the first slice

Edge (c) retirement of byte-storage programs, the external-payload sweep
from old space, `PreparedRuntime` lease re-keying and the shadowed-binding
policy (decision 9), and the reservation-token fallback (moot after 1a).
