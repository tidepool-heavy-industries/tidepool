# glimpse + peek — design notes

Context: dogfooding tidepool-repl as the actual end-user (an LLM driving it), the
governing cost is **cost-to-correct-answer** = round-trips + tokens-emitted +
tokens-perceived. Two primitives attack the two biggest terms:

- **`glimpse`** — cuts *tokens-perceived* (summarize instead of dumping).
- **`peek`** — cuts *round-trips* (a cheap existence-proof that makes a failable
  effect total; "parse, don't validate" applied to effects).

Both are the payoff of the records rewrite: known shapes → cheap faithful previews.

## Measured motivation (2001 real `grepGlob "fn " "**/*.rs"` hits)

| render | chars | vs dump |
|---|---|---|
| dump (JSON)              | 235,858 | 1× |
| columnar TSV (`table`)   | 177,691 | 1.33× |
| **`glimpse`**            | **466** | **506×** |

Lesson that overturned the initial "columnar table" priority: **format is a ~25%
tweak; the summarize-vs-dump *decision* is 500×.** Lead with `glimpse`. `table`
(columnar TSV) is a secondary nicety for when every row is genuinely wanted.

---

## 1. `glimpse` — collection summary (value form)

Prototype that worked live (Hit-specific; generalize via ToJSON):

```haskell
glimpse :: ToJSON a => [a] -> Value
glimpse xs = object
  [ "count"  .= length xs
  , "fields" .= fieldsOf xs            -- keys of the first row's toJSON Object
  , "sample" .= take 3 xs
  , "top"    .= topFreq xs ]           -- top-5 (value,count) of the first String column
```
- **Generic over `ToJSON a`** — `toJSON` each element, treat as `Object`, read keys
  via `KM.toList`. Works on any record (`[Hit]`, `[Proc]`, `[Doc]`) and on `[Value]`.
- **Frequency column:** default to the first String-valued field; offer
  `glimpseOn :: ToJSON a => (a -> Text) -> [a] -> Value` for an explicit key.
- **Placement:** pure, no effects → `Tidepool.Prelude` re-export (always unqualified)
  or a small `Tidepool.Glimpse` module. Prefer Prelude re-export for zero-ceremony.

### Wiring `glimpse` as the oversized-result default — the real decision
Current oversized handling = `truncVal` (cut with stub markers, *recoverable* — you
can re-query a stub). `glimpse` is *lossy*. Options:
1. **Lossy glimpse default** — cheapest, but you lose the ability to get the full data
   without re-running. Bad: silently drops recoverability.
2. **Keep truncation default, `glimpse` opt-in** (`glimpse xs`) — safe, but the 500×
   win only lands when the caller remembers to ask.
3. **⭐ Glimpse-with-a-handle (recommended hybrid)** — oversized `[record]`/`Array` →
   emit `glimpse` BY DEFAULT, but include a stub-id / "bind the result and re-query"
   hint (reuse the existing pagination stub mechanism) so the full data is one call
   away. Token-cheap by default, recoverable on demand. Best of both.
   - Touch point: the `paginateInteractive`/`paginateTrunc` bodies in
     `orchestrate_module_source` (`tidepool-mcp/src/preamble.rs`) + `render.rs`.
     Detect "large homogeneous array of objects" → glimpse + stub, else current truncVal.

Open: does glimpse-default apply to ALL oversized results or only homogeneous
`[object]`? Heterogeneous/nested blobs → fall back to `truncVal` (or `shape`, see §4).

---

## 2. `peek` — file capability (the interesting one)

The novel form: a glimpse that is a **capability, not a value**. A `FilePeek` proves
the file exists and previews it; `read` only accepts a `FilePeek`. Turns
`readFile "missing"` (failable → error → round-trip) into `peek "missing" → Nothing`
(total → data).

```haskell
-- OPAQUE type: constructor NOT exported, so a FilePeek can only come from `peek`
-- (existence is proven by construction).
data FilePeek = FilePeek
  { path      :: Text
  , size      :: Int
  , lineCount :: Int
  , headLines :: [Text]     -- first N (e.g. 10)
  , tailLines :: [Text]     -- last N
  , kind      :: Text }     -- "text" | "binary" | "large"
  deriving (Show, Eq)

peek :: FilePath -> M (Maybe FilePeek)     -- Nothing = does not exist / not accessible
read :: FilePeek -> M Text                 -- total; the ONLY blessed full read

-- idiom: peek path >>= maybe (handleMissing path) read
```
- **Existence proof:** `peek` returns `Nothing` for missing → no error round-trip.
  Because `FilePeek`'s constructor is hidden, you *cannot* fabricate one, so holding a
  `FilePeek` is a proof the file existed at peek time. `read :: FilePeek -> M Text` is
  then total (modulo TOCTOU, acceptable).
- **Bounded preview:** `peek` must NOT slurp a 2MB file. v1 can `fsMeta` + (small file
  → `readFile` + head/tail; large file → read only first N bytes/lines). A clean impl
  wants a bounded-read effect (`FsReadBounded :: FilePath -> Int -> Fs Text` or
  head/tail lines) — otherwise the preview costs a full read. **Decision:** ship v1 as
  `fsMeta`-gated slurp-then-take (correct, not optimal); add the bounded-read effect
  as a follow-up if peek-on-huge-files matters.
- **Placement:** composes Fs effects → a `.tidepool/lib/Peek.hs` module (or fold into
  the Fs surface in `effect_decls.rs` if a new bounded-read effect is added). Export
  `FilePeek` (opaque), `peek`, `read`, and field accessors — NOT the constructor.
- **`readFile` coexistence:** keep `readFile :: FilePath -> M Text` as the escape
  hatch (when you're sure); make `peek`→`read` the *documented default* in vocab.
  Do NOT deprecate readFile yet.
- Extends the `FileMeta` work (phase 2): `FilePeek` is `FileMeta` + a bounded preview.

---

## Related fix surfaced while prototyping: decl-scope gap

`renderJson` (and other `Tidepool.Orchestrate` helpers) are NOT in scope in the
decl-plane module (`Tidepool.Session.Lib.G<g>`), only the expr module. Hit this
defining `tsvTable`/`glimpse` as session decls. Fix = add the Orchestrate import to
`session_decl_module_env` (`tidepool-mcp/src/preamble.rs`) so decls see the same
helper surface as expressions. This is the concrete instance of the "surface idioms
into decl scope" item; do it alongside, since glimpse/peek will often be authored as
`.tidepool/lib` decls that want `renderJson` et al.

---

## Future glimpse modalities (captured, not yet scoped)

Same "cheap proof/preview to decide the next commit" family:

3. `survey :: Glob -> M Value` — subtree map: `{files, exts, biggest, deepest, bytes}`
   (= the census from the hotspot dogfood, as a primitive).
4. `shape :: Value -> Value` — infer schema of an arbitrary blob: `{type, keys:{k:type},
   arrayLens, depth}`. The oversized-*nested*-Value fallback (vs glimpse for arrays).
5. `countOnly :: Regex -> Glob -> M Int` — cardinality without payload (`grep -c`); the
   cheapest glimpse, one Int, "how many before I fetch all?"
6. `spread :: (a -> k) -> Int -> [a] -> [a]` — diverse sample: n exemplars per distinct
   key (`spread (.path) 1 hits` = one hit/file). Maximizes variety/token vs `take 3`.
7. `delta :: Eq a => [a] -> [a] -> Value` — change-only glimpse vs a baseline
   (`{added, removed, unchanged:count}`); pairs with `it` for the refine loop.

---

## Recommended build order

1. `glimpse` + `glimpseOn` into Prelude (pure, cheap, proven prototype). Ship it.
2. Fix the decl-scope gap (Orchestrate helpers into `session_decl_module_env`).
3. `glimpse`-with-stub-handle as the oversized homogeneous-array default (hybrid §1.3).
4. `peek`/`FilePeek`/`read` capability (v1 = fsMeta-gated slurp+take; opaque FilePeek).
5. Later: bounded-read effect for peek; `survey`/`shape`/`countOnly`/`spread`/`delta`.

Verification for each: prototype live in a session first (dogfood-build-measure, as
was done for glimpse: 506×), then promote to stdlib + a gotcha_registry probe.
