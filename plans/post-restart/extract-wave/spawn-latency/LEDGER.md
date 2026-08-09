# sub-TL `spawn-latency` — LEDGER

Branch `root.extract-wave.spawn-latency`. Spec: `00-spec.md`. Parent:
`extract-wave`.

One row per item: the measurement, the decision, the per-binary pass/fail
receipt counts, and anything that conflicted at fold.

---

## Reading of the code, before any work (2026-08-08)

Anchors verified in this worktree at `be05f291`, so the specs below cite real
line numbers rather than the campaign's remembered ones.

**D1 — the silent union is exactly as the codex review describes.**
`haskell/app/Main.hs:349-358` builds

```haskell
let tyconMeta      = collectDataCons tycons
    usedMeta       = map dcToMeta (Map.elems usedDCs)   -- authoritative translation
    scanMeta       = collectUsedDataCons reachBinds     -- SECOND full translation
    transitiveMeta = collectTransitiveDCons reachBinds
    wiredInMeta    = wiredInDataCons
    allMeta = mergeMetaPreserving
                [ wiredInMeta, tyconMeta, usedMeta, scanMeta, transitiveMeta ]
```

`collectUsedDataCons` (`Translate.hs:1027-1039`) runs `translate` over every
reachable RHS from a fresh `TransState`, keeps `tsUsedDCs`, and throws the IR
away. There is no assertion anywhere between `usedMeta` and `scanMeta`; the
merge is a union and extraction proceeds. CONFIRMED absent, as reviewed.

The emitted-id walk the defense needs is cheap and well-typed: the only
constructor ids that reach the wire are `NCon !Word64 ![Int]`
(`Translate.hs:98`) and `FDataAlt !Word64` (`Translate.hs:107`). `NCase`'s
`Word64` is the case *binder*, not a constructor — it is not part of the
check.

**C1 — the double compile is real, and the current bracket hides half of it.**
`GhcPipeline.hs:174` runs `load' … LoadAllTargets` (a full compile of every
home module, including `core2core`), and `GhcPipeline.hs:191-216` then runs
`parseModule` / `typecheckModule` / `hscDesugar` / `core2core` over every
summary AGAIN. The `ghc_session` phase brackets `sessionT0`(117) →
`sessionT1`(180), so it ALREADY CONTAINS `load'` — i.e. the Phase-B
"session boot 26–32%" figure is *session setup + depanal + a full first
compile*, not boot. The 60–66% `core` figure is the SECOND loop's `core2core`
alone. That materially changes how C1 should be read and is the first thing
the measurement must separate.

**C1 third finding (wave TL, from the two above; spec amended at `ecfc4eb1`) —
the typecheck refutation is UNSOUND.** `GhcPipeline.hs` ~222's own comment says
each summary's `parseModule`/`typecheckModule` "redoes its typecheck
independently of `load'`". The second loop typechecks (~193) AND core2cores
(~213), and `load'` already did both. So the `typecheck` row (~219) sums only
ONE of the TWO typechecks per extract — the other is inside `ghc_session`,
unlabelled. **"Under 6% typecheck" measures half the typecheck cost and does
NOT refute the home-module typecheck suspicion.** Home-module typecheck cost is
OPEN, not refuted. C1 is correspondingly UPGRADED: confirmed in code, only its
SIZE unknown — the breakdown partly CONCEALED C1 rather than refuting it.

This does not weaken E6 or D1: 60–66% in the SECOND loop's `core2core` alone is
ample warrant. Ordering unchanged.

**Bracket design — SETTLED as a partition (third option; supersedes both
earlier proposals).** Two flat, non-overlapping rows replacing the compile
lane's `ghc_session`:

    ghc_setup   session DynFlags setup + guessTarget/setTargets + depanal
    ghc_load    load', and nothing else

Neither narrowing (wave TL's first wording) nor nesting (mine) was right.
Narrowing would have left a name meaning less than it used to — the
`classify_extract` tombstone's failure mode. Nesting violates a DOCUMENTED
invariant I had not checked: `tidepool-harness/src/timing.rs:25`, *"Stages are
FLAT and non-nesting: a collector sums by `stage` and never has to reason about
containment"*, with `EXTRACT_PHASES` (timing.rs ~133) an ordered flat list — a
nested `load` row makes every flat-sum consumer double-count `load'`, silently,
because both rows are individually correct. The partition satisfies all three
constraints at once: flat contract holds, historical `ghc_session` is exactly
`ghc_setup + ghc_load` (recovered by the addition a flat-sum collector already
performs), and `load'` gets its own unambiguous row. `load'` is NOT decomposed
internally.

**Consumer finding that shapes the retirement.** `PHASE_GHC_SESSION` must NOT
be deleted: `haskell/src/Tidepool/Binders.hs:257` (`classifyBlock`, the
`--classify` lane) is a SECOND emission site, bracketing `getSessionDynFlags`
alone — no depanal, no `load'`. So the name TODAY denotes two spans differing by
orders of magnitude across two lanes, disambiguated only by the `extract.` /
`classify.` prefix (timing.rs ~37 documents exactly that hazard).

**The partition ELIMINATES that ambiguity rather than documenting it** — a
result of this item worth recording as such. After the change `ghc_session` has
exactly ONE emitter (`classifyBlock`), the compile lane having moved to
`ghc_setup`/`ghc_load`, and the prefix stops being load-bearing for that name.
The tombstone is therefore written in the strong form — *"`ghc_session` now
denotes exactly one span … the compile lane's former use is succeeded by
`ghc_setup` + `ghc_load`"* — not as a residual exception a later reader must
respect. Same principle that made the partition beat nesting: eliminate an
ambiguity rather than annotate it. Carried into C1's receipts, since a diff
that reads as "renamed a timing row" would otherwise hide it.

**Added C1 done criterion (Inanna via root, wave spec `fa552226`).** The same
commit that fixes the brackets retires
`plans/self-iterating-harness/11-turn-latency-contract.md`: replacement written
at `11-extract-timing-contract.md` (a NEW path, so a naive citation of the old
one fails loudly rather than silently resolving to stale numbers), old file
deleted, all references updated. Re-grepped list: `one-spawn-turn-protocol.md`
:28, `spawn-latency/00-spec.md`:16, `turn_latency_bench.rs`:8, `timing.rs`:29.
`plans/post-restart/extract-wave.md`:81 also cites it but is the wave TL's file
— left to its owner, dev explicitly told not to touch it.

**C1 second finding — the session path emits NO timing at all.**
`runSessionPipeline` (`GhcPipeline.hs:326-…`) has zero `emitPhase` calls.
`runPipelineSession` routes to it whenever `isSessionScopeActive`
(`Session.hs:173-174` — true iff any `Val.G<g>` iface is injected). So every
Phase-B number was taken on `runNormalPipeline`, i.e. turns with NO injected
session values. Real dogfood turns 2+ inject Vals and are, today,
**completely unmeasured**. Any persistent-extractor decision taken on the
normal path alone would be taken on the wrong path.

**E6 — `canonicalizeDFlags` (`GhcPipeline.hs:597-624`) forces `updOptLevel 2`
plus `Opt_ExposeAllUnfoldings` / `Opt_ExposeOverloadedUnfoldings` on every
summary**, and is re-applied per module at 192/436. Tiering it is where the
core-phase time is.

---

## Planned decomposition (measurement-first ordering)

Ordering is set by the Phase-B breakdown (core-phase dominant), not by spawn
count. No item below is sized off a spawn-count argument.

> The "home-module typecheck suspicion REFUTED" premise this ordering was
> originally handed is WITHDRAWN (see the third finding above) — that row
> measures half the typecheck cost. The ordering itself is unchanged: it rests
> on the second loop's `core2core` alone, which the amendment did not disturb.

| Wave | Dev | Item | Why here |
|---|---|---|---|
| 1 | `c1-timing` | C1 measurement | Cheap, gates the pivotal decision, and must run before any C1 fix is even proposed. Touches `GhcPipeline.hs` + `Timing.hs` only. |
| 1 | `d1-defense` | D1 part A — the defense, additive | Ships the hard-fail subset check, the independent Core visitor, and the mutation test with the second translation STILL PRESENT. Zero behaviour change if the two translations agree; a loud failure if they don't. Touches `Translate.hs` + `Main.hs` + `test-fidelity/`. |
| 2 | `d1-remove` | D1 part B — remove the second translation | Only safe once part A's hard fail exists. Deletes `scanMeta`. Folds together with part A as ONE landing on this branch before `submit_branch`. |
| 2 | `e6-tiered-o2` | E6 — tiered `-O2` | Promoted by the measurement. Semantics-sensitive, FULL gate set at zero tolerance. |
| 3 | `d2-runtime-closure` | D2 — `RuntimeTypeClosure` | The chain root. Sequenced after D1 because D1's hard fail is what makes a reachability-narrowed table safe to ship. |
| 4 | as capacity | C2/E5, E1–E4 | Sized honestly against the measurement, not against the old model. |

**WAVE 2 HELD — on DEPENDENCY, which was always the real constraint.**

Correction of my own reasoning, kept visible rather than rewritten. I held wave
2 on box conditions: "44 waiters against a 3-slot cap, ~15x capacity". **That
number was fiction** — it came from `pgrep -fc "ghc-slots.sh"`, which matches
every agent session whose command line mentions the script. Kernel truth via
`lslocks | grep tidepool-ghc` (WRITE = holder, WRITE* = blocked waiter) at the
same moment: **4 holders, 3 waiters** against 4 slots. A healthy queue, never a
15x one.

The same args-grep-counts-agents trap I had diagnosed for
`pgrep -fc tidepool-extract` one message earlier — and I committed it on the
ADJACENT instrument in the very message where I was being careful about the
first. Applying a lesson to one instrument and not its neighbour is the specific
failure worth remembering.

**But the hold stands, because the queue was never the binding constraint.**
Both wave-2 devs are blocked on wave 1 FOLDING, by dependency:

- `d1-remove` deletes `scanMeta`, which is only safe once `d1-defense`'s hard
  fail exists — that is the whole reason D1 was split A/B.
- `e6-tiered-o2` edits `canonicalizeDFlags` in `GhcPipeline.hs`, the file
  `c1-timing` owns until it folds.

So the box-conditions justification was both WRONG and REDUNDANT. Had I spawned
on the corrected numbers, I would have created two devs that immediately
conflict with unfolded wave-1 work. The right decision survived the bad
reasoning, which is luck, not method — the dependency argument is the one that
should have been load-bearing all along.

Waves 1 and 2 are file-disjoint within themselves (`GhcPipeline.hs`/`Timing.hs`
vs `Translate.hs`/`Main.hs`/`test-fidelity/`) so they run in parallel.

D1 is split A/B deliberately: the spec's non-negotiable is that the defense and
the removal ship **together**, and folding both before `submit_branch` is one
landing. Splitting them lets part A be gated with the union still in place —
which is the only way to learn whether the two translations actually disagree
today, before the union that hides it is deleted.

### Reachability caveat, carried verbatim

Table = 164–166 constructors; fragment-reachable = 24 (turn 1) then 15
(turn 2) → 6.8:1 (~15%) then 11.1:1 (~9%). That is
**single-digit-to-low-teens percent reachable**, NOT "hundreds vs dozens".
Every size estimate in this ledger is taken from those figures.

---

## NAMED-GUARD RULE — binds every item in this lane

Now SWARM-WIDE (root's durable memory; wave `OPERATIONAL.md` `19b8dca9`).
Verbatim as root recorded it:

> if a gate exists to catch one specific failure mode, the receipt shows that
> test passing BY NAME with its own pass line; cross-lane guards additionally
> name the base commit; base proves the tree, name proves execution — both, or
> neither is established.

Applied UNIFORMLY here, not only to D2's cross-lane guard. An item of mine
reporting an aggregate where a named guard exists is out of compliance with a
standing rule, not merely under-detailed. Concretely, per item:

- **D1** — the mutation check's own PASS line by label, and the
  `TIDEPOOL_TEST_DROP_DC` leg's failure assertion by name. Not `N/N passed`.
- **C1** — the wire-inertness check (timing off vs on ⇒ byte-identical stdout
  and emitted files) named explicitly with its diff-empty receipt.
- **E6** — the mis-tiering detection-power demonstration named, with the gate
  it turned RED identified.
- **D2** — boot's ConTags guard by name AND the base commit it ran against.

## D1 CHECK B — the invariant I specified is FALSE by design

`d1-defense` hit CHECK B on real gates (**20 passed / 4 failed / 24 total**,
harness acceptance) and STOPPED per spec rather than deciding. All four
identical: `GHC.Types.(#,#)` found in reachable Core, absent from meta.cbor.
**CHECK A never fired — not once.** The runtime was never at risk.

Cause, verified in `Translate.hs` rather than taken on report: dedicated `Case`
clauses (~1914, ~1957, ~2000+) match `case <primop> args of (# a, b #) -> body`
and desugar straight into chained primop splits (`splitMultiReturnPrimOp`),
never routing through `mapAltCon`'s `DataAlt dc -> recordDC dc`. By design — an
unboxed tuple has no runtime representation once split.

**The spec defect is mine.** CHECK B asserts *every DataCon in reachable Core is
in the metadata*. That is FALSE BY DESIGN: the translator's job includes NOT
translating whole classes of Core. Two classes in one day (`error`'s HasCallStack
args, then unboxed-tuple multi-return); `Translate.hs` holds many more
interception patterns. Exclusions cannot fix it — each moves the visitor toward
re-deriving translator internals, destroying the independence B exists for.

**Resolution (approved by wave TL, escalated to root):**

1. ONE exclusion, **categorical not elision-specific** — `(#,#)` is an unboxed
   tuple with no runtime heap representation, so it can never require metadata.
   A TYPE-level fact, not a translator fact, so independence is preserved.
   Derived from the existing `isUnboxedTupleDataCon`, not restated.
2. REVERT the CallStack exclusion — elision-specific, the refused class.
3. DOWNGRADE CHECK B to a loud named diagnostic. **BLOCKED** — see below.

Holds the non-negotiable on its exact words: fail-extraction attaches to
**(a) = CHECK A**, untouched; **(b)**'s stated purpose is keeping (a) from being
self-confirming, and what demonstrates that is **(c)**, the mutation test.

### The hole in my own argument (wave TL caught it)

My case for (3) rests entirely on **which check the mutation fires**, and I
asserted it without running it. **If the mutation fires CHECK B, downgrading B
means the mutation no longer fails extraction — requirement (c) broken by the
fix to the defense, leaving a mutation test that passes while testing nothing.**

The 30/30 receipt FELT like coverage because it was a real, named, passing
result about the mutation test. It answered a different question: that the
mutation fires *something* under the old shape, not that it fires **A**. **A
green receipt adjacent to a claim is not evidence for the claim.**

Sharper than the wave's earlier instances: it would have been introduced BY a
correction, DURING a conversation about verification, with a passing receipt
attached.

Subtlety that makes this substantive rather than ceremonial: the deletion site
was chosen as a constructor **only `recordDC` supplies** — a claim about
metadata SOURCES. That is not the same as being **emitted**, which is what CHECK
A keys on. The two can come apart.

**(3) was HELD** until the mutation was re-run with B downgraded and its output
named CHECK A.

### RESOLVED — both legs name CHECK A, verified under the NEW shape

**Two legs, two constructors, two call sites, both naming CHECK A.** That is the
load-bearing line: one site firing could be a property of that site; **two
independent sites firing is a property of the check.**

- knob (`TIDEPOOL_TEST_DROP_DC`), `GHC.Internal.Base.:|` — run against the
  binary with all three changes applied. `CHECK A ... FAILED`, EXIT=1, output
  dir empty.
- physical deletion (worker-path `recordDC`), `GHC.Num.Integer.IS` — same
  message shape, `DELETED_LEG_EXIT=1`, output dir empty. `git status` restore
  proof captured in the same run, scoped and full-repo, both empty.

Mechanism established rather than asserted: `:|` is genuinely EMITTED (confirmed
via `TIDEPOOL_DUMP_CLOSED` as a real `Con`/`DataAlt` in closed Core), unlike
`(#,#)` which desugars into chained primops and never reaches the wire. So
dropping its only `recordDC` supplier removes it from metadata while it is still
on the wire — precisely CHECK A's contract. That converts "the test happens to
fire A" into "the test fires A for the reason CHECK A exists".

### The provisional invariant, CHECKED at fold — conclusion holds, mechanism was imprecise

I held the no-re-run argument provisionally and checked it against the diff.
**The conclusion survives; the stated mechanism does not, and the correction
matters for anyone reading the receipt later.**

The dev's claim was "all three changes touch only `collectReachableConDCs` and
CHECK B's severity, leaving CHECK A's path invariant." In `Main.hs`'s
`assertMetaCoversEmitted`:

    reachableMeta = map dcToMeta (collectReachableConDCs reachBinds)
    nameById      = Map.fromList [ (dcmId m, dcmQualName m) | m <- reachableMeta ]
    nameOf vid    = ... Map.lookup vid nameById
    missingEmitted = emittedConIds nodes `Set.difference` allMetaIds

- CHECK A's **FIRING CONDITION** (`missingEmitted`) depends on `emittedConIds`
  and `allMetaIds` only. **Genuinely invariant** across all three changes.
- CHECK A's **MESSAGE** resolves names through `nameById` ← `collectReachableConDCs`.
  So the three changes DO reach CHECK A's path — its diagnostic text.

Conclusion still holds for the physical-deletion receipt: `GHC.Num.Integer.IS`
is neither an unboxed tuple (so the categorical exclusion does not drop it) nor
a CallStack constructor (so the revert only adds to the map). Its resolution is
unaffected and the message is byte-identical under the new shape. **No re-run
owed** — but for a reason one step off the one stated.

### Follow-up wart found in the same read (non-blocking, for D1-B)

A MULTI-ELEMENT unboxed tuple **can** be emitted: `Translate.hs` ~2088-2090
does `recordDC dc` then emits `FDataAlt (varId (dataConWorkId dc))` for the
heap-box case. If such a constructor ever went missing from metadata, CHECK A
would fire **correctly** — safety intact — but print `<name unresolvable>`,
because `isUnboxedTupleDataCon` removed it from `nameById`. The exclusion is
right for CHECK B's purpose and wrong for CHECK A's name lookup, which shares
the same source. Fix at D1-B: build `nameById` from an UNFILTERED walk, keeping
the filter only where CHECK B consumes it.

**Still PROVISIONAL, to confirm against the diff at fold:** the dev's argument
that the physical-deletion leg needs no re-run rests on all three changes
touching only `collectReachableConDCs` and B's severity, leaving CHECK A's path
invariant. Sound conditional on that premise — which is a claim about code on
its branch that I cannot see. **A mechanism claim from someone closer to the
code is input, not verification.** If the diff shows any of the three touching
CHECK A's path, that leg is re-run at fold.

### Blast radius of the downgrade — bounded structurally, not by search

**CHECK B is NEW to this item** (the codex review's finding was that the defense
did not exist). So nothing predating this landing can depend on its fatality,
and the downgrade's blast radius is exactly the tests this item wrote: the four
`Fidelity.D1Defense` checks and the two mutation legs.

Stronger than "only negative tests are at risk", because it does not stake the
claim on a grep being exhaustive — a search can silently return a plausible
wrong answer, so **do not stake a claim on a search when another route exists.**
The grep (Haskell `exitFailure` hits are all runner plumbing; Rust `is_err()`
sites assert runtime/JIT errors) is corroboration, not foundation.

**Receipt must attribute re-runs to the right cause:** two mutation legs plus
the D1Defense group on account of the DOWNGRADE; `corpus_report`, acceptance,
quick tier and fidelity on account of `--no-fail-fast` and confirming the four
previously-failing acceptance tests now pass. Different reasons — a receipt that
does not say which applies to which lets a later reader assume the downgrade
forced all of it and over-estimate its blast radius.

## Denominators, and "keep in sync" is not a guard

**DENOMINATOR RULE (wave, `1b1f7c35`) — every receipt leg carries `N passed /
M total`.** An inherited red in `tidepool-runtime`
(`mock_stack_lockstep::mock_stack_matches_production` — `Fork` added to
`standard_decls()` without updating the hand-maintained `EFFECT_NAMES` mirror,
`tidepool-testing/src/eval_harness.rs:403`; not ours, routed to root) truncates
nextest runs under default fail-fast, and **`--no-fail-fast` is in neither
`battery.sh` nor `battery-shard.sh`**. A sibling lane banked 198 of 877 tests
with real PASS lines, real names, real timings, and 679 never run.

Why this is the sharpest of the three failure shapes:

    never started     instant exit, clean log        ran nothing
    queued, killed    only "all slots busy" in log   ran nothing
    fail-fast trunc.  REAL pass lines and timings    198/877

The first two are visible in the log. **The third answers YES to every adoption
question** — it started, it ran, it passed what it ran. Only the denominator
tells. Check log CONTENT, never exit status: an environment failure exits in
milliseconds and reads as a fast pass. This strengthens the named-guard rule
rather than replacing it — naming the test proves it ran, the denominator
proves the suite around it did.

**C1's folded receipt VERIFIED against this rule, by an independent route.**
Counted `#[test]`/`#[tokio::test(...)]` declarations across the 11
`acceptance_*` binaries: **24**, matching the run's `24 tests run: 24 passed, 0
skipped`. Complete surface, not a truncated slice — no re-verification owed.
Verified by counting declarations in source rather than trusting the run's own
summary, which is the point of the rule.

> Method note, third instance: three successive grep patterns returned a clean
> `0` across all 11 files before one matched (`#[tokio::test(flavor = …)]` with
> arguments defeated each). What caught it was not pattern discipline but
> **implausibility** — zero tests across eleven acceptance binaries cannot be
> true. A zero from a mistyped pattern reads exactly like a real zero, so the
> defense that actually works is a prior expectation about the answer's rough
> size, not more care with the regex.

**ONE-RED COUNT, and why it needs the denominator rule to be sound.** Root's
advisory: exactly ONE expected inherited red box-wide
(`mock_stack_matches_production`) until the mock fix folds. That converts "is
this red inherited?" from an argument into a COUNT — one is inherited, two
means the second is yours, no cache-consistent A/B needed.

**EXPIRY: valid only until the mock fix folds** (two commits — `Fork` added as
the immediate unblock, then `EFFECT_NAMES` derived from `standard_decls()`).
After that the expected count is zero. Do not cite the allowance past it.

**The two rules interlock and neither is sound alone.** The count is only valid
on a COMPLETE run: under default fail-fast a run stops at the first failure, so
"I only saw one red" is guaranteed by construction rather than observed, and a
second red behind it never executes. `--no-fail-fast` is a PREREQUISITE for the
count to mean anything. Used together they are strong; the count used without
it is self-confirming — the same shape as a mutation test whose
independent collector is the translator it is checking.

Practical, since the battery scripts self-acquire via `$PWD` and worktree copies
lack root's `7d57cea5` fix: pass it explicitly as
`scripts/battery-shard.sh <crate> --no-fail-fast -E '<filter>'`.

**"KEEP THIS IN SYNC WITH X" IS NOT A GUARD — binding on D2.** The `Fork` break
above is the demonstration: `eval_harness.rs:387` documents that the list can
drift AND cites the prior instance (`f1a480e6`) — then it drifted again. A
comment saying a mechanism can fail silently is **a recorded decision to keep
it**, and it reads as diligence while providing none; it is evidence that
hand-maintenance was already tried and already failed.

Applied to D2: anywhere the reachability work is tempted to leave "keep this in
sync with X" as the guard, that comment IS the failure, not the fix. **Derive
from the source.** Same shape as the freer-scaffolding mandatory roots — the
five must be DERIVED as roots (from `freer_names`, the single source `ConTags`
itself resolves against), never listed with a note asking future readers to
remember why.

> **VERBATIM into D2's dev spec** (wave TL: "it will do more work than the
> requirement itself"). The requirement as originally raised — "carry the five
> as mandatory roots" — is satisfiable by exactly the hand-maintained list plus
> an explanatory note, i.e. by the mechanism that had just failed one file over.
> So the spec carries this sentence, not just the requirement:
>
> *A hand-listed set with an explanatory comment would be the identical
> mechanism that just broke, reintroduced by the item whose whole purpose is
> narrowing that table.*

## THE GATE BAR I SPECIFIED FOR THREE ITEMS COULD NOT SEE THEM

Verified by me, independently by the wave TL, and ruled at `b9c37c57`:

    haskell_suite_differential   0 refs to Command/extract/compile_haskell;
                                 replays 350 frozen .cbor from suite_cbor/
    corpus_report                replays 128 frozen .cbor from corpus_cbor/

**Neither invokes the extractor.** They are JIT-vs-eval differentials over
pre-generated CBOR, producing identical results whether the extractor is correct
or catastrophically broken. So for every item that changes what the extractor
EMITS — E6 (tiering), D1-B (removing a metadata source), D2 (narrowing the
table) — they are structurally incapable of detecting the regression. I named
the hardened differential with `COMPARED_FLOOR` as **the** gate for extractor
changes in all three specs. It was the wrong instrument in all three.

**These are correct instruments cited without their prerequisite.**
`haskell/CLAUDE.md` documents that fixtures must be REGENERATED after serializer
changes; the wave gate list omitted that, so we ran a JIT differential and read
it as extractor coverage. Same shape as the pinned trio, one level up — and
found the same way, by someone relying on it rather than auditing it.

**The honest coverage set for extractor changes:**

1. **The item's own hard fail** where it has one (D1's CHECK A is D1-B's primary
   detector — the A/B split exists precisely to provide it).
2. **`extract-fidelity-test`** — real pipeline end to end. KNOWN HOLE, found
   today: its fixtures (erasure symmetry, recognizer qualification,
   unboxed-tuple arity, D1 defense) never touch JSON/Aeson, so Aeson-touching
   extractor behaviour is uncovered. A thin set with an unstated hole is worse
   than a thin set.
3. **harness acceptance** — spawns real extracts end to end. The strongest real
   signal, and the reason it costs ~1845s.

**FIXTURE REGENERATION IS A SEQUENCED WAVE-LEVEL ACTION, never a lane's call**
(ruled). Shared directories, other lanes in flight, redeploy-class blast radius
— and booby-trapped for the well-intentioned: `haskell/CLAUDE.md` warns that
pruning `*_u<n>.cbor` from `suite_cbor` drops `compared` below
`COMPARED_FLOOR`, so a naive regeneration **breaks the very floor it was meant
to protect.** Route requests upward.

Consequence taken: the hardened differential is DROPPED from D1-B's gate list
(zero signal for it, ~900s of contended slot), with the reason recorded in its
receipt so a missing differential line does not read as convenience. D2's spec
inherits the corrected set.

## Reliance finds gaps that audits do not — shapes how D2's spec is written

Four guards this wave turned out to cover less than their names, and **all four
were found by someone CHECKING A GUARD THEY WERE RELYING ON, not by anyone
auditing guards.** That is not coincidence and it is worth building on.

An audit asks "does this guard exist and pass?" — both answers are yes, so it
moves on. Reliance asks a SPECIFIC question and notices the guard does not
answer it. The gap is only visible from the angle of the thing you wanted it to
prove. Which means: **guard coverage cannot be established by review; it is
established at the moment of use, by whoever is about to lean on it.**

Consequence for D2, which leans on a CROSS-LANE guard (boot's ConTags pins) it
did not write: the spec must not say "run the harness-acceptance shard and check
it is green". It must say what D2 needs that guard to PROVE — that a
reachability-narrowed table still boots the machine on a pure entry term — and
require the dev to confirm the guard actually observes that, by name, before
treating its pass as coverage. If it does not, that is the finding, and it is a
finding only the person relying on it will ever see.

The fourth instance (E6) went further than a guard covering less than its name.
**"The three pinned id-stability tests" had NO AUTHORITATIVE REFERENT.** My
spec cites the wave spec; the wave spec says "session_table_qualified_identity +
two quick-tier assertions"; `codex-review-2026-08-08.md:99` says "the three
pinned id-stability tests" and names none. **Three documents lean on the set,
none enumerates it.** My mis-identification was not sloppiness against an
available source — grepping was the only move available, and the defect is that
a load-bearing citation was unresolvable.

ENUMERATED at wave commit `f181eb33`, by exact path:

    tidepool-repr::extend_checked_equivalence::distinct_ids_sharing_a_qualified_name_collide_regardless_of_input_order
    tidepool-repr::extend_checked_equivalence::merge_table_skip_filter_cannot_dodge_the_qualified_name_collision_guard
    tidepool-runtime::session_table_qualified_identity

**All three are DataConId qualified-name guards. None observes VarIds AT ALL** —
not `localVarId`, not `stableVarId`. So "extractor id-stability" never covered
the VarId space by any of these tests, which makes the E6 caution more warranted
than either of us first stated. First thing this wave found with **no referent**,
as distinct from a referent narrower than its name: a phrase cited by three
documents, load-bearing in two specs, gating a design conversation with root, and
resolving to nothing.

**THE INVERSION THAT MATTERS FOR D2.** The trio is nearly useless for E6, which
perturbs VarIds — but it is **directly on point for D2**, which narrows the
DataConTable itself. A reachability-narrowed table is exactly the change that
could drop or collide a qualified name, which is precisely what all three guards
observe. So the instruction "if the pinned tests fire, STOP and escalate" is
weak for E6 and **strong for D2** — the one item where those tests are a real
guard rather than silence. D2's spec must say so, with the enumeration, rather
than repeating the shorthand.

Standing rule now in the wave spec: **if a change touches `localVarId`'s path,
no pinned test is watching and a direct experiment is owed.** And: a PASS is
silence, not consent — but a FIRE still means STOP and escalate.

## Positive control: an INSTRUMENT needs the same proof as a test

My own error, banked because it generalizes the anti-vacuity rule I had already
imposed on D1's mutation test. I correctly diagnosed that
`pgrep -fc tidepool-extract` over-counts (it matches every agent shell carrying
`TIDEPOOL_EXTRACT=…`: 22–24 matches against 3–4 real compiles), wrote a
replacement, and reported its output as a finding **without once running it
against a known-positive case**. Linux truncates `comm` to 15 chars, so
`tidepool-extract-bin` appears as `tidepool-extrac` and any pattern carrying the
full 16-char `tidepool-extract` returns 0 on a busy box and 0 on an idle one. I
reported "zero extracts running" when four were.

Correct instruments (also wave `OPERATIONAL.md` `6033a70b`):

    ps -eo comm= | grep -c '^tidepool-extrac'    # real concurrent compiles
    cat /proc/loadavg                            # actual load

Anchor and stop at `extrac`. Do NOT generalize "comm truncates to 15" further —
a 39-char value appears in the same output, so the rule has exceptions I have
not chased.

**The rule:** a correction to an instrument needs the same verification as the
instrument it replaces. A zero from a mistyped pattern is indistinguishable from
a real zero, which is precisely why it survives review — the same reason D1's
mutation test must prove it can FAIL before its pass means anything. Applied to
a dev's test and not to my own measurement, which is the asymmetry to watch.

## Standing check: does the gate detect the mistake it is named for?

Ratified method note (root, via wave TL): the C1 finding came from distrusting
what a row's NAME claimed it bracketed, not from distrusting the number.
Pointed at the rest of this chain, two items have the same shape — a plausible
label sitting on top of a narrower measurement. Both get a mutation-shaped
requirement in their dev spec, the same way D1 got one:

- **E6.** The gate set is named "FULL, zero tolerance", but a green gate only
  proves what the suites actually EXERCISE. Exposed unfoldings change what
  extraction sees; if every gate path happens to be unfolding-invariant, green
  proves less than its name claims. REQUIREMENT: the E6 dev must show that a
  deliberate MIS-tiering (e.g. denying optimized Core to a module that needs
  it) turns at least one named gate RED. An all-green run with no demonstrated
  detection power is not acceptance — it is the E6 analogue of a vacuous
  mutation test.
- **D2, MANDATORY-ROOTS HAZARD (cross-lane, from boot's ConTags audit; wave
  spec `df458ebc`).** `ConTags::try_from` (`effect_machine.rs` ~203) requires
  ALL FIVE freer scaffolding constructors — `Control.Monad.Freer.Val`/`.E`,
  `Data.OpenUnion.Union`, `Data.FTCQueue.Leaf`/`.Node` — and fails machine
  construction if any is absent. They are NOT in `wiredInDataCons`
  (`Translate.hs` ~2737, verified). On a PURE entry term (`pure (…)`, which is
  what item 0's boot path compiles first) only `Val` is reachable in Core, so a
  reachability-narrowed table captures none of the other four and boots
  nothing — surfacing as `MissingConTags` far from the diff.

  **REQUIREMENT:** carry the five as mandatory roots UNCONDITIONALLY — not "if
  reachable", not "if the effect row is non-empty" — plus a test pinning them
  through narrowing **on a pure entry term specifically** (a test over an
  effectful term passes while the real path breaks).

  **Supplier correction (mine, code-traced, reported upward).** The wave spec
  attributes their presence to `tyconMeta = collectDataCons tycons`. That
  cannot be right: `mg_tcs` is a module's OWN TyCons, the five live in the
  freer-simple PACKAGE, and freer-simple is not vendored under `haskell/` — so
  the home-TyCon sweep can never supply them. The actual route is
  `collectTransitiveDCons` (`Translate.hs` ~1094-1129): seeded from binder
  `idType`s, `closeTyCons` expands through `dataConOrigArgTys`, so from any
  binder typed `Eff …` it reaches `Eff`'s cons `Val`/`E`, then through `E`'s
  field types the `Union` and `FTCQueue` TyCons, yielding `Union`/`Leaf`/
  `Node`. **They ride on the binder TYPE, not on Core reachability.**

  Why the correction matters: the two stories point D2 at different things to
  protect. Under the spec's story a dev preserves the home-TyCon sweep; under
  the traced story the five are reachability-independent and break only if
  `RuntimeTypeClosure` replaces the binder-type closure — which is exactly what
  "target/result + boundary + session-bound types" reads like. A dev following
  the spec's framing could preserve `tyconMeta`, replace `transitiveMeta`, and
  ship the break while believing they had heeded the warning.

  NOT YET MEASURED. The D2 dev's FIRST step, before any design, is to dump a
  `meta.cbor` for a pure entry and attribute the five to a source empirically.
  The guard above is adopted regardless, being correct under either story.

- **D2 FOLD-ORDERING CONSTRAINT (wave TL owns it; binds when D2 can be called
  done).** Boot pinned the ConTags regression guard into
  `tidepool-harness/tests/acceptance_*.rs` so `-E 'binary(/^acceptance_/)'`
  selects it — correct placement, but **that test lives in boot's branch**.
  Until boot folds into `root.extract-wave` and I merge that base, my
  harness-acceptance shard does not contain the guard: it would run green over a
  suite that simply LACKS the test, and D2 would look gated while the thing
  built to catch D2's failure mode was absent from the run. Same defect one
  level up from the one boot just fixed.

  Rules: land and gate D2's implementation whenever; D2 is NOT done until its
  harness-acceptance shard has run on a base carrying boot's pins. The wave TL
  signals when they are on `root.extract-wave`; merge, re-run, and record THAT
  run as the receipt, **naming the base commit it ran against**. A per-binary
  pass count alone is insufficient here — a passing count over a suite missing
  the test is exactly the failure mode.

  Strengthening I am adding: the receipt must also show the guard test **by
  name with its own pass line**, not merely an aggregate count on a named base.
  Naming the base proves which tree ran; naming the test proves the guard
  executed. Cheap, and it closes the residual case where the pins are present
  but the shard's filter fails to select them.

- **D2.** "Fragment-reachable" is a label over a measurement taken pre-wrap on
  real session Core. D1's hard fail guards constructors that are EMITTED but
  absent from metadata — it does NOT guard a constructor the RUNTIME needs that
  the fragment never emits (Rust-side rendering of a sibling set is the
  plausible case; `dcmTypeName` /`constructors_of_type` exists precisely
  because Rust resolves a rendered type name to its constructor set). A
  narrowed table can therefore be green on the gate corpus and wrong on a path
  the corpus does not walk. REQUIREMENT: the D2 dev must name which
  runtime-observable roots cover that case and show the sibling-set rule is
  driven by what Rust actually asks for, not by what the fragment happens to
  build.

---

## C1 measurement — landed (2026-08-08/09)

Full report: `01-c1-measurement.md`. Summary and what changed mid-item, for
anyone reading this ledger before the report:

- **Bracket design was corrected once, before landing.** First instructed to
  NEST `depanal`/`load`/`inject` inside `ghc_session`; the wave TL then
  reversed that (nesting violates `timing.rs`'s documented "stages are FLAT"
  invariant) and asked for a PARTITION instead: `ghc_setup` + `ghc_load` as
  two flat, non-overlapping rows recovering the old `ghc_session` figure as
  their sum. The shipped code is the flat design only — nesting was never
  committed.
- **`ghc_session` is RETIRED on the compile lane, kept on the `--classify`
  lane** (`Binders.hs`'s `classifyBlock`, a much smaller span —
  `getSessionDynFlags` alone). One emitter, one meaning now; see
  `timing.rs`'s `PHASE_GHC_SESSION` tombstone.
- **`plans/self-iterating-harness/11-turn-latency-contract.md` is RETIRED**
  (`git rm`'d in the same commit that landed the bracket) in favour of
  `plans/self-iterating-harness/11-extract-timing-contract.md`, per an
  explicit root/Inanna done-criterion added mid-item. Every reference updated
  except `plans/post-restart/extract-wave.md:81` (root's own file, outside
  this namespace — left alone on request).
- **The sub-TL spec's "typecheck suspicion REFUTED" framing is WITHDRAWN.**
  The `typecheck` phase counts only ONE of the two typechecks a turn pays
  (`load'` redoes a first typecheck internally, undecomposed); the report
  treats home-module typecheck cost as OPEN.
- **Headline number is the `ghc_load`-share RATIO, demonstrated stable
  across two load arms** (1-min loadavg ~11→~34, `ghc_load/extract.total`
  moved <2pp), not an absolute — per a mid-item root throttle directive
  triggered by a box-wide load-92 incident. Absolutes are reported as
  contended upper bounds. `parMakeCount`/`-j` confirmed ABSENT (sequential
  `load'` and sequential second loop) — the mechanistic reason the ratio held.
- **Session path reached** (`runSessionPipeline`, 5 samples via
  `tidepool-repl::decl_plane::record_syntax_selectors_localized`). **A
  post-hoc self-audit ("audit every number, not just the corrected
  instrument") found the item's own first draft mis-stated the trigger** —
  it is a PRIOR BARE-EXPRESSION eval (any type; the auto-bound `it` alias),
  not "a user-defined ADT bind", falsified by cross-checking against
  `value_fidelity::bind_references_earlier_binding`'s plain-`Int` run, which
  also writes an `it` iface and would enter the session path too given a
  turn after it. The audit also found the "generation depth" column was
  reconstructed from `Wrote session iface:` output text (counting prior
  writes), not counted in-process — no code anywhere counts what a compile
  actually injects — so it is SUSPECT, not the trustworthy `Val.G<n>`
  live-count an earlier draft claimed. Both corrected in the report. Net
  effect unchanged: the depth axis reached is NOT `Lib.G<n>` (session-decl
  generation, the one E2's O(n²) concern is about). **The `Lib.G<n>` scaling
  question is an explicit, reported UNANSWERED gap** — no existing
  `tidepool-repl` test drives 5+ sequential decls in one session, and
  reaching the session-VALUE axis reliably would need in-process
  instrumentation of `injectSessionScope`'s injected-module count, which
  this item did not add.

---

## Item rows

*(measurement / decision / receipts filled in as each item folds)*

| Item | Status | Measurement | Decision | Receipts |
|---|---|---|---|---|
| C1 measurement | **done** (`c1-timing`) | `01-c1-measurement.md`. `ghc_load` (=`load'` alone) is 28–32% of `extract.total`, second loop (`typecheck`+`core`) 62–69%, combined ~96–97% — demonstrated stable across a 1-min-loadavg swing of ~11→~34 (two arms, 6 samples each, ratio moved <2pp). `ghc_load` is 97–98% of `ghc_setup+ghc_load` in both arms — almost none of the historical "session boot 26–32%" was boot. Typecheck-suspicion REFUTED framing withdrawn mid-item (measures one of two typechecks); treated as OPEN. Session path reached (5 samples, sequence order) but the generation-count label is reconstructed from output text (SUSPECT, see report), not `Lib.G<n>` decl-chain scaling (UNANSWERED). | Evidence only, no decision recorded here (that's the wave TL's per spec) — see report §"Answers (3)" | wire-inertness ×2 (stdout+files byte-identical); `extract-fidelity-test` 26/26; `tidepool-harness` acceptance shard 24/24, 0 failed; `cargo check --workspace` clean |
| D1-A defense | **done** (`d1-defense`, folded) | CHECK A (emitted `NCon`/`FDataAlt` ids ⊆ meta) hard-fails pre-write; CHECK B (independent syntactic Core visitor, never calls the translator) downgraded to diagnostic — its invariant is false by design. `scanMeta` still present (removal is D1-B). | Both mutation legs name CHECK A — two constructors, two call sites. CHECK B's fatality retired on measured evidence, not preference. Does NOT move the wire (checks run pre-write, no output bytes change on a passing extraction). | `extract-fidelity-test` **30/30** (4 D1Defense checks by name, incl. an anti-vacuity control and a permanent pin on CHECK A's message); harness acceptance **20/24 → 24/24**; `corpus_report` **1/1**; `cargo nextest` quick **1862/1862, 9 skipped**; both mutation legs exit 1, output dir empty; `git status` restore proof empty (scoped + full-repo) |
| D1-B removal | queued | — | — | — |
| E6 tiered `-O2` | **done** (`e6-tiered-o2`, report `02-e6-tiered-o2.md`) | Reachable-module rule (target + desugar-stage transitive `Var`-reference closure) tiers `core2core` off for validation-only modules in `runNormalPipeline` ONLY (`runSessionPipeline` untouched — scope decision, see report). 14-module/10-excluded fixture: `core` 2948.5→933.3ms (31.7% of former); `core2core` alone estimated 2908.8→893.7ms (30.7% of former, ~3.25×) — single load regime, not independently load-swing-verified like C1. Session-path consequence stated explicitly: this tier lands on turn-1-shaped extracts, NOT the turn-2+ session path real dogfood sessions dominate. Wire moves when tiering actually excludes ≥1 module (byte-inert otherwise) — root ruled ordinary wire-moving change (new trigger, not new class, per the pre-E6 two-import-breadth control). Three pinned id-stability tests PASS, framed as "these properties are intact," not "id stability is intact" — none observes `localVarId`, the mechanism perturbed. Soundness argument's one unenforced assumption (no `{-# RULES #-}` in home Core, empirically true but previously unguarded) is now enforced by `tidepool-codegen/tests/e6_no_rules_pragma.rs`, quick tier, positive-controlled; scoped to HOME modules only, with the scope justified (a package RULE's RHS is scope-resolved at its own package definition site and structurally cannot name a home module — package RULES DO fire here, `canonicalizeDFlags` never disables them, but cannot reach across the package/home boundary). | **"detection power demonstrated" ≠ "the battery would have caught it."** Detection power: mis-tiering `Tidepool.Aeson.Value` (genuinely called by the fixture) produces `BOTH_FAIL kind=4 TypeMetadata` via a probe on the real `check_jit_vs_eval_captured` oracle — but **no standing named gate catches it** (`extract-fidelity-test`'s fixtures never touch JSON/Aeson; `haskell_suite_differential`/`corpus_report` read static pre-generated CBOR and never invoke the extractor at all — a wave-wide battery-validity finding, escalated separately, bigger than this item). **Risk-profile finding: the tier's reachability computation is the ONLY barrier between a wrong exclusion and silent-at-compile/loud-at-JIT corruption** — `load'`-unfolding-fallback safety theory tested and REFUTED empirically (sentinel fires exactly as documented; no fallback mechanism catches a wrong exclusion on its behalf). | `cargo nextest` quick **1875/1875, 9 skipped** (incl. the new RULES guard, named); `extract-fidelity-test` **30/30** (baseline AND with the fault injected — the latter is the coverage-hole finding, not a pass on the actual fault); `haskell_suite_differential` **1/1**, `compared=312` vs `COMPARED_FLOOR=300` (identical before/after by construction — static fixtures, never regenerated by this item); `corpus_report` **1/1** (same invariance); harness acceptance **24/24, 0 skipped**; `cargo check --workspace --all-targets` clean |
| D2 runtime closure | queued | — | — | — |
| **Pivotal: persistent extractor** | **DECIDED 2026-08-08** | boot = 0.6–1.9% of extract.total (`startup`+`ghc_setup` vs total); `ghc_load` 97–98% of `ghc_setup+ghc_load`; double compile ~96–97% combined | **Persistent server REJECTED on the latency/boot argument** (E4 amortization survives, unmeasured); **C1 local fix promoted ahead of both**; **precompiled (FAT) interfaces = surviving candidate, commitment DEFERRED** pending E2 generation-scaling + E4 fat-iface bytes. Full reasoning in the section above. | evidence = `01-c1-measurement.md` |

---

## THE PIVOTAL DECISION — persistent extractor — RECORDED 2026-08-08

Made on C1's measurement (`01-c1-measurement.md`), as the wave done-criterion
requires. Three parts; the first is a rejection, the second a promotion, the
third a bounded deferral with the measurement attached.

### 1. PERSISTENT SERVER — REJECTED on the boot argument, which is measured

A persistent server's distinctive benefit over precompiled interfaces is
avoiding per-turn process and GHC-session establishment. That is now measured
directly, and it is nearly nothing:

    startup     44–91 ms
    ghc_setup   49–142 ms   (DynFlags setup + guessTarget/setTargets + depanal)
    ------------------------------------------------------------------
    combined    ~93–233 ms  against extract.total of 8,105–17,424 ms
                = 0.6–1.9% of a turn's extract

`ghc_load` is **97–98% of `ghc_setup + ghc_load` in both load arms.** The
historical "session boot 26–32%" was never boot — it was `load'` performing a
full first compile of every home module. Boot, properly bracketed, is ~1%.

A resident process costs lifecycle management, crash recovery, cross-turn cache
invalidation, and a new class of state-leak bug. **1–2% does not buy that.**
Every other benefit a persistent server would deliver is compiled-state
retention, which precompiled interfaces deliver with strictly less machinery.

Supporting: `inject` measured **0 ms** at every sampled session turn — splicing
live thin ifaces into the HPT is free. The interface direction has no measured
overhead at the point where it would show up.

**SCOPE OF THE REJECTION — DEPTH (amended 2026-08-08, wave TL's residual).**
`ghc_setup` is setup + `depanal`, and `depanal` walks the module graph, which
the `Lib.G<n>` chain GROWS. Every boot figure above was taken with that axis
pinned at `Lib.G1` (the session vehicle's decl compiled once and never grew;
top-level bindings flat at 1706–1709). **So the rejection is scoped: the server
is rejected on boot cost FOR SESSIONS AT THE DEPTHS SAMPLED.** Depth-scaling of
`ghc_setup` is the one measurement that could reopen it — and a repeated
`depanal` over a growing graph would be a residency argument arriving through
the one door the latency scoping did not close.

Asserting part 1 at full strength while part 3 defers *because* that same axis
is unmeasured would have been inconsistent. It is now scoped instead.

**Why this is not a separate errand:** `ghc_setup`-vs-depth comes from the SAME
vehicle and the SAME run as part 3's already-required `Lib.G<n>`
compile-time-vs-generations measurement. It is one more column, not another
experiment. Folded into that prerequisite rather than queued alongside it.

**Design constraint that measurement must respect (from the data already in
hand).** At a FLAT module graph, `ghc_setup` ranged **68→287 ms across five
session turns — a 4.2x spread with zero module growth.** That is the noise
floor. A depth sweep of 2–3 generations cannot clear it; the sweep needs
roughly 8–10 generations before a trend is distinguishable from contention.
This is also why option (1) was not "one run": no existing `tidepool-repl` test
drives more than 2–3 sequential `repl.def(...)` calls, so the vehicle has to be
built.

**Prediction, recorded as a prediction and NOT as evidence:** `depanal` is a
header-parse downsweep, O(n) in module count with a small constant, whereas
E2's O(n²) concern is in the compile chain. If so, `ghc_setup`'s SHARE should
SHRINK with depth as the denominator grows faster, hardening part 1. If the
measurement contradicts this, that contradiction is the finding and part 1
reopens.

**SCOPE OF THE REJECTION, stated so it is not over-read:** rejected on the
LATENCY argument. One persistent-server argument survives untouched because
this measurement does not address it — **E4's per-process cache death** (the
FatIface decode cache dies with each disposable extractor, so every turn
re-decodes). That is a genuine amortization argument for residency, it is
UNMEASURED, and it is not refuted here. If it is ever the reason to build a
server, it must be measured first and argued on its own terms — not smuggled
back in on the boot argument, which is now closed.

### 2. NEITHER, FIRST — C1's own fix is promoted ahead of both

    ghc_load                    28–32% of extract.total
    typecheck + core (2nd loop) 62–69%
    ------------------------------------------------
    combined                    ~96–97%

Nearly the entire extract is **two back-to-back full compiles of the same
home-module set inside ONE process.** That waste is *within-process*, so no
persistence architecture is required to recover any of it. Eliminating the
redundant `load'` is worth **~30% of every extract** for a local change, needs
no new architecture, and re-bases the arithmetic either architectural option
would be sized against. Doing persistence first would be optimizing the
survivor of a duplication we have not yet removed.

Demonstrated contention-robust: the ratio moved <2 pp across a 1-min loadavg
swing of ~11→~34 (two arms, n=6 each), with a mechanism — the prerequisite
check confirmed `parMakeCount` is never set, so `load'` and the second loop are
both sequential and contention has no structural reason to degrade them
asymmetrically.

### 3. PRECOMPILED INTERFACES — surviving candidate, NOT yet committed

Deferred, with the measurement attached and the two deciding measurements
named. After C1's fix, ONE compile of the home-module set per turn remains
(~65% of today's extract). Whether interfaces beat that turns on two figures
this measurement does NOT provide:

- **compile time vs `Lib.G<n>` generations** — E2's O(n²) home-module-chain
  axis. Explicitly UNANSWERED. The session-path vehicle reached 5 samples but
  its `data P = …` decl compiled ONCE (`Lib.G1`) and never grew; top-level
  bindings stayed flat at 1706–1709 throughout, so nothing could have scaled.
  No existing `tidepool-repl` test drives 5+ sequential `repl.def(...)` calls
  (deepest found: 2–3). Needs a new vehicle, or in-process instrumentation of
  `injectSessionScope`'s actual injected-module count.
- **fat-iface bytes and decode cost per turn** — E4. Unmeasured. This is the
  cost side of the interface option and the amortization case for residency;
  one measurement bears on both.

Interfaces additionally must carry unfoldings: `GhcPipeline.hs`'s PHASE 3
comment records that iface resolution WITHOUT `-O2` unfoldings bakes
`ErrorSentinel`s that surface later as `kind=4 TypeMetadata`. So "precompiled
interfaces" here means FAT ifaces, which is exactly why the E4 number is a
prerequisite and not a nicety.

**This is a deferral WITH the measurement attached and its coverage stated, per
the sanctioned form — not a deferral for want of one.** What it defers is the
commitment to build; what it decides, on measured evidence, is that the server
option is not the answer to the latency problem and that C1's local fix comes
before either.

## Wire-moving items flagged to root

| Item | Moves the wire? | Evidence |
|---|---|---|
| C1 (`c1-timing`, folded) | **NO** | Timing is stderr-only and env-gated. Proven, not assumed: wire-inertness checked TWICE (once on the superseded nested build, once on the shipped flat partition) — `TIDEPOOL_TIMING` unset vs `=1` produced byte-identical stdout AND byte-identical emitted files for the same input, `diff -rq` empty on both. This item's named guard; shown by name in its receipts. |
| E6 (`e6-tiered-o2`, done) | **YES, conditionally** — inert when nothing is excluded, moves when tiering excludes ≥1 module | `cmp` byte-diff on a 14-module/10-excluded fixture (same pre/post binary, same input); byte-IDENTICAL on a 2-module/0-excluded control; same pre-E6 binary run twice is byte-identical (rules out run-to-run noise). Mechanism: `localVarId` (internal/floated bindings) hashes the raw GHC `Unique`, allocation-order-sensitive by the code's own doc comment; a pre-E6-only two-import-breadth experiment showed this context-dependence PRE-EXISTS E6 (same fixture, same pre-E6 binary, only import breadth differs → internal ids already differ) — E6 is a new TRIGGER for an existing property, not a new CLASS of instability. Root ruled: ordinary wire-moving change, enters the redeploy set. Full detail in `02-e6-tiered-o2.md`. |

Remaining items are assessed as they land. D1-A is expected inert (a hard-fail
check that changes nothing on a passing extraction); D1-B and D2 both change
what metadata is COLLECTED or what Core is produced, so each must state its wire
status explicitly rather than inherit C1's or E6's.

## Fold conflicts

*(logged here as they occur; expected overlap with sub-TL `boot` is
`haskell/app/Main.hs` and `tidepool-runtime/src/session/compile.rs`)*
