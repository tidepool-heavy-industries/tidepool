# Sub-TL `boot` — spec

Parent: `extract-wave` (branch `root.extract-wave`). Read
`plans/post-restart/extract-wave.md` (the wave spec) and
`plans/post-restart/extract-wave/OPERATIONAL.md` FIRST. You own items **0**
and **0b**.

Your artifact namespace is `plans/post-restart/extract-wave/boot/**`. Keep a
`LEDGER.md` there: one row per item with the decision, the receipt counts, and
anything that conflicted at fold. Do NOT edit `plans/post-restart/extract-wave.md`
or `plans/README.md` — those are the wave TL's.

## Item 0 — one-compile bootstrap, Track 1

`plans/post-restart/one-compile-bootstrap.md` Track 1 is the CONFIRMED recipe.
Track 2 (the unified realm machine) is explicitly NOT yours and must not be
coupled to this work.

The measured motivation (live dogfood, 2026-08-08, clean cache): boot pays
FOUR extract compiles (~96s) before the first model call —

1. ~15s outer session boot seed (`tidepool-harness/src/selfharness/driver.rs:562`,
   `pure (toJSON (0 :: Int))`) purely to seed the RunLLMTurn-only stack's ConTags;
2. ~15s answerer boot seed (`tidepool-harness/src/harness.rs:430`, the SAME
   trivial compile for the answerer stack);
3. ~30s `compile_outer` of the render framing;
4. ~30s `compile_outer` of the loop body — only then does the first model turn fire.

The seeds are scaffolding become load-bearing. `ResidentSession::bootstrap`
(`tidepool-runtime/src/session/resident.rs:208`) is program-shaped at
construction, so at boot a fake program is manufactured to fit the API slot.
The lazy lifecycle it needs already exists underneath: `PersistentSession`
starts `machine: None` (`tidepool-runtime/src/session/persistent.rs:271`) and
the REPL already bootstraps from the first REAL compiled expression
(`tidepool-repl/src/session.rs` ~881).

### Recipe (Codex's, endorsed — implement in this order)

1. Unbootstrapped `ResidentSession` constructor around the already-lazy
   `PersistentSession`.
2. First real run compiles the machine entry directly (mirror the REPL).
3. DELETE both seed compiles (driver.rs:562, harness.rs:430).
4. Emit render + loop from ONE extract invocation. `compile_turn` is
   single-target today (`{target}.cbor`, `tidepool-harness/src/compile.rs`
   ~104).

   > **CORRECTED 2026-08-09** (wave TL, recorded at `a4642cba`; codex ledger
   > item 10). This step originally read "multi-target is Phase B's
   > multi-binder machinery, which is FOLDED and available to you (gate open
   > at a45fa843)". **That machinery does not exist.** Phase B DEFERRED the
   > `writeWholeModuleClosed` work to a successor
   > (`one-spawn-turn-protocol-phase-b.md:99`); its actual multi-binder work
   > is about tuple BINDERS on a session bind turn (`Main.hs` ~671–736), a
   > different thing from emitting several compile targets from one GHC
   > session. Verified in-tree: `writeWholeModuleClosed` takes a single
   > `targetName` (`Main.hs:333`), and the CLI has only `--target` (136) and
   > `--all-closed` (138).
   >
   > Multi-target emission is therefore a NEW prerequisite work item inside
   > item 0, on the `Main.hs` writer side — see `03-targets-prereq.md`. It
   > gates step 4 (wave 3) and nothing else.
5. Outer machine boots from render; loop lands as the second JIT function in
   the same machine.
6. Answerer session created lazily; its first model-written block boots its
   machine (no pre-model cost — that block is compiled anyway).

End state: exactly ONE GHC compile pre-model. That residual ~30s is D1/D2's
target, not yours.

### Fix ladder — the honest fallback

The wave spec's D7 fix ladder is, in order of principle:

1. **Eliminate** (the mechanism fix) — the recipe above. This is what you are
   trying to land.
2. **Cache** (interim, hour-sized): the seed source is CONSTANT per
   (decl-list, stack), so a content-addressed disk cache of the seeds'
   `(CBOR, table)` removes ~30s of every launch with zero GHC. Superseded
   harmlessly by 1.
3. One extract invocation for both seeds — strictly weaker than either; only
   if 1 hits a deep blocker.

**You may downgrade to rung 2 only with the blocker NAMED in your LEDGER and
reported to me.** "Eliminated, or explicitly downgraded to the cache interim
with the blocker named" is the wave's done-criterion — a silent downgrade is a
failure.

### Sequencing and the coordination obligation (READ THIS)

- Step 3 touches both boot sites, which live in mid-rewrite files. This is the
  D7 fix-ladder item and is sequenced **after driver-async's fold**. Confirm
  with me before a dev starts editing `driver.rs`.
- **The moment the boot-site work folds into your branch, tell me
  immediately** (`notify_parent`). The realm-build lane's step 4
  (`resident.rs` ~208, its ResidentSession conversion) is HELD on it, and root
  green-lights that lane the moment I report your fold. Do not batch this
  notice with other news; it gates another wave.
- Do NOT touch `resident.rs`'s pending / `ChildSuspended` machinery. Your
  business there is the constructor and the boot sites only.

### Constraint carried from the design

Keep SEPARATE source-level capability rows regardless of what you do to the
machine lifecycle. A common super-row would let answerer code import forbidden
verbs (`runLLMTurn`), weakening the compile-time boundary the row-scoping work
built. The two-rows fact is real either way (positional union tags per row);
only the per-launch GHC cost for constant artifacts is the defect.

## Item 0b — nameable effect vocabulary

**Effect vocabulary available in scope ≠ effects present in M's row.** Today
the answerer compile omits the `RunLLMTurn` GADT/helpers ENTIRELY — see the
module haddock of `haskell/lib/Tidepool/Harness.hs`, which spells out the three
contexts: the outer harness session (`tidepool_mcp::runllmturn_decl()`,
`effect_defs.rs` ~735), the answerer (`answerer_decls()`,
`selfharness/driver.rs:167`), and a general Agent turn (`agent_decls()`,
`tidepool-harness/src/engine.rs:661`).

**Goal:** make stable effect types/helpers **nameable in EVERY compile**, with
`Member` controlling **executability**. A name that is in scope but not in the
row must fail at the type level with a comprehensible `Member` error, not with
"not in scope".

This is the recorded prerequisite for the one-file harness
(`plans/self-iterating-harness/15-generic-surface-wave.md`). The one-file cut
itself is NOT yours — it lands only after this exists and typechecks the
answerer path. Your acceptance is: the vocabulary is nameable everywhere,
`Member` gates execution, and the answerer path typechecks.

Watch the interaction with item 0: widening what is *nameable* must not widen
what is *in the row* (see the capability-rows constraint above). A test that
pins "answerer code naming `runLLMTurn` fails to typecheck with a Member
error, and does not compile through" is the load-bearing acceptance here.

Coordinate with the generic-surface wave's extension-set work if you touch
pragma/extension lists — codex review item 6 records live drift across four
surfaces (`tidepool-mcp/src/preamble.rs:27`,
`tidepool-runtime/src/session/render.rs:267`, the binder parser profile,
`tidepool-mcp/src/eval_prep.rs:116`). Do not add a fifth list. If you need one,
escalate to me.

Also relevant (codex review item 2): the compile cache **cannot key GHC flags**.
If item 0b's vocabulary arrives via command-line flags rather than source
pragmas, it is cache-UNSAFE by construction — either inject pragmas into
rendered source, or the cache key gains a flags component first. Prefer
pragmas-in-source.

## Structure

Decompose into reviewed dev leaves (`spawn_dev`, model **sonnet**), gate your
own folds, then `submit_branch` to me. Parallelize: item 0's steps 1–3 and
item 0b are largely independent of step 4's multi-target emission.

## Gates

Every item passes the wave gates in `../OPERATIONAL.md`: hardened differential
(floors intact), `corpus_report`, `extract-fidelity-test` 26/26 [**STALE — see
below**], harness
acceptance. Item 0 additionally needs a live-shaped receipt that the pre-model
compile count actually dropped — a test or an instrumented run showing the
extract-spawn count from launch to first model call, not an argument that it
should have.

Receipts are per-binary pass/fail counts. Copy `../OPERATIONAL.md`'s Block
section verbatim into every dev spec.

---

## CORRECTION 2026-08-09 — the `26/26` figure above is STALE

`extract-fidelity-test` is **not** 26 checks. D1-A added four `D1Defense`
checks; the total is 30 and will move again. The figure is left visible
rather than deleted, so anyone holding a copy of this spec recognises what
changed.

**State the property, never the figure:** *every pre-existing check passes,
named guards appear by name, report the actual N/N.* Any number is context,
never a target.

Why this matters more than staleness — a hardcoded count converts the
denominator rule from a CHECK into a LOOKUP. The dev stops asking *"did
everything run?"* and starts asking *"does it match the spec?"* Those are
the same question **until something silently stops running**, which is the
only case either question exists for. So a stale count does not merely go
out of date: it **disables the rule that would have caught it going out of
date**, and fails in the direction that looks correct.

Worst on items that ADD tests — every item in this lane does. `boot-lazy`
adds ConTags pins, `boot-targets` adds two multi-target pins, `boot-vocab`
adds the `Member` negative test. Each moves its own total *because of its
own work*; and if a check silently stopped running while the total landed
back on a spec'd number, the truncated run would match the doc exactly.

Contrast with `boot-count`'s `PRE_MODEL_EXTRACT_COMPILES = 4`, which is
sound: it asserts a **property in code** that must change by a known
amount, with a failure message naming the win condition — not a figure in
prose for a human to reconcile against. The same number is correct in one
place and a trap in the other.
