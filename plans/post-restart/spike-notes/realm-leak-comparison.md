# realm-leak-comparison: one machine vs N machines, same workload

Scope: answers exactly one question left open by
`plans/post-restart/spike-notes/realm-lifetime.md` — that lane measured 32
create-and-drop session machines and found ~6.3-6.5 MB of JIT code memory
retained after all 32 drop, but its baseline (32 machines, ~16 fragments
each) was never compared against an equal-sized single-machine run. This lane
makes that comparison directly, plus a secondary check on whether the 256 MiB
`ArenaMemoryProvider` reservation is committed or merely reserved address
space. It does not design the realm machine, does not propose a landing
shape, and does not write verdict language — that is the TL's job.

Test: `tidepool-codegen/tests/realm_leak_comparison.rs::realm_leak_comparison_one_vs_many_machines`.
Command: `cargo nextest run -p tidepool-codegen -E 'test(realm_leak_comparison)' --no-capture`.

## Method

Both arms run back-to-back in **one process**, from **one shared baseline**,
so the two retained deltas are directly comparable (no cross-test baseline
drift, no cross-process noise):

- **ARM A** (today's architecture — one machine per answerer session): 32
  session machines, 16 fragments each, each machine dropped before the next
  is created. 32 × 17 = 544 total compiled functions (16 fragments + 1
  bootstrap dummy per machine).
- **ARM B** (the unified/realm shape): ONE session machine, 512 fragments
  added in sequence, dropped once at the end. 513 total compiled functions
  (512 fragments + 1 bootstrap dummy).

Fragment bodies are identical between arms: global fragment index `n` in
`{0..512}` is `build_value_fragment(n)` (`Con(C1, [Lit n])`) in **both** arms
— the only variable is how many machines the same 512 compiled bodies are
spread across. Neither arm runs the fragments (`run_pure_and_bind`) — this
lane is about `JITModule`/arena growth (COST B in the sibling lane), not
session-heap/persistent-root growth (COST A), which the sibling lane already
covers.

Five checkpoints, reading both `VmRSS:` and `VmSize:` from
`/proc/self/status` at each: baseline, after arm A's 32nd (last) machine is
populated but before it drops, after that last machine drops (all 32 are now
dropped — the other 31 were already dropped mid-loop), after arm B's one
machine is populated with all 512 fragments but before it drops, and after
arm B's machine drops. The test makes no assertion on RSS/VmSize direction —
only on structural function counts (17 for arm A's last machine, 513 for arm
B) — so it cannot go red on a noisy box.

## Results — all 4 runs

Command run 4 times back-to-back, unmodified between runs:
`cargo nextest run -p tidepool-codegen -E 'test(realm_leak_comparison)' --no-capture`.

### Run 1

| checkpoint | RSS (bytes) | VmSize (bytes) |
|---|---|---|
| baseline | 4,775,936 | 158,371,840 |
| after arm A (32nd machine, pre-drop) | 11,280,384 | 8,748,306,432 |
| after arm A drop (all 32 dropped) | 11,280,384 | 8,748,306,432 |
| after arm B (1 machine, 512 fragments, pre-drop) | 13,713,408 | 9,017,794,560 |
| after arm B drop | 13,705,216 | 9,016,741,888 |

Retained deltas: arm A RSS = 6,504,448 B (6.20 MB); arm B RSS = 2,424,832 B
(2.31 MB); arm A VmSize = 8,589,934,592 B (exactly 8 GiB); arm B VmSize =
268,435,456 B (exactly 256 MiB).

### Run 2

| checkpoint | RSS (bytes) | VmSize (bytes) |
|---|---|---|
| baseline | 4,730,880 | 158,371,840 |
| after arm A (pre-drop) | 11,300,864 | 8,748,306,432 |
| after arm A drop | 11,300,864 | 8,748,306,432 |
| after arm B (pre-drop) | 13,750,272 | 9,017,794,560 |
| after arm B drop | 13,709,312 | 9,016,741,888 |

Retained deltas: arm A RSS = 6,569,984 B (6.27 MB); arm B RSS = 2,408,448 B
(2.30 MB); arm A VmSize = 8,589,934,592 B; arm B VmSize = 268,435,456 B.

### Run 3

| checkpoint | RSS (bytes) | VmSize (bytes) |
|---|---|---|
| baseline | 4,820,992 | 158,371,840 |
| after arm A (pre-drop) | 11,259,904 | 8,748,306,432 |
| after arm A drop | 11,259,904 | 8,748,306,432 |
| after arm B (pre-drop) | 13,725,696 | 9,017,794,560 |
| after arm B drop | 13,692,928 | 9,016,741,888 |

Retained deltas: arm A RSS = 6,438,912 B (6.14 MB); arm B RSS = 2,433,024 B
(2.32 MB); arm A VmSize = 8,589,934,592 B; arm B VmSize = 268,435,456 B.

### Run 4

| checkpoint | RSS (bytes) | VmSize (bytes) |
|---|---|---|
| baseline | 4,706,304 | 158,371,840 |
| after arm A (pre-drop) | 11,276,288 | 8,748,306,432 |
| after arm A drop | 11,276,288 | 8,748,306,432 |
| after arm B (pre-drop) | 13,709,312 | 9,017,794,560 |
| after arm B drop | 13,684,736 | 9,016,741,888 |

Retained deltas: arm A RSS = 6,569,984 B (6.27 MB); arm B RSS = 2,408,448 B
(2.30 MB); arm A VmSize = 8,589,934,592 B; arm B VmSize = 268,435,456 B.

### Spread across the 4 runs (not averaged away)

| | min | max | spread | spread in 4 KiB pages |
|---|---|---|---|---|
| arm A retained RSS | 6,438,912 | 6,569,984 | 131,072 B | 32 pages |
| arm B retained RSS | 2,408,448 | 2,433,024 | 24,576 B | 6 pages |
| arm A retained VmSize | 8,589,934,592 | 8,589,934,592 | 0 | 0 (byte-exact, all 4 runs) |
| arm B retained VmSize | 268,435,456 | 268,435,456 | 0 | 0 (byte-exact, all 4 runs) |

VmSize is **byte-identical across all 4 runs** in both arms — a deterministic
number, as expected for a fixed-size virtual reservation. RSS is noisier than
the sibling lane's "within one page" reproducibility: arm A spreads across 32
pages (~2% of its ~6.4 MB retained value) and arm B across 6 pages (~1% of
its ~2.4 MB value). This is worth stating plainly since the sibling lane's
own number was tighter — the extra churn here likely comes from this test
doing 32 distinct `compile_session` + 16×`add_function` cycles (32 separate
ISA/flag-builder/name-arena allocations) versus the sibling's single-shot
32-machine loop measuring only baseline/peak/after, not intermediate
per-machine states; either way the spread is small relative to the effect
size (>2.5x arm A vs arm B) and does not change the reading below.

## Does unification reduce the leak, and by how much?

**Yes, plainly and reproducibly: spreading the identical 512-fragment
workload across one machine instead of 32 leaks roughly 2.7x less resident
memory.** Averaged across the 4 runs, arm A (32 machines) retains ~6.37 MB
of RSS after all machines drop; arm B (1 machine, same 512 fragments) retains
~2.36 MB — a reduction of about 62%, consistent to within a few percent on
every run (ratio 2.68x-2.77x per run, never crossing).

Fitting a `retained_KB = fixed_per_machine + variable_per_function ×
total_functions` model to the two arms' averages (32 machines/544 functions
vs 1 machine/513 functions) gives **~125 KB fixed overhead per machine plus
~4.4 KB per compiled function**. That variable-cost figure lines up closely
with the sibling lane's own Test 1 finding of "~5 KB RSS per function within
one machine," and the fixed-cost figure predicts a lone 17-function machine
(1 dummy + 16 fragments, the sibling's Test 2 shape) would retain about
199 KB — matching the sibling's independently-measured "~200 KB per machine
across 32 machines" almost exactly, even though that number came from a
different test with a different baseline. **This lane turns that
extrapolation into a receipt**: the ~200 KB/machine the sibling inferred is
overwhelmingly *fixed per-machine bookkeeping* (arena/pipeline/name-arena
setup, ISA/flag-builder state, segment/module scaffolding), not a function-
count effect — each individual `add_function` call costs only ~4-5 KB on
top of that fixed floor. Unification's win is exactly that: it pays the
~125 KB machine-setup tax once instead of once per session, for the same
compiled-function payload.

**This is a real reduction, not a wash and not a reversal.** Cranelift's
leak-on-drop behavior (confirmed by the sibling lane) still means arm B's
~2.36 MB is never reclaimed either — unification does not eliminate the
leak, it eliminates the *per-machine multiplier* on top of it. For a
cycle-scoped realm design specifically, this means: the leak this design
was worried about introducing is smaller, per unit of compiled work, than
the leak today's N-machines-per-cycle architecture already pays. The
verdict question this feeds is "does unification make the JITModule-leak
finding worse, better, or a wash" — the answer these numbers support is
**better, by roughly a factor of ~2.7x at this fragment count**, growing
larger as fragment count per session grows smaller relative to the fixed
per-machine floor (a 4-fragment-per-machine baseline would fare worse for
arm A than this test's 16, since the ~125 KB fixed cost dominates even more).

## The 256 MiB `ArenaMemoryProvider` reservation: reserved, not committed

**Reserved virtual address space, committed lazily per segment — confirmed
both by reading the vendored source and by the VmSize/VmRSS numbers above.**

Source (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/cranelift-jit-0.129.1/src/memory/arena.rs`):

- `ArenaMemoryProvider::new_with_size` (`arena.rs:108-124`) calls
  `region::alloc(size, region::Protection::NONE)` — a single up-front `mmap`
  of the full requested size (256 MiB here, per
  `tidepool-codegen/src/pipeline.rs:162-164`) with **`PROT_NONE`**, i.e. no
  physical pages committed, just address space claimed. The struct doc
  comment (`arena.rs:82-95`) says this explicitly: "initially allocated with
  PROT_NONE and gradually updated as the JIT requires more space."
- Actual space is carved out lazily by `allocate_segment`
  (`arena.rs:169-185`), which advances a `position` cursor within the
  pre-reserved region and calls `Segment::new` → `set_rw()`
  (`arena.rs:41-46`, `region::protect(... READ_WRITE)`) — this is what
  actually commits pages, and only for the segment's own size, not the full
  256 MiB.
- **The leak is bigger than "committed pages stay resident": the entire
  256 MiB *reservation* leaks, not just its committed portion.** `Drop for
  ArenaMemoryProvider` (`arena.rs:209-221`) skips `free_memory()` once any
  segment is finalized (true of every real tidepool machine, per the sibling
  lane). `free_memory` (`arena.rs:196-206`) is the only code path that drops
  `self.alloc` (a `ManuallyDrop<Option<region::Allocation>>` — `ManuallyDrop`
  means the compiler will **never** run `region::Allocation`'s own `Drop`
  unless `free_memory` explicitly takes it out and drops it). So a leaked
  `ArenaMemoryProvider` doesn't just lose its committed pages — the whole
  256 MiB `mmap` mapping itself is orphaned and stays mapped in the process's
  address space for the rest of the process's life, no matter how little of
  it was ever touched.

The measurement confirms this precisely: arm A's VmSize delta is
**8,589,934,592 bytes exactly = 8 × 1024³ = 32 × 256 MiB**, byte-identical
across all 4 runs — 32 machines leaked 32 full 256 MiB reservations, no more
and no less, regardless of how many pages within each were ever committed.
Arm B's VmSize delta is **268,435,456 bytes exactly = 256 MiB**, also
byte-identical across all 4 runs — one machine, one reservation, leaked
whole, independent of its 512-fragment payload. Meanwhile RSS in the same
runs grew by only single-digit megabytes total (~6.4 MB for 32 machines,
~2.4 MB for 1) — orders of magnitude below the 8 GiB / 256 MiB virtual
figures. That gap (kilobytes-to-low-megabytes of RSS against gigabytes of
VmSize) is the direct, measured signature of "reserved, not committed."

**Address-space implication for concurrent machines.** Each machine —
leaked or not — permanently claims 256 MiB of the process's virtual address
space once it finalizes. On a 64-bit Linux process (typical
`sizeof(void*)==8`, 47-bit user address space ⇒ 128 TiB, or up to 128 PiB
under 5-level paging on newer kernels/hardware), 256 MiB per machine means
address-space exhaustion would require on the order of **~500,000 leaked
machines at 128 TiB of address space** (or vastly more under 5-level
paging) before virtual memory itself becomes the binding constraint — far
beyond any realistic number of concurrent or historically-leaked sessions a
long-running process would accumulate. In practice, **RSS is the real
constraint, not address space**: the ~2.7x RSS reduction unification buys
(measured above) is the number that matters for how long a long-running
realm process can run before physical memory pressure bites; the 256 MiB
virtual reservation per machine is a real, measured, and permanent
per-machine cost, but not one that will independently exhaust a 64-bit
process's address space at any plausible machine count.

## Receipts

- `cargo nextest run -p tidepool-codegen -E 'test(realm_leak_comparison)' --no-capture`
  — run 4 times back-to-back, all 4 `PASS`, numbers transcribed verbatim
  above (no run's output was cherry-picked or averaged away).
- `cargo nextest run -p tidepool-codegen` (full crate, includes the new test
  file) — **655 tests run: 655 passed (4 slow), 8 skipped**.
- Both arms in the test assert only structural facts
  (`functions_defined() == 17` for arm A's last machine,
  `functions_defined() == 513` for arm B) — no assertion anywhere on
  RSS/VmSize direction, so the test cannot go red on a noisy box.
- `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/cranelift-jit-0.129.1/src/memory/arena.rs`
  read in full for the reservation-vs-commit characterization above
  (`new_with_size`, `allocate_segment`, `Segment::set_rw`, `Drop for
  ArenaMemoryProvider`, `free_memory`).
