#### Lifetime contract (amended after source review; replaces the proposed contract and its review list)

Source facts (verified on `engine/stg-production-cutover` at ddb9d2746):

- **Slots.** `PreparedMachine::install` bump-claims `[claimed_slots, +n)` of one
  machine-wide `RootWords` sized once (`SESSION_TOP_SLOTS = 4096`,
  `tidepool-runtime/src/session/prepared.rs`) and never returns it
  (`prepared_program/machine.rs` `install`). `plan.rs` assigns absolute slots
  `base + index`. `emit.rs` `atom_value` loads `vmctx.prepared_tops`, then
  loads `tops + slot*8`, where the slot is an immediate. Every heap-top and
  import slot is pushed onto `MachineState.persistent_roots`, and nothing
  removes it except rollback. `compile_for_install` reads
  `next_top_slot_base()`, and install then rejects a stale base
  (`TopSlotBaseMismatch`). Two compiles before one install lose a compile. The
  mismatch is typed, not unsound.
- **Ids.** `ProgramId((programs.len()-1) as u32)`, and lookup is
  `programs.get(id.0)`. That is a vector index. `PreparedRuntime` treats the
  first program specially (`machine: Option<(PreparedMachine, ProgramId)>`).
  The first install also creates the nursery with that program's descriptors
  and `set_stack_map_registry`.
- **Merged rows, mixed writers.** `MachineState.prepared_callables` and
  `prepared_enters` (`machine_state.rs`) are `HashMap`s keyed by header. Callables
  and enters use `extend`/`insert`, so the last writer wins. `enter_owned_headers`
  includes the "evaluated-chain" descriptors (`prepared_program.rs`), so
  interned constructor headers are shared between programs.
  `PreparedMachine.descriptor_registry` uses `BTreeMap::extend`, so the last
  writer wins. `DescriptorSpace::extend_descriptors` (`tidepool-heap/src/gc/raw.rs`)
  and `DescriptorInterner::absorb` (`interner.rs`) keep the first writer. None of
  these rows records an owner. `descriptors: Vec<Arc<ObjectDescriptor>>` and
  `statics: Vec<Arc<StaticRegion>>` only grow. `clear_prepared_entries` runs only
  in `Drop`.
- **Constructor descriptors are ownerless.** The interner mints one
  `Arc<ObjectDescriptor>` per `SymbolIdentity` for the machine's life. A
  constructor cell's header names no program.
- **The collector sees only the nursery.** `evacuate_descriptor` (`raw.rs`)
  returns static references (`static_reference`) and admitted old-space
  references (`DescriptorOldSpace::admit`) unchanged and never traces through
  them. The prepared path never calls `OldSpace::stage_compaction` or
  `commit_compaction`. Only the Core `jit_machine.rs` does. No prepared mark phase
  exists. Old-space arenas are freed only in `Drop for PreparedMachine`.
- **Statics are closed.** Relocations in `image.rs` `add_relocation` are
  `Local`-only. `Global` refs are `Unsupported::Global`, and heap tops are
  `StaticHeapEdge`. `static_region.rs` documents the regions as immutable, so a
  static object reaches only statics of its own program and that program's byte
  storage.
- **Raw address edges.** A static `Address` field holds
  `plan.bytes[..].as_ptr()` (`image.rs` `initialize_atom`), and
  `run.rs` `write_atoms` does the same through `byte_tops`. These point into
  the program's `PinnedBytes`, which `CompiledProgram` owns. Descriptors
  classify them as scalars, so no header or trace edge exists.
- **Code-embedded addresses.** Descriptor header words and
  `dispatchers.demand_address` (`apply.rs`, pinned by
  `CompiledProgram._dispatchers`) are `iconst`s in generated code.
- **Roots.** `complete_root_snapshot` joins stack, rust, persistent, stowed,
  code, and remembered roots plus the tail slots. `remembered_slots` is purged
  only by `forget_remembered_range`/`retire_old_space_arena`. The stack-map
  chain is a LIFO `Vec<*const StackMapRegistry>` (`push`/`pop` only).
- **Quiescence is asserted, not enforced.** Install runs `collect_on`, citing
  "no generated frames are live between installs". Nothing checks
  `call_depth`, `rust_roots_len`, `prepared_old_space`, `gc_state` custody, or
  disposition.
- **Leases and bindings.** `realm_leases` is keyed by realm, not program.
  `release_binding` refuses while leased. `RealmId::ROOT` bindings from
  `retain_top` are never closed. `resolve_import` picks the newest generation,
  but older generations of a rebound name stay live roots. No parked
  continuation exists yet (`machine.rs` `handles` doc). The typed-resume contract
  adds parked sites.

Decisions:

1. **Complete edge set.** A program `P` is live iff the major mark reaches it
   through one of these edges:
   - (a) a live object whose header is a descriptor that `P` owns (thunk,
     function, or PAP layout);
   - (b) a live static object inside `P`'s static region;
   - (c) a live object or root word holding an address inside `P`'s byte
     storage. This is found by the layout's `Address` field reps and a
     program byte-range table;
   - (d) a parked continuation whose site record names `P`;
   - (e) an explicit pin.

   A live `P` makes its root block's words roots (edge g), and they reach its
   imports' values and, through (a), their owners. Constructor headers are not
   program edges. They feed the census (decision 3). Import graphs are acyclic,
   but value graphs are not, so liveness is a fixpoint worklist, never
   reference counting.
2. **Prepared major mark over heap, statics, and old space.** At quiescence
   (decision 6), a major collection first runs an ordinary prepared minor
   collection. It then marks from handles, bindings, parked and stowed roots,
   and pins, with no stack roots, over nursery, old space, and static objects.
   It uses `ObjectDescriptor::for_each_trace_slot` for managed fields and the
   same layout's `Address` reps for (c). It classifies a word by
   `static_reference` (records `P` and continues), `DescriptorOldSpace::admit`
   (traces), or the nursery exact-start map. The mark is a new walk in
   `tidepool-heap/src/gc/raw.rs` beside `cheney_copy`, not a second collector.
   Old-space compaction reuses `OldSpace::stage_compaction`/`commit_compaction`
   extended to descriptor-format arenas, which is one mechanism for both
   engines. External payloads reached from marked old objects feed the existing
   external sweep.
3. **Multi-owner index rows and a live-header census.** Every machine-wide row
   becomes `header -> (value, owners: SmallSet<ProgramId>)`. This covers
   `prepared_callables`, `prepared_enters`, `descriptor_registry`,
   `DescriptorSpace.descriptors`, and interner entries. Install adds its owner.
   A shared header's metadata already agrees, because the interner refuses a
   conflicting declaration. A shared enter row may keep any owner's code
   pointer, because every owner's `prepared_enter` handles that evaluated
   constructor. When the recorded owner retires, the row switches to a
   surviving owner's pointer.
   Retirement removes its owner, and a row is deleted only when its owner set
   is empty **and** the mark's census (the set of headers seen on marked
   objects) excludes it. An ownerless constructor descriptor therefore survives
   while any cell carries it, even after every declaring program retires.
   Deleting before the `Arc` drops prevents a reused allocation address from
   aliasing a stale row.
4. **Stable program ids.** `ProgramId` is issued by `MonotonicIdIssuer`
   (`tidepool-repr`, already used for `binding_ids`). `programs` becomes a
   `BTreeMap<ProgramId, InstalledProgram>`. No id is reused. The first program
   loses its special role: `PreparedMachine::empty` creates nursery, GC state,
   and `vmctx` with no program. Stack maps start as an empty chain. Runtime
   default-entry addressing names an id explicitly.
5. **Per-program fixed-address root blocks replace the shared slot table.**
   - *Change.* `compile_for_install` allocates the program's `RootBlock`, a
     boxed `RootWords` of `top_slots + import_slots` words owned by the
     `CompiledProgram`. Planning assigns block-local indices (`TopSlotBase` and
     `claimed_slots` disappear). `emit.rs` replaces
     `load(vmctx, VMCTX_PREPARED_TOPS_OFFSET)` + `load(tops, abs*8)` with
     `iconst(block_addr)` + `load(block, local*8)`. `run.rs`
     `initialize_heap_tops`/`write_atoms` and `invocation.rs` read the program's
     own block. `vmctx.prepared_tops` is then unused by prepared code and is
     removed.
   - *Soundness.* Generated code reads only its own program's slots (confirmed
     in review). A cross-program PAP runs the producer's code against the
     producer's block, and the producer is live by edge (a). The block address
     is as stable as the code that embeds it, and both are freed together at
     retirement.
   - *Cost.* A 10-byte `movabs` replaces one memory load, which is neutral to
     slightly cheaper. The compiled code is bound to one block, as it is already
     bound to interned descriptor addresses, so nothing is lost. The machine has
     no fixed slot capacity: `TopTableExhausted` becomes an allocation failure.
     The compile/install race disappears, because an abandoned compile drops an
     unregistered block and no shared base exists.
   - *Fallback if review rejects the embedded address.* This covers the case
     where a code artifact must be reusable across installs. The fallback keeps
     the shared table but issues a reservation token from `compile_for_install`
     that holds a range until install or drop. Freed ranges are coalesced
     first-fit, and a range is reused only after its program's code is freed.
     It keeps the vmctx load and still fragments, so it is not preferred.
6. **Enforced quiescence gate.** `PreparedMachine::quiesce(&mut self)` returns a
   `Quiescent<'_>` token only when:
   - `call_depth == 0`, `rust_roots_len() == 0`, and
     `prepared_old_space().is_none()`;
   - GC state is installed (not taken by a collection);
   - `disposition() == Reusable`.

   Major collection, retirement, and install-time `collect_on` require the
   token. The allocation trigger (`prepared_gc_trigger`) has no
   `&mut PreparedMachine` and can never construct one, so no major collection
   is allocation-triggered. Parked continuations are heap objects rooted in the
   registry, not native frames, so they do not block quiescence.
7. **Retirement order** (per unmarked program, after mark, under the token):
   1. deregister its block's persistent roots and purge remembered slots within
      its block, static region, and byte ranges (`forget_remembered_range`);
   2. remove its owner from call and enter rows;
   3. remove its owner from descriptor rows and interner entries, deleting rows
      per decision 3;
   4. unlink its stack-map registry by identity. The chain becomes
      owner-keyed, not LIFO;
   5. drop its static region from `statics` and `DescriptorSpace`;
   6. free its root block;
   7. push a receipt;
   8. drop the `CompiledProgram`: code, `PinnedBytes`, dispatchers, and
      descriptor `Arc`s.

   Old-space compaction commits before step 1, so no surviving slot names a
   retired address. A failure before step 1 aborts the retirement without side
   effects. After an integrity failure, teardown stays metadata-driven as
   today.
8. **Runtime receipt releases leases and program facts.** Major collection
   returns `RetirementReceipt { programs, handles, block_words, old_bytes_reclaimed,
   external_bytes_reclaimed, code_owners_freed }`. `PreparedRuntime` drains it
   synchronously after the call, not as a closure invoked inside collection.
   The drain removes `ProgramFacts` and releases that program's leases.
   `realm_leases` becomes `program_leases: BTreeMap<ProgramId, Vec<SessionVarId>>`
   plus `realm -> programs`, so realm close releases pins, not identity leases.
   Pins: install holds a pin until `bind_top` or explicit `unpin` (closing the
   install-to-bind gap). Parked sites pin through (d).
9. **Shadowed-binding retirement precedes any notebook reclamation.** A root
   binding pins its value, the value pins its program, and that program's block
   pins every import, so no notebook program is collectable while every
   generation stays bound. The runtime therefore retires a binding when its name
   is shadowed by a newer generation, no live program declares it at a pinned
   `required_generation`, and no lease remains. Retirement releases the handle;
   it does not reclaim memory. The major collection then decides. Until this
   policy lands, notebooks never call major collection.

Acceptance:

- escaped closures and cross-program PAPs run after their producer's binding,
  realm, and pin retire;
- a cycle of two programs whose values refer to each other is reclaimed once
  unrooted;
- a constructor cell outlives every declaring program and still observes and
  cases correctly;
- a program with byte storage is kept by an `Address` field alone;
- every retirement path runs only under `Quiescent`, and a forced call from the
  GC trigger or inside a run is refused with a typed error;
- repeated install/run/retire/major cycles far beyond 4096 cumulative slots
  keep flat counts for handles, block words, persistent roots, remembered
  slots, index rows, stack-map links, statics, installed code owners, and old
  and external bytes. Each count is reported separately.

First slice (proves bounded residency, at `PreparedMachine` level, no notebook):

- **Scope:**
  - decision 4 (monotonic ids, programless heap creation);
  - decision 5 (root blocks and the codegen change);
  - decision 6 (gate);
  - decision 3 (owner sets on the four rows, census for descriptor rows);
  - decision 2 (mark phase, plus descriptor-format old-space compaction);
  - decision 7 (retirement order);
  - decision 8 (receipt with pins only).
- **Test:** a synthetic loop of 10k iterations, each of which:
  1. installs program `k` importing `k-1`'s retained handle;
  2. runs it and retains a result that is a PAP over `k-1`'s function;
  3. releases `k-1`'s handle;
  4. quiesces, collects, and asserts the counters above are flat after warm-up;
  5. applies the surviving PAP at the end.
- **Deferred:**
  - edge (c): programs with non-empty byte storage stay permanently pinned and
    are reported, not retired;
  - parked sites (d), which wait on the typed-resume contract;
  - external-payload sweep from old space (bytes counted, not asserted flat);
  - `PreparedRuntime` lease re-keying and shadowed-binding policy (decision 9),
    so notebooks keep today's no-reclamation behavior;
  - the reservation-token fallback.
