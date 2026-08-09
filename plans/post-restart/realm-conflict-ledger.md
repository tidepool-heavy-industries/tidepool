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
| C3 | 2026-08-08 | `git rebase root.realm-spike` (checklist lane, second rebase — onto `7ec1dd6c`, the TL's fold of C1/C2 + proto's Z5) | `plans/post-restart/realm-conflict-ledger.md` | Same file, third time: this lane had independently written its OWN row numbered **Z5** (for the checklist lane's rebase, a different event from proto's Z5 already in the table), plus its own drift-analysis subsection and its own full rewrite of the "Running tally" block — all three overlapping the TL's post-C1/C2 versions of the same regions. | Renumbered the checklist lane's row to **Z6** (proto's Z5 stands; it was folded first). Kept the TL's "Running tally" as the base (it already accounts for C1/C2 and proto's Z5) and updated its counts/bullets to add the checklist lane's Z6 contribution rather than overwriting. Table-row insertion point moved out of the conflict hunk entirely (a plain, non-conflicting edit to the `Zero-conflict folds` table) so a future fourth writer lands a clean insert instead of a fourth numbering collision. | 5 | No — both Z5(proto) and Z6(checklist) rows, both drift analyses, and both tally authors' bullets survive. |

**TL sign-off on C1/C2/C3 (required by the recording rule).** All three
resolutions reviewed against the pre-conflict text of each side: nothing was
dropped in any of them, so none needed a stop-and-ask. Proto's two corrections
to the TL's own analysis (in C1/C2) are ACCEPTED as factually right — the claim
that proto skipped step 5 because it "ran out of budget" was the TL's inference
and proto never said it; the real reason was proto's own mixed-path finding
turning that work into a whole-session conversion. The softening of "zero
conflicts" to "zero CODE conflicts" is likewise correct now that C1/C2/C3
exist.

Worth stating plainly because it is now the sharpest and most repeated result
here: **the only artifact this lane's four-plus concurrent agents have EVER
collided on is this file — the document describing the collisions — and it has
now happened three times running (C1, C2, C3).** Two agents appending prose
rows to one small shared table conflicted three times in a row; three agents
appending ~330, ~90, and ~30 lines of Rust to one 3400-line source file
conflicted zero times. The pattern is strong enough now to state as a
conclusion, not an anecdote: **the counter is a shared mutable cell nobody
owns.** `Z<n>`/`C<n>` is a plain incrementing integer that every writer reads,
increments, and appends against independently, with no lock, no reservation,
and no author field distinguishing "the next number" from "the number I
happened to see last." Rust's borrow checker enforces exclusive access to a
mutable cell; this ledger's numbering scheme has no equivalent, so every
concurrent writer picks the same next integer by construction, every time,
regardless of how careful any individual agent is. The fix is not "be more
careful" (three careful agents already collided three times) — it is giving
the counter an actual owner, e.g. requiring `merge`/`rebase --continue` to
mint the next `Z`/`C` number atomically at fold time rather than letting each
lane pre-assign one before it knows who else is in flight.

## Zero-conflict folds

| # | when | operation | branch | files touched | note |
|---|------|-----------|--------|---------------|------|
| Z1 | 2026-08-08 | `merge` into `root.realm-spike` | `root.realm-spike.lifetime` | 4 files: `plans/post-restart/spike-notes/realm-lifetime.md`, `tidepool-codegen/tests/realm_root_growth.rs`, `tidepool-codegen/tests/realm_module_growth.rs`, `tidepool-codegen/src/jit_machine.rs` | Clean. Note the fourth file: this lane DID edit `jit_machine.rs` — the file the extract wave's runtime territory also touches — adding two accessors (`functions_defined`, `old_space_bytes_used`) into the existing accessor block near `persistent_roots_count`. Additive `impl`-block insertion, no conflict against the fork point. |
| Z2 | 2026-08-08 | `merge` into `root.realm-spike` | `root.realm-spike.leakcmp` | 2 files: `plans/post-restart/spike-notes/realm-leak-comparison.md`, `tidepool-codegen/tests/realm_leak_comparison.rs` | Clean. Second lane to add a file under `spike-notes/` and a file under `tidepool-codegen/tests/` — same two directories `lifetime` had already written to. Sibling files in a shared directory do not conflict; only same-file hunks do. Worth stating, since directory-level overlap is what timid partitioning usually optimizes against. |
| Z3 | 2026-08-08 | `git rebase root.realm-spike` (proto lane, before `submit_branch`) | `root.realm-spike.proto` | rebased 3 commits over Z1+Z2; overlap was `tidepool-codegen/src/jit_machine.rs` (both sides) and `tidepool-codegen/tests/` (sibling files) | Clean, 0 minutes. **The sharpest data point of the experiment: the first case where two lanes edited the SAME FILE concurrently and substantially.** `lifetime` added two accessors before `is_suspended`; `proto` added ~330 lines to the same `impl` block — registry types above the struct, two fields inside it, new methods after `run_child_fragment_pure` — plus a parameter change to `finish_suspendable` and two call sites. Zero conflicts, because the hunks landed in different regions of a 3400-line file. Same-file overlap is not same-hunk overlap, and the timidity this experiment tests conflates them. (Transcribed from the proto branch, which stays unmerged — the parent cannot observe a child's own rebase.) |
| Z4 | 2026-08-08 | `git rebase harness-interaction-surface` (tip cd0f4002; jit-chain-2 folded into it) | `root.realm-spike` (this lane) | 6 commits replayed. Incoming diff in this lane's territory: `jit_machine.rs` +90/-, `pipeline.rs` +149, `stack_map.rs` +163, `gc.rs` +11, `binding_table.rs` +91, `datacon_env.rs` +259, a NEW `emit/apply.rs` (+403) extracted from `emit/expr.rs` (-356), plus a D9 signature change with call sites in `tidepool-runtime` and `tidepool-repl` | **Clean, 0 minutes, 0 conflicts.** Root sent this specifically because jit-chain-2 "restructures your exact territory" — and it did, including an extraction refactor. Still nothing to resolve. | 0 | no |
| Z5 | 2026-08-08 | `git rebase root.realm-spike` (proto lane, second rebase — onto the jit-chain-2 fold, tip `cd0f4002`) | `root.realm-spike.proto` | `tidepool-codegen/src/jit_machine.rs` (both sides), plus jit-chain-2's `emit/apply.rs` (+403, extracted from `expr.rs`), `pipeline.rs` (+149), `stack_map.rs` (+163), `host_fns/gc.rs` | **Zero conflicts on the CODE side**, in the case the experiment most wanted: not additive-vs-additive but **additive-vs-RESTRUCTURING**. jit-chain-2 refactored `jit_machine.rs` (63 insertions / 27 deletions — a ConTags re-resolve in `install_registries`, a `gc_retry` consolidation in the stream-tail path) while proto had ~330 lines outstanding against the same `impl`. Still no overlap: jit-chain-2 rewrote existing function BODIES, proto added new types, fields, and methods. Restructuring and accretion touch disjoint regions almost by construction. The only conflicts this rebase produced (C1, then C2 when it was repeated onto `0dc93715`) were in this ledger file — prose, not code. |
| Z6 | 2026-08-08 | `git rebase root.realm-spike` (checklist child lane, three hops: onto `f6680cbc` mid-flight, then `feb0e0ff` once Z4's transfer-proof commit landed, then `7ec1dd6c` once C1/C2 + proto's Z5 folded) | `root.realm-spike.checklist` (this lane) | 1 file this lane owns: `plans/post-restart/spike-notes/realm-checklist.md`, plus this ledger (see C3 above for the third hop's conflict) | **Clean on the doc-content hops** (0 conflicts, 0 minutes) — a prose-only doc rebasing across a restructuring fold in code it cites but never edits. The third hop conflicted, but only in THIS ledger file (C3), never in the owned doc. |

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

### Z6 anchor-drift re-verification (independent measurement of Z4's cost, from the doc-author side)

Root's Z4 entry above measured drift on a sample of symbols root chose to
check. This lane independently re-verified EVERY anchor actually cited in
`realm-checklist.md` (not a sample) against the post-rebase tree at
`root.realm-spike` commit `feb0e0ff`, per the checklist's own "spot-check
ten, then check them ALL after a rebase this size" instruction. Findings:

- **Confirms Z4's shape.** `jit_machine.rs` drift was zero up to line ~217
  (struct fields), +6 from ~471 to ~1183 (a `#313 defense`-adjacent addition
  before that zone), +19 more (→+25 total) from ~1183 to ~1616, and +45 total
  from ~2149 onward (the `enter_nested_child`/`run_child_fragment`/
  `drive_effect_loop` region Z4 already flagged). `resident.rs` was +3
  uniformly from `ChildSuspended`'s raise site (:509→:512) onward, but its
  earlier third (`pending` field, `ChildSuspended`'s own definition, the
  zero-copy doc comment) had ZERO drift — not `+3` as a blanket assumption
  would have guessed. Lesson: drift is not a single delta per file; it steps
  at each real insertion point, and assuming a uniform offset from one
  sampled symbol would have mis-anchored several citations in this same
  file.
- **One anchor was wrong before the rebase too**, unmasked only by doing a
  full re-check instead of trusting the original: `resident.rs:159` (cited
  twice, for "`ResidentSession<H, O>` is generic over `H`") pointed at a
  doc-comment line, not the struct declaration (:155) or the `handlers: H`
  field (:163) it was actually citing evidence for. The rebase didn't cause
  this — re-deriving every anchor from scratch caught a pre-existing
  imprecision that spot-checking ten anchors the first time had missed.
- **One correction was semantic, not positional — the sharper of the two
  things flagged for re-read.** The fold's D9 change narrowed
  `BindingTable::seed_external_env` from an unconditional every-live-binding
  sweep to a referenced-`VarId` intersection (`binding_table.rs:193-201`,
  with its own test suite stating the narrowing explicitly at
  `binding_table.rs:259-262`). This directly falsified part of Item 2's
  original finding (that the env sweep leaked every realm's bindings into
  every fragment) — the doc's Item 2 section was rewritten, not just
  re-numbered, and its cost rating narrowed from "REAL, unqualified" to
  "REAL but only for the display-name `current` layer; the VarId-keyed half
  is now close to FREE as a side effect of unrelated work." The OTHER
  flagged risk (Item 4's cancellation safepoint, given `gc.rs` changed by 11
  lines) turned out to be a false alarm on inspection: the 11-line diff was
  entirely `host_alloc_gc`'s alloc-retry consolidation (~line 1164),
  unrelated to the cancellation safepoint, which read byte-identical to the
  pre-rebase version.

This is a second data point for the same lesson Z4 already drew: the
re-verification cost is real and does not scale with conflict count (Z6's
underlying rebase had zero conflicts and zero git-visible cost — the C3
conflict logged above is a separate event, on the ledger itself, not on the
thing Z6 measures) — it scales with how many prose claims cite line numbers,
and once with how carefully the ORIGINAL claim was checked. Sampling symbols
to estimate drift (as Z4's table did) is a fine cost estimate; it is not a
substitute for re-reading every anchor a document actually relies on, because
drift is non-uniform within a file and because a small fraction of
"line-number" corrections turn out to be semantic corrections wearing a
line-number's clothes.

| Z7 | 2026-08-08 | `git rebase harness-interaction-surface` (tip `ee5fb242`; parent advanced again during `submit_branch`) | `root.realm-spike` (this lane) | 15 commits replayed. Incoming diff: `plans/post-restart/extract-wave.md` only (+8) — **zero source files in any crate**. | Clean, 0 minutes. Triggered by `submit_branch` refusing with `needs_rebase` rather than by a ping — the tool enforces the cascade, which is why the parent's fold stays clean. Transfer proof carries forward unchanged (`git diff --name-only 81bf2c13 HEAD` outside `plans/` is empty), so the 684/684 at Z4 still describes this tree's source exactly. | 0 | no |

## Running tally

- Conflicting folds: 3 (C1, C2, C3 — all this DOC file, never code)
- Zero-conflict folds: 7 (Z1, Z2 folds; Z3, Z4, Z5, Z6, Z7 rebases)
- Same-file concurrent edits: 3 (Z3 `jit_machine.rs`; Z4 and Z5 the same file vs an incoming restructuring fold) — still zero CODE conflicts
- Total resolution minutes: 13 (C1 2, C2 6, C3 5)
- **Anchor-drift re-verification cost: real but unmeasured in minutes** (Z4, Z6) — the largest non-conflict cost the experiment has found, now measured twice (once from the code side, once from a downstream doc-author's full re-check) with the same conclusion
- One anchor-drift correction was semantic rather than positional (Z6, Item 2's `seed_external_env` narrowing) — the sharpest single data point so far for "a clean rebase is not a free rebase for prose that cites code"
- **Ledger-numbering collisions: 3 (C1, C2, C3), all on this table's own counter, none on the code it records** — see the "shared mutable cell nobody owns" analysis above C3; strong enough now to treat as this lane's second headline finding alongside the code-conflict-cost result
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
