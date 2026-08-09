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
| C1 | 2026-08-08 | `git rebase root.realm-spike` (proto lane, second rebase — onto the jit-chain-2 fold) | `plans/post-restart/realm-conflict-ledger.md` | Two overlapping hunks in THIS file: both sides wrote a `Z3` row, and both rewrote the running tally. The TL had transcribed proto's Z3 (correctly, and better worded) and appended a "Reading of the experiment" section; proto's original commit still carried its own wording of the same row. | Took the TL's side wholesale — it already contained proto's content, so proto's side was strictly redundant. Nothing was dropped: the surviving text is a superset. | 2 | No — TL's row is a transcription of proto's, plus analysis proto did not have. |
| C2 | 2026-08-08 | `git rebase root.realm-spike` (proto lane, THIRD rebase — onto `0dc93715`, the TL's own post-jit-chain-2 rebase) | `plans/post-restart/realm-conflict-ledger.md` | Same file, worse: both sides had independently written a row numbered **Z4** for DIFFERENT events (the TL's rebase of `root.realm-spike`; proto's rebase of `root.realm-spike.proto`), and both had rewritten the "Honest bound" paragraph of the reading section. Two overlapping hunks, neither side redundant this time. | Renumbered proto's row to **Z5** and kept both — they document different rebases. Took the TL's revised "Honest bound" (a superset of proto's, since it accounts for the TL's own Z4) and grafted proto's two unique points as a named addendum rather than interleaving them. | 6 | No — both rows and both analyses survive. |

**TL sign-off on C1/C2 (required by the recording rule).** Both resolutions
reviewed against the pre-conflict text of each side: nothing was dropped, so
neither needed a stop-and-ask. Proto's two corrections to the TL's own analysis
are ACCEPTED as factually right — the claim that proto skipped step 5 because it
"ran out of budget" was the TL's inference and proto never said it; the real
reason was proto's own mixed-path finding turning that work into a whole-session
conversion. The softening of "zero conflicts" to "zero CODE conflicts" is
likewise correct now that C1/C2 exist.

Worth stating plainly because it is the funniest and most useful result here:
**the only artifact four concurrent lanes ever collided on was this file — the
document describing the collisions.** Two agents appending prose rows to one
small shared table conflicted twice; three agents appending ~330, ~90, and ~30
lines of Rust to one 3400-line source file conflicted zero times.

## Zero-conflict folds

| # | when | operation | branch | files touched | note |
|---|------|-----------|--------|---------------|------|
| Z1 | 2026-08-08 | `merge` into `root.realm-spike` | `root.realm-spike.lifetime` | 4 files: `plans/post-restart/spike-notes/realm-lifetime.md`, `tidepool-codegen/tests/realm_root_growth.rs`, `tidepool-codegen/tests/realm_module_growth.rs`, `tidepool-codegen/src/jit_machine.rs` | Clean. Note the fourth file: this lane DID edit `jit_machine.rs` — the file the extract wave's runtime territory also touches — adding two accessors (`functions_defined`, `old_space_bytes_used`) into the existing accessor block near `persistent_roots_count`. Additive `impl`-block insertion, no conflict against the fork point. |
| Z2 | 2026-08-08 | `merge` into `root.realm-spike` | `root.realm-spike.leakcmp` | 2 files: `plans/post-restart/spike-notes/realm-leak-comparison.md`, `tidepool-codegen/tests/realm_leak_comparison.rs` | Clean. Second lane to add a file under `spike-notes/` and a file under `tidepool-codegen/tests/` — same two directories `lifetime` had already written to. Sibling files in a shared directory do not conflict; only same-file hunks do. Worth stating, since directory-level overlap is what timid partitioning usually optimizes against. |
| Z3 | 2026-08-08 | `git rebase root.realm-spike` (proto lane, before `submit_branch`) | `root.realm-spike.proto` | rebased 3 commits over Z1+Z2; overlap was `tidepool-codegen/src/jit_machine.rs` (both sides) and `tidepool-codegen/tests/` (sibling files) | Clean, 0 minutes. **The sharpest data point of the experiment: the first case where two lanes edited the SAME FILE concurrently and substantially.** `lifetime` added two accessors before `is_suspended`; `proto` added ~330 lines to the same `impl` block — registry types above the struct, two fields inside it, new methods after `run_child_fragment_pure` — plus a parameter change to `finish_suspendable` and two call sites. Zero conflicts, because the hunks landed in different regions of a 3400-line file. Same-file overlap is not same-hunk overlap, and the timidity this experiment tests conflates them. (Transcribed from the proto branch, which stays unmerged — the parent cannot observe a child's own rebase.) |
| Z4 | 2026-08-08 | `git rebase harness-interaction-surface` (tip cd0f4002; jit-chain-2 folded into it) | `root.realm-spike` (this lane) | 6 commits replayed. Incoming diff in this lane's territory: `jit_machine.rs` +90/-, `pipeline.rs` +149, `stack_map.rs` +163, `gc.rs` +11, `binding_table.rs` +91, `datacon_env.rs` +259, a NEW `emit/apply.rs` (+403) extracted from `emit/expr.rs` (-356), plus a D9 signature change with call sites in `tidepool-runtime` and `tidepool-repl` | **Clean, 0 minutes, 0 conflicts.** Root sent this specifically because jit-chain-2 "restructures your exact territory" — and it did, including an extraction refactor. Still nothing to resolve. | 0 | no |
| Z5 | 2026-08-08 | `git rebase root.realm-spike` (proto lane, second rebase — onto the jit-chain-2 fold, tip `cd0f4002`) | `root.realm-spike.proto` | `tidepool-codegen/src/jit_machine.rs` (both sides), plus jit-chain-2's `emit/apply.rs` (+403, extracted from `expr.rs`), `pipeline.rs` (+149), `stack_map.rs` (+163), `host_fns/gc.rs` | **Zero conflicts on the CODE side**, in the case the experiment most wanted: not additive-vs-additive but **additive-vs-RESTRUCTURING**. jit-chain-2 refactored `jit_machine.rs` (63 insertions / 27 deletions — a ConTags re-resolve in `install_registries`, a `gc_retry` consolidation in the stream-tail path) while proto had ~330 lines outstanding against the same `impl`. Still no overlap: jit-chain-2 rewrote existing function BODIES, proto added new types, fields, and methods. Restructuring and accretion touch disjoint regions almost by construction. The only conflicts this rebase produced (C1, then C2 when it was repeated onto `0dc93715`) were in this ledger file — prose, not code. |

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

Z4 transfer proof: `cargo nextest run -p tidepool-codegen` → **684 passed, 0
failed, 8 skipped** at `0dc93715` (pre-rebase this lane was 655/655; jit-chain-2
contributes the additional 29). The two sibling lanes' four added tests
(`realm_root_growth`, `realm_module_growth` ×2, `realm_leak_comparison`) all
survive the fold green, so their measurements still hold on the new shape.

**The lesson for the experiment is that prose anchors are the fragile thing,
not code.** Three lanes' concurrent code edits to one 3400-line file cost zero
minutes; the docs describing that file needed a full re-verification pass. A
conflict ledger that only counted git conflicts would have recorded this rebase
as free.


## Running tally

- Conflicting folds: 2 (C1, C2 — both this DOC file, never code)
- Zero-conflict folds: 5 (Z1, Z2 folds; Z3, Z4, Z5 rebases)
- Same-file concurrent edits: 3 (Z3 `jit_machine.rs`; Z4 and Z5 the same file vs an incoming restructuring fold) — still zero CODE conflicts
- Total resolution minutes: 8 (C1 2, C2 6)
- **Anchor-drift re-verification cost: real but unmeasured in minutes** (Z4) — the largest non-conflict cost the experiment has found
- Sides dropped: 0

## Reading of the experiment (TL)

Four lanes, three of which edited `tidepool-codegen/src/jit_machine.rs`
concurrently, produced **zero CODE conflicts**. (Written when the count was
zero conflicts outright; C1 and C2 have since landed, both of them prose
collisions in this ledger itself — see proto's addendum below. No code conflict
has ever occurred.) The lane was launched to test whether we are too timid about
merge conflicts; on this evidence, we are.

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
   step 5 was optional and its own mixed-path finding had turned that work from
   an addition into a whole-session conversion, not because it was avoiding the
   file. (Correction from proto: budget was not the reason.) So the experiment
   did not actually test a restructuring-vs-restructuring overlap. That remains
   unmeasured.

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

**Proto's addendum (two points not in the above).** First, the mechanism behind
Z4/Z5's zero cost is structural, not luck: a restructuring edit rewrites existing
function BODIES while an accretion edit APPENDS new items, so the two occupy
disjoint regions of a file almost by construction. That is why
additive-vs-restructuring came out nearly as cheap as additive-vs-additive, and
it predicts that the genuinely expensive case is restructuring-vs-restructuring.
Second, the experiment HAS now produced conflicts — C1 and C2, both of them two
agents writing prose into the same table in this very ledger. That is the honest
shape of the residual risk: **narrow shared artifacts, not large shared files.**
The one thing four lanes collided on was the document describing the collisions.
