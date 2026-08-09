# Conflict ledger — realm-spike lane

This lane was launched deliberately overlapping the extract wave's runtime
territory to measure what merge conflicts actually cost (Inanna, 2026-08-08:
"a test to see if we're being too timid about merge conflicts"). The ledger is
a first-class deliverable — the experiment's data, not bookkeeping.

## Recording rule

One row per rebase/fold conflict, however boring. A fold that produced ZERO
conflicts is also recorded (as a zero-row entry with the branch and the files
it touched) — "no conflict" is the measurement, not the absence of one.

Stop-and-ask applies only when a resolution would DROP a side's behavior.
Mechanical resolutions are logged and moved past.

| # | when | operation | file | hunk shape | resolution | minutes | side dropped? |
|---|------|-----------|------|------------|------------|---------|---------------|

## Zero-conflict folds

| # | when | operation | branch | files touched | note |
|---|------|-----------|--------|---------------|------|
| Z1 | 2026-08-08 | `merge` into `root.realm-spike` | `root.realm-spike.lifetime` | 4 files: `plans/post-restart/spike-notes/realm-lifetime.md`, `tidepool-codegen/tests/realm_root_growth.rs`, `tidepool-codegen/tests/realm_module_growth.rs`, `tidepool-codegen/src/jit_machine.rs` | Clean. Note the fourth file: this lane DID edit `jit_machine.rs` — the file the extract wave's runtime territory also touches — adding two accessors (`functions_defined`, `old_space_bytes_used`) into the existing accessor block near `persistent_roots_count`. Additive `impl`-block insertion, no conflict against the fork point. |

| Z2 | 2026-08-08 | `merge` into `root.realm-spike` | `root.realm-spike.leakcmp` | 2 files: `plans/post-restart/spike-notes/realm-leak-comparison.md`, `tidepool-codegen/tests/realm_leak_comparison.rs` | Clean. Second lane to add a file under `spike-notes/` and a file under `tidepool-codegen/tests/` — same two directories `lifetime` had already written to. Sibling files in a shared directory do not conflict; only same-file hunks do. Worth stating, since directory-level overlap is what timid partitioning usually optimizes against. |

| Z3 | 2026-08-08 | `git rebase root.realm-spike` (proto lane, before `submit_branch`) | `root.realm-spike.proto` | rebased 3 commits over Z1+Z2; overlap was `tidepool-codegen/src/jit_machine.rs` (both sides) and `tidepool-codegen/tests/` (sibling files) | Clean, 0 minutes. **The sharpest data point of the experiment: the first case where two lanes edited the SAME FILE concurrently and substantially.** `lifetime` added two accessors before `is_suspended`; `proto` added ~330 lines to the same `impl` block — registry types above the struct, two fields inside it, new methods after `run_child_fragment_pure` — plus a parameter change to `finish_suspendable` and two call sites. Zero conflicts, because the hunks landed in different regions of a 3400-line file. Same-file overlap is not same-hunk overlap, and the timidity this experiment tests conflates them. (Transcribed from the proto branch, which stays unmerged — the parent cannot observe a child's own rebase.) |

## Running tally

- Conflicting folds: 0
- Zero-conflict folds: 3 (Z1, Z2 folds; Z3 rebase)
- Same-file concurrent edits: 1 (Z3, `jit_machine.rs`) — still zero conflicts
- Total resolution minutes: 0
- Sides dropped: 0

## Reading of the experiment (TL)

Four lanes, three of which edited `tidepool-codegen/src/jit_machine.rs`
concurrently, produced **zero conflicts and zero resolution minutes**. The
lane was launched to test whether we are too timid about merge conflicts; on
this evidence, we are.

The mechanism is worth naming precisely, because the wrong lesson is "conflicts
don't happen." What actually happened:

1. **Same-file is not same-hunk.** `jit_machine.rs` is 3400 lines. Three lanes
   appended to different regions — an accessor block, a struct's field list, a
   new method cluster — and git resolved all of it positionally.
2. **Additive-sibling specs did the real work.** Every lane was scoped to add
   rather than restructure, which is the discipline
   `recursive-tl-dev-review-loop` already prescribes. The cheap outcome here is
   evidence FOR that spec discipline, not evidence that spec discipline is
   unnecessary.
3. **The one predicted collision is the one nobody hit.** `proto` identified
   `resident.rs:196` as a Track-1 site where lifting the `ChildSuspended` wall
   WILL collide with the extract wave — and correctly did not touch it, because
   step 5 was optional and it ran out of budget, not because it was avoiding the
   file. So the experiment did not actually test a restructuring-vs-restructuring
   overlap. That remains unmeasured.

Honest bound on the conclusion: this measures **additive concurrent work on one
large file**, which is cheap. It does not measure two lanes rewriting the same
function, or a lane rebasing across another's signature change to a
widely-called function. `proto` did change `finish_suspendable`'s signature and
that still cost nothing — but it had only two call sites, both in the same file.
The timidity worth keeping is about shared narrow interfaces, not shared files.
