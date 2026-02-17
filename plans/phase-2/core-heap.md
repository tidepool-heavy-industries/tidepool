# Phase 2: core-heap

**Owner:** Claude TL (depth 1, worktree off main)
**Branch:** `main.core-heap`
**Depends on:** core-eval scaffold (HeapObject layout, Heap trait, ThunkId)
**Produces:** Arena allocator with copying GC. Implements Heap trait.

---

## Wave 1: Scaffold + Arena (1-2 workers, gate)

### scaffold-arena

**Task:** Implement ArenaHeap: bumpalo-backed nursery with HeapObject memory layout, Heap trait impl. HeapObject is a manual memory layout (raw byte buffers + unsafe accessors), NOT a Rust enum — see decisions.md.

**Read First:**
- `core-eval/src/heap.rs` (Heap trait, HeapObject, ThunkState from core-eval scaffold)
- `tidepool-plans/decisions.md` (D5 — variable-size, D6 — code pointer, D7 — indirection)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-heap/src/arena.rs` — `ArenaHeap` struct wrapping `bumpalo::Bump`
2. ThunkId addressing: index into `Vec<*mut HeapObject>`
3. Implement `Heap` trait: `alloc` (bump pointer with variable stride), `force`, `read`, `write`
4. Nursery size constant (default 4MB, configurable)
5. HeapObject header: tag byte at offset 0, size u16 at offset 1, variant-specific fields follow. All objects aligned to 8 bytes. See decisions.md for exact layout. Define accessor functions (read_tag, read_size, read_closure_code_ptr, etc.) that all crates share.
6. Nursery exhaustion → signal GC (not panic)
7. ThunkId stability: tag change must not invalidate addressing

**Verify:** `cargo test -p core-heap`

**Done:** Alloc/read/write work. State transitions correct. Nursery exhaustion signals GC.

**Tests:**
- Alloc N objects of varying sizes — all retrievable
- State transition: Unevaluated → BlackHole → Evaluated
- ThunkId addressing correct after transition
- Alloc after nursery full → GC trigger signal
- Zero-payload object (Lit) allocates correctly
- Large closure (10+ captured vars) allocates correctly

**Benchmark:** Alloc throughput vs Box-per-node.

**Boundary:**
- Variable-size objects: ALWAYS read size from header. Never assume fixed layout.
- No `HashMap` for address lookups. Vec indexed by ThunkId.
- Do NOT use `unsafe` without documenting the safety invariant.

**Gate:** TL reviews arena before spawning GC workers.

---

## Wave 2: GC (2 workers, parallel)

### gc-trace

**Task:** Root enumeration and transitive marking. Pure function: no arena mutation.

**Read First:**
- `core-heap/src/arena.rs` (ArenaHeap, HeapObject layout)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-heap/src/gc/trace.rs`
2. Signature: `fn trace(roots: &[ThunkId], arena: &Arena) -> ForwardingTable`
3. ForwardingTable: maps old_addr → new_addr for every reachable object
4. Root set (exhaustive): Env stack ThunkIds, eval continuation stack, pending thunk update stack
5. Transitive trace: follow Closure captured ptrs, Con fields, Evaluated indirection, Thunk env refs
6. Compute new_addr by bump-allocating into a size counter (don't actually copy)

**Verify:** `cargo test -p core-heap -- trace`

**Done:** ForwardingTable covers exactly the transitive closure of roots.

**Tests:**
- All reachable objects in table, unreachable NOT in table
- Double-referenced object appears exactly once
- ForwardingTable covers transitive closure
- Empty root set → empty table

**Boundary:**
- NO MUTATION of the arena. This function is pure.
- BlackHole is GC-visible. Do not skip it during trace.

---

### gc-compact

**Task:** Copy live objects to new arena, rewrite all internal pointers.

**Read First:**
- `core-heap/src/gc/trace.rs` (ForwardingTable)
- `core-heap/src/arena.rs` (HeapObject layout)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-heap/src/gc/compact.rs`
2. Signature: `fn compact(table: &ForwardingTable, old_arena: Arena) -> Arena`
3. Allocate fresh arena sized to total live bytes
4. Copy each object from old_addr to new_addr (read size from header — variable-size)
5. Rewrite internal pointers:
   - Evaluated(ptr): look up in table, write new addr
   - Closure captured[i]: look up each, write new addr
   - Con fields[i]: look up each, write new addr
   - Closure code_ptr: DO NOT rewrite (code space, not heap)
   - BlackHole: copy as-is
6. Update ThunkId index table to new arena locations
7. Drop old arena

**Verify:** `cargo test -p core-heap -- compact`

**Done:** All correctness invariants pass. No dangling pointers after compact.

**Tests:**
- After compact, zero pointers into old arena
- Object at exact address from ForwardingTable
- Indirection chains shortened
- BlackHole preserved verbatim
- Variable-size objects: size from header, not assumed
- Closure captured ptrs all rewritten
- Closure code_ptr NOT rewritten
- Eval identical with/without GC (proptest)
- Large heap (100K+ objects) completes correctly

**Benchmark:** GC pause time, overhead as % of eval.

**Boundary:**
- Do NOT rewrite code_ptr. It points to code space, not heap.
- Read size from header for EVERY object. Never assume fixed size.

---

**After wave 2:** TL wires trace + compact into unified GC. Trigger: alloc_ptr exceeds nursery limit. After GC, if live data > 75% of nursery, double nursery size. `cargo test -p core-heap`. Commit. File PR.

---

## Domain Anti-Patterns

In addition to the base anti-patterns file:

- Do NOT rewrite `code_ptr` during GC compact — code space, not heap.
- Variable-size: always read size from header. Never assume fixed layout.
- No `HashMap` for ForwardingTable. Vec indexed by arena offset.
- BlackHole is a GC-visible state. Trace it, copy it, preserve it.
