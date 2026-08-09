# Sub-TL `boot` — LEDGER

Items **0** (one-compile bootstrap, Track 1) and **0b** (nameable effect
vocabulary). Spec: `00-spec.md`. Wave operational block:
`../OPERATIONAL.md`.

One row per item: decision, receipt counts, fold conflicts.

---

## Anchors read (2026-08-08)

Confirmed against the tree at `be05f291`, so the specs below cite live lines:

| Anchor | Line | What it is |
|---|---|---|
| `tidepool-runtime/src/session/persistent.rs` | 269–271 | `PersistentSession::new` → `machine: None` — the lazy lifecycle already there |
| `tidepool-runtime/src/session/persistent.rs` | 393 | `bootstrap_if_needed` — no-op when already live |
| `tidepool-runtime/src/session/resident.rs` | 208 | `ResidentSession::bootstrap` — the program-shaped constructor (the defect) |
| `tidepool-runtime/src/session/resident.rs` | 220 | its `core.bootstrap_if_needed(expr, &table)` + `seed_session_table` |
| `tidepool-repl/src/session.rs` | 975, 1105, 1257, 1327, 1469, 2002 | the REPL's bootstrap-from-first-real-compile — the pattern to mirror |
| `tidepool-harness/src/harness.rs` | ~430 | answerer boot seed (`pure (toJSON (0 :: Int))`) |
| `tidepool-harness/src/harness.rs` | 790 | `force()` — the ONLY consumer of `self.boot` |
| `tidepool-harness/src/selfharness/driver.rs` | ~552 | outer boot seed (same trivial compile) |
| `tidepool-harness/src/selfharness/driver.rs` | 624 | `compile_outer` — one `compile_turn` per call |
| `tidepool-harness/src/selfharness/driver.rs` | 929, 1507 | the loop and render `compile_outer` calls |
| `tidepool-harness/src/compile.rs` | 104 | `compile_turn` — single-target (`{target}.cbor`) |
| `haskell/app/Main.hs` | 333 | `writeWholeModuleClosed` — one target, one `meta.cbor` |
| `tidepool-mcp/src/eval_prep.rs` | 116 | `effects_module_source_at` — GADTs+helpers emitted ONLY for the row |
| `tidepool-mcp/src/effect_defs.rs` | ~790 | `runLLMTurn :: Member RunLLMTurn effs => …` — ALREADY row-polymorphic |
| `tidepool-codegen/src/jit_machine.rs` | 1940–1956 | `add_function` re-resolves ConTags per fragment table |

### Two findings that shape the work

1. **Step 6 is free.** `self.boot` is consumed at exactly one site,
   `force()` (harness.rs:790). Once `force()` builds an unbootstrapped
   session, the answerer's machine boots on its first real compile — which
   IS the model's first block. No separate work item; it is an assertion in
   `boot-lazy`'s acceptance.
2. **ConTags at a pure boot.** The outer machine would boot from `render`,
   whose expr is pure (`pure (Loaded.render …)`) and may carry no
   RunLLMTurn ConTags. `add_function` re-resolves ConTags against each
   fragment's table (jit_machine.rs ~1940–1956), so a `MissingConTags`
   boot is recoverable when the loop fragment lands; and with step 4's
   merged meta the render table already carries them. Both legs are pinned
   by test, not argued.

---

## Decomposition — 4 dev leaves, 3 waves

| Dev | Item | Scope | Gate |
|---|---|---|---|
| `boot-count` | 0 (receipt) | extract-spawn counter + launch→first-model-call acceptance test; commit the BASELINE (expect 4) | none — wave 1 |
| `boot-vocab` | 0b | vocabulary-vs-row split in `effects_module_source_at`; answerer `Member` negative test | none — wave 1 |
| `boot-lazy` | 0 steps 1–3 (+6) | `ResidentSession::unbootstrapped`, bootstrap-on-first-real-run, DELETE both seeds | **held on parent's driver.rs go-signal** |
| `boot-onecompile` | 0 steps 4–5 | multi-target emission from ONE GHC session; render+loop in one invocation | after `boot-lazy` folds |

Wave 1 runs unconditionally (neither dev touches `driver.rs`). Waves 2 and
3 are sequential: they overlap heavily in `driver.rs` and
`tidepool-harness/src/compile.rs`, so running them in parallel would buy
nothing but conflict.

`boot-count` deliberately lands FIRST so item 0's done-criterion is an A/B
against a committed red-line rather than an after-the-fact assertion.

Full dev specs: `01-wave2-lazy-boot.md`, `02-wave3-one-compile.md`.

### Wave status

| Wave | State | Gate |
|---|---|---|
| 1 (`boot-count`, `boot-vocab`) | RUNNING (GO'd 2026-08-08) | — |
| 2 (`boot-lazy`) | HELD | `root.harness-lifecycle` fold into `root.extract-wave`, then merge that base into this branch |
| 3 (`boot-onecompile`) | HELD | wave 2's fold |

**The hold is for the ELIMINATE rung.** Root ruled the D7 rung-2 cache
interim explicitly OFF for this lane: the blocker (a half-folded
`harness-lifecycle` chain, so our base lacks the async driver rewrite) has
an imminent resolution, so downgrading would be improvising around a
short wait. No downgrade taken; none contemplated.

Verified rather than assumed, on the wave TL's check: the spec's
sequencing clause says "after driver-async's fold", and driver-async HAD
folded — into `root.harness-lifecycle`, which had not folded to root.
Our base does not contain it. `harness-lifecycle` is +3827/-367 across 43
files, including `harness.rs` and `driver.rs`, i.e. both seed sites and
all three of wave 3's target functions. A dev editing them today would
write against code that does not survive.

---

## Recipe amendments (against `00-spec.md`'s six steps)

1. **Step 6 is not a separate step.** `self.boot` has exactly ONE
   production consumer, `force()` (post-async `harness.rs` 817). Once
   `force()` builds an unbootstrapped session, the answerer's machine
   boots on its first real compile — which IS the model's first block.
   Step 6 falls out of steps 1–3 and is discharged as an ASSERTION in
   `boot-lazy`'s acceptance (pin: no machine after `force()`, machine
   after the first turn), not as work. Recorded here so the spec's "six
   steps" never later reads as an unfinished item.

2. **The ConTags hazard was MIS-STATED by me, and the corrected mechanism
   is stronger, not weaker** (re-verified 2026-08-09 under root's
   sequencing gate).

   I originally described the risk as "render is pure, so its table may
   lack RunLLMTurn's ConTags". That is wrong about what ConTags are.
   `ConTags::try_from(&DataConTable)`
   (`tidepool-codegen/src/effect_machine.rs` 203–250) resolves the
   **freer-simple scaffolding constructors** — `Control.Monad.Freer.Val`,
   `.E`, `Data.OpenUnion.Union`, `Data.FTCQueue.Leaf`, `.Node`
   (`tidepool-repr/src/freer_names.rs` 23–43). It does NOT resolve
   per-effect GADT constructors like `RunLLMTurnWith`. Any term at `Eff`
   type carries them; `pure` at `Eff` literally builds a `Val`.

   The decisive evidence is the seed itself: the thing being deleted is
   `pure (toJSON (0 :: Int))`, documented in-code as "a trivial effectful
   seed carrying the full effect-stack ConTags". A PURE expression through
   this exact template demonstrably yields a ConTags-resolvable table
   today — that is how the machine boots right now. `render` goes through
   the SAME `template_turn_for(&outer_decls(), &stack, …)` with strictly
   more in its module (the qualified `Loaded` import, the state and
   compaction helpers). **It cannot be worse-conditioned than the seed it
   replaces.**

   Also checked, because it was the other way this could fail:
   `seed_session_table` goes away, so the session table now starts empty
   and grows from the first run's `merge_table`. `merge_table`
   (`persistent.rs` 356–373) extends with unseen entries and rejects only
   id-collisions with differing content — growth is fine, and is already
   the normal case today, since the trivial seed table is the SMALLEST
   table any session ever holds and every real turn merges a bigger one in.

   The pin still stands and is still worth having (wave TL's addition,
   accepted): `add_function` re-resolves ConTags per fragment table
   (`jit_machine.rs` 1940–1956), and silent self-healing is the same shape
   as the boot seed itself — scaffolding that goes load-bearing because
   nothing names it. But the test now asserts the REAL invariant rather
   than a proxy: the machine boots from render's own (expr, table) and
   runs that fragment to completion, and a loop fragment carrying real
   effect sites then runs and suspends correctly on the same machine.

### 2d. Strict-mode skip path: UNREACHABLE, not merely test-forbidden

Structural requirement added to `boot-targets` (wave TL, accepted) beyond
the fail-on-any-bad-target test.

`--targets`' forbidden behaviour — silently skipping a failed binding — is
**not a bug**. It is `--all-closed`'s CORRECT behaviour, in the very code
the strict mode is adapted from. A test pin therefore defends that
boundary from OUTSIDE: it does not stop a later refactor re-unifying the
two modes onto a shared skip path, and it does not stop a reader seeing
the skip as intentional, because in the other mode it is.

So: strict mode must not be able to REACH the skip. The skip lives in a
branch strict mode structurally never enters; if the traversal must be
shared, the strict path's failure handling is a DIFFERENT FUNCTION, not a
conditional. The test then guards something already impossible instead of
being the only thing between us and `--all-closed`'s semantics.

The specific regression designed against: someone later "simplifies" the
two paths into one loop with `if strict then error else skip`. Every test
still passes, the guarantee is gone, and nothing in the diff looks wrong.
A boolean is hoistable; separate functions and a lexically-unreachable
branch are not.

**Record the landed shape here at fold**, so a later reader knows the
unreachability was deliberate and does not tidy it into a flag.

**Shape landed — SEPARATE FUNCTION** (the strongest of the two acceptable
shapes; reported by `boot-targets`, **to be re-verified by this TL at
fold**):

- `translateTargetClosed` has **no catch/skip branch at all** — it is not
  a shared traversal carrying a boolean.
- `--all-closed`'s try-and-skip lives entirely in a DIFFERENT function,
  `processFile`'s own `(_, True) -> do` arm, **which never calls
  `translateTargetClosed`**.

So strict mode does not decline to skip; there is no skip in its call
graph to decline. The `if strict then error else skip` regression this
requirement was written against cannot be reached by a refactor that
"removes duplication", because there is no duplicated branch to merge —
the two behaviours never share a function.

### 2b. Named-guard receipt rule (wave-wide, `19b8dca9`) — pushed to all devs

> If a gate exists to catch ONE specific failure mode, the receipt must
> show that specific test passing **by name, with its own pass line** —
> not the aggregate that contains it. Where the guard is cross-lane, the
> receipt must ALSO name the base commit it ran against.

This closes the hole immediately past 2a's placement fix: getting a guard
into a `binary(/^acceptance_/)`-selected file means the shard CAN run it,
not that it DID. A renamed binary, an `#[ignore]`, a `cfg`, or an
env-gated early return each leaves a correct tree, a green aggregate, and
an unrun guard. Pushed to all four live devs with the specific tests named
per lane (ConTags pins + spawn-count red-line; the `Member` negative test;
fail-on-any-bad-target + per-target asks + COMPARED_FLOOR as a number).

Third instance this wave of a label not establishing what its name
implies — mislabeled timing rows, a guard outside its gate set, an
aggregate standing in for a specific test. Treated as this wave's
characteristic failure, not three coincidences.

### 2c. The ConTags SUPPLIER attribution was also wrong (upstream, corrected)

Recorded so this lane does not repeat it: the four non-`Val` scaffolding
constructors do NOT arrive via `collectDataCons` (the unfiltered
home-TyCon sweep). freer-simple is not vendored under `haskell/`, so its
TyCons can never be in a home module's `mg_tcs`. The real supplier is
`collectTransitiveDCons` — the binder-TYPE closure (`Translate.hs`
~1094–1129) — reaching `Eff` → `Val`/`E` → `E`'s field types
`Union effs b` / `FTCQueue (Eff effs) b a`. All five come from the TYPE,
reachability-independent. Corrected upstream at `6df9482b`.

Nothing in this lane changes: the pins are correct under either story, and
the verdict that cleared wave 2 rested on the SEED ITSELF BEING PURE, not
on where table entries come from.

The lesson, passed to every dev as a reporting standard: **a correct
conclusion can travel with a wrong mechanism, and the wrong mechanism is
the dangerous half — it aims the fix at the wrong object.** A dev
following the original note would have preserved `tyconMeta`, replaced
`transitiveMeta`, and shipped the exact break the warning existed to
prevent, while believing they had complied. Devs are instructed to report
the mechanism they actually TRACED, with file and line, and to say so when
it contradicts their spec.

### 2a. That audit turned up a live cross-lane hazard, now routed

Following the mechanism through produced a finding this sub-TL did not
set out to make, recorded by the wave TL at `df458ebc` and routed to
sub-TL `spawn-latency`:

`ConTags` needs all FIVE scaffolding constructors, but only `Val` is
reachable in a pure entry term. `E`, `Union`, `Leaf`, `Node` are in the
table today **only** because `collectDataCons` sweeps every home-module
TyCon with no reachability filter — they are not in `wiredInDataCons`
(`Translate.hs` ~2737). **D2 replaces exactly that sweep** with a
reachability-derived closure, and as originally specified would have
stripped four of the five and broken the boot path this wave is building.
D2 now carries a correctness requirement to keep the five as mandatory
roots, with its own pin on a pure entry term.

The general shape, worth carrying forward: **the current table's
over-collection is load-bearing in undocumented places.** This boot path
depended on it and nobody had written that down.

**Placement requirement (wave TL, binding).** `boot-lazy`'s test (2) — a
loop fragment with real effect sites running on the render-booted machine
and suspending at its hole — is the regression guard that catches D2
getting this wrong. It therefore MUST live in a
`tidepool-harness/tests/acceptance_*.rs` binary, so
`scripts/battery-shard.sh tidepool-harness -E 'binary(/^acceptance_/)'`
selects it — that shard is in D2's mandatory gate set. A guard sitting
outside the other lane's gates catches nothing. The test's doc comment
must say what it guards and why, so a later reader does not delete it as
redundant.

3. **Item 0b's landing is scoped to the mechanism + RunLLMTurn.** The
   vocabulary/row split is the mechanism; the policy for this landing is
   `row ∪ {RunLLMTurn}` at every call site. Deliberate, not a shortcut:
   `Tidepool.Effects` compiles as a home module whose every TyCon feeds
   the extractor's metadata collection, so dumping ~14 unused GADTs into
   a narrow-row compile would be a direct latency regression against this
   wave's own target — and the narrow-row compiles are precisely the
   pre-model ones. Helpers still spelled `-> M x` stay row-gated;
   `boot-vocab` reports which converted and which held out, and the list
   lands below at fold.

---

## Post-async anchor verification (done during the wave-2 hold)

Read-only, via `git show root.harness-lifecycle:<path>` — no checkout, no
worktree contact. Anchors in `00-spec.md` (driver.rs ~552, harness.rs
~430) HAVE moved; `01-wave2-lazy-boot.md` and `02-wave3-one-compile.md`
carry the re-derived ones.

**What the async conversion did NOT change** — checked because the wave
TL asked whether the first REAL compile moves on either path, since that
is the exact hook `bootstrap_if_needed` hangs off:

- `force()` still bootstraps from `self.boot` at the same point in the
  same sync function (817).
- `run_one_cycle` (now `async`, 674) still calls `bootstrap()` and then
  `render_framing`, so the outer session's first real compile is still
  the pre-loop render.
- `compile_outer` (631), `render_framing` (1551) and
  `run_loop_fragment_inner` (981) are byte-identical to pre-async apart
  from `async` on their callers.

**The hook does not move.** The recipe stands as written; only line
numbers change. Wave 3's fusion also survives: post-async `run_one_cycle`
still renders and loops against the SAME `prior_state`, which is what
makes one module with two targets correct.

One new obligation the async rewrite adds: `harness.rs` 3251/3294
fabricate the `boot` field in test fixtures and 3320 `fake_session`
bootstraps eagerly so `NodeTree::force` has a machine to register.
Deleting the field lands on those three sites; `boot-lazy`'s spec names
them.

---

## Item rows

### Item 0 — one-compile bootstrap (Track 1)

- **Fix-ladder rung:** rung 1 (Eliminate). No downgrade taken; none
  contemplated. Root ruled the rung-2 cache interim explicitly OFF.
- **Blocker (if any downgrade to rung 2):** _none._

#### `boot-count` — FOLDED (branch `…boot.boot-count` @ `7cd52183`)

The live-shaped receipt, landed FIRST so item 0's win is an A/B against a
committed red line rather than an after-the-fact assertion.

- `EXTRACT_SPAWNS` process-global counter in
  `tidepool-harness/src/compile.rs` (incremented at `compile_turn`'s
  `cmd.output()`), `extract_spawn_count()` / `reset_extract_spawn_count()`.
- `tidepool-harness/tests/acceptance_boot_compile_count.rs` as its OWN
  test binary, asserting against `PRE_MODEL_EXTRACT_COMPILES` — verified
  present at `= 4` post-fold, with a failure message naming the wave's win
  condition so a future dev knows whether it got better or worse.
- **Spawn-site survey** (the part that makes the number honest): only
  `compile.rs::compile_turn` is reachable before the first model call —
  four spawns, being `Harness::new`'s boot seed,
  `SelfHarnessDriver::bootstrap`'s boot seed, `compile_outer(render)`, and
  `compile_outer(loop)`. `tidepool-runtime`'s `run_turn` /
  `classify_block` / `compile_session_turn` and `compile_haskell_salted`
  (the MCP eval path) are post-model-only or never reached by the driver.

**Receipts (named-guard rule honoured):**

| leg | result |
|---|---|
| `acceptance_boot_compile_count` (own pass line) | PASS, 1/1, twice — pre-rebase 115.4s and post-rebase re-verify 65.5s |
| **measured pre-model extract-spawn count** | **4**, both runs |
| harness acceptance aggregate (11 other `acceptance_*`) | 25 passed, 0 failed, 0 skipped |
| tier-1 nextest (pure Rust) | 1862 passed, 9 skipped, 0 failed |
| check / clippy / fmt | clean before and after both rebases |
| transfer proof | tested @ HEAD `7cd52183` |

**The D7 figure is now MEASURED, not inherited.** Four was the live-dogfood
observation the plan carried; it is now an instrument reading, and item 0's
drop is checkable against it.

Declared deviations, both accepted: an unrelated nextest-cap cherry-pick
taken mid-session under a do-now directive (later absorbed by the parent's
own copy at rebase); and skipping a full aggregate re-sweep after rebases
per the batch-not-re-sweep guidance, having re-verified the affected test
standalone.

**Instrument note, carried as doctrine:** this counter counts spawns **at
the source, inside the code path**, which is why its number is trustworthy
in a week when three separate external-observation instruments were each
wrong in a different direction. An in-code counter establishes its
referent; an external pattern match asserts one.

### Item 0b — nameable effect vocabulary — LANDED, UNVERIFIED

**Decision:** mechanism + `RunLLMTurn`, exactly as scoped. `EffectDecl`
gains `helpers_row_polymorphic`; `effects_module_source_with_vocab(row,
vocab, row_args)` holds the body and `effects_module_source_at` delegates;
`emits_helpers_for(eff, row_effects)` is the single-sourced emission gate,
**`pub(crate)`**.

**Central constraint HELD, established independently by the dev rather
than assumed:** `agent_decls()` / `answerer_decls()` / `outer_decls()` /
`standard_decls()` are all **unchanged**. `RunLLMTurn` reaches the
vocabulary only via `vocab_with_runllmturn` (`engine.rs:684`), **never the
row**. Item 0b widened what is NAMEABLE and did not widen what is IN THE
ROW.

**Acceptance test exists and asserts BOTH directions** —
`run_llm_turn_is_a_member_error_not_a_scope_error_in_the_answerer_stack`:
`Member` + `RunLLMTurn` present, `not in scope` / `variable not in scope`
absent. A negative test asserting only "compilation failed" passes for the
wrong reason; this one cannot.

**Receipts — HONEST STATUS:**

| leg | result |
|---|---|
| `cargo check` / clippy / fmt | PASS (predecessor, pre-stop) |
| tier-1 nextest | PASS (predecessor, pre-stop) |
| `tidepool-mcp` shard | PASS modulo 3 inherited `Fork` reds (see below) |
| `agent_stack_scoping` | PASS; `Member` test inspected by name |
| extract-fidelity | **NEVER RAN** |
| harness acceptance | **NEVER RAN** |
| `tidepool-runtime` | **NEVER RAN** |

The three `tidepool-mcp` reds are the **`Fork` divergence**, verified by
this TL directly rather than accepted through the inheritance chain:
`tidepool-mcp/src/lib.rs:877` asserts the standard row **omitting
`Fork`**, while `fork_decl()` is in `standard_decls()`
(`effect_decls.rs:261`). A hand-written copy of a production list gone
stale — the same class item 0b's own predicate exists to prevent, one file
over. This was the **fourth** site found; the wave TL's sweep then found
**five across two crates** (ledger item 17 for the central pass).

### Item 0 prerequisite — `--targets` — LANDED, PARTIALLY VERIFIED

Multi-target emission from one GHC session: `Main.hs` split into
`translateTargetClosed` / `writeClosedTargets` / `runMultiTargetClosed`
with a `--targets` flag; `compile_turns` + thin `compile_turn` wrapper
(`compile.rs` 149, 198).

**The riskiest property is CONFIRMED, and by reading rather than by test**
— strict mode is structurally unable to reach `--all-closed`'s
skip-on-failure path, landed as the stronger of the two acceptable shapes:

- `translateTargetClosed` (`Main.hs` 356–367) has **no try/catch at all**;
  it propagates unconditionally.
- `runMultiTargetClosed` (540–546) calls it directly inside a bare `forM`
  — no per-target `try`.
- `--all-closed`'s skip lives entirely in `processFile`'s own `(_, True)`
  arm (200–281), calling a **different** function (`translateModuleClosed`)
  with its own local `try`, and never touching `translateTargetClosed`.

**SHAPE LANDED: SEPARATE FUNCTION.** Strict mode does not decline to skip;
there is no skip in its call graph to decline. The `if strict then error
else skip` regression cannot be produced by a refactor that "removes
duplication", because there is no duplicated branch to merge.

Single-target contract unchanged: `writeWholeModuleClosed`'s signature and
all three call sites (`Main.hs` 292, 587, 695) untouched.

**Receipts — HONEST STATUS:**

| leg | result |
|---|---|
| `cabal build tidepool-extract-bin` | **PASS** — linked clean |
| `cargo check --workspace` | **PASS** |
| `cargo fmt --all -- --check` | **PASS** |
| `cargo clippy --workspace` | **NO VERDICT** — killed mid-run by the stop |
| tier-1 nextest | never started |
| extract-fidelity | **NEVER RAN** |
| harness acceptance (both named pins) | **NEVER RAN** — pins confirmed to EXIST by name (`acceptance_multi_target.rs` 74, 129), never executed |
| `tidepool-runtime` | **NEVER RAN** |
| hardened differential | **NEVER RAN — COMPARED_FLOOR NEVER MEASURED** |
| `corpus_report` | **NEVER RAN** |

**No figure exists for COMPARED_FLOOR or extract-fidelity N/N from this
branch.** The dev also declined to claim the three inherited reds as
re-verified live, carrying them explicitly as inherited from `ef9ac291`'s
message and this ledger rather than laundering them as its own
observation.

Found while reading, **reported not fixed** (verification-only boundary):
unused imports at `Main.hs` 22, 25, 26, 44; `result` at 231 shadowing 171
(pre-existing, confirmed at HEAD).

---

## Throttle-era scheduling decisions (2026-08-09, box hit load 92)

Root's throttle directive (broker-wrap EVERY heavy invocation including
quick-tier `nextest`) relayed to all four devs immediately, each with the
parts biting its own lane. Two scheduling decisions follow, both approved
by the wave TL:

1. **Expensive shared legs run ONCE, post-fold, on the composed branch** —
   not per-dev. The cost argument is secondary; the real one is that a
   hardened-differential green on one child's branch does not establish
   the COMPOSED branch is green, and the composed branch is what folds
   upward. Two expensive runs proving less than one is the wrong trade
   twice over.

   **Exception, deliberate: `boot-targets` keeps the differential,
   `corpus_report`, and extract-fidelity on its own branch.** Its diff
   changes the extractor's emission path, and `haskell/app/Main.hs` is
   shared with sub-TL `spawn-latency` — an extraction regression
   discovered after composition could not be attributed between two lanes
   touching the same file. `boot-lazy` (Rust session lifecycle) is
   relieved of them; `boot-targets` is not.

   **RATIFIED as standing wave policy** (wave TL): expensive legs run once
   post-fold on the composed branch, EXCEPT where a lane touches a file
   another wave also touches — there they run on the lane's own branch
   first, for attributability. Today that is `haskell/app/Main.hs` and
   nothing else. If another such file appears, apply the same test without
   asking.

   The cost argument that settles it: debugging a composed regression
   across two waves costs more slots than the run spent to prevent it.
   Unattributable is expensive in a way one extra differential is not.

   The part that makes the asymmetry SAFE is telling the affected dev
   explicitly that the relief does not extend to it, rather than letting
   it infer from a sibling's instructions. **A rule that has to be
   inferred is a rule that gets inferred wrong.**

2. **No fifth concurrent child until at least two of four have folded** —
   held even if slots free up. Four live devs plus wave 3 is the
   amplification that produced load 92.

**Wave 3 slips; scope NOT cut** (wave TL's ruling). Wave 3 is the
render+loop fusion — the step taking item 0 from two pre-model compiles to
one. Cutting it to recover schedule would leave the headline result half
delivered while keeping all of the cost, and the throttle is a transient
condition being actively worked, not a new baseline. No date estimated
while slot waits are running 25 minutes; "unknown, gated on two folds" is
the honest carry.

### Load measurement — both obvious instruments are broken

Recorded because a wrong reading here licenses exactly the behaviour the
throttle forbids:

- `pgrep -fc tidepool-extract` OVER-counts ~5x (22–24 against 4 real
  compiles) — it matches every agent shell / `bash` / `zsh` / `timeout`
  carrying `TIDEPOOL_EXTRACT=…`, so it tracks how many AGENTS exist.
- Matching `comm` against the full binary name UNDER-counts to a constant
  ZERO — `comm` is capped at 15 chars for userspace processes, so
  `tidepool-extract-bin` shows as `tidepool-extrac` and the full-name grep
  can never match. (Corrected at `480bc367`: "Linux truncates comm to 15"
  was too general — the >15-char values that exist are kernel threads. The
  anchored pattern below is right either way, so no command changed.)

Correct: `ps -eo comm= | grep -c '^tidepool-extrac'` and
`cat /proc/loadavg`. **A zero from a mistyped pattern reads exactly like a
quiet box** — it looks like permission to proceed, not like an error.

### The unsatisfiable triple — reported, fixed, and CONFIRMED by one observation

**The report.** A broker-wrapped command that must queue longer than the
environment's ~380s process kill can NEVER complete, however compliant the
dev is. `ghc-slots.sh run` has no timeout — it loops forever (sweep, then
rotating kernel-blocked waits). So a queued dev is not being *refused* a
slot; it is being *killed while queued*. "Wrap everything" + "~380s kill"
+ "3 slots across three waves" is unsatisfiable under contention, and it
cannot be fixed at the lane level.

Evidence was DEMONSTRATED, not inferred: `boot-targets` died twice at
~340s while correctly queued, having refused to bypass both times.

**Three mechanism fixes followed within the hour:** `ghc-slots.sh detach`
(root's `bf3026af` — `setsid`, returns instantly, the detached child is
`run` itself so it holds and releases the `flock`: full slot discipline,
not a bypass); the per-run `ghc-heavy` cap 3→1 (`fc3363dc`); and the slot
count 3→4 (`45b93ee1`), gated on lanes confirming the cap.

**The receipt, and it is a single observation validating all three
together:** `boot-targets` went from two attempts dying at ~340s to
acquired-and-complete on the FIRST attempt afterwards, well inside its
45-minute priority window. `cabal build tidepool-extract-bin` linked
clean.

The arithmetic that was actually wrong: the box-wide extract ceiling is
`slots × per-run cap`. It was 3×3 = **9**, which is how three fully
compliant lanes produced seven concurrent extracts. It is now 4×1 = **4**
— a net reduction WITH more lanes progressing concurrently.

**Recorded as the argument for the no-bypass rule**, since it is rare to
get it this cleanly: the dev refused to bypass twice, got nothing visible
for it both times, and reported anyway. Bypassing would have unstuck one
worktree and left the mechanism broken for every lane, with nobody knowing
why the box kept falling over.

### The adoption sweep: FOUR questions, and three ways a leg fakes "done"

Run as a receipt with **ack by name**, not a broadcast — root endorsed
making it formal, since the receipts rule applies to adoption claims like
everything else. One question is not enough: a lane that adopted `detach`
and then ran five legs concurrently is WORSE for the queue than one that
never adopted it, and a lane on a stale copy is worse than both.

1. long legs launched via `detach`?
2. at most ONE brokered leg at a time?
3. absolute parent path `/home/inanna/dev/tidepool/scripts/ghc-slots.sh`,
   never a copy?
4. **does the log show the work actually STARTING** — not merely that the
   command returned?

Q4 was added after a leg exited in milliseconds on a missing-GHC PATH
error: no slot time, no failing test, and a log a hurried reader takes for
done. **A lane can answer yes to 1–3 and be running nothing.**

Q4 then found a THIRD variant nobody anticipated:

| variant | appearance | reality |
|---|---|---|
| never started | instant exit, near-empty log | ran nothing |
| queued, then killed | log shows only "all slots busy" | ran nothing |
| **fail-fast truncated** | **real PASS lines, names, timings** | **198 of 877 — 23% coverage** |

The first two are visible in the log. **The third looks exactly like a
completed run** and answers yes to all four questions; the only tell is the
completed count against the crate total. `nextest` fail-fasts by default,
so ANY lane hitting an inherited red gets a truncated run that reads as a
receipt.

**Consequent receipt rule for this lane: report coverage DENOMINATORS, not
bare counts** — `N passed / M total`. `198 passed` and `198 passed of 877`
are the same number and completely different claims. The wave's subject,
found in the test runner's default behaviour.

This also raised the priority of the `Fork`/mock divergence below from
"route it" to "it is silently truncating other waves' verification while
it remains unfixed".

### The `Fork` / `EFFECT_NAMES` divergence — a SECOND occurrence

`mock_stack_lockstep::mock_stack_matches_production` fails box-wide:
production `standard_decls()` carries `Fork` (`effect_defs.rs:955`), the
hand-written 11-entry `EFFECT_NAMES`
(`tidepool-testing/src/eval_harness.rs:403`) ends at `RunLLMTurn`.

Established INHERITED by `boot-lazy` in the strong form: same worktree,
diff stashed vs restored, back-to-back, **byte-identical output on both
legs** — showing the assertion's *inputs* are unchanged by the diff, not
merely that both legs fail. Those are different claims and only the first
settles inheritance. It also reasoned that no Haskell-cache confound axis
exists (the test is a pure-Rust list comparison per its own doc comment)
rather than reciting the caveat — the rule applied instead of performed.

**The list's own doc comment (`eval_harness.rs:387`) records a PRIOR
drift** — it "CAN drift… that's exactly what happened when the SG effect
was cut and Lsp/Time were added — see `f1a480e6`". So the comment predicted
its own recurrence, which is the argument that the fix is not "update the
list" but **stop hand-maintaining it**: derive `EFFECT_NAMES` from
`standard_decls()`.

Same class `boot-vocab` is single-sourcing `emits_helpers_for` against, one
file over: a hand-maintained duplicate of a production list, kept in sync
by convention, diverging silently the moment production changed. Routed
out of this lane; `boot-lazy` correctly told not to fix it (out of spec,
and the decision is not its to take).

### All three of today's structural defects closed AT SOURCE

All three were found from inside this subtree, by devs checking what a
thing does rather than reporting what it is supposed to do:

| defect | found by | fix |
|---|---|---|
| battery scripts self-acquire via `$PWD` → mandated tier runs a STALE broker | `boot-count`, corroborated independently by `boot-vocab` | parent broker at absolute path, 6 slots, markers |
| a wrapped leg queued past the ~380s kill can NEVER complete | `boot-targets` (died twice, refused to bypass) | `detach` subcommand |
| nextest fail-fast silently truncates a leg to a fraction of a crate | `boot-lazy` (198/877, refused to bank it) | `--no-fail-fast` in both battery tiers (`7d57cea5`) |

**Interim, because `$PWD` means a dev runs its OWN copy:** pass
`--no-fail-fast` EXPLICITLY as an extra battery arg rather than waiting for
the fix to propagate or starting a cherry-pick treadmill. Explicit beats
propagation; it works either way and stops being needed at the next tip.

### The one-red count — a cheap inheritance test WITH AN EXPIRY

Root's box-wide advisory: **exactly ONE** expected inherited red,
`mock_stack_matches_production`. That converts "is this inherited?" from an
argument into a **count** — see two reds, and the second is yours, no
cache-consistent A/B required.

**It expires** when the mock fix folds (two commits: `Fork` added as the
immediate unblock, then `EFFECT_NAMES` DERIVED from `standard_decls()`,
dependency direction left to engineering rather than assumed). Recorded
with the expiry attached because a shortcut whose precondition has silently
lapsed is this wave's characteristic failure in its purest form — someone
citing "only one expected red" a week after the count changed.

### A guard that never executes; and three inherited reds

`boot-targets`' `--ignore-default-filter` run of `tidepool-runtime` surfaced
**three** reds, all established inherited by MECHANISM READ (zero GHC), not
by diff-overlap:

1. `mock_stack_matches_production` — `Fork` in `standard_decls()`, absent
   from the hand-written mock list.
2. `sum_type_rejected_at_compile_time` — the test derives **`FromJSON`**;
   the non-nullary-sum `TypeError` exists **only on the `ToJSON` side**.
   Verified in this tree: `grep -c TypeError FromJSON.hs → 0`,
   `Value.hs → 4`; `FromJSON.hs:172` routes `GFromJSONSum 'False` to
   `GFromJSONTaggedSum`, a working TaggedObject decoder.
3. `qq_fmt_brace_inside_hole_non_string_expr_still_works` — `toExp`'s
   `Let` case is commented out; `let … in …` inside a QQ hole has never
   been implemented. File byte-identical at `HEAD~1`.

**The durable finding: a guarantee whose enforcing test lives in a tier
nobody runs is not enforced.** `tidepool-runtime` is excluded by nextest's
`default-filter`, so #2 sat latent since 2026-08-07. Same family as the
three-variant table one level out — not a receipt that overstates, but a
**guard that never executes at all**.

**The substantive gap behind #2, routed out:** a non-nullary sum deriving
`FromJSON` yields a decoder that fails at RUNTIME (key-not-present) rather
than being rejected at compile time. The fix is to IMPLEMENT a `TypeError`
on `GFromJSONSum 'False`, or to decide such sums are supported-but-lossy
and retire the test — a design decision with a behaviour change, **not a
repair of something that broke.**

### Duplicated prose lies in its new home — the second edge of single-sourcing

`FromJSON.hs:153` says the `'False` branch goes "to a compile-time
rejection". `Value.hs:146` and `:208` say the same and are TRUE there.
The `FromJSON` copy is **not stale — it is INHERITED from a sibling module
where it was true**, carried across with the `IsNullarySum` type family
that the commit itself flags as "duplicated rather than shared".

**Duplicate a mechanism and you duplicate its explanation into a context
where the explanation lies.** So the argument for deriving rather than
restating is not only that the code drifts: the *prose travels with the
copy and stops being true*, while still reading as authoritative. Carried
into `boot-vocab`'s `emits_helpers_for` note.

### Convergence is evidence only when the paths are INDEPENDENT

Recorded as a qualification on the earlier convergence signal (L4 and this
TL independently reaching the `emits_helpers_for` extraction, treated as
evidence the structure was right).

On red #2 the wave TL and this TL converged on a "the `TypeError` is
deferred by type-family dispatch" story — and **both were wrong**. The
wave TL had grepped the commit diff for `TypeError` and matched

    error "unreachable: non-nullary sum FromJSON is a compile-time TypeError"

**a string literal inside an error message** — prose about the mechanism,
not the mechanism — and reported it as "I verified the FromJSON side
specifically". This TL had inferred deferral from the type-family shape
without reading the file.

Two parties reading the same misleading token is not corroboration. The
earlier convergence counted because the paths were genuinely independent
(one reasoning from a call site's requirements, one from a doctrine about
doc comments); this one counted for nothing. **Convergence is evidence
about the structure only when the routes to it do not share an input.**

Settled by a dev's mechanism read and this TL's direct grep, against two
TLs' agreement — which is the reason a dev is told to contradict its TL
when a stated mechanism does not match what it observes.

### We enforced derive-don't-restate on code while violating it on the RULES

The wave's most self-implicating finding, and therefore the one most likely
to be true of whatever we build next.

`OPERATIONAL.md`'s Block was pasted verbatim into every dev spec by
standing instruction. **A pasted Block freezes at spawn time, and the Block
changed ~10 times in one day** — throttle v1→v2, `detach`, the
one-brokered-leg envelope, coverage denominators, the holder rule, the
named-guard rule. Every spec pasted before an amendment carries the
staleness **invisibly**: nothing in it announces that the source moved.

The visible tip of it was the tier-1 self-contradiction (a dev copying the
Block got both "wrap every invocation including the quick tier" and "tier
1, safe unattended", the wrong one sounding more actionable). That was one
contradiction inside one file; every pasted copy carried the same defect
with no tell at all.

**N pasted copies is exactly the antipattern the same file bans two
sections down.** Third artifact today where duplication carried its own
justification into a context where it stopped being true — after
`EFFECT_NAMES` vs `standard_decls()`, and `FromJSON.hs:153`'s inherited
comment. This one is ours.

**Fix: read by REF, never `cat` the worktree copy** (wave TL, `b5e036cc`):

    git show root.extract-wave:plans/post-restart/extract-wave/OPERATIONAL.md

The first proposed fix was `cat` **in the dev's own worktree** — which
carries the same defect one level down: a dev's copy is frozen at FORK
time, earlier than spawn. Measured rather than argued, in this tree:

    diff <(git show root.extract-wave:…/OPERATIONAL.md) plans/…/OPERATIONAL.md → DIFFERS

Even this TL's copy was stale, one hop closer than any dev. A dev following
that step 0 literally would read **a stale copy of the fix for staleness**.

**`git show <ref>:<path>` is the absolute-parent-path equivalent for
docs.** `$PWD/scripts/ghc-slots.sh` silently gave 3 slots when the broker
had 6; a local `OPERATIONAL.md` silently gives yesterday's rules. Same
shape, different artifact. Secondary benefit: `git show` puts the text in
the dev's own transcript, so "did you read it" is answerable **from the
record** rather than on trust — strictly better than the paste on the very
axis the paste was defending.

**Corollary — a TL's own messages are frozen at send time too.** Every dev
was told: *if the canonical Block contradicts something I told you
directly, the Block wins, and tell me so I reconcile.* Without that, we
replace N pasted copies with N+1 authorities, the newest one hardest to
notice **because it arrives as conversation rather than as a document.**

### Doctrine does not exempt the doctrine's carrier

Recorded verbatim at the wave TL's request, because it is the sharpest
form of this wave's lesson and it was earned by this TL getting it wrong:

**A wrong mechanism riding in on a sweep finding is worse than one
arriving anywhere else — it inherits the sweep's trust unearned.**

The instance: this TL told a dev that `detach`'s `setsid` drops the
interactive environment, so a detached `cabal` comes up without GHC. That
was **inferred from what "new session" sounds like, never tested.**
`setsid nohup env` preserves marker variables and the full `PATH`, and
`detach` is literally `setsid nohup "$0" run -- "$@"` with no scrubbing.

The real cause: the launching shell never had GHC — a bare tool call has
`cabal` from the nix profile but no `ghc`, and shell state does not persist
between tool calls, so an earlier `nix develop` is gone.

**Why the wrong mechanism was actively harmful, not merely inaccurate:**
it implied `run` is safe and `detach` is risky. The identical failure hits
both, because the cause is the caller's environment. A dev acting on it
would reasonably have retreated to `run`, losing `detach`'s
survive-the-queue property while still hitting the bug.

This is the standing instruction given to all four devs — *report the
mechanism you actually traced, with file and line, not the one you expected
to find* — violated by the person who issued it. Devs were told explicitly
to contradict this TL when a stated mechanism does not match what they
observe; they are closer to the evidence.

### Two rules this lane generated (wave-wide, `a5f06b85`)

**1. Report the HOLDERS, not just the waiters.** A queue-depth number
names a symptom; the holder list names a cause. Method: read both
`lslocks` columns, and check holder AGES and COMMANDS — a slot held an
hour by an internally-serialised `--ignore-default-filter` run is a
different problem from four slots doing work.

Earned the hard way: this TL escalated a ~20-waiter starvation without
checking holders, and **two of the four box-wide slots were held by its
own child** (`boot-targets`), starving its sibling `boot-vocab` — whose
fold is another wave's critical path. The escalation would not have
survived the holder list. Seventh instance of the wave's characteristic
failure (a measurement establishing less than its name implies), and the
first one that was this TL's own.

**2. A mechanism adopted is not a mechanism bounded.** State the envelope
in the same breath as the capability. Concretely: at most ONE brokered leg
per dev at a time.

`detach` was handed down as the fix for dying while queued, without saying
it bounds *waiting*, not *parallelism*. A dev given a capability and no
envelope will reasonably infer the wrong one. This is the companion to
"a new mechanism announced is not a mechanism adopted" — and it changes
what a box-wide adoption sweep must ask, because **a lane that adopted
`detach` and then ran five legs concurrently is WORSE for the queue than
one that never adopted it.** The sweep needs both questions.

**Attribution, recorded precisely because that is this wave's discipline:**
the omission was SHARED. This TL under-specified (branch and timing, never
concurrency); the wave TL reviewed and ratified it, writing that
own-branch gating was worth "the extra run" — singular, as though one leg
— and did not say serialise either. A gap that survives review is a
different and more dangerous class than a gap one person made: the usual
defence already ran and missed it. Both facts were given to `boot-targets`
so it calibrates on the system's fallibility, not only its sub-TL's, and
was told explicitly to push back on a reviewed instruction that looks
costly from where it stands — it was the one positioned to see the holder
list.

**Corollary, from `boot-targets`' own judgment:** a running holder and a
queued waiter are DIFFERENT OBJECTS. A killed GHC leg wastes the slot time
already spent and returns the slot no sooner than draining does — so
drain, then hold. Kill only work that is known-void, never to free
capacity. Cancelling a queued waiter is free and does relieve pressure.

### Instruments: count at the SOURCE, not by external pattern match

Three external-observation instruments were wrong in three different
directions within one day:

| instrument | failure |
|---|---|
| `pgrep -fc tidepool-extract` | OVER-counts ~5x — matches shells carrying `TIDEPOOL_EXTRACT` in the environment |
| `grep tidepool-extract-bin` on `comm` | UNDER-counts to a constant ZERO — `comm` caps at 15 chars for userspace |
| `pgrep -f "ghc-slots.sh"` | OVER-counts — 22 agent sessions + 8 bash + 5 zsh read as waiters, against a kernel truth of 4 holders / 4 waiters |

Correct: `ps -eo comm= | grep -c '^tidepool-extrac'`,
`lslocks | grep tidepool-ghc` (WRITE = holder, WRITE\* = blocked waiter),
`cat /proc/loadavg`.

**The general form: an args-grep counts command lines, not processes doing
work** — and it fails in whichever direction is least convenient. A zero
from a mistyped pattern reads exactly like a quiet box: not like an error,
like permission to proceed.

**The lesson that generalises** (carried up as a swarm-wide suggestion):
`boot-count`'s `acceptance_boot_compile_count` counts extract spawns **at
the source, inside the code path**, which is why its number (4) is
trustworthy in a week when every external instrument was wrong. **An
in-code counter establishes its referent structurally; an external pattern
match only asserts one.** Hence: name the instrument alongside any number
in a receipt. That is the named-guard rule one level down — a measurement,
like a test name, must establish its referent rather than assert it.

### Sixth instance of the wave's characteristic failure

`OPERATIONAL.md`'s Block briefly contradicted itself: the new throttle
bullet required wrapping `nextest` at ANY tier while the tier list ~14
lines below still called tier 1 "(pure-Rust, safe unattended)". Devs copy
that Block verbatim, and the stale line was the more actionable-sounding
of the two. Reported across the namespace boundary rather than edited;
fixed at `6033a70b` by marking the phrase **WITHDRAWN** rather than
deleting it — so a dev who already copied the old text recognises what
changed instead of half-remembering a blessing. A visible retraction beats
a silent deletion for exactly the reason this wave keeps rediscovering.

---

## Item 0 landing note — draft (goes into the fold message)

**Why this item mattered beyond ~30s of launch latency.**

The wave's characteristic failure is a name asserting more than what was
established. It showed up four times while item 0 was merely being
*specified*: timing rows named for a phase they did not measure; a guard
named for a gate set it was not in; an aggregate offered as evidence for a
specific test; and a plan claiming machinery ("Phase B's multi-binder")
that did not exist. Twice more in the mechanism stories themselves — a
ConTags model and then a ConTags *supplier* attribution, each a correct
conclusion riding a wrong mechanism.

The thing item 0 deletes is the same failure in the codebase rather than
in the plans. A **boot seed** that seeds nothing: scaffolding whose name
outlived its justification, manufactured to fit an API slot that demanded
a program before one existed, and load-bearing for two full GHC compiles
per launch precisely because the name made it look intentional.

That the item which deletes this pattern kept generating fresh instances
of it *while being written* is not a curiosity. It says this seam is one
where names routinely outlive what licensed them — which is the argument
for keeping receipt discipline HIGH through the folds rather than relaxing
it once the interesting findings stop arriving. The findings stopping is
not evidence that the seam changed.

---

## Fold conflicts

### OWNERSHIP CHANGE: `effects_module_source_with_vocab` has an EXTERNAL consumer

Item 0b's restructure moved the whole body of `effects_module_source_at`
into `effects_module_source_with_vocab(row_effects, vocab_effects, row)`,
leaving the old name a one-line delegation (`boot-vocab`, `d6fce023`).
worktree-wave's **L4** is now a consumer of that function, adding a
conditional `hiding (error, (<|>))` to the generated import line when
`RepoEvent` is present.

**They found a real bug on first reading.** The hiding predicate must key
on `row_effects`, NOT `vocab_effects` — helper emission is gated at `:273`
on `in_row || eff.helpers_row_polymorphic`, and `RepoEvent` is not
row-polymorphic, so its `(<|>)` helper exists exactly when `RepoEvent` is
in the ROW. The superset assert at `:177` means the only constructible
mismatch is vocab-with / row-without, where the helper is NOT emitted —
so hiding there would hide the Prelude's `Alternative` while emitting no
replacement, leaving a name resolving to nothing.

**Be clear-eyed about the direction.** Making the vocab/row distinction
explicit is right — it is where 0b's acceptance lives — but it CREATED A
NEW WAY TO BE WRONG. Before, there was no `vocab_effects` to key on
mistakenly. Every consumer now faces a choice it did not previously have,
and the first external one got it wrong, caught only because another wave
happened to be reading. That is not an argument against the design; it is
an argument that the design owes its consumers more than it used to.

**Standing rule on this function, agreed by both waves:** a predicate
about what was EMITTED must DERIVE from the emission gate, never restate
it — so it cannot diverge if `RepoEvent` later becomes row-polymorphic,
which the vocabulary story makes plausible.

`boot-vocab` honours it structurally rather than in prose: the gate is
extracted as **`emits_helpers_for(eff, row_effects) -> bool`** (name
PUBLISHED and closed by this TL; L4 keys on it), visibility private or
`pub(crate)` — **never `pub`**, since an interface wider than its
consumers is the same failure one level out. Called at `:273` and by any
consumer, rather than documenting the rule and hoping. Same doctrine as
the strict-mode unreachability requirement in `boot-targets`' lane: **a
doc comment documents against a mistake; a shared predicate makes it
unavailable.**

`boot-vocab` owns the extraction PRE-FOLD (root's call): a trivial
refactor of code that dev wrote this week, in its own file, provable a
no-op by its own tests — and doing it here makes the cross-lane edit
DISAPPEAR rather than shrink. L4 then lands exactly one term.

**Why single-sourcing is not tidiness — root's framing, kept verbatim
because it is sharper than "they could drift":** two copies agree today
and **diverge silently the moment `RepoEvent` becomes
`helpers_row_polymorphic` — which is not hypothetical, it is the exact
state `boot-vocab`'s vocabulary-without-row story exists to enable.** The
failure is a wrong preamble with **NO BUILD ERROR**, in precisely the
mismatched (vocab-with / row-without) pairs item 0b's own tests construct.
A sync comment is not a weaker mechanism here; it is a mechanism that
fails SILENTLY in the one configuration this item was built to reach.

**Convergence, worth recording as evidence the structure is right:** L4
reasoned from "my hiding term must fire exactly when the helper loop
emits" and reached the extraction; this TL reasoned from "a doc comment
documents against a mistake, a shared predicate makes it unavailable" and
reached the same place. Two directions, one structure — not a preference
either side imported.

**Doctrine refinement from L4's gate set:** their
`vocab_only_repoevent_still_emits_its_gadt` pins that the discriminating
gate tests the HELPER condition specifically, so it cannot pass for the
wrong reason if the vocabulary split were broken outright. That is a guard
against **a test passing for a reason other than the one it is named
for** — the next refinement past "show the test by name". A named pass
line proves the test RAN; this proves it tested the THING. Same failure
family as this wave's characteristic one, caught a level deeper.

**Fold cadence:** `boot-vocab` folds PROMPTLY when 0b is green, not
batched behind its siblings — L4 needs the function on a shared base, and
cherry-picking `d6fce023` fails inspection (a WIP commit spanning seven
files across four crates). Nothing is blocked meanwhile: L4 verifies
against `d6fce023` in a disposable checkout. Prompt-when-green, NOT hurry
the work — verification is not cut to fold sooner.

**Known fold points, flagged before the fact:**

1. `tidepool-mcp/src/eval_prep.rs` — `harness-lifecycle` adds
   `template_haskell_anchored` and edits `template_haskell_impl` (+87);
   `boot-vocab` targets `effects_module_source_at`. Different functions,
   same file. Expect a MECHANICAL conflict when the new base merges in,
   not a semantic one. `boot-vocab` is instructed to keep its diff
   localized and not reformat surrounding code.
2. `tidepool-harness/src/harness.rs`, `selfharness/driver.rs` — replaced
   wholesale by the async rewrite. This is why waves 2+3 are held rather
   than conflict-resolved.
3. `haskell/app/Main.hs` — see below.

_No conflicts resolved yet._ Expected overlap with sub-TL `spawn-latency`:
`haskell/app/Main.hs` (its D1 reworks `writeWholeModuleClosed`'s metadata
merge ~line 348; our step 4 splits the same function's per-target emission)
and `tidepool-runtime/src/session/compile.rs` / `turn.rs`. Per the
realm-spike conflict experiment these are NOT pre-negotiated — minimal
localized diffs, log anything non-mechanical here at fold.
