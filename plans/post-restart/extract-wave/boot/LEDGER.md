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
_Shape landed: pending._

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

- **Fix-ladder rung:** rung 1 (Eliminate) attempted. _Not yet resolved._
- **Blocker (if any downgrade to rung 2):** _none recorded._
- **Receipts:** _pending._

### Item 0b — nameable effect vocabulary

- **Decision:** _pending._
- **Receipts:** _pending._

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
  ZERO — Linux truncates `comm` to 15 chars, so `tidepool-extract-bin`
  shows as `tidepool-extrac` and the full-name grep can never match.

Correct: `ps -eo comm= | grep -c '^tidepool-extrac'` and
`cat /proc/loadavg`. **A zero from a mistyped pattern reads exactly like a
quiet box** — it looks like permission to proceed, not like an error.

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
