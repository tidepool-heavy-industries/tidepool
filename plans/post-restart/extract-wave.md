# Spec: extract-side latency wave TL

**GATE OPEN (2026-08-08): Phase B is FOLDED at a45fa843** — single
ownership of writeWholeModuleClosed is yours. Detail in
`plans/one-spawn-turn-protocol-phase-b.md`.

**WIRE BREAK IN EFFECT (from Phase B):** `--emit-stmt-binders`/
`--emit-binders` no longer exist; the Rust side requires an extract
supporting `--classify`. The DEPLOYED server/extract pair on this box is
still the old consistent pair and dogfood is PAUSED — do not run
`scripts/redeploy.sh` yourself; root owns the redeploy at dogfood
resume. Your test runs build the repo extract fresh (battery scripts),
so this does not affect your lanes. Stale-skew now fails LOUD as
VersionSkew naming the flag (phase-b pinned it against the old binary's
exact output) — if you ever see a `parse error on input '<-'` flavored
failure in a session context, that diagnosis path is already fixed;
suspect something else.

Owns the extract-side half of the latency program: the metadata
over-collection chain's root plus the declaration-path cluster.

## Structure (Inanna, 2026-08-08): one TL, TWO SUB-TLs, each with devs

Fork two sub-TLs rather than running one flat dev pool:

- **sub-TL `boot`** — item 0 (one-compile bootstrap Track 1) + item 0b
  (nameable effect vocabulary). Rust-side session/boot territory plus the
  Haskell decl-list surface.
- **sub-TL `spawn-latency`** — the D/C/E chain below, re-aimed by the
  Phase-B measurement (core-phase-dominant), through the pivotal
  persistent-extractor decision. haskell/ extractor territory.

Each sub-TL decomposes into reviewed dev leaves and gates its own folds;
this TL folds sub-TL branches and owns the composed gate + the
one-format-wire redeploy coordination. Per the realm-spike conflict
experiment: do NOT pre-partition files between the sub-TLs — allocate
shared-artifact namespaces (plan numbering, ledger files) up front, and
log any real conflict at fold.

Coordination point with the realm-build lane (running in parallel):
`resident.rs:208` — the realm build's ResidentSession conversion (its
step 4) is HELD until this wave's boot-site work lands; everything else
in both lanes proceeds concurrently.

## Item 0 (FIRST): one-compile bootstrap, Track 1

`plans/post-restart/one-compile-bootstrap.md` Track 1 is the confirmed
recipe — unbootstrapped `ResidentSession` constructor over the
already-lazy `PersistentSession`, first real run boots the machine
(mirror the REPL), DELETE both boot seeds, render+loop in one extract
invocation, answerer boots from the model's first block.

> **PREMISE CORRECTED 2026-08-08** (codex review item 10; verified). This
> entry previously said render+loop "needs Phase B's multi-binder". **No such
> machinery exists** — Phase B deferred the `writeWholeModuleClosed` work to a
> successor and the writer still takes ONE `targetName` (`Main.hs:333`; CLI has
> `--target`/`--all-closed`, no `--targets`). Building a STRICT explicit-target
> mode by adapting the `--all-closed` loop (`Main.hs:185`) is a PREREQUISITE
> work item inside item 0. It must fail if EITHER target fails and must
> preserve per-target asks/warnings. Full detail in
> `one-compile-bootstrap.md`'s step 4.
>
> **Sequencing (extract-wave TL's call, reported to root):** the hard
> dependency is step 4's, not steps 1–3's. Seed deletion does not consume
> multi-target, so wave 2 (`boot-lazy`, in flight) continues; `--targets`
> lands as its own item before wave 3 (`boot-onecompile`), which cannot start
> without it. Supersedes D7's
cache-interim (step 2 of the fix ladder below) if it lands first.
Keep separate source-level capability rows regardless.

## Item 0b: nameable effect vocabulary (one-file-harness prerequisite)

Effect vocabulary available in scope ≠ effects present in M's row. Today
the answerer compile omits the `RunLLMTurn` GADT/helpers entirely
(`haskell/lib/Tidepool/Harness.hs` module haddock). Make stable effect
types/helpers nameable in EVERY compile, with `Member` controlling
executability — this is the recorded prerequisite for the one-file
harness (see `plans/self-iterating-harness/15-generic-surface-wave.md`);
the one-file cut itself lands only after this exists and typechecks the
answerer path.

## Coexistence note (Inanna, 2026-08-08)

The realm-machine spike (`realm-spike.md`) runs as a PARALLEL lane
deliberately not partitioned away from this lane's runtime files — it is
an experiment in merge-conflict cost. Do not pre-negotiate file
boundaries with it; resolve conflicts at fold and log anything
non-mechanical.

## The measured motivation (jit-chain's experiment, 2026-08-09, pre-wrap on
## real session Core — size all absolute wins from THESE figures)

- turn 1: table=164 constructors, fragment-reachable=24 → 6.8:1 (~15%)
- turn 2: table=166, reachable=15 → 11.1:1 (~9%) — worsens as the table grows

Phase-B measurement (2026-08-08, `11-turn-latency-contract.md`) RE-AIMS the
spawn items: 60–66% of a turn's extract spawn is GHC's `core` phase,
26–32% session boot, under 6% typecheck. The standing home-module
typecheck suspicion is REFUTED — promote E6 (tiered -O2) and D1 (double
translation of reachable Core), and read C1's double-compile suspicion
against this breakdown before spending on it. Spawn count is no longer
where the time is.
- downstream: turn 1 emits 232 Cranelift funcs / 13,348 blocks for a
  24-constructor fragment
- CAVEAT (carry with the number): "single-digit-to-low-teens percent
  reachable" — the table is 164–166, NOT "hundreds vs dozens".

### CORRECTION to the Phase-B re-aim (extract-wave TL, 2026-08-08)

From spawn-latency's code audit, verified independently against
`GhcPipeline.hs` on the merged tree. Two of the three clauses above are
**unsound as stated**. What the rows actually bracket:

- `sessionT0` ~117, `load' Nothing LoadAllTargets` ~174, `sessionT1` +
  `emitPhase "ghc_session"` ~180/181. **`load'` is INSIDE the `ghc_session`
  bracket**, and it parses, typechecks AND core2cores every home module.
- The `typecheck` (~219) and `core` (~220) rows sum only the SECOND loop
  (~191–216), whose own comment says it plainly: "each summary's own
  `parseModule`/`typecheckModule` redoes its typecheck independently of
  `load'`".

Therefore:

1. **"26–32% session boot" is not boot.** It is setup + depanal + a full first
   compile of every home module. Half the double compile has been sitting in a
   row labelled "session boot" the whole time.
2. **The typecheck refutation does not follow.** "Under 6% typecheck" measures
   ONE of the TWO typechecks; the other is inside `ghc_session`. The standing
   home-module typecheck suspicion is NOT refuted by this evidence — it is
   unmeasured. Treat it as OPEN.
3. **C1 is upgraded** from "read skeptically against the breakdown" to
   confirmed-in-code; only its SIZE is unknown. The breakdown did not refute
   C1, it partly concealed it.
4. E6 and D1 stay promoted. 60–66% in the second loop's core2core ALONE is
   ample warrant, and nothing here weakens it.

**Bracket design consequence:** `load'` needs no internal decomposition. Its
ENTIRE cost is the double compile's cost, because the second loop independently
redoes both halves. One new phase row around `load'` (leaving `ghc_session` =
setup + depanal) answers C1 directly.

### SCOPE CAVEAT on every Phase-B number (same audit)

### C1 DONE CRITERION: retire `11-turn-latency-contract.md` (Inanna, via root)

The doc is judged no-longer-useful — its numbers are turn-1-only and its rows
are mislabeled, both established by the audit below. It is NOT deleted out from
under this lane, because this lane cites it and C1 rewrites the very
instrumentation it describes. Instead **C1 retires it**, so the doc dies at the
moment its replacement exists and there is no window where the measurement
machinery is undocumented.

The SAME commit that fixes the phase brackets (rows mean what their names say;
the session path emits phases) must:

1. either replace the doc with a short current contract, or fold the contract
   statement into C1's receipts;
2. DELETE the stale file;
3. update every reference. The full list as of 2026-08-08 — note two are in
   CODE, not docs:
   - `plans/one-spawn-turn-protocol.md:28`
   - `plans/post-restart/extract-wave.md:81` (this file's own citation)
   - `plans/post-restart/extract-wave/spawn-latency/00-spec.md:16`
   - `tidepool-harness/examples/turn_latency_bench.rs:8`
   - `tidepool-harness/src/timing.rs:29`

`timing.rs:29` sits in the flat-stages passage that governs the `ghc_setup` /
`ghc_load` partition, so C1 is editing that file anyway — the reference update
lands naturally in the same change rather than as separate bookkeeping. Re-grep
before landing; this list is dated.

Until C1 lands, the doc STANDS, carrying root's correction header (03d33d1b) so
nobody cites it naively.

### SCOPE CAVEAT on every Phase-B number (same audit)

`runSessionPipeline` (`GhcPipeline.hs` ~326) emits **zero** `emitPhase` calls.
`runPipelineSession` (~106) routes there whenever `isSessionScopeActive`
(`Session.hs` ~173: true iff any `Val.G<n>` iface is injected). So the 60/26/6
split was measured **only on the non-session path** — turn-1-shaped extracts.
Turns 2+, which carry E2's O(n²) `Lib.Gn` chain and are the ones that compound
over a dogfood session, are UNMEASURED. Any lane citing the Phase-B breakdown
should read it as "turn 1", not "a turn"; the persistent-extractor decision
must not be taken on the normal-path numbers alone.

## Items (D/C/E numbering from the campaign; all green-lit on merit —
## correctness gates required, benchmarks optional)

- **D1** Reachable Core is translated TWICE: writeWholeModuleClosed's
  scanMeta = collectUsedDataCons re-runs the FULL translator per reachable
  RHS just to rediscover tsUsedDCs (Main.hs ~329, Translate.hs ~1027),
  discarding the IR. Fix: translateModule is the one authoritative producer
  (IR + used DCs + types + effect sites); defense-in-depth = cheap Core
  visitor asserting subset, never a second translation.
  **Codex review 2026-08-08 (see `codex-review-2026-08-08.md` item 7): the
  subset defense does NOT currently exist** — Main SILENTLY UNIONS the two
  translations' results (Main.hs ~348), and the runLLMTurn/fork rewrite
  makes the seeded/unseeded paths genuinely diverge, so disagreement is
  live. The D1 fix MUST ship a hard fail: walk emitted FlatNode
  constructor/data-alt IDs and fail extraction if output metadata omits
  any, plus an independent reachable-Core collector. Acceptance includes a
  mutation test: deleting one recordDC call must fail extraction, not
  produce output. Silent under-collection is the signature of the owed
  garbage-con_tag intermittent — treat this as correctness, not cleanup.
- **D2** Metadata = every constructor of every home-module TyCon, no
  reachability (mg_tcs → collectDataCons, Translate.hs ~2718). Fix:
  RuntimeTypeClosure from runtime-observable roots (built/matched cons in
  reachable Core; sibling sets where rendering needs them; target/result +
  boundary + session-bound types). THE chain root — shrinks the table,
  wrapper chain, and CBOR for free.
  **CROSS-LANE HAZARD (extract-wave TL, 2026-08-08 — D2 MUST handle this or it
  breaks boot).** `ConTags::try_from(&DataConTable)`
  (`tidepool-codegen/src/effect_machine.rs` ~203) requires ALL FIVE freer
  scaffolding constructors — `Control.Monad.Freer.Val`, `.E`,
  `Data.OpenUnion.Union`, `Data.FTCQueue.Leaf`, `.Node`
  (`tidepool-repr/src/freer_names.rs` ~23–43) — and `?`s out if any is
  missing. They are NOT in `wiredInDataCons` (`Translate.hs` ~2737, verified:
  list/bool/char/unit/numeric/tuple/ordering only). For a PURE machine-entry
  term — `pure (…)`, which is what BOTH the seed being deleted and item 0's
  render entry are — only `Val` is reachable; `E`/`Union`/`Leaf`/`Node` are
  not.
  **SUPPLIER CORRECTED (spawn-latency, verified by this TL) — the first
  attribution in this note was WRONG and dangerously so.** It said the five
  ride in on `tyconMeta = collectDataCons tycons`. They cannot:
  `tycons` is `mg_tcs` (a module's OWN TyCons) and the five live in the
  freer-simple PACKAGE (`Control.Monad.Freer`, `Data.OpenUnion`,
  `Data.FTCQueue`), which is NOT vendored under `haskell/` — verified, no
  such source in the tree. An external package's TyCons never enter a home
  module's `mg_tcs`, and `type M = Eff '[…]` defines a synonym, not a
  datacon-carrying TyCon.
  The real supplier is `collectTransitiveDCons` — the binder-TYPE closure
  (`Translate.hs` ~1094–1129). It seeds from `idType` of every top-level
  binder and `closeTyCons` expands through newtype reprs AND
  `dataConOrigArgTys` field types. From any binder mentioning `Eff`: `Eff`'s
  datacons are `Val`/`E`; `E`'s field types are `Union effs b` and
  `FTCQueue (Eff effs) b a`, so the closure reaches the `Union` and
  `FTCQueue` TyCons, yielding `Union` and `Leaf`/`Node`. All five, **from the
  TYPE alone** — reachability-independent. `isGhcCompilerTyCon` does not
  filter them.
  **Why the correction changes what D2 protects.** Under the wrong story the
  five ride on Core reachability, so a dev protects them by preserving the
  home-TyCon sweep. Under the true story they are immune to any narrowing of
  reachable Core and break ONLY if `RuntimeTypeClosure` replaces
  `collectTransitiveDCons` — which is live, since this spec lists
  "target/result + boundary + session-bound types" among D2's roots, reading
  exactly like a binder-type-closure replacement. A dev following the wrong
  warning would preserve `tyconMeta`, replace `transitiveMeta`, and ship the
  precise break while believing they had complied.
  **The guard is unchanged and correct under both stories:** carry the five as
  mandatory roots, unconditionally — not "if reachable", not "if the effect
  row is non-empty" — with a test pinning them through narrowing on a PURE
  entry term specifically. Treat it as a D2 correctness requirement on par
  with D1's hard fail.
  Still traced from code, NOT measured. D2's first step is an empirical
  attribution: dump a `meta.cbor` for a pure entry and attribute the five to a
  source, settling it rather than leaving it argued.
  **When D2 lands, the mandatory-roots set becomes a named, tested artifact
  with a permanent home in the codegen or extract docs** (root's call) — not
  folklore recoverable only from this note.
- **C1** GHC compiles every home module twice per extract (load'
  LoadAllTargets + unconditional second parse/typecheck/core2core loop —
  GhcPipeline.hs ~165/~184; session path ~366/~387). Leading suspect for
  the 6.8s extract_spawn. First: bracket load' SEPARATELY from the second
  loop under TIDEPOOL_TIMING (the capture that was queued and abandoned).
- **C2** resolveExternals expands the full external closure BEFORE target
  reachability (Resolve.hs ~75 → Translate.hs ~651 prune); the
  isNeverResolve fences are the tell. Fix: demand-driven worklist. ALSO E5:
  the queue is list-prepend + visited-later — a real worklist with
  scheduled-or-visited membership.
- **E1** Declaration turns pay multiple disposable GHC boots. Phase B kills
  the --emit-* spawns; the remainder = ONE parse/typecheck transaction
  returning binders+diagnostics+interfaces+Core, committed atomically.
- **E2** Lib.Gn → Lib.G(n-1) linear home-module chain ⇒ O(n²)
  declaration-heavy sessions (SESSION-COMPOUNDING; dogfood-critical). Fixes:
  retained home-package state / compile-each-generation-once / periodic
  compact checkpoints. Rendering-only fixes miss the issue.
- **E3** cumulative_exports_before walks all prior turns per render →
  incremental persistent maps. Cleanup-sized.
- **E4** FatIface fallback decodes a whole module's mi_extra_decls for one
  unfolding; per-process cache dies with each disposable extractor.
- **E6** canonicalizeDFlags forces -O2 on every module summary → tiered
  (validation parse/typecheck-only; optimized Core only for
  target+reachable). SEMANTICS-SENSITIVE: exposed unfoldings affect
  extraction — differential + corpus + extract-fidelity suites mandatory.

### DECISION RECORDED (spawn-latency, 2026-08-08, on C1's measurement)

Measured: `ghc_load` (`load'` alone) **28–32%** of `extract.total`;
typecheck+core (second loop) **62–69%**; combined **~96–97%**.
`ghc_load / (ghc_setup + ghc_load)` = **97–98%** on both arms. True boot
(`startup + ghc_setup`) = **93–233 ms = 0.6–1.9%**. Robust, not assumed: the
ratio moved <2 pp across a 1-min loadavg swing of ~11→~34 (two arms, n=6 each),
with a mechanism — `parMakeCount` is never set, so both arms are sequential and
contention cannot degrade them asymmetrically. Absolutes swung ~40%; the ratio
did not.

1. **PERSISTENT SERVER — REJECTED, on latency only.** Its distinctive benefit
   over interfaces is avoiding per-turn process + session establishment, which
   measures 0.6–1.9%. The historical "session boot 26–32%" was never boot —
   `ghc_load` is 97–98% of that span. `inject` measured 0 ms at every sampled
   session turn. A resident process costs lifecycle, crash recovery, cross-turn
   invalidation, and a new class of state-leak bug; 1–2% does not buy that.
   **Scope, so it is not over-read: E4's per-process cache death SURVIVES
   untouched** — the FatIface decode cache dies with each disposable extractor,
   so every turn re-decodes. That is a real amortization argument for residency,
   UNMEASURED, not refuted here. If it ever becomes the reason to build a
   server it must be measured and argued on its own terms, never smuggled back
   in on the boot argument, which is closed.
   **Residual flagged by this TL, now RESOLVED AS SCOPED (`abd00d60`):**
   `ghc_setup` contains `depanal`, which walks a module graph the `Lib.G<n>`
   chain GROWS; the sampled sessions were pinned at `Lib.G1`. So the rejection
   is scoped — **the server is rejected on boot cost FOR SESSIONS AT THE DEPTHS
   SAMPLED**, with depth-scaling of `ghc_setup` named as the one measurement
   that could reopen it. A repeated `depanal` over a growing graph would be a
   residency argument arriving through the one door the latency scoping did not
   close — the same shape as E4's, and named as precisely.
   Why scoped rather than measured now: at a FLAT module graph (top-level
   bindings 1706–1709 throughout), `ghc_setup` ranged **68 → 287 ms across five
   turns — a 4.2x spread with zero module growth.** That is the noise floor; a
   2–3 generation sweep cannot clear it, distinguishing a depanal trend needs
   ~8–10 generations, and no existing `tidepool-repl` test drives more than 2–3
   sequential `repl.def(...)` calls. The depth column is therefore folded into
   part 3's ALREADY-REQUIRED `Lib.G<n>` compile-time-vs-generations measurement
   — same vehicle, same run, one more column — not queued as a second errand.
   Recorded as a PREDICTION, explicitly not as evidence: `depanal` is a
   header-parse downsweep, O(n) in module count with a small constant, whereas
   E2's O(n²) lives in the compile chain — so `ghc_setup`'s SHARE should shrink
   with depth. If the measurement contradicts it, that contradiction is the
   finding and part 1 reopens on its own terms.
2. **NEITHER FIRST — C1's own fix is promoted ahead of both.** ~96–97% of a
   turn is two back-to-back full compiles of the same module set INSIDE ONE
   PROCESS, so no persistence architecture recovers any of it. Removing the
   redundant `load'` is ~30% of every extract for a local change, and it
   re-bases the arithmetic either architecture would be sized against.
3. **PRECOMPILED (FAT) INTERFACES — surviving candidate, DEFERRED with the
   measurement attached.** After C1's fix one compile remains (~65% of today's
   extract). Two figures decide it and neither exists: compile time vs
   `Lib.G<n>` generations (E2's axis — the session vehicle never grew past
   `Lib.G1`) and fat-iface bytes/decode per turn (E4 — one measurement serves
   both this and the residency case). Interfaces must be FAT: unfolding-less
   iface resolution bakes `ErrorSentinel`s that surface as `kind=4
   TypeMetadata`.

**The pivotal decision: persistent extractor.** E1/E2/E4 all point at it;
C1's measurement decides between precompiled interfaces vs persistent
server. Measure first (Codex ranking: binder-vs-validation spawn breakdown;
compile time vs #generations; modules typechecked per generation;
fat-iface bytes per turn; worklist pushes vs unique vars), then commit.

## Constraints

- STANDING DEV-SPEC RULE (from the generic-surface wave, 2026-08-08 — copy
  into every dev spec, both sub-TLs): a "pre-existing/inherited red" claim
  requires a cache-consistent A/B baseline run in the dev's OWN worktree —
  same compile-cache state on both legs, the dev's diff absent vs present.
  An argument from "my diff doesn't touch the failing test files" is
  invalid for global surfaces (prelude exports, pragma/extension sets,
  shared flags): every Haskell compile is downstream of those whether or
  not its file is in the diff. Note the cache confound explicitly: a
  fingerprint-invalidating change makes a naive comparison measure
  cold-vs-warm, not the diff. Empirically (this wave): both devs given
  this instruction produced sound baselines; the one that wasn't produced
  a plausible wrong argument.

- The `classify` phase vocabulary decision is binding (see
  plans/one-spawn-turn-protocol.md): extract phase `classify` after
  ghc_session; classify_extract retired with a doc tombstone.
- Extractor id-stability is a PINNED invariant (three permanent tests from the
  ConTags incident). Changing id-minting fires them; that's the design
  conversation happening, not a test to silence.
  **CORRECTED 2026-08-09 (twice — see below). The shorthand over-claims, and
  the trio was never enumerated anywhere.** No document in this repo names its
  members: this spec said "session_table_qualified_identity + two quick-tier
  assertions", `codex-review-2026-08-08.md:99` says "the three pinned
  id-stability tests" without naming any. The actual three, by exact path, each
  read to confirm:
  - `tidepool-repr::extend_checked_equivalence::distinct_ids_sharing_a_qualified_name_collide_regardless_of_input_order`
  - `tidepool-repr::extend_checked_equivalence::merge_table_skip_filter_cannot_dodge_the_qualified_name_collision_guard`
  - `tidepool-runtime::session_table_qualified_identity`

  **All three are DataConId qualified-name guards. None observes VarIds at
  all** — not `localVarId`, not `stableVarId`. `localVarId` bakes the raw GHC
  `Unique` for internal/floated bindings and is allocation-order-sensitive by
  its own doc comment. A green triple means constructor-identity guarding is
  intact; it does NOT mean ids did not move. **Their silence is not consent.**
  A change on `localVarId`'s path has no pinned guard and owes a direct
  experiment.
  (My first correction here named `realm_varid_pinning` and `VarIdMechanismTest`
  as two of the three — both real tests, neither in the trio. That was
  relayed from a grep for plausible-looking tests rather than from a source
  that defines the set, because no such source exists. Fourth guard this wave
  covering less than its name; also the first citation found to have **no
  referent at all**.)
- One-format wire policy: extract changes that move the wire ship both
  sides via redeploy, fail loud on skew.
- **GATE BAR CORRECTED 2026-08-09:** `haskell_suite_differential` and
  `corpus_report` **never invoke the extractor** — verified, zero
  `Command`/`compile_haskell`/`TIDEPOOL_EXTRACT` references; they replay frozen
  CBOR from `suite_cbor`/`corpus_cbor` as JIT-vs-eval differentials. They
  therefore **cannot observe any change to what the extractor emits** (E6, D1-B,
  D2) and pass identically whether it is correct or broken. I named the hardened
  differential as *the* gate for extractor changes in three specs; that was
  wrong. Real extractor coverage is `extract-fidelity-test` (real pipeline; its
  fixtures never touch JSON/Aeson — a known hole) plus harness acceptance (real
  end-to-end extracts). Fixture REGENERATION is the missing prerequisite and is
  a sequenced wave-level action, never a lane's call — shared directories,
  redeploy-class blast radius, and a naive prune drops `compared` below
  `COMPARED_FLOOR`.
- Correctness gates: hardened differential (floors), corpus_report,
  extract-fidelity-test (ALL tests — report actual N/N, never match a
  hardcoded number; it was 26 pre-D1-A and is 30 after), harness acceptance.
  E6 additionally needs
  the full set with zero tolerance.

## D7 (measured live, 2026-08-08 dogfood): boot pays FOUR extract compiles
## (~96s) before the first model call

Clean-cache wizard launch decomposition (release, box under moderate load;
extract_spawn 14-33s each):

1. ~15s — outer session boot seed (`driver.rs` bootstrap): full-template
   compile of `pure (toJSON (0 :: Int))` purely to seed the
   RunLLMTurn-only stack's ConTags.
2. ~15s — the answerer `Harness::new`'s own boot seed: the SAME trivial
   compile for the answerer stack.
3. ~30s — `compile_outer` of the render framing (`Loaded.render …`).
4. ~30s — `compile_outer` of the loop body (`Loaded.loop …`,
   sites=[(0,"Contribution")]) — only after this does the first model
   turn fire.

The two boot seeds are CONSTANT per (decl-list, stack) — identical source
every launch. `compile_turn` is deliberately cache-free ("turns are
one-shot"), which is right for turns and wrong for boot seeds: a
content-addressed disk cache of the two seeds' (CBOR, table) — or
precomputed ConTags shipped without GHC at all — removes ~30s of every
launch. Render/loop compiles (3)+(4) are per-cycle and belong to the
existing D1/D2/E1 work. Sequencing unchanged (after Phase B); this entry
just pins the measured boot shape so the win is sized honestly.

### D7 revised (Inanna's design question, same day): the seeds shouldn't exist

"Why do we need boot seeds at all?" — answer: we don't, structurally.
`Session::bootstrap` is program-shaped at construction (needs expr+table
to bring up the machine), and at boot no real program exists yet, so a
fake one (`pure (toJSON 0)`) is manufactured to fit the API slot. The
seed is scaffolding become load-bearing.

Fix ladder, in order of principle:
1. **Eliminate** (the mechanism fix): each session's FIRST REAL compile
   (outer: render; answerer node: the model's first block) already
   yields the (expr, table) bootstrap consumes — defer machine
   construction to first turn, or make construction not demand a
   program. Seeds stop existing. Touches Session::bootstrap
   (tidepool-runtime/harness) + both boot sites; sequence after
   driver-async's fold (driver.rs) — candidate first item of the
   extract wave.
2. **Cache** (interim, hour-sized): seed source is constant per
   (decl-list, stack) — content-addressed disk cache of (CBOR, table),
   zero GHC per launch. Land whenever; superseded harmlessly by 1.
3. One extract invocation for both seeds: strictly weaker than either;
   only if 1 hits a deep blocker.

The two-rows fact stays real either way (positional union tags per row);
only the per-launch GHC cost for constant artifacts is the defect.

---

# CLOSING STATE (extract-wave TL, 2026-08-09)

Wrap-up directive from Inanna: gate runs STOPPED subtree-wide, recovered
branches merged **as-is with their unverified flags intact**, everything
centralized into one branch. Verification and bug-hunts happen ONCE, on the
merged tip, afterward. **The unverified status travelling in these notes is the
deliverable, not green legs.**

## Landed and verified

| item | receipt |
|---|---|
| **C1** — `load'`/second-loop timing split | folded; `ghc_setup`/`ghc_load` partition, flat rows, `ghc_session` tombstoned to the classify lane |
| **D1-A** — the hard-fail defense | folded; CHECK A hard-fails, CHECK B a loud diagnostic, **two mutation legs at two call sites both naming CHECK A**, anti-vacuity control + permanent CHECK A message pin |
| **E6** — tiered `-O2` | folded; `core2core` 2908.8→893.7 ms (~3.25×) on the 14-module/10-excluded fixture; fidelity 30/30, acceptance 24/24, quick 1875/1875, differential `compared=312` vs floor 300. **Moves the wire** (in root's redeploy set) |
| **pivotal decision** | persistent server REJECTED on latency (true boot 0.6–1.9%); C1's own fix promoted ahead of both architectures; FAT interfaces deferred with the measurement attached |

## Landed, NOT verified — flags intact, legs run in root's central pass

| item | state |
|---|---|
| **item 0 steps 1–3 + 6** (`boot-lazy`) | both boot seeds DELETED, unbootstrapped `ResidentSession`. **The drop from 4 is NOT MEASURED** — legs were killed under the stop directive before the measurement ran. Harness acceptance 26/26 passed; `tidepool-repl`, `tidepool-runtime`, `extract-fidelity` **never ran**. `PRE_MODEL_EXTRACT_COMPILES` deliberately **stays at 4** so the central pass gets a test that FAILS LOUDLY if the drop did not happen, rather than a constant edited to match an expectation |
| **item 0b** (`boot-vocab`) | `effects_module_source_with_vocab` + `emits_helpers_for` (pub(crate)); three legs outstanding |
| **`--targets` prerequisite** (`boot-targets`) | multi-target emission, strict-mode skip unreachable **as a separate function**; differential/corpus/fidelity outstanding |
| **D1-B** (`d1-remove`) | `scanMeta` removal + `nameById` decoupling; gates stopped mid-run |

**Item 0's headline is an EXPECTED 4 → 2, not 4 → 1, and the 2 is UNMEASURED.**
Two claims, both needing to survive quoting:
- Even fully verified, this is 4 → 2. The remaining two are the render and loop
  compiles wave 3 would have fused. Do not let "item 0 landed" imply the end
  state.
- The drop itself has no measurement behind it. Both seed compiles are deleted
  in the code; nobody has yet observed the count fall. Do not quote "4 → 2" as a
  result.

(I wrote "4 → 2, measured" in the first draft of this section, from a receipt
that predated the stop directive. Boot caught it. That is the wave's own failure
mode reaching the closing summary — the most-quoted artifact — and it is worth
leaving the note visible rather than silently fixing the line.)

## Cut, and routed forward ready-to-spawn

- **Wave 3 / `boot-onecompile`** — render+loop fusion (item 0 steps 4–5). Spec on
  disk at `extract-wave/boot/02-wave3-one-compile.md`, premise-corrected. Both
  its gates (boot-lazy's fold, `--targets` landing) are satisfied by this fold,
  so it spawns with no unknowns.
- **D2** — reachability-narrowed `RuntimeTypeClosure`. Hand-off carries: the
  enumerated pinned trio and which risk each test observes; mandatory freer
  roots **derived from `freer_names`**, never hand-listed; the corrected gate
  set; empirical supplier attribution as step one.
- **C2/E5, E1, E2, E3, E4** — unstarted, unchanged.

## Standing hazards this wave established (not fixed here)

1. `haskell_suite_differential` and `corpus_report` **never invoke the
   extractor** — frozen-CBOR JIT differentials. They cannot gate extractor
   changes absent fixture regeneration, which is a sequenced wave/root action.
2. The pinned "id-stability" trio is **three DataConId guards** and observes no
   VarIds. `localVarId` determinism has no guard.
2a. **CHECK A guards EMITTED constructors only.** A constructor the runtime
   needs but the fragment never emits is invisible to it — which is exactly the
   sibling-set case D2 must justify when it narrows the table. D1-A's defense is
   real and is not a general metadata-completeness guarantee.
3. `kind=4 TypeMetadata` has **at least two causes** — the signature does not
   identify one.
4. The exempted rustc class has **no box-wide bound** (`nice -j4` is
   per-invocation, so N lanes = 4N), and `lslocks` has lost the
   is-it-safe-to-launch property.
5. Five hand-written copies of the standard effect row across two crates
   (ledger item 17).
