# Haskell-as-interface polish wave (accepted with Inanna, 2026-07-04)

Governing rule: **prefer EXISTING in-weights packages** (`errors`, `witherable`,
`validation`, `generic-lens`, `safe`) over hand-maintained helpers — free model
fluency, zero maintenance. Every deviation from canonical Haskell/GHCi is a tax;
every canonical mirror is free. These are 5–10% compounding polish, not features.

Not for this wave until the current one lands: bridged-records SOT crate + gate
green + nextest tiering + negative-assertion strip. THEN this.

## Accepted — do

- **`it` last-result binding** (repl `session_run`): GHCi parity, `foo` then
  `length it`. [#3]
- **Echo inferred result type** on `eval` (as the repl already does) + `:type
  +d`/`+v`. Easy, high signal. [#5]
- **Hide partial Prelude + safe spellings — ONE stanza.** [#6+#8] Hide
  `head/tail/init/last/(!!)/fromJust/read/foldr1`; re-export safe forms from the
  `safe` package (`headMay`/`lastMay`/`atMay`, `readMaybe`, list `!?`). Add a
  single MCP-interface doc note: "unsafe partials (`head`, …) are not in scope —
  use `headMay`/`atMay`/`readMaybe`." Kills the worst crash class. (Only real
  tension in the set: wants a clean-context A/B before final commit.)
- **Railway kit via the `errors` package** (bare re-export, NOT hand-rolled):
  `note`, `hush`, `whenLeft`/`whenRight`, `hoistMaybe`, … [#9]
- **`Validation`** (accumulating errors) via the `validation` package — ONLY if
  it doesn't bloat the surface. `validationToEither`/`bindValidation`. [#10]
- **MonadFail idiom highlighted in examples/docs**: `Right x <- verb` as the
  default happy-path spelling, modeled everywhere. [#11]
- **Arrow/Category/Function combinators bare** (STRONG): `>>>`, `<<<`, `&`, `on`,
  `&&&`, `***`, `|||`. [#12]
- **`witherable` package** (use the real one): `wither`, `mapMaybe`, `filterA`,
  `catMaybes` over any Traversable. [#13]
- **Doc `run`/`runArgv` as the `System.Process` mirror**: `Proc{exitCode,stdout,
  stderr}` == `readProcessWithExitCode`'s triple. [#14]
- **Foldable/Monoid aggregation**: `foldMap`/`fold`/`<>` + sensible
  Semigroup/Monoid instances on result collections. [#15]
- **`generic-lens` label optics** (`^. #field`) so records navigate by the SAME
  optics grammar as JSON (`^? key "x" . _Int`). [#16]
- **Uniform deriving on ALL result types** — `Show, Eq, Ord, Generic, ToJSON` —
  make it a law (the verb-sweep started it). [#17]
- **`tidepool://cheatsheet` + haddock-flavored tool descriptions** (STRONG): the
  curated bash++ exemplar stable lives here; the description is the
  highest-read prompt in the system. Ties into [[bash-plus-plus-optimization-loop]]'s
  exemplar-stable milestone. [#20]

## Deferred (real features, not tweaks — spike before building)

- **Typed holes surfaced** [#1] and **Hoogle-by-type over the vocab** [#2] — the
  most ambitious (type-driven-development flex) but round-trip-heavy and require
  capturing/reshaping GHC diagnostics. Defer as major features w/ GO/NO-GO spike.
- **`:doc` haddock lookup** [#4] — later.
- **Lazy-effect streaming idioms** (`takeWhileM`/`unfoldrM`/`iterateM`) [#18] and
  **`interact`-shaped `mapLines`** [#19] — nontrivial; #19 only if genuinely easy.
- **Schema-level `expect :: TypeString`** result type-assert [#21] — too feature-y.

## Rejected

- **`Data.List.NonEmpty` mirror** [#7] — adds overhead + colliding names; not needed.
