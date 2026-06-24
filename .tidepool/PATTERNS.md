# Tidepool Patterns — Worked Examples

Every block below is a paste-able eval `code` body (do-notation; the server
wraps it). The model: **a tidepool program is a coroutine; `ask` is its yield;
the calling agent is its scheduler; the continuation — not KV — is its call
stack.**

`ask`/`llm` are STRUCTURED: pass a `Schema`, get back a validated `Value`,
extract with optics (`v ^? key "f" . _String`).
`Schema = SObj [(Text,Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema`.
- `ask schema prompt` — suspend to the caller (no token burn; they answer).
- `llm schema prompt` — autonomous server-side model call (costs tokens).

Verbs used here ride along with the auto-imported Library: `census`, `refs`,
`hitsByFile`/`sizeRank`, `patchFile`/`insertAfter`/`writeChecked`, the `Edit`
verbs, `linesOf`/`filteredT`/`overFileM`, the Kleisli combinators (`&&&`/`***`/`|||`).

---

## P1. The Aperture — scout, suspend with a digest, steer the tail

Compute cheaply → `ask` a menu (an `SEnum`) → the caller scouts freely during
the gap → resume → only the chosen expensive tail runs.

```haskell
hits <- sgFind Rust "unsafe { $$$B }" ["tidepool-heap/src", "tidepool-codegen/src"]
let byFile = hitsByFile (map (\m -> (matchFile m, matchLine m, matchText m)) hits)
let files  = map fst (sizeRank 5 byFile)
v <- ask (SObj [("file", SEnum files)])
          ("Found " <> pack (show (len hits)) <> " unsafe blocks. Deep-dive which file?")
let target = case v ^? key "file" . _String of { Just f -> f; _ -> "" }
pure (toJSON [ matchFile m <> ":" <> pack (show (matchLine m))
             | m <- hits, matchFile m == target ])
```

Discipline: **acts go after apertures; pure scouting never needs one.** For a
yes/no gate use `SBool`; for an open answer, `SStr`.

## P2. Census → Classify → Persist — the model DECIDES, code does the rest

Structural sweep → autonomous `llm` classification with an `SEnum` (the model
picks a label, never emits syntax) → persist at the task boundary (P7).

```haskell
fns <- sgFind Rust "fn $N($$$A) -> $R { $$$B }" ["tidepool-codegen/src/emit"]
verdicts <- mapM (\m -> do
              v <- llm (SObj [("kind", SEnum ["hot-path", "setup", "diagnostic"])])
                       ("Classify this compiler fn:\n" <> stake 400 (matchText m))
              pure (matchFile m <> ":" <> pack (show (matchLine m)),
                    case v ^? key "kind" . _String of { Just k -> k; _ -> "?" }))
            (stake 20 fns)
kvSet "emit-census" (toJSON verdicts)
pure (toJSON (len verdicts))
```

`llm` is autonomous (server-side, costs tokens); swap it for `ask` to suspend
to the caller instead. The schema forces a clean label — no fence-stripping.
**Orchestration rule:** let the LLM DECIDE (`SEnum`/`SBool`) and let
deterministic code EMIT syntax (regex/AST) — models are unreliable at
generating domain-specific syntax directly.

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
`writeChecked` so a botched compose can't land. For line-range/anchor edits,
the `Edit` verbs (`applyEdits`/`editsJ`, 1-based) lower to a context-anchored
patch on an all-or-nothing apply.

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

## Anti-patterns (enforced by convention)

- **ask-as-print**: suspension is for *decisions*; use `putStrLn`/`say` for
  traces and `pure`/a returned `Value` for results.
- **mega-ask**: one decision per suspension — split it or menu it with `SEnum`.
- **KV ping-pong**: thread state through the continuation inside one flow; touch
  KV only at task boundaries.
- **premature ask**: compute first — never ask what 10ms of compute answers.
- **shapeless ask**: the `Schema` already constrains the reply; still state the
  intent in the prompt, and end every `case (v ^? …) of` with a safe default arm.
- **LLM-emits-syntax**: never ask the model for a regex/AST pattern directly;
  have it pick a strategy (`SEnum`) and emit the syntax in code.
