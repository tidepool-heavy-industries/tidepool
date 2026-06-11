# Tidepool Patterns — Worked Examples

Runnable companions to `plans/llm-continuation-patterns.md` (P1–P9). Every
block below is a paste-able eval `code` body (do-notation; the server wraps
it). The model: **a tidepool program is a coroutine; `ask` is its yield; the
calling agent is its scheduler; the continuation — not KV — is its call
stack.**

Verbs used here ride along with the auto-imported Library: `census`, `refs`,
`Asks.choose`/`approve`/`oracle` (bare), `watch`, `saga`, `patchFile`,
`insertAfter`, `writeChecked`, `linesOf`/`filteredT`/`overFileM`, `(&&&)`,
`pick`/`yn`/`?!`/`triage` (preamble, LLM-gated).

---

## P1. The Aperture — scout, suspend with a digest, steer the tail

Compute cheaply → `ask` with a menu → the caller scouts freely during the
gap → resume → only the chosen expensive tail runs.

```haskell
hits <- sgFind Rust "unsafe { $$$B }" ["tidepool-heap/src", "tidepool-codegen/src"]
let byFile = hitsByFile (map (\m -> (matchFile m, matchLine m, matchText m)) hits)
i <- choose ("Found " <> pack (show (len hits)) <> " unsafe blocks. Deep-dive which file?")
            (map fst (sizeRank 5 byFile))
let target = fst (sizeRank 5 byFile !! i)
ctx <- mapM (\m -> pure (matchFile m <> ":" <> pack (show (matchLine m))))
            (filter (\m -> matchFile m == target) hits)
pure (toJSON ctx)
```

Discipline: **acts go after apertures; pure scouting never needs one.**
Sub-forms: menu (`choose`), gate (`approve "about to write 14 files"`),
open question (`oracle`).

## P1+P4. Census → Classify → Persist (the house archetype)

Structural sweep → inline-LLM batch classification → suspend ONCE with only
the contested cases (Tribunal) → persist the ruling at the task boundary.

```haskell
ms <- hsDef "emit" ["tidepool-codegen/src"]
fns <- sgFind Rust "fn $N($$$A) -> $R { $$$B }" ["tidepool-codegen/src/emit"]
verdicts <- mapM (\m -> do
                    r <- pick ["hot-path", "setup", "diagnostic"]
                           ?! ("Classify this compiler fn:\n" <> stake 400 (matchText m))
                    pure (matchFile m <> ":" <> pack (show (matchLine m)), r))
                 (stake 20 fns)
let (sure, unsure) = partition (\(_, r) -> case r of { Sure _ -> True; _ -> False }) verdicts
ruling <- if isNull unsure
            then pure ""
            else oracle ("Classified " <> pack (show (len sure)) <> " confidently. Contested:\n"
                         <> intercalate "\n" (map fst unsure)
                         <> "\nReply one category per line, or 'skip'.")
kvSet "emit-census" (object ["sure" .= map fst sure, "ruling" .= ruling])
pure (toJSON (len sure, len unsure))
```

The caller's attention is spent only where the model's confidence ran out
(P9.2: counts + exemplars in the prompt, never corpora). KV is touched once,
at the boundary (P7). `triage`/`survey` are pre-built degenerate forms.

## P3. The Escalator — pure rule → inline model → suspend

```haskell
let cheap t = if "test_" `isPrefixOf` t then Just True else Nothing
flaky <- escalate cheap (\t -> "Is " <> t <> " a flaky test name?") "prop_gc_roundtrip"
pure (toJSON flaky)
```

`escalate` runs the heuristic, falls to `yn ?!` (inline Haiku), and only
suspends to you when confidence runs out. `triageAuto` is the batch form.

## P5. The Deferred Decision — compute alternatives, then ask

```haskell
src <- readFile ".tidepool/lib/Optics.hs"
let drafts = map (\(name, f) -> (name, src & linesOf . filteredT (isPrefixOf "-- |") %~ f))
                 [("terse", stake 60), ("shouted", toUpper)]
i <- choose "Two haddock rewrites drafted - apply which?" (map fst drafts)
writeChecked ".tidepool/lib/Optics.hs"
             [("nonempty", not (isNull (snd (drafts !! i))))]
             (snd (drafts !! i))
```

The dual of P1: steer *after* the cheap work, before the commitment. The
continuation holds every branch's state; suspension is free.

## P6. The Socratic Checkpoint — validate backward before persisting

`writeChecked` is the pure-check form; `approve` is the judgment form:

```haskell
report <- census "tidepool-eval/src"
ok <- approve ("About to snapshot this census to KV:\n" <> stake 500 (vshow report))
if ok then kvSet "eval-census" report >> pure "saved" else pure "held"
```

## Saga + Watch (crash-resumable / long-watch coroutines)

`saga` checkpoints each step to KV — re-running the SAME saga skips
completed steps, so a timeout mid-pipeline costs only the unfinished tail:

```haskell
r <- saga "audit-emit" 
  [ ("census",  census "tidepool-codegen/src/emit")
  , ("hotspots", do { ms <- hsDef "emit_case" ["tidepool-codegen/src"]; pure (toJSON (map matchFile ms)) })
  , ("verdict", do { ok <- approve "census + hotspots cached. Proceed to deep scan?"; pure (toJSON ok) })
  ]
pure r
```

(`sagaReset "audit-emit"` forgets the checkpoints.) `watch` inverts control:
scan, suspend, and each resume re-scans and reports a structured `vdiff` —
reply `check`/`stop`:

```haskell
watch "cargo-target" (do { fs <- glob "target/debug/*.d"; pure (toJSON (len fs)) })
```

## Chained Text Surgery — exactly-once patches, optics for the line-level

`patchFile` errors loudly on absent OR ambiguous needles (ambiguity is how
string surgery corrupts silently); `insertAfter` anchors on a unique line;
`overFileM` reports what changed instead of editing blind.

```haskell
r1 <- patchFile "target/demo.cfg" "retries = 3" "retries = 5"
r2 <- insertAfter "target/demo.cfg" "[limits]" "max_depth = 64"
r3 <- overFileM "target/demo.cfg" (linesOf . filteredT (isPrefixOf "#")) (("# [reviewed] " <>) . sdrop 2)
pure (object ["patch" .= r1, "insert" .= r2, "sweep" .= r3])
```

Big needles? Pass them via the eval `input` field and use `patchJ input` —
nothing needs escaping in code. For whole-file rewrites, end the chain with
`writeChecked` so a botched compose can't land.

> STATUS 2026-06-11: `patchFile`/`patchJ` currently trap at runtime (#313
> t11 family — double `breakOn`, fix in flight on root; see
> plans/gotcha-audit.md). `insertAfter`, `overFileM`, `writeChecked` verified
> working. Until the fix lands, do exact-once replacement inline:
> `breakOn` twice in your own do-block works fine.

## Kleisli plumbing — (&&&), (***), (|||), firstK, secondK

Arrow-style combinators for `a -> M b` pipelines (Prelude-level, JIT-safe):

```haskell
profile <- (getFileSize &&& readFile) "CLAUDE.md"          -- one input, two probes
sized   <- firstK getFileSize ("flake.nix", "keep-me")      -- enrich a tuple's first slot
merged  <- (pure . ("dir: " <>)) ||| (pure . ("file: " <>)) $ (Right "x.rs" :: Either Text Text)
pure (object ["size" .= fst profile, "merged" .= merged, "sized" .= fst sized])
```

`f &&& g` fans one input out to two effectful probes; `f *** g` maps a pair
component-wise; `f ||| g` merges an `Either`. They read left-to-right where
nested do-blocks would bury the shape.

## Match ergonomics (already in the preamble)

`sgFind` results are records: `matchText`, `matchFile`, `matchLine`,
`matchVars`, `matchReplacement`, plus `var m "NAME"` for captures — no
positional pattern matching needed:

```haskell
ms <- hsDef "linesOf" [".tidepool/lib"]
pure (toJSON (map (\m -> matchFile m <> ":" <> pack (show (matchLine m))) ms))
```

## Anti-patterns (from the design doc — enforced by convention)

- **ask-as-print**: suspension is for *decisions*; use `putStrLn`/`say`.
- **mega-ask**: one decision per suspension — split (P2) or menu it.
- **KV ping-pong**: thread state through the continuation inside one flow;
  KV only at task boundaries (P7).
- **premature ask**: compute first (P5) — never ask what 10ms of compute
  answers.
- **shapeless ask**: state the expected reply form in the prompt (P9.1) and
  end every `case answer of` with a safe default arm (P9.3).
