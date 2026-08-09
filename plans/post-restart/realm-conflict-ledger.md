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

| Z4 | 2026-08-08 | `git rebase harness-interaction-surface` (tip cd0f4002; jit-chain-2 folded into it) | `root.realm-spike` (this lane) | 6 commits replayed. Incoming diff in this lane's territory: `jit_machine.rs` +90/-, `pipeline.rs` +149, `stack_map.rs` +163, `gc.rs` +11, `binding_table.rs` +91, `datacon_env.rs` +259, a NEW `emit/apply.rs` (+403) extracted from `emit/expr.rs` (-356), plus a D9 signature change with call sites in `tidepool-runtime` and `tidepool-repl` | **Clean, 0 minutes, 0 conflicts.** Root sent this specifically because jit-chain-2 "restructures your exact territory" — and it did, including an extraction refactor. Still nothing to resolve. | 0 | no |

### Z4 anchor drift (the real cost of this rebase)

The rebase cost zero conflict-minutes but was NOT free: it silently invalidated
file:line anchors in already-committed findings docs. Git cannot flag this —
the docs are prose, not code. Measured drift:

| anchor | pre-rebase | post-rebase | semantics |
|---|---|---|---|
| `gc.rs` `extend_stowed_roots` fold | 902 | **902** | unchanged |
| `machine_state.rs` `register_persistent_root` | 485 | **485** | unchanged |
| `machine_state.rs` `clear_persistent_roots` | 494 | **494** | unchanged |
| `machine_state.rs` `deregister_stowed_root` | 520 | **520** | unchanged |
| `machine_state.rs` `free_session_heap` | 428 | **428** | unchanged |
| `jit_machine.rs` `suspended_continuation` | 180 | **180** | unchanged |
| `jit_machine.rs` `add_function` | 1285 | 1291 | +6 |
| `jit_machine.rs` `enter_nested_child` | 2149 | 2194 | +45 |
| `jit_machine.rs` `run_child_fragment` | 2188 | 2233 | +45 |
| `pipeline.rs` arena reservation | 162-164 | 173-176 | +11 |
| `resident.rs` `run_child` | 415 | 418 | +3 |
| `resident.rs` `ChildSuspended` raise site | 509 | 512 | +3 |

Every semantic claim in every lane's doc survived; only positions moved, and
the most load-bearing anchors (the GC root fold, the whole persistent-root API)
did not move at all. Corroboration worth recording: jit-chain-2 independently
added a comment at `pipeline.rs:173` reading "256MB virtual reservation —
demand-paged (PROT_NONE → committed on write)", which confirms the `leakcmp`
lane's reserved-not-committed finding from an unrelated author.

**The lesson for the experiment is that prose anchors are the fragile thing,
not code.** Three lanes' concurrent code edits to one 3400-line file cost zero
minutes; the docs describing that file needed a full re-verification pass. A
conflict ledger that only counted git conflicts would have recorded this rebase
as free.

## Running tally

- Conflicting folds: 0
- Zero-conflict folds: 4 (Z1, Z2 folds; Z3, Z4 rebases)
- Same-file concurrent edits: 2 (Z3 `jit_machine.rs`; Z4 same file vs an incoming restructuring fold) — still zero conflicts
- Total resolution minutes: 0
- **Anchor-drift re-verification cost: real but unmeasured in minutes** (Z4) — the only non-zero cost the experiment has found
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

Honest bound on the conclusion, REVISED after Z4: the first three data points
measured only **additive concurrent work on one large file**, which is cheap.
Z4 raised the difficulty — a rebase across an actual restructuring fold
(jit-chain-2: a 403-line extraction out of `emit/expr.rs`, a D9 signature change
with call sites in two other crates, GC-path consolidation) — and it too cost
zero conflict-minutes.

What remains genuinely unmeasured is two lanes rewriting **the same function**.
`proto` changed `finish_suspendable`'s signature for free, but it had two call
sites, both in the same file. Nobody in this lane collided on a narrow shared
interface, because nobody was assigned overlapping ownership of one — the specs
were additive-sibling by construction.

So the defensible claim is narrower than "conflicts are cheap": **file-level and
even directory-level overlap is not worth avoiding, and neither is rebasing
across someone else's refactor.** The timidity worth keeping is about two agents
owning the same function or the same narrow interface — which is a
decomposition question, not a merge question, and the existing
additive-sibling spec discipline already handles it.

The one real cost the experiment surfaced was not a conflict at all: prose
file:line anchors drifting under a refactor (Z4), which git cannot detect and
which a conflict-only ledger would have scored as free.
