# D2 — `RuntimeTypeClosure` — SPECIFIED, UN-STARTED, ROUTED FORWARD

**Status 2026-08-11 (lane `spawn-latency`): STILL UN-STARTED — BLOCKED ON A
SIBLING LANE, not on doubt.** Every claim below has been re-validated against
HEAD (`fc94ac3b`) and the adjustments live in
`plans/post-restart/extract-wave/spawn-latency/04-turn-latency-plan.md` §1 and
§1a. **Read that alongside this file; where they disagree, §1 is newer.**

Why blocked: D2's entire edit is the `allMeta` assembly in
`haskell/app/Main.hs` (`:406-416` in `processFile`, `:580-605` in
`writeClosedTargets`), and the in-flight `batch-turns` lane owns that file.
There is no version of D2 that does not touch it. It picks up when that lane
folds.

What §1/§1a add on top of this document:
- The premise HOLDS at HEAD (unfiltered `mg_tcs`, CHECK A live, the traced
  binder-type route intact, `wiredInDataCons` still lacking the five).
- **This file's headline requirement is unimplementable as literally written.**
  "DERIVE the five from `tidepool_repr::freer_names`, never hand-list" — but
  `freer_names` is a RUST module and the narrowing happens in Haskell, which
  cannot import it, and no Haskell-side mirror of those names exists. §1a
  reframes it into the constraint that actually delivers the intent: *do not
  replace or bypass the binder-type closure* (it supplies all five with no
  list on either side of the boundary).
- Four more things moved under this doc: `--targets` created a SECOND
  metadata-assembly site (`processFile`'s, with no CHECK A); `meta.cbor` is now
  merged ACROSS targets, a per-target-vs-union closure hazard that did not
  exist at hand-off; `isTypeMetadataVar`'s prefix-only match is still unfixed
  here so §4's `kind=4` ambiguity is live; the compile memo means any
  extract-cost measurement must pin its own memo dir.
- §5's receipt requirement is re-sized: standing hazard 6 applies to D2 exactly
  as it did to D1-B, since `allMeta` is forced by the UNTIMED
  `assertMetaCoversEmitted`. Size the win on `meta.cbor` bytes/entries and
  `cbor_encode`, not a `translate`-phase delta.

---

**Status: CUT from the extract-wave (Inanna, 2026-08-09, wrap-up directive). No
code was written. No dev was spawned.**

This is the hand-off. Everything below was established during the wave at real
cost — several of the items are corrections to things that were confidently
wrong on first statement — so the next agent should **inherit the reasoning
rather than re-derive it**. Where a claim here is unverified, it says so.

---

## The item

Metadata is currently every constructor of every home-module TyCon with no
reachability filter (`mg_tcs` → `collectDataCons`, `Translate.hs` ~2718). Fix:
a `RuntimeTypeClosure` built from runtime-observable roots — constructors built
or matched in reachable Core, sibling sets where rendering needs them, and
target/result + boundary + session-bound types.

It is the chain root: it shrinks the DataConTable, the wrapper chain, and the
CBOR together.

**Size it from these figures and no others** (jit-chain, 2026-08-09, pre-wrap on
real session Core): turn 1 table = 164 constructors, fragment-reachable = 24
(6.8:1, ~15%); turn 2 table = 166, reachable = 15 (11.1:1, ~9%). **Carry the
caveat with the number: "single-digit-to-low-teens percent reachable", NOT
"hundreds vs dozens".** Any writeup that rounds this up is wrong.

**Sequencing rationale, which is now satisfied:** D2 was to follow D1 because
D1's hard fail is what makes a reachability-narrowed table safe to ship. **D1-A
landed that hard fail (CHECK A) and is folded**, so D2's dependency is met.
D1-B (the `scanMeta` removal) was only ever a diff-collision constraint, not a
design dependency.

---

## 1. THE MANDATORY-ROOTS HAZARD — a correctness requirement, and its supplier was misattributed

`ConTags::try_from` (`tidepool-codegen/src/effect_machine.rs` ~203) requires ALL
FIVE freer scaffolding constructors — `Control.Monad.Freer.Val`/`.E`,
`Data.OpenUnion.Union`, `Data.FTCQueue.Leaf`/`.Node` — and fails machine
construction if any is absent. They are NOT in `wiredInDataCons`
(`Translate.hs` ~2737, verified).

On a **PURE entry term** (`pure (…)`, which is what item 0's boot path compiles
first) only `Val` is reachable in Core. So a reachability-narrowed table
captures none of the other four and **boots nothing** — surfacing as
`MissingConTags` far from the diff.

**REQUIREMENT:** carry the five as mandatory roots UNCONDITIONALLY — not "if
reachable", not "if the effect row is non-empty" — plus a test pinning them
through narrowing **on a pure entry term specifically**. A test over an
effectful term passes while the real path breaks.

### The supplier correction — this changes what you must protect

The wave spec originally attributed their presence to `tyconMeta =
collectDataCons tycons`. **That cannot be right**: `mg_tcs` holds a module's OWN
TyCons, the five live in the freer-simple PACKAGE, and freer-simple is not
vendored under `haskell/` — so the home-TyCon sweep can never supply them.

**Traced route: `collectTransitiveDCons`** (`Translate.hs` ~1094-1129). It seeds
from binder `idType`s and `closeTyCons` expands through `dataConOrigArgTys`, so
from any binder typed `Eff …` it reaches `Eff`'s cons `Val`/`E`, then through
`E`'s field types the `Union` and `FTCQueue` TyCons, yielding `Union`/`Leaf`/
`Node`. **They ride on the binder TYPE, not on Core reachability.**

Why it matters: the two stories point at different things to protect. Under the
spec's story you preserve the home-TyCon sweep; under the traced story the five
are reachability-independent and break only if `RuntimeTypeClosure` replaces the
**binder-type closure** — which is exactly what "target/result + boundary +
session-bound types" reads like. A dev following the original framing could
preserve `tyconMeta`, replace `transitiveMeta`, and ship the break while
believing they had complied.

**NOT MEASURED.** The traced route is code-read, not observed. **D2's FIRST STEP,
before any design: dump a `meta.cbor` for a pure entry term and attribute the
five to a source empirically.** Bank the answer either way — including if it
shows neither story was right. That answer is worth more than the one confirming
either of us.

### Derive the roots, do not list them

`eval_harness.rs:387` documents that a hand-maintained list can drift AND cites
the prior drift (`f1a480e6`) — then it drifted again, which is what broke
`EFFECT_NAMES` during this wave. **A comment saying a mechanism can fail
silently is a recorded decision to keep it, and it reads as diligence while
providing none.**

So the five must be **DERIVED** from `tidepool_repr::freer_names` — the single
source `ConTags::try_from` itself resolves against — never hand-listed. Verbatim
into the dev spec, because the requirement alone does not rule out its own worst
implementation:

> *A hand-listed set with an explanatory comment would be the identical
> mechanism that just broke, reintroduced by the item whose whole purpose is
> narrowing that table.*

---

## 2. THE GATE SET — corrected, and two of the four carry NO SIGNAL

**Verified during the wave, independently by two agents:**

    haskell_suite_differential   0 refs to Command/extract/compile_haskell;
                                 replays 350 frozen .cbor from suite_cbor/
    corpus_report                replays 128 frozen .cbor from corpus_cbor/

**Neither invokes the extractor.** They are JIT-vs-eval differentials over
pre-generated CBOR, and they produce identical results whether the extractor is
correct or catastrophically broken. **Do not cite them as coverage for D2.** The
wave's original gate bar named the hardened differential with `COMPARED_FLOOR`
as *the* gate for extractor changes; that was wrong for E6, D1-B and D2 alike.

**The honest coverage set for a metadata-narrowing change:**

1. **D1's CHECK A** (`Main.assertMetaCoversEmitted`) — hard-fails extraction
   when an emitted `NCon`/`FDataAlt` id is absent from `meta.cbor`. This is
   D2's primary detector and the reason D2 is safe to attempt at all.
   **Its limit, which matters for D2 specifically:** it guards constructors that
   are EMITTED. A constructor the RUNTIME needs but the fragment never emits is
   invisible to it — and Rust resolving a rendered type name to its constructor
   set (`dcmTypeName` / `DataConTable::constructors_of_type`) is exactly that
   case. **D2 must name which runtime-observable roots cover it**, and show the
   sibling-set rule is driven by what Rust actually asks for, not by what the
   fragment happens to build.
2. **`extract-fidelity-test`** — real pipeline end to end. KNOWN HOLE: its
   fixtures (erasure symmetry, recognizer qualification, unboxed-tuple arity, D1
   defense) never touch JSON/Aeson, so Aeson-touching extractor behaviour is
   uncovered. Scheduled as fixture work by root; unfixed at hand-off.
3. **harness acceptance** — spawns real extracts end to end. The strongest real
   signal, ~1845s.

**FIXTURE REGENERATION IS A SEQUENCED ROOT/WAVE ACTION, never a lane's call**
(ruled box-wide). Shared directories, other lanes in flight, redeploy-class
blast radius — and booby-trapped: `haskell/CLAUDE.md` warns that pruning
`*_u<n>.cbor` from `suite_cbor` drops `compared` below `COMPARED_FLOOR`, so a
naive regeneration **breaks the very floor it was meant to protect.** Route
requests upward.

---

## 3. THE PINNED ID-STABILITY TRIO — enumerated, and D2 is the item they actually guard

The phrase *"extractor id-stability is a PINNED invariant"* appears in three
documents. **None of them enumerated the set** — it was a load-bearing citation
with no referent, and the membership was guessed (wrongly, twice) before someone
finally ran them.

**ENUMERATED (wave `f181eb33`), by exact path:**

    tidepool-repr::extend_checked_equivalence::distinct_ids_sharing_a_qualified_name_collide_regardless_of_input_order
    tidepool-repr::extend_checked_equivalence::merge_table_skip_filter_cannot_dodge_the_qualified_name_collision_guard
    tidepool-runtime::session_table_qualified_identity

**All three are DataConId qualified-name guards. None observes VarIds at all.**

**THE INVERSION THAT MAKES THEM MATTER HERE:** the trio was near-useless for E6
(which perturbs `localVarId`) — but it is **directly on point for D2**, which
narrows the `DataConTable` itself. A reachability-narrowed table is exactly the
change that could drop a constructor or collide a qualified name, which is
precisely what all three observe. **So "if the pinned tests fire, STOP and
escalate" is silence for E6 and a real guard for D2.** D2 is the one item in
this lane where that trio earns its citation.

State in the spec what D2 needs each test to prove, and confirm it observes
that, **before** treating a pass as coverage — the reliance-not-audit rule
applied ahead of the fact. Every guard gap found during this wave was found by
someone relying on a guard, never by anyone auditing guards.

---

## 4. `kind=4 TypeMetadata` IS AMBIGUOUS — check the base before attributing

The signature now has **at least two distinct causes**:

1. A wrongly-excluded module under E6's tier (poison `ErrorSentinel`, PHASE 3's
   documented failure class).
2. `Translate.isTypeMetadataVar` (`Translate.hs` ~2868) matching on
   OCCURRENCE-NAME PREFIX ONLY (`$trModule`/`$krep`/`$tc`/`krep$`/`tr$Module`)
   with no RHS inspection — float-out/CSE gives Generic-deriving's
   `KnownSymbol` backing strings those prefixes, poisoning a load-bearing
   `Addr#` literal. A root dev was fixing this at hand-off time.

**D2 narrows the very table this poisoning path feeds.** If D2's verification
surfaces a `kind=4`, the first question is *which of the two* — and the fix may
have landed since. **Check `isTypeMetadataVar`'s state at the base you are
testing against before attributing anything to reachability narrowing.**

---

## 5. Standing constraints that apply

- **ONE EMISSION PATH.** The turn mode reaches translation through the shared
  `writeWholeModuleClosed`. Do NOT add a second route to
  `translateModuleClosed`. Do NOT touch `Translate.hs`'s recognizer /
  qualification tables.
- **Wire status must be stated with evidence, never inherited.** D2 changes what
  metadata is emitted, so it will almost certainly move the wire. Flag it for
  root's redeploy set. (D1-A did not move it; C1 did not; E6 did.)
- **`Translate.hs` and `Main.hs` conflict expectations:** a root dev's surgical
  `isTypeMetadataVar` diff, plus whatever D1-B landed. Mechanical, different
  functions. Log anything non-mechanical.
- **Receipts:** actual `N passed / M total` per leg — never a hardcoded expected
  count, which converts the denominator rule from a check into a lookup and
  fails in the direction that looks correct. Named guards shown BY NAME with
  their own pass lines. Cross-lane guards additionally name the base commit.
- **D2's harness-acceptance gate is not satisfied on a base predating boot's
  ConTags pins** — the guard lives in boot's branch. The receipt must name both
  the base commit AND the guard test by name: base proves which tree ran, the
  test name proves the guard executed.
