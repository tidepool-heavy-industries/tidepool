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
| D1-A defense | in flight (`d1-defense`) | — | — | — |
| D1-B removal | queued | — | — | — |
| E6 tiered `-O2` | queued | — | — | — |
| D2 runtime closure | queued | — | — | — |
| **Pivotal: persistent extractor** | BLOCKED on C1 → **C1 done, evidence in** | — | Not yet recorded — wave TL's call, per spec | — |

## Wire-moving items flagged to root

*(none yet — an item that moves the extract wire is reported to `extract-wave`
for root's redeploy set the moment it is identified)*

## Fold conflicts

*(logged here as they occur; expected overlap with sub-TL `boot` is
`haskell/app/Main.hs` and `tidepool-runtime/src/session/compile.rs`)*
