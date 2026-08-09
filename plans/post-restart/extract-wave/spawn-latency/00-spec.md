# Sub-TL `spawn-latency` — spec

Parent: `extract-wave` (branch `root.extract-wave`). Read
`plans/post-restart/extract-wave.md` (the wave spec) and
`plans/post-restart/extract-wave/OPERATIONAL.md` FIRST. You own the D/C/E
chain and drive the pivotal persistent-extractor decision.

Your artifact namespace is `plans/post-restart/extract-wave/spawn-latency/**`.
Keep a `LEDGER.md` there: one row per item with the measurement, the decision,
the receipt counts, and anything that conflicted at fold. Do NOT edit
`plans/post-restart/extract-wave.md` or `plans/README.md` — those are mine.

## The measurement that re-aims this lane (READ BEFORE PLANNING)

Phase-B measurement, 2026-08-08
(`plans/self-iterating-harness/11-turn-latency-contract.md`):

- **60–66%** of a turn's extract spawn is GHC's `core` phase
- **26–32%** session boot
- **under 6%** typecheck

**The standing home-module typecheck suspicion is REFUTED.** Consequences,
binding on your ordering:

- **Promote E6** (tiered `-O2`) and **D1** (double translation of reachable
  Core) — both are core-phase work, which is where the time actually is.
- **Read C1's double-compile suspicion against this breakdown BEFORE spending
  on it.** C1 was the leading suspect under the old model; the new breakdown
  may or may not support it. Measure, don't assume.
- **Spawn count is no longer where the time is.** Do not size any item off a
  spawn-count argument.

The reachability motivation (jit-chain's experiment, 2026-08-09, pre-wrap on
real session Core — size all absolute wins from THESE figures):

- turn 1: table = 164 constructors, fragment-reachable = 24 → 6.8:1 (~15%)
- turn 2: table = 166, reachable = 15 → 11.1:1 (~9%) — worsens as the table grows
- downstream: turn 1 emits 232 Cranelift funcs / 13,348 blocks for a
  24-constructor fragment
- **CAVEAT, carry it with the number:** "single-digit-to-low-teens percent
  reachable". The table is 164–166. NOT "hundreds vs dozens". Any writeup that
  rounds this up is wrong.

## Ordering

1. **D1** — highest priority, and it is a CORRECTNESS item, not cleanup.
2. **C1 measurement** — the bracket, cheap, and it is the input to the pivotal
   decision. Run it early even though the fix may not follow.
3. **E6** — promoted, semantics-sensitive, highest gate bar.
4. **D2** — the chain root; shrinks table + wrapper chain + CBOR for free.
5. **C2/E5, E1, E2, E3, E4** — as capacity allows, sized honestly.
6. **The pivotal decision** — see below. This one MUST be reached.

## D1 — reachable Core is translated TWICE (correctness, not cleanup)

`writeWholeModuleClosed`'s `scanMeta = collectUsedDataCons reachBinds` re-runs
the FULL translator per reachable RHS just to rediscover `tsUsedDCs`
(`haskell/app/Main.hs` ~329/~348, `haskell/src/Tidepool/Translate.hs` ~1023),
discarding the IR. The authoritative translation already returns `tsUsedDCs`
(`Translate.hs` ~443).

**Fix:** `translateModule` is the ONE authoritative producer (IR + used DCs +
types + effect sites). Defense-in-depth is a cheap Core visitor asserting a
subset relation — NEVER a second translation.

### The codex-review amendment — this defense DOES NOT EXIST TODAY

`plans/post-restart/codex-review-2026-08-08.md` item 7, CONFIRMED absent.
`collectUsedDataCons` is a second full *unseeded* translation, not a syntax
visitor, and Main **SILENTLY UNIONS** the two translations' results
(`mergeMetaPreserving [wiredInMeta, tyconMeta, usedMeta, scanMeta,
transitiveMeta]`, Main.hs ~348) and proceeds. The runLLMTurn/fork rewrite makes
the seeded and unseeded paths genuinely diverge, so disagreement is LIVE. No
missing constructor was found on the day of the review, but silent
under-collection is the signature of the still-owed garbage-`con_tag`
intermittent.

**The D1 fix MUST ship a hard fail. Non-negotiable acceptance:**

1. Walk emitted `FlatNode` constructor / data-alt IDs and **fail extraction**
   if the output metadata omits any. Not a warning. Not a merge.
2. An **independent reachable-Core collector** (the cheap visitor), separate
   from the translator, so the check is not self-confirming.
3. A **mutation test**: deleting one `recordDC` call must FAIL extraction, not
   produce output. If the mutation still produces output, the defense is not
   real and the item is not done.

Removing the second translation without shipping (1)–(3) is a regression in
safety even though it is a win in time. Ship them together.

Note the interaction with the PINNED extractor id-stability invariant: you are
changing what metadata is *collected*, not how ids are *minted*. If the three
pinned tests fire, STOP and escalate to me — that is a design conversation with
root.

## C1 — the double compile, and the measurement that gates it

GHC compiles every home module twice per extract: `load' LoadAllTargets` plus
an unconditional second parse/typecheck/core2core loop
(`haskell/src/Tidepool/GhcPipeline.hs` ~165/~184; session path ~366/~387).

**FIRST, and separately from any fix:** bracket `load'` SEPARATELY from the
second loop under `TIDEPOOL_TIMING`. This is the capture that was queued and
abandoned. It is cheap and it is the evidence the pivotal decision needs.

Then read the result against the Phase-B breakdown above before proposing a
fix. If the double compile is not where the core-phase time goes, say so in the
LEDGER and do not spend on it.

## E6 — tiered `-O2` (promoted; highest gate bar)

`canonicalizeDFlags` forces `-O2` on every module summary. Tier it: validation
modules get parse/typecheck only; optimized Core only for target + reachable.

**SEMANTICS-SENSITIVE** — exposed unfoldings affect extraction. The FULL gate
set with ZERO tolerance: hardened differential (floors), `corpus_report`,
`extract-fidelity-test` 26/26, harness acceptance. One regression blocks it.
Do not land E6 on a partial gate run.

## D2 — metadata over-collection (the chain root)

Metadata is currently every constructor of every home-module TyCon with no
reachability (`mg_tcs` → `collectDataCons`, `Translate.hs` ~2718). Fix:
`RuntimeTypeClosure` from runtime-observable roots — built/matched constructors
in reachable Core; sibling sets where rendering needs them; target/result +
boundary + session-bound types.

This is THE chain root: it shrinks the table, the wrapper chain, and the CBOR
for free. Size the win from the 6.8:1 / 11.1:1 figures above, with the caveat
attached. Sequence it after D1 — D1's hard-fail defense is exactly what makes a
reachability-narrowed table safe to ship.

## C2 / E5 — external closure and the worklist

`resolveExternals` expands the full external closure BEFORE target reachability
(`haskell/src/Tidepool/Resolve.hs` ~75 → `Translate.hs` ~651 prune); the
`isNeverResolve` fences are the tell. Fix: a demand-driven worklist. E5 folds
in: the queue is list-prepend + visited-later; make it a real worklist with
scheduled-or-visited membership.

## E1–E4

- **E1** Declaration turns pay multiple disposable GHC boots. Phase B killed
  the `--emit-*` spawns; the remainder is ONE parse/typecheck transaction
  returning binders + diagnostics + interfaces + Core, committed atomically.
- **E2** `Lib.Gn → Lib.G(n-1)` linear home-module chain ⇒ O(n²)
  declaration-heavy sessions. SESSION-COMPOUNDING and dogfood-critical. Fixes:
  retained home-package state / compile-each-generation-once / periodic compact
  checkpoints. **Rendering-only fixes miss the issue** — do not accept one.
- **E3** `cumulative_exports_before` walks all prior turns per render →
  incremental persistent maps. Cleanup-sized.
- **E4** FatIface fallback decodes a whole module's `mi_extra_decls` for one
  unfolding; the per-process cache dies with each disposable extractor.

## THE PIVOTAL DECISION — persistent extractor

E1/E2/E4 all point at it. C1's measurement decides between **precompiled
interfaces** vs **a persistent server**.

**Measure first, then commit.** The Codex ranking of what to measure:

- binder-vs-validation spawn breakdown
- compile time vs number of generations
- modules typechecked per generation
- fat-iface bytes per turn
- worklist pushes vs unique vars

This is a wave DONE-CRITERION: the decision is made on C1 measurement evidence
and RECORDED in your LEDGER, **or** explicitly deferred with the measurement
attached. A deferral with no measurement is not an acceptable outcome. Report
the decision to me as soon as you reach it — do not hold it until submit.

## Binding constraints

- The `classify` phase vocabulary decision is binding (see
  `plans/one-spawn-turn-protocol.md`): extract phase `classify` after
  `ghc_session`; `classify_extract` retired with a doc tombstone.
- **One emission path.** The turn mode reaches translation through the shared
  `writeWholeModuleClosed`. Do NOT add a second route to
  `translateModuleClosed`. Do NOT touch `Translate.hs`'s recognizer /
  qualification tables.
- **One-format wire policy:** extract changes that move the wire ship both
  sides via redeploy and fail loud on skew. You do NOT run the redeploy — root
  owns it at dogfood resume. If an item moves the wire, flag it to me
  explicitly so it enters the redeploy set.
- Extractor id-stability is a PINNED invariant. See D1 above.
- Every hand-rolled JSON string in the extract goes through
  `Tidepool.Binders.jsonString` (the FULL escaper). Verify sidecars with a
  strict parser (`python3 -c 'json.load(...)'`, never `strict=False`) — a raw
  newline inside a string renders as a line break, so invalid JSON looks
  correct in a terminal.

## Expected file overlap with sub-TL `boot`

`haskell/app/Main.hs` — `boot`'s item 0 step 4 (render + loop from ONE extract
invocation) touches turn-mode emission while your D1 touches
`writeWholeModuleClosed`'s metadata merge. Same file, different regions. Per
the realm-spike conflict experiment we do NOT pre-partition: write minimal
localized diffs, and log any non-mechanical conflict at fold.

## Structure

Decompose into reviewed dev leaves (`spawn_dev`, model **sonnet**), gate your
own folds, then `submit_branch` to me. Correctness gates are required per item;
benchmarks are optional except where an item's whole claim is a time win — then
the measurement IS the receipt.

Receipts are per-binary pass/fail counts, never exit codes. Copy
`../OPERATIONAL.md`'s Block section verbatim into every dev spec.
