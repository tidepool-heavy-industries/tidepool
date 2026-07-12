# Pointing tidepool-repl at tidepool: three signals, one culprit

*Session notes, 2026-07-05. Raw material for a "what is the stateful repl actually
FOR" post. The thesis under test: the win isn't a faster bash pipeline — it's
answering questions no static tool can, by folding a substrate into a derived
signal and keeping the substrate off-context.*

## The setup

One question, asked live: *what does 6 months of git history say about where
this codebase is risky?* Everything ran through `tidepool-repl` — a GHCi-style
session where the value heap persists across turns. No script written to disk, no
dashboard, no jq. Just bind-a-substrate-once and fold it many ways.

## The context-economy receipt (the part that matters)

The raw material was big:

- `git log --name-only` over 6mo → **472,107 characters**
- parsed to `commits :: [(Text,[Text])]` → 1150 commits, **10,694 file-touches**
- a co-change pair map → **3074 distinct file pairs**
- commit subjects → 1316 classified by type

**None of that ever entered the model's context window.** It lived in the session
heap. Across ~20 turns the model received: a handful of integers, a few ~12-row
ranked lists, and three small JSON summary objects. Every heavy value was folded
*in the session* (`T.length`, `sum`, `Map.fromListWith`, `L.sortOn`) and only the
conclusion crossed the boundary.

That's the difference from bash. In bash, the second time you want to slice the
same data you either re-run `git log | grep | sort | uniq` from scratch or you
`cat` the intermediate into context and eyeball it. There's no typed resident
value to hold, so the data leaks into the window by default. Here the default is
inverted: it stays put unless you explicitly ask for a fold of it.

## What we actually found

### 1. Cross-language coupling — the signal no static tool can see

For each commit, form all unordered pairs of files it touches; tally across all
commits. Then classify each strongly-coupled pair by what edge could *possibly*
link it:

- **same-crate** (58 pairs) — healthy cohesion, the compiler sees these
- **cross-crate Rust↔Rust** (17) — should have a `use`/Cargo edge
- **cross-language Haskell↔Rust** (11) — *no static edge can ever exist*

The 11 cross-language pairs are the real hidden coupling. No call graph crosses
the HS↔RS boundary; no shared symbol exists for grep to match. The coupling lives
**only** in the temporal signal of what-changes-with-what. Folding per-Haskell-file:

```
Translate.hs   → bound to 71 Rust files   (weight 238)
Prelude.hs     → bound to 51 Rust files   (weight 147)
Main.hs        → 43   GhcPipeline.hs → 38   CborEncode.hs → 29
```

`Translate.hs` (GHC Core → Tidepool IR) is the measurable Haskell half of the
"hylo boundary" the project's tagline names. Touch the IR shape there and 71 Rust
files are in the blast radius with zero compiler warning.

### 2. Layering honesty — cross-referencing the project's OWN dep graph

Used the repo's own `Repo.crateGraph` verb (tidepool analyzing tidepool) to ask:
of the 17 cross-crate Rust co-change pairs, how many have no declared Cargo
dependency either direction? **Answer: 1 of 17** — and that one (`examples ↔
tidepool-mcp`) turned out to be a measurement artifact (`examples/` is a
directory of two crates, not a graph node). So: **every genuine cross-crate
coupling is dependency-backed.** The declared architecture matches the temporal
reality. Clean result, and the probe incidentally exposed a limit of the
dir≈crate heuristic — which we reported rather than hid.

### 3. Fix-magnetism — joining churn to commit INTENT

Pulled commit subjects, classified by conventional-commit prefix (feat 370, fix
301, test 155, ...), joined sha→type back onto sha→files. Per hot file, the
absolute number of `fix` commits:

```
Translate.hs   38 fixes / 95 commits    boundaryWeight 238
emit/expr.rs   29 / 86
mcp/src/lib.rs 27 / 155
host_fns.rs    26 / 94
```

The whole GHC-extraction frontend (Resolve 52%, GhcPipeline 47%, Main 42%,
Translate 40% by *ratio*) is the most bug-prone surface — which tracks, it
wrestles GHC internals.

## The synthesis — three orthogonal lenses, one culprit

| Signal (different substrate each) | Translate.hs | rank |
|---|---|---|
| Absolute bug-fixes | 38 | #1 |
| Cross-language boundary weight | 238 (71 Rust files) | #1 |
| Total churn | 95 commits | #2 |

Temporal coupling, commit-intent, and raw churn are computed independently and
all point at the same file. That convergence is the strongest kind of finding —
not an artifact of one metric. If you wanted one file to harden or split, the
data has no ambiguity: `Translate.hs`.

## The transferable method (this, not the churn demo, is the asset)

1. **Substrate once, folds many.** Pull the expensive raw thing ONE time, bind
   it, then every question is a cheap fold over the resident value.
2. **Helpers each add one lens.** Grow a mini analysis library across turns —
   each item one composable fold, not one cramped mega-expression.
3. **Normalize for surprise, not volume.** Raw counts rank the *busy*; a
   coefficient (Jaccard = co/(a+b−co)) ranks the *surprising*. It flipped the
   ranking once — `eval↔repr` (0.29) beat the noisier `codegen↔runtime`.
4. **Classify to separate expected from smell.** Bucket by whether a static edge
   *could* exist. The smell is the coupling the type-checker can't see.
5. **The temporal axis is a first-class data source** — orthogonal to code
   structure. Reach for it when "what moves together" matters and static analysis
   comes up empty.

## Honesty notes (kept, because they're the credibility)

- The one apparent layering violation was a `crateTag` bucketing artifact, not a
  real break — said so.
- `boundaryWeight: 0` on Rust rows is *by construction* (the metric only indexes
  the Haskell side of cross-language pairs), not "those files are safe" — said so.
- Self-corrected mid-run twice rather than overclaiming.

## Friction encountered (product telemetry)

- Prelude partial-shadowing (`head`/`tail`/`!!`) fired three times — each a clean
  compile error naming the safe form (`L.head`, pattern-match in comprehension).
  Working as designed; a mild fluency tax on reflexive Haskell.
- Record-dot ate `length.snd` as a field selector — needs `length . snd` with
  spaces. Real ambiguity of the record-dot surface.
- An interrupted turn wedged the session "busy"; `session_reset` (drops the heap)
  was the only lever. Rebuilding the substrate in one batched call was fast enough
  that it didn't hurt — but a non-heap-dropping abort would've been nicer.

## One-line pitch

*Expensive once, resident forever, aggregate don't dump — and the payoff isn't a
faster grep, it's a signal grep can't produce.*
