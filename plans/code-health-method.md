# How to use tidepool for code health analysis

Companion to `code-health.md`. Documents the patterns used to produce the
findings so they can be reproduced and extended.

## The core trick

One tidepool eval replaces 5-15 bash commands because Haskell composes effects
(glob, read, ast-grep) with pure transforms (filter, sort, group, aggregate)
in a single expression. The result is structured JSON, not text to parse.

## Pattern 1: Census with foldMap

Multi-aggregate in one pass. No intermediate variables needed.

```haskell
send (FsGlob "tidepool-*/src/**/*.rs")
  >>= mapM (\f -> send (FsRead f) <&> \c -> (T.takeWhile (/= '/') f, length (T.lines c), length [l | l <- T.lines c, T.isInfixOf "unsafe" l]))
  <&> foldMap (\(crate,loc,u) -> Map.singleton crate (Sum loc, Sum u))
  <&> Map.map (\(Sum l, Sum u) -> object ["loc" .= l, "unsafe" .= u])
  <&> toJSON
```

## Pattern 2: Intra-file duplication detection

Pairwise line-overlap comparison within each file. Pure Haskell, O(n²) per file
but only over function bodies (not full files). Bash can't do this at all —
there's no text-processing pipeline for "find pairs of functions with >65%
shared lines."

```haskell
send (FsGlob "tidepool-*/src/**/*.rs") >>= mapM (\f -> send (FsRead f) <&> (f,))
  <&> concatMap (\(f, c) ->
    let ls = T.lines c
        starts = [n | (n, l) <- zipWithIndex ls, T.isInfixOf "fn " l, not (T.isPrefixOf "//" (T.strip l))]
        bodies = zipWith (\s e -> (s, map T.strip . filter (not . T.null . T.strip) . take (min 80 (e - s)) . drop s $ ls)) starts (drop 1 starts ++ [length ls])
    in [(T.takeWhileEnd (/= '/') f, s1, s2, shared * 100 `div` total)
       | (s1, b1) <- bodies, (s2, b2) <- bodies, s1 < s2,
         let shared = length (filter (`elem` b2) b1), let total = max 1 (length b1),
         shared > 5, shared * 100 `div` total > 65])
  <&> sortBy (flip compare `on` (\(_,_,_,p) -> p)) <&> take 15 <&> toJSON
```

## Pattern 3: ast-grep + Set operations

Structural code search composed with set algebra. "Files that use .unwrap()
but not .expect()" — two ast-grep queries, Set difference, done.

```haskell
do
  unwraps <- send (SgFind Rust "$X.unwrap()" ["tidepool-codegen/src"])
  expects <- send (SgFind Rust "$X.expect($MSG)" ["tidepool-codegen/src"])
  let unwrapFiles = Set.fromList (map matchFile unwraps)
      expectFiles = Set.fromList (map matchFile expects)
  pure $ object
    [ "unwrap_only" .= Set.toList (Set.difference unwrapFiles expectFiles)
    , "both" .= Set.toList (Set.intersection unwrapFiles expectFiles)
    ]
```

## Pattern 4: Cross-file semantic queries

"Types with the same name in different crates" — collect all struct/enum names
across the workspace, group by name, filter to names appearing in >1 crate.
Bash grep can find `struct Foo` but can't join across files to detect collisions.

```haskell
send (FsGlob "tidepool-*/src/**/*.rs") >>= mapM (\f -> send (FsRead f) <&> (f,))
  <&> concatMap (\(f, c) ->
    [(name, T.takeWhile (/= '/') f) | l <- T.lines c, T.isInfixOf "struct " l || T.isInfixOf "enum " l,
     let name = T.words l & dropWhile (\w -> w /= "struct" && w /= "enum") & drop 1 & take 1 & head & T.takeWhile (\ch -> ch /= '{' && ch /= '(' && ch /= '<'),
     T.length name > 2])
  <&> Map.fromListWith (<>) . map (\(n,c) -> (n, [c]))
  <&> Map.filter (\crates -> length (nub crates) > 1) <&> toJSON
```

## Pattern 5: Batch LLM triage

Haskell finds ALL candidates (expensive pure computation, zero LLM tokens).
Formats a single numbered report. ONE llm call classifies everything at once.

```haskell
do
  -- ... Haskell finds 8 duplicate pairs, reads the actual code ...
  let report = T.intercalate "\n===\n" [renderPair i pair | (i, pair) <- zip [1..] pairs]
  verdict <- llm ("For each pair: extract / intentional / trivial. One line each.\n\n" <> report)
  pure $ object ["verdict" .= verdict]
```

The LLM sees all pairs together and can make comparative judgments. Cost: one
Haiku call (~500 tokens) instead of an agent reading 8 code blocks in its
context window (~20K tokens).

## Key idioms

| Idiom | What it replaces |
|-------|------------------|
| `>>= mapM (\f -> send (FsRead f) <&> (f,))` | N separate Read tool calls |
| `<&> Map.fromListWith (+)` | `sort \| uniq -c` |
| `<&> foldMap (\x -> (Sum a, Any b))` | multiple separate aggregation passes |
| `Set.difference a b` | `comm -23 <(sort a) <(sort b)` |
| `sortBy (flip compare \`on\` f) & take 10` | `sort -rn \| head` |
| `send (SgFind Rust pattern dirs)` | `ast-grep` CLI (but composable with everything above) |
| `llm (report)` | agent reading everything in context |

## What bash can't do

- Per-function complexity metrics (nesting depth, branch count)
- Pairwise similarity detection across function bodies
- Cross-file semantic joins (struct name → impl block → method count)
- ast-grep results composed with Set/Map operations
- Structured JSON output the calling LLM can query in the next eval
- Multi-phase analysis via KV persistence within a session

## Pattern 2b: cross-file EXACT-block dedup (supersedes Pattern 2 for "extract a helper")

Pattern 2 (in-file fuzzy line-overlap %) was validated against the actual
cleanup and had a poor error profile for *production extractable* dups:

- **False positives (~50% of its top hits):** it can't tell test from
  production (test-fixture builders + duplicated `#[cfg(test)]` `FromValue`
  impls topped the list), and a high line-% over *divergent* code (two different
  `quote!` generators that share a function header) reads as "extractable" when
  it isn't.
- **False negatives:** it's in-file-only, so it UNDER-counts the real wins. The
  two genuine dups (the `runtime_case_trap` / `heap_force` ABI signatures) repeat
  ACROSS files (3 and 5 sites); the in-file detector saw 0 and 2.

The fix is exact, normalized, cross-file, test-excluded block matching:

```haskell
send (FsGlob "tidepool-*/src/**/*.rs") >>= mapM (\f -> send (FsRead f) <&> (T.takeWhileEnd (/= '/') f,))
  <&> concatMap (\(name, c) ->
        let al = T.lines c
            -- exclude the test module (everything from the first #[cfg(test)])
            tstart = case [n | (n,l) <- zipWithIndex al, T.isInfixOf "cfg(test)" l] of { [] -> length al; (x:_) -> x }
            -- normalize: collapse whitespace, drop blanks + // comments
            kept = [(n+1, T.unwords (T.words l)) | (n,l) <- zipWithIndex al, n < tstart,
                    let s = T.strip l, not (T.null s), not (T.isPrefixOf "//" s)]
            wins = takeWhile ((== 8) . length) (map (take 8) (tails kept))  -- 8-line windows
        in [(T.intercalate "§" (map snd w), [name]) | w <- wins])
  <&> Map.fromListWith (\a b -> nub (a ++ b))           -- window-text -> distinct files
  <&> Map.toList <&> filter (\(_, fs) -> length fs >= 2)  -- cross-file only
  <&> nubBy ((==) `on` (sort . snd))                    -- coalesce overlapping windows: one row per file-set
  <&> sortBy (flip compare `on` (length . snd)) <&> take 10 <&> toJSON
```

**Why it's better:** EXACT normalized match → no divergent-code FP; `cfg(test)`
cutoff → no test FP; cross-file grouping → catches the repeated-ABI-sig class
(the actual wins); `nubBy` on the sorted file-set → collapses the overlapping-
window noise into one row per duplicated region.

**Lesson:** a similarity *percentage* is a candidate generator, not a verdict.
"Is this worth extracting?" needs the structural pull (the tidepool eval) plus a
test/production + identical/divergent check — exactly the find→pull→judge loop.
Run Pattern 2b first (high-confidence exact dups), keep Pattern 2 only as a
fuzzy second pass for near-dups a human will adjudicate.

Findings from 2b on the current tree (post-cleanup): `YieldError`
(yield_type.rs) vs `RuntimeError` (host_fns.rs) duplicate their whole
variant+`#[error]` message set; optimizer-pass trait boilerplate repeats across
4 passes; thunk-store accessors (arena.rs/heap.rs); double-decompose math
(eval.rs/host_fns.rs). These are the real next targets.
