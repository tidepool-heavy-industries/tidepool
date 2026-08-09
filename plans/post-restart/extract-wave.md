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
invocation (needs Phase B's multi-binder — hence the gate above),
answerer boots from the model's first block. Supersedes D7's
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
- Extractor id-stability is now a PINNED invariant (three permanent tests
  from the ConTags incident — session_table_qualified_identity + two
  quick-tier assertions). Changing id-minting fires them; that's the
  design conversation happening, not a test to silence.
- One-format wire policy: extract changes that move the wire ship both
  sides via redeploy, fail loud on skew.
- Correctness gates: hardened differential (floors), corpus_report,
  extract-fidelity-test 26/26, harness acceptance. E6 additionally needs
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
