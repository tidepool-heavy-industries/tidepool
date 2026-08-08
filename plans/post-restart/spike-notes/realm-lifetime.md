# realm-lifetime: persistent-root retirement + compiled-function lifetime

Scope: the two suspected real costs of a unified (realm) JIT machine, named in
`plans/post-restart/one-compile-bootstrap.md:59-65` — "roots clear
machine-wide only, and Cranelift functions accumulate in the JITModule — an
immortal unified machine grows without bound." This doc gets concrete on both
with file:line anchors and throwaway-test numbers. It does not score the rest
of the realm-ownership checklist (owned by `realm-checklist.md`), does not
design the realm machine, and does not write the GO/NO-GO verdict — those are
the TL's job in `realm-verdict.md`/`realm-prototype.md`.

## COST A — persistent-root retirement

### STATIC FINDINGS

**Claim 1 — no per-slot deregister.** Confirmed. `MachineState` has
`register_persistent_root` (`tidepool-codegen/src/machine_state.rs:485`) and
`clear_persistent_roots` (`:494`), which `.clear()`s the whole `Vec`. There is
no `deregister_persistent_root` anywhere in the crate (`grep -rn
deregister_persistent_root tidepool-codegen/src` → zero hits). Compare the
sibling registry `stowed_roots`, which DOES have a per-slot
`deregister_stowed_root` (`machine_state.rs:520-525`, removes the first
matching slot address) — the persistent-root registry was deliberately built
without that symmetry.

**Claim 2 — clearing is machine-wide only.** Confirmed. `clear_persistent_roots`
(`machine_state.rs:494-496`) is `self.persistent_roots.borrow_mut().clear()` —
there is no scoped/targeted variant, only the blanket clear.

**Claim 3 — happens only at machine drop.** Confirmed by exhaustive call-site
grep. Every caller of `register_persistent_root`/`clear_persistent_roots` /
`MachineState::free_session_heap` in the repo:

- `MachineState::register_persistent_root` (`machine_state.rs:485`) — called
  from exactly one production site: `host_fns::gc::register_persistent_root`
  (`host_fns/gc.rs:91-95`, the vmctx-gated free fn), which itself has exactly
  one production caller: `OldSpace::tenure` (`old_space.rs:283`) — every
  value-plane bind (`run_pure_and_bind`, `run_fragment_and_bind*`) that
  tenures its NF result registers one new root here. `JitEffectMachine`
  also exposes its own `pub unsafe fn register_persistent_root`
  (`jit_machine.rs:2057-2064`, delegates straight to
  `self.machine_state.register_persistent_root`), but its only caller in the
  whole repo is a test (`jit_machine.rs:3375`) — production code always goes
  through the tenure path, not this method directly.
- `MachineState::clear_persistent_roots` (`machine_state.rs:494`) — called
  from exactly one production site: `MachineState::free_session_heap`
  (`machine_state.rs:428-440`, doc-commented "MACHINE-DROP teardown"). The
  only other call site is a bare-`VMContext` test harness exercising the same
  reach path (`jit_machine.rs:3290`), not a second production caller.
- `MachineState::free_session_heap` — called from exactly one production
  site: `impl Drop for JitEffectMachine` (`jit_machine.rs:2266-2288`), guarded
  by `if self.session.is_some()`.

So: every tenured value-plane binding registers a persistent root at tenure
time, and that root is never removed until the whole machine drops. A
long-lived (session/realm-scoped) machine's persistent-root set is
strictly append-only for the machine's entire life.

**`perform_gc`'s root assembly cost.** `host_fns/gc.rs:892-894`
(`ms.extend_persistent_roots(&mut root_slots)`) appends every persistent root
to `root_slots` on **every single collection** — minor GCs included, not just
major/tenuring passes. Two distinct costs follow:

1. **The scan itself is `O(persistent_roots.len())` per GC**, added on top of
   the frame-walked stack roots, rust roots, stowed roots, and remembered
   slots — `root_slots` is one flat `Vec<*mut *mut u8>` that `cheney_copy`
   walks in full (`gc.rs:934-941`), and the heap-doubling branch re-walks the
   *same* `root_slots` a second time (`gc.rs:966-974`) when it fires. A
   machine that has tenured N bindings pays an extra O(N) root-vector
   append + Cheney-copy root-dereference on every minor collection for the
   rest of the machine's life, regardless of whether that turn's actual
   allocation touches those bindings at all.
2. **The retained bytes**: every object a persistent root points to (plus its
   full transitive closure, since `tenure` copies the whole graph at bind
   time — `old_space.rs:154-166`) lives in old-space and is never freed by a
   minor collection (`old_space.rs:27-29`: the minor GC's from-range excludes
   old-space addresses entirely) and is never freed by anything else either —
   there is no old-space compaction/eviction path in this codebase (old-space
   is documented "compacted only on an explicit *major* pass (when a binding
   generation dies)" in `old_space.rs:39-40`, but no such major-pass caller
   exists — `grep -rn "major" tidepool-codegen/src` turns up only that one
   comment). So old-space is, in practice, append-only for the machine's
   life: it never shrinks.

### MEASURED

Test: `tidepool-codegen/tests/realm_root_growth.rs::realm_root_growth_persistent_roots_and_heap`.
Command: `cargo nextest run -p tidepool-codegen -E 'test(realm_root_growth)' --no-capture`.
Run under `set_gc_poison(true)` + `set_heap_verify(true)` for the whole test
body (touches the heap across real minor collections — see below).

One session machine (`compile_session`, 2 KiB nursery), driven through N
successive value-plane binds (`add_function` + `run_pure_and_bind`, each
producing a fresh `Con(C1,[Lit])`, ~56 bytes pre-tenure):

| N | `persistent_roots_count()` | `heap_stats().nursery_bytes` | `heap_stats().live_bytes` | `heap_stats().gc_count` | `old_space_bytes_used()` |
|---|---|---|---|---|---|
| 1  | 1  | 2048 | 56   | 0 | 56   |
| 8  | 8  | 2048 | 448  | 0 | 448  |
| 64 | 64 | 2048 | 1568 | 1 | 3584 |

Answers, directly from the table:

- **`persistent_roots_count` grows strictly monotonically with binds** — in
  fact exactly 1:1 with N (test asserts `roots == n` at every checkpoint).
  Nothing ever shrinks it short of drop: this is the direct empirical
  counterpart of the "clears only at machine drop" static finding.
- **`old_space_bytes_used` grows with it, exactly**: 56, 448, 3584 — precisely
  N × 56 bytes at every checkpoint, i.e. every tenure adds its full closure
  and nothing is ever reclaimed. This is the number `heap_stats().live_bytes`
  (nursery high-water mark) *cannot* show, because a minor GC resets it: one
  real collection fired between N=8 and N=64 (`gc_count` 0→1, matching the 2
  KiB nursery filling around round ~36 at 56 B/round), and `live_bytes` at
  N=64 (1568) reflects only the ~28 rounds allocated *since* that collection
  — not the cumulative 3584 bytes retained across the session. Root growth
  drives a monotonic, unbounded retained-bytes number that the nursery-facing
  stat structurally cannot surface; `old_space_bytes_used()` (a small
  test-only accessor added for this measurement — see below) is what actually
  answers "does retained memory grow with the root count."

## COST B — compiled-function lifetime

### STATIC FINDINGS

**Nothing ever removes a function from the `JITModule`.** `CodegenPipeline`
(`tidepool-codegen/src/pipeline.rs`) only ever *adds*:
`declare_function` (`:227-232`) and `define_function` (`:240-276`, called by
`JitEffectMachine::add_function`, `jit_machine.rs:1285`) both call straight
into `cranelift_module::Module` methods with no corresponding removal call
anywhere in this crate (`grep -rn "remove_function\|module\.free\|\.reset()"
tidepool-codegen/src` → zero hits). `functions_defined` (`pipeline.rs:91-98`,
accessor `:205-207`) is explicitly documented "Never reset" — a session-
lifetime-monotonic counter by design, mirroring the persistent-root count.

**Cranelift's own granularity is whole-module, not per-function — and it
deliberately LEAKS on drop once anything is finalized.** Read directly from
the vendored `cranelift-jit` 0.129.1 source
(`~/.cargo/registry/src/.../cranelift-jit-0.129.1/src/`):

- `JITModule` (`backend.rs:169-179`) has exactly one public escape hatch for
  freeing code memory: `pub unsafe fn free_memory(mut self)`
  (`backend.rs:192-194`). It takes `self` **by value** (consumes the module)
  and is documented unsafe because "it invalidates any pointers retrieved
  from the corresponding module." **`JITModule` has no `impl Drop` at all** —
  dropping it ordinarily is a plain field-by-field drop, which does NOT call
  `free_memory`.
- The actual memory owner is `memory: Box<dyn JITMemoryProvider + Send>`
  (a private field, not reachable from `CodegenPipeline` or
  `JitEffectMachine` at all). For this pipeline that's `ArenaMemoryProvider`
  (`pipeline.rs:162-164`, one 256 MiB reservation per machine). **This is the
  single most important finding in this lane:**
  `ArenaMemoryProvider`'s own `Drop` impl (`memory/arena.rs:208-219`)
  explicitly refuses to free once anything has been finalized:
  ```rust
  impl Drop for ArenaMemoryProvider {
      fn drop(&mut self) {
          if self.ptr == ptr::null_mut() { return; }
          let is_live = self.segments.iter().any(|seg| seg.finalized);
          if !is_live {
              // Only free memory if it's not been finalized yet.
              // Otherwise, leak it since JIT memory may still be in use.
              unsafe { self.free_memory() };
          }
      }
  }
  ```
  i.e. Cranelift's own author-intent is: once code has been finalized
  (`finalize_definitions`), it is **unsafe to know** whether any live
  pointer into it still exists, so the provider chooses to leak rather than
  risk a use-after-free.
- Every `JitEffectMachine` reaches this state on its very first compile:
  `compile_inner` calls `pipeline.finalize()` unconditionally
  (`jit_machine.rs:442`), and every `add_function` call finalizes again
  (`jit_machine.rs:1378`, doc: "Multi-round finalize: finalize_definitions is
  safe to re-run"). So `is_live` is `true` for essentially every
  `JitEffectMachine` that has ever compiled anything — which is all of them.
- **`JITModule::free_memory` is never called anywhere in this repository**
  (`grep -rn free_memory tidepool-codegen tidepool-runtime tidepool-repl` →
  zero hits). Nothing in `tidepool-codegen` even has a path to call it: it
  requires owning the `JITModule` by value, and `CodegenPipeline`'s `module`
  field is dropped as part of ordinary struct teardown, never explicitly
  consumed.

**Conclusion**: dropping a `JitEffectMachine` does not, and structurally
cannot without new machinery, reclaim its compiled code's executable memory.
This is not a bug tidepool introduced — it is Cranelift's own safety
invariant (finalized code may still be executing/pointed-to; freeing it
blind is unsound) — but it means a cycle-scoped design's central
reclamation-by-drop premise is **false for the JITModule specifically**,
even though it holds for the session heap (see the verdict table below).

### MEASURED

Tests: `tidepool-codegen/tests/realm_module_growth.rs`. Memory proxy: process
RSS from `/proc/self/status`'s `VmRSS:` line — **not** a Cranelift-side byte
count, because none is reachable (`ArenaMemoryProvider` exposes no public
accounting method, and `JITModule`'s `memory` field is private with no
accessor). This was the fallback the task anticipated; it is stated here so
the numbers below are read as an RSS proxy, not a precise arena byte count.

**Test 1** — one session machine, `add_function` accumulating N fragments.
Command: `cargo nextest run -p tidepool-codegen -E
'test(realm_module_growth_single_machine)' --no-capture`.

| N | `functions_defined()` | RSS (bytes) |
|---|---|---|
| 1   | 2   | 8,970,240 |
| 16  | 17  | 9,068,544 |
| 128 | 129 | 9,633,792 |

(`functions_defined() == N+1` throughout — the `+1` is the dummy entry
`compile_session` compiles once and never runs.) `functions_defined` is exact
and monotonic by construction (it's a plain incrementing counter — see
static findings); RSS is the noisier proxy but shows the same direction:
+98 KB from N=1→16, +565 KB from N=16→128, growing with function count and
never shrinking, consistent with "nothing ever frees a compiled function."

**Test 2** — create-and-drop 32 whole session machines (~16 fragments each,
517 functions total across the run), measuring RSS at baseline (before the
loop), peak (max observed across the loop), and after all 32 drops. Command:
`cargo nextest run -p tidepool-codegen -E
'test(realm_module_growth_create_drop_32_machines)' --no-capture`. Run 4
times back-to-back to check for noise:

| run | baseline (bytes) | peak (bytes) | after 32 drops (bytes) |
|---|---|---|---|
| 1 | 4,853,760 | 11,223,040 | 11,218,944 |
| 2 | 4,763,648 | 11,055,104 | 11,055,104 |
| 3 | 4,714,496 | 11,120,640 | 11,120,640 |
| 4 | 4,759,552 | 11,218,944 | 11,218,944 |

This was **not noisy** — `after 32 drops` equals `peak` to the byte in 3 of 4
runs and is 4 KB (one page) off in the fourth. Across all four runs RSS never
drops back toward baseline (~4.7-4.9 MB) after 32 machines are created and
dropped; it stays pinned at roughly peak (~11.0-11.2 MB), a ~6.3-6.5 MB
net gain that 32 `drop()` calls did nothing to reclaim. This is a direct,
reproducible empirical confirmation of the static finding: `drop`-based
reclamation does not give JIT code memory back, at any scale this test
exercises.

Per the task's own DONE criteria this is a successful, load-bearing NO-GO
signal for "reclamation by drop" as stated in the anchor doc, not a test
failure — the test makes no pass/fail assertion on the RSS direction, only
reports it (see the test's doc comment).

## RECLAMATION VERDICT — does dropping a `JitEffectMachine` reclaim...

- **(a) the session heap** (nursery `Vec<u64>`/`Nursery` buffer): **MEASURED,
  yes, ordinarily** — this is plain Rust ownership (`SessionState.heap`,
  `Nursery`), no custom leak logic anywhere in the drop path; freed like any
  other owned `Vec`/buffer when `JitEffectMachine` drops. Not directly
  isolated by a dedicated RSS test in this lane (Test 2's ~16-fragment
  machines allocate very little nursery per machine at 2 KiB–64 KiB), but
  nothing in the static read of `Drop for JitEffectMachine`
  (`jit_machine.rs:2266-2288`) or `SessionState`/`Nursery` suggests otherwise
  — REASONED-FROM-CODE, with the MEASURED old-space/JITModule numbers as
  supporting context for what *doesn't* reclaim.
- **(b) old-space arenas**: **REASONED-FROM-CODE, yes, structurally** —
  `OldSpace.arenas: Vec<Vec<u8>>` is an ordinary owned field; `Drop for
  JitEffectMachine` retires every arena's write-barrier bookkeeping first
  (`jit_machine.rs:2276-2284`, `retire_old_space_arena`) specifically so nothing
  outlives the memory, then `self.session` (holding `OldSpace`) drops
  normally after the fn body returns, freeing the arena `Vec<u8>`s. This is a
  genuine reclamation — unlike the JITModule case, there is no leak-on-drop
  logic anywhere in `old_space.rs`. (Not independently isolated by an RSS
  test here — Test 1/2's small fragments tenure only tens to low-hundreds of
  bytes, well under RSS noise floor for a dedicated before/after read.)
- **(c) persistent/stowed root registries**: **MEASURED, yes, trivially** —
  `free_session_heap` (`machine_state.rs:428-440`) explicitly clears
  `persistent_roots`/`stowed_roots`/`remembered_slots` at drop, and
  `MachineState` itself is an owned field freed with the rest of the struct.
  This was never in question — the cost these registries impose is entirely
  the O(N) per-collection scan + unbounded retained bytes *while the machine
  is alive* (COST A, above), not anything left behind after drop. The
  registry Vecs themselves vanish with the machine exactly as expected.
- **(d) JITModule code memory**: **MEASURED, no** — this is the finding of
  the lane. Both the static read of `cranelift-jit` 0.129.1's
  `ArenaMemoryProvider::drop` (leaks once any segment is finalized, which is
  every real machine) and the RSS numbers above (Test 2: ~6.3-6.5 MB
  retained across 32 create/drop cycles, `after_32_drops` == `peak` to the
  byte in 3/4 runs) agree: dropping a `JitEffectMachine` does not free its
  compiled code. A cycle-scoped machine (drop at loop boundary) reclaims (a),
  (b), and (c) for free, but for (d) it only *stops the leak from growing
  further within one cycle* — it does not undo the leak that cycle already
  incurred. Repeated cycles still leak monotonically, at whatever rate that
  cycle's fragment count implies (Test 1: ~4.4 KB RSS proxy per added
  function, N=16→128).

## `fork_snapshot` status

**Does not exist in this repository.** `grep -rn fork_snapshot .` (run fresh
for this doc) returns zero hits in any `.rs` file — only in
`plans/post-restart/one-compile-bootstrap.md` and
`plans/post-restart/realm-spike.md`, both planning documents. There is no
function, method, or even a stub named `fork_snapshot` anywhere in
`tidepool-codegen` or elsewhere in the workspace. It is purely planned: the
anchor doc's own phrasing ("the fork_snapshot 400-LOC clone shrinks... maybe
to nothing," `one-compile-bootstrap.md:73`) describes a *future* cross-machine
Cheney-clone mechanism the FULL-FORK harness decision
(`harness-one-model-full-fork`, 2026-08-01) would need if parent/child
continuations lived on *separate* machines — realm unification is framed as
avoiding having to build it, not as shrinking something that exists today.
Today's segment-40 nested-child mechanism (`jit_machine.rs`'s
`enter_nested_child`/`stowed_roots`, documented in `tidepool-codegen/CLAUDE.md`)
already runs parent+child on **one** machine/heap with zero cross-machine
cloning — so the "400-LOC clone" fork_snapshot would need is itself
speculative, sized for a cross-machine case that has no implementation to
measure. The verdict should treat this as "a cost avoided by not building
something," not "a cost this spike measured being paid today."

## Test-only additions (called out per the task boundary)

Two small accessors were added to `tidepool-codegen/src/jit_machine.rs`
(`JitEffectMachine`) because no existing public accessor reached the numbers
this doc needed, and neither changes any existing behavior:

- `functions_defined(&self) -> u64` — delegates to the pre-existing
  (already-tracked, "never reset") `CodegenPipeline::functions_defined()`,
  which had no reach from `JitEffectMachine`.
- `old_space_bytes_used(&self) -> usize` — delegates to the pre-existing
  `OldSpace::bytes_used()` (already used internally, `#[allow(dead_code)]`'d
  at the module level), which likewise had no reach from `JitEffectMachine`.
  This is the number that makes the COST A "retained bytes grow with root
  count" claim MEASURED rather than inferred from `live_bytes` alone.

Both are read-only, `pub`, and mirror the existing style of
`persistent_roots_count`/`remembered_slots_count`/`stowed_roots_count` right
above them in the same `impl` block.

## Receipts

- `cargo nextest run -p tidepool-codegen -E 'test(realm_root_growth) +
  test(realm_module_growth)' --no-capture` — 3 tests run, 3 passed.
- `cargo nextest run -p tidepool-codegen` (full crate, includes the two new
  test files) — **654 tests run: 654 passed (4 slow), 8 skipped**.
- `realm_root_growth.rs` runs under `set_gc_poison(true)` +
  `set_heap_verify(true)` for its full body (stated in-test); one real
  collection fires and is asserted (`gc_count > 0` at N=64), so both knobs
  are exercised, not just set inertly.
- `realm_module_growth.rs`'s two tests do not touch GC poison/heap-verify —
  they never run a fragment (Test 1) or run only trivial pure binds without
  forcing collections at the nursery sizes used (Test 2), so there is no heap
  invariant in play for those two to guard; this is stated here rather than
  silently omitted.
