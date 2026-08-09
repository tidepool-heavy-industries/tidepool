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

Ordering is set by the Phase-B breakdown (core-phase dominant; the home-module
typecheck suspicion REFUTED), not by spawn count. No item below is sized off a
spawn-count argument.

| Wave | Dev | Item | Why here |
|---|---|---|---|
| 1 | `c1-timing` | C1 measurement | Cheap, gates the pivotal decision, and must run before any C1 fix is even proposed. Touches `GhcPipeline.hs` + `Timing.hs` only. |
| 1 | `d1-defense` | D1 part A — the defense, additive | Ships the hard-fail subset check, the independent Core visitor, and the mutation test with the second translation STILL PRESENT. Zero behaviour change if the two translations agree; a loud failure if they don't. Touches `Translate.hs` + `Main.hs` + `test-fidelity/`. |
| 2 | `d1-remove` | D1 part B — remove the second translation | Only safe once part A's hard fail exists. Deletes `scanMeta`. Folds together with part A as ONE landing on this branch before `submit_branch`. |
| 2 | `e6-tiered-o2` | E6 — tiered `-O2` | Promoted by the measurement. Semantics-sensitive, FULL gate set at zero tolerance. |
| 3 | `d2-runtime-closure` | D2 — `RuntimeTypeClosure` | The chain root. Sequenced after D1 because D1's hard fail is what makes a reachability-narrowed table safe to ship. |
| 4 | as capacity | C2/E5, E1–E4 | Sized honestly against the measurement, not against the old model. |

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

## Item rows

*(measurement / decision / receipts filled in as each item folds)*

| Item | Status | Measurement | Decision | Receipts |
|---|---|---|---|---|
| C1 measurement | in flight (`c1-timing`) | — | — | — |
| D1-A defense | in flight (`d1-defense`) | — | — | — |
| D1-B removal | queued | — | — | — |
| E6 tiered `-O2` | queued | — | — | — |
| D2 runtime closure | queued | — | — | — |
| **Pivotal: persistent extractor** | BLOCKED on C1 | — | — | — |

## Wire-moving items flagged to root

*(none yet — an item that moves the extract wire is reported to `extract-wave`
for root's redeploy set the moment it is identified)*

## Fold conflicts

*(logged here as they occur; expected overlap with sub-TL `boot` is
`haskell/app/Main.hs` and `tidepool-runtime/src/session/compile.rs`)*
