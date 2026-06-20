# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Tidepool

Compile freer-simple effect stacks into Cranelift-backed state machines drivable from Rust. Haskell expands, Rust collapses. The language boundary is the hylo boundary.

---

## Rules

All rules from the exomonad project apply here. Additionally:

### Locked Decisions

The Key Decisions Reference section below is the source of truth for all architectural decisions. Every entry is final. Do not deviate from locked decisions. Do not re-derive them. If you need a decision that isn't there, escalate to the human.

### Plans

`plans/README.md` tracks the current active plan. Read it before starting new work.

---

## Orchestration Model

This project is built by a tree of agents managed by ExoMonad. Understanding the execution model is mandatory for every TL agent.

### Roles

- **Human (root):** Owns `main`. Makes architectural decisions. Approves phase gates.
- **TL (Claude Opus):** Owns a subtree branch (e.g., `main.core-repr`). Decomposes work into leaf specs, spawns agents, merges their PRs. Never writes implementation code.
- **Leaf (Gemini):** Spawned via `spawn_leaf_subtree`. Owns a leaf branch (e.g., `main.core-repr.scaffold`). Implements one task spec. Files PR. Iterates against Copilot review until clean. Calls `notify_parent` when done.
- **Worker (Gemini):** Spawned via `spawn_workers`. Works in the parent's directory. Does NOT create branches, commit, or file PRs. Writes code, runs verify, calls `notify_parent`. The parent reviews and commits.

### Fire-and-Forget Execution

The TL's workflow is: **decompose -> spec -> spawn -> move on**. The TL does not wait, poll, review intermediate output, or manually re-spec.

**Convergence is leaf + Copilot, not TL:**

1. TL writes spec, spawns leaf, returns immediately
2. Leaf works -> commits -> files PR
3. GitHub poller detects Copilot review comments -> injects into leaf's pane
4. Leaf reads Copilot feedback, fixes, pushes
5. Copilot re-reviews; loop repeats until clean
6. Leaf calls `notify_parent` with `success` -> TL gets `[CHILD COMPLETE]`
7. TL reviews the merged diff (parallel merges may interact), then merges

**`notify_parent` means DONE** — not "I filed a PR." The leaf owns its quality.

### Spawn Tool Selection

All spawn tools take the same structured `AgentSpec` (name, task, read_first, context, steps, verify, done_criteria, boundary). Branch names auto-derived from `spec.name`.

| Tool | Default? | Use when | Litmus test |
|------|----------|----------|-------------|
| `spawn_leaf_subtree` | **Yes** | Any well-specified implementation task | Will the agent add mod declarations, deps, or re-exports? Multiple agents in parallel? → leaf. |
| `spawn_workers` | No | Single agent doing scaffolding you'll commit yourself, OR multiple agents with provably zero file overlap | Can you list every file each agent touches, and the lists don't intersect at all? Not even lib.rs or Cargo.toml? If you have to think about it → use leaf. |
| `spawn_subtree` | No | Task needs further decomposition or architectural judgment | 10-30x more expensive. Almost never needed. |

**`spawn_leaf_subtree` is the default.** The worktree isolation and Copilot review loop make it the safe choice. The overhead (branch + PR) is handled automatically by tooling. The quality improvement from Copilot review is significant and free.

**`spawn_workers` is the exception.** Workers share your directory. Use only for single-agent scaffolding gates where you review and commit directly. If any agent needs to touch Cargo.toml, lib.rs, or mod declarations alongside other agents — use leaf subtrees.

### Spec Quality (You Only Get One Shot)

Since the TL doesn't iterate on specs, the v1 spec must be production-quality. All `AgentSpec` fields map directly to prompt sections:

| Field | Purpose |
|-------|---------|
| `boundary` | DO NOT rules — known failure modes (rendered FIRST in prompt) |
| `read_first` | Exact files to read before coding |
| `steps` | Numbered concrete actions with code snippets |
| `verify` | Exact shell commands to run |
| `done_criteria` | Measurable checklist for completion |
| `context` | Freeform: code snippets, type signatures, examples |

**Anti-patterns / boundary section is mandatory and comes first.** Known Gemini failure modes:

| Failure Mode | Rule |
|---|---|
| Adds unnecessary dependencies | "ZERO external deps. Do NOT add serde/tokio/etc." |
| Invents escape hatches | "No `todo!()`, `Raw(String)`, `Other(Box<dyn Any>)`" |
| Writes thinking-out-loud comments | "Doc comments only. No stream-of-consciousness." |
| Renames types/variants | "Use EXACT type signatures below." |
| Makes architectural decisions | "Do not change module structure." |
| Overengineers | "This is N lines in M files, not a new module." |

Specs are self-contained. The leaf has no context from previous attempts. Include complete code snippets and full file paths.

### Escalation, Not Iteration

If a leaf fails after 3+ Copilot rounds, it calls `notify_parent` with `failure`. The TL then: re-decomposes (smaller leaves), tries a different approach, or escalates to the human. The TL never manually fixes a leaf's code.

### Branch Hierarchy

```
main                              [human]
├── main.core-repr                [TL - Claude]
│   ├── main.core-repr.scaffold   [leaf - Gemini]
│   ├── main.core-repr.serial     [leaf - Gemini]
│   └── main.core-repr.pretty     [leaf - Gemini]
├── main.core-eval                [TL - Claude]
│   └── ...
```

PRs target parent branch (not main). Merged via recursive fold up the tree.

---

## Project Structure

```
tidepool/
├── tidepool/              ← Facade crate + MCP server binary (`cargo install tidepool`)
├── tidepool-repr/         ← Core IR types: CoreExpr, DataConTable, CBOR serial
├── tidepool-eval/         ← Tree-walking interpreter: Value, Env, lazy eval
├── tidepool-heap/         ← Manual heap + copying GC for JIT runtime
├── tidepool-optimize/     ← Optimization passes: beta, DCE, inline, case reduce
├── tidepool-bridge/       ← FromCore/ToCore traits + derive macros
├── tidepool-bridge-derive/← Proc-macro for bridge derives
├── tidepool-macro/        ← Proc-macro for effect stack declarations
├── tidepool-effect/       ← Effect handling: DispatchEffect, EffectHandler, HList
├── tidepool-codegen/      ← Cranelift JIT compiler + effect machine
├── tidepool-runtime/      ← High-level API: compile_haskell, compile_and_run, cache
├── tidepool-mcp/          ← MCP server library (generic over effect handlers)
├── tidepool-testing/      ← Test utilities + property-based generators (internal)
├── examples/
│   ├── guess/             ← Demo: number guessing game
│   └── tide/              ← Demo: REPL
├── haskell/               ← Haskell harness (tidepool-extract) + test suite + stdlib
│   └── lib/Tidepool/      ← Haskell stdlib (auto-imported in MCP)
├── flake.nix              ← Dev shell (Rust + GHC 9.12 with fat interfaces)
└── CLAUDE.md              ← YOU ARE HERE
```

### Build & Test

```bash
nix develop                              # Enter dev shell (provides Rust + GHC 9.12)
cargo check --workspace                  # Type check
cargo test --workspace                   # Run all tests
cargo test -p tidepool-codegen           # Run tests for one crate
cargo test -p tidepool-eval -- test_name # Run a single test by name
cargo clippy --workspace                 # Lint
cargo fmt --all -- --check               # Format check
```

### Rebuilding the Haskell Toolchain

After changing `haskell/` code (Translate.hs, GhcPipeline.hs, Prelude, etc.):

```bash
cd haskell && cabal build tidepool-extract-bin # Build the Haskell compiler
cp $(cabal list-bin tidepool-extract-bin) ~/.local/bin/tidepool-extract-bin  # Install
rm -rf ~/.cache/tidepool/                    # Clear cached CBOR (stale after binary changes)
```

The system PATH binary at `~/.local/bin/tidepool-extract-bin` is called by `~/.cargo/bin/tidepool-extract` wrapper. Both must be current.

### Regenerating Test Fixtures

The Haskell integration tests use pre-compiled CBOR fixtures in `haskell/test/suite_cbor/`. After changing the Haskell serializer or adding test bindings to `haskell/test/Suite.hs`:

```bash
cd haskell && cabal run tidepool-extract-bin -- test/Suite.hs --all-closed \
  --include lib --target-module-only --output-dir test/suite_cbor
# --include lib + --target-module-only: Suite.hs imports Tidepool.QQ
# --output-dir: default derives from module basename → test/Suite_cbor (wrong dir)
```

### MCP Server

The `tidepool` binary is an MCP server. See README.md for full setup instructions.

```bash
cargo install --path tidepool   # Install the binary
tidepool                        # Run (expects MCP JSON-RPC on stdio)
```

### Diagnostics

All debug instrumentation is env-gated, OFF by default. Knob → what it shows → when to reach for it:

| Knob | Layer | What it shows | Reach for it when |
|------|-------|---------------|-------------------|
| `TIDEPOOL_TRACE=calls` | JIT runtime | Every closure call: name, arg, result (`tidepool-codegen/src/debug.rs`) | Tracing which function received/returned a bad value (e.g. wrong type at a case dispatch) |
| `TIDEPOOL_TRACE=heap` | JIT runtime | `calls` + heap-object validation before use | Suspected heap corruption / bad pointer breadcrumbs |
| `TIDEPOOL_TRACE_EFFECTS=1` | Effect machine | Effect dispatch at the JIT↔Rust boundary | Effect results arriving wrong / lazy-result suspicion |
| `TIDEPOOL_LAZY_RESULTS=0` | Effect machine | Kill-switch: disables lazy effect results (typed Stream/List channel) | Bisecting whether a bug is in the lazy-results path |
| `TIDEPOOL_DUMP_CLOSED=<needle>` | Haskell extract | Dumps closed Core for bindings whose binder name matches needle | Inspecting what Core the JIT actually receives for a binding |
| `TIDEPOOL_VARID_AUDIT=1` | Haskell extract | VarId collision report (distinct binders → same 64-bit id) | SIGILL/case-trap hunts; ruling out id collisions |
| `TIDEPOOL_VARID_AUDIT=<hex>,<hex>` | Haskell extract | Resolves specific VarIds (e.g. `lam_binder` values from `TIDEPOOL_TRACE=calls`) to source names + enclosing top-level binder | Naming the function a JIT trace implicates |
| `TIDEPOOL_JOINREC_DEBUG=1` | Haskell extract | joinrec-translation forensics (the `[313-joinrec]` spew from the #313 hunt) | Join-point conversion bugs (jumps compiled as calls, wrong continuation) |
| `TIDEPOOL_IFACE_DEBUG=1` | Haskell extract | `[fat-iface]` interface-loading trace | Missing unfoldings / "unresolved external" mysteries |
| `TIDEPOOL_FP_DEBUG=1` | Runtime cache | Binary-fingerprint memo keys + sidecar hit/miss (`tidepool-runtime/src/cache.rs`) | Stale-cache suspicion. Note: kernel ctime has ~3ms granularity — sub-tick writes legitimately memo-hit |
| `NONCE=<x>` / `FORCE=1` | `repro313` test | Cache-busting fresh compile / forces Int result inside the user continuation | Re-running the #313 regression gate against a fresh compile |

Always-on breadcrumbs (`[CASE TRAP]`, `[BUG]` bad-pointer lines on stderr) stay unconditional: they fire only on actual compiler bugs, which must be loud. If you see one, that's a reportable codegen bug, not user error.

---

## Key Decisions Reference

Critical architectural decisions for daily work:

- **CoreFrame variants:** Var, Lit, App, Lam, LetNonRec, LetRec, Case, Con, Join, Jump, PrimOp
- **No type variants** — types stripped at serialization in Haskell
- **RecursiveTree\<CoreFrame\>** as CoreExpr type alias
- **CBOR** via serialise (Haskell) / ciborium (Rust)
- **Cast/Tick/Type erasure** happens in Haskell serializer, NOT in Rust
- **HeapObject:** manual memory layout (raw byte buffers + unsafe accessors), NOT a Rust enum
- **GC:** Copying collector, custom RBP frame walker, split gc-trace/gc-compact
- **freer-simple continuations:** Leaf/Node tree (type-aligned sequence), NOT single closures
- **Union tags:** unboxed Word# constants (0##, 1##, ...) indexing the effect type list

---

## Haskell Standard Library (`haskell/lib/Tidepool/`)

MCP users get `import Tidepool.Prelude hiding (error)` auto-imported. Additional modules available via the `imports` field. Inline data declarations in `helpers` are fully supported and right for eval-local types; promote types to a `.tidepool/lib/` module when they need to be REUSED across evals. (Verified 2026-06-11: ADT + deriving Show + custom typeclass instances in helpers compile and run.)

### Tidepool.Prelude (auto-imported)

Everything MCP users need in one import.

> **Text, not String — the #1 usability trap.** The Prelude standardizes on `Text`. `show` returns `Text` (not `String`), and `pack` is polymorphic (identity on Text, `T.pack` on String), so `pack (show x)` works fine. `lines`/`words`/`unlines`/`unwords` all operate on `Text`. `error` takes `Text`. String literals are `Text` via `OverloadedStrings`. The underlying `String` type (`[Char]`) works fine at moderate scale (verified to 20K chars) but `Text` is still the faster, idiomatic default. Stick to `Text` everywhere.

- **Types**: Int, Double, Char, Bool, Text, String, Maybe, Either, Map, Set, Value
- **Text ops**: pack/unpack, toUpper/toLower, strip, splitOn, replace, words/lines/unwords/unlines, isPrefixOf/isSuffixOf/isInfixOf, intercalate (Text→[Text]→Text), joinText, tReverse
- **Polymorphic ops**: `pack` (String→Text or Text→Text identity), `len` (Text or [a] → Int), `isNull` (Text or [a] → Bool), `stake`/`sdrop` (like take/drop on both Text and [a])
- **List ops**: map, filter, foldl/foldr/foldl', sort/sortBy, nub/nubBy, groupBy, partition, transpose, intersperse, zip/zip3/unzip/unzip3, elemIndex/findIndex, find, span/break/takeWhile/dropWhile, tails, unfoldr, mapAccumL, concatMap, reverse, splitAt, replicate, head/tail/last/init, zipWithIndex, imap, enumFromTo, length, take, drop, null (list-only versions still available)
- **Char**: isDigit, isAlpha, isAlphaNum, isSpace, isUpper, isLower, digitToInt, toLowerChar, toUpperChar, ord, chr
- **Numeric**: even/odd, abs'/signum'/min'/max' (monomorphic Int), round (Double→Int), parseIntM/parseInt, parseDoubleM/parseDouble
- **JSON**: Value(..), toJSON, (.=), object, lenses (`key`/`members`/`nth`/`values` / `_String`/`_Number`/`_Bool`/`_Array`/`_Object`/`_Int`/`_Double`/`_Null`, `^?`/`^..`/`preview`/`toListOf`), helpers (`?.`/`lookupKey`/`asText`/`asInt`). **No JSON parsing in Haskell** — `encode`/`decode` removed; use `runJson`/`httpGet` (parsed on Rust side)
- **Map**: fromList/toList, insert/delete/adjust, union/intersection/difference/unionWith/intersectionWith, singleton/empty, findWithDefault, foldlWithKey'/foldrWithKey, mapKeys/mapWithKey/filterWithKey
- **Monadic**: mapM/forM/foldM, when/unless/void/join/guard, (>=>)/(<=<), filterM, replicateM, zipWithM, concatMapM
- **Kleisli profunctor**: (&&&)/(***)/(|||) for `a -> M b` (fanout / pair-wise / Either-merge), firstK/secondK

### Tidepool.Text (in scope as TT., or import explicitly)

- `camelToSnake :: Text -> Text`
- `snakeToCamel :: Text -> Text`
- `capitalize :: Text -> Text`
- `titleCase :: Text -> Text`
- `padLeft :: Int -> Text -> Text`
- `padRight :: Int -> Text -> Text`
- `padLeftWith :: Int -> Char -> Text -> Text`
- `padRightWith :: Int -> Char -> Text -> Text`
- `center :: Int -> Text -> Text`
- `centerWith :: Int -> Char -> Text -> Text`
- `indent :: Int -> Text -> Text`
- `dedent :: Text -> Text`
- `wrap :: Int -> Text -> Text`
- `slugify :: Text -> Text`
- `truncateText :: Int -> Text -> Text`

### Tidepool.Table (in scope as Tab., or import explicitly)

- `parseCsv :: Text -> [[Text]]`
- `parseTsv :: Text -> [[Text]]`
- `parseDelimited :: Char -> Text -> [[Text]]`
- `renderTable :: [[Text]] -> Text`
- `renderTableWith :: Char -> Char -> [[Text]] -> Text`
- `column :: Int -> [[Text]] -> [Text]`
- `sortByColumn :: Int -> [[Text]] -> [[Text]]`
- `filterByColumn :: Int -> (Text -> Bool) -> [[Text]] -> [[Text]]`

### Heuristic Combinators — `Q a` (in preamble, auto-available)

First-class questions: `Q a` bundles schema + parser + confidence threshold.
- `pick cats ?? prompt` — classify. `yn ?? prompt` — judge. `obj schema ?? prompt` — extract.
- `Schema` ADT for `obj`/`llmJson` (NOT a JSON Value): `SObj [(Text, Schema)] | SArr Schema | SStr | SNum | SBool | SEnum [Text] | SOpt Schema`. Nested extraction: `obj (SObj [("items", SArr (SObj [("title", SStr), ("sev", SEnum ["p0","p1","p2"])]))]) ?? prompt`.
- `txt "field"`, `num "field"` — single-field extraction.
- `bar 0.95 q` — raise confidence threshold.
- Applicative: `(,) <$> pick cats <*> num "pri" ?? prompt` — one Haiku call, multiple extractions.
- `q ?! prompt` — returns `Sure a | Unsure Double a` (preserves confidence).
- `triage q render items`, `survey q render items`, `sift q render items` — batch ops.

### Haskell → JSON Rendering

Values returned via `pure x` are automatically rendered to JSON by the Rust runtime:

| Haskell type | JSON |
|-------------|------|
| `Int`, `Double` | number |
| `Text`, `String` | string |
| `Bool` | true/false |
| `[a]` | array |
| `(a, b)`, `(a, b, c)` | array (tuples are arrays, not objects) |
| `Maybe a` | value or null |
| `Value` | passthrough |
| `Map Text a` | object |
| `()` | null |

To get named fields, return `Value` via `object ["name" .= x, "size" .= y]`.

### Known Limits (audited 2026-06-11 — see `plans/gotcha-audit.md`)

The JIT runs a strict subset of Haskell, but the failure modes are now LOUD:
compile errors name the unsupported symbol, runtime errors carry the Haskell
message, unbounded recursion is a clean "stack overflow" yield error — not
SIGSEGV. The short, true standing list:

- **`read`/`reads`**: clean COMPILE error ("Unsupported FFI call: …gmpn…" with
  a GMP hint). Use `parseInt`/`parseDouble` from Prelude.
- **`T.takeWhile`/`T.dropWhile` — DIRECT use fixed, Prelude-wrapped use still
  broken.** The EPS unpoison (9a827a3) made GHC load unfoldings, so `map
  (T.takeWhile p) ts` used STRAIGHT from a user module is now correct (Core
  verified; pinned by `tidepool-runtime/tests/repro_takewhile_pap.rs`). But the
  `takeWhileT`/`dropWhileT` Prelude shadows are NOT retirable: delegating them to
  `T.takeWhile`/`T.dropWhile` (eta-reduced OR eta-expanded) was MEASURED BROKEN
  2026-06-11 — `repro_takewhilet_alias_pap.rs` went 10/14 red vs 14/14 for the
  manual `T.pack . go . T.unpack` body (three-way control). Corruption fires when
  `T.takeWhile` is reached through the cross-module Prelude wrapper with an
  operator-section predicate (`(/= '/')`) — even saturated — but not with a named
  predicate. So keep the manual shadows; the retirement plan
  (`plans/takewhile-shadow-retirement.md`) is CLOSED/REJECTED. Underlying
  codegen bug (cross-module wrapper + section predicate) awaits a mechanism fix.
- **`cycle`**: unresolved external (clean yield error, verified post-sentinel-fix);
  use manual recursion.
- ~~Double `T.breakOn` in a cross-module fn~~ (#313 t11): FIXED (TailCtx
  leak in the emit hylo, commit 0317fe5; guarded by repro313 tests).
- **Non-tail recursion** overflows ~10-20K frames with a clean yield error;
  tail recursion is unbounded (TCO).

Stale fears, verified gone: Integer defaulting in untyped local helpers,
`sum`/`product`/`maximum`/`minimum`/`foldr1`/`last`/`init` (error-worker
sentinel fix, commit 4273c51), `Floating` ops (`sqrt`/`sin`/`exp`/`log`),
`round` (correct banker's rounding), `even`/`odd`, `nub` — all fine.

If you DO hit a SIGILL/SIGSEGV, that's a compiler bug — report it. Common
root causes: constructor tag mismatch, missing external binding.

### MCP Eval Patterns

**Aperture pattern** (`ask` as decision gate): Place `ask` after data gathering, before expensive operations. The computation does the grunt work (scan files, parse, format a menu), then suspends. During the gap between suspend and resume, the caller can do independent scouting (bash, grep, other evals) using the surfaced information, then resume with an informed choice that steers the rest of the computation. The suspended eval is a coroutine checkpoint; the gap is a free-form intelligence window.

```haskell
-- Haskell gathers context, suspends for steering
data <- expensiveScan
answer <- ask (formatMenu data)
-- Caller scouts independently, then resumes
if shouldProceed answer
  then expensiveAnalysis data  -- only runs if steering says yes
  else pure "skipped"
```

**Census pattern**: One eval replaces N tool calls. `fsGlob` + `mapM fsMetadata` + filtering gives a codebase overview in a single round-trip.

**Diff-on-the-input-lane pattern** (`[patch|]`/Diff verbs): in EVALS, multi-line
`[patch|...|]` literals in `code` are corrupted by template indentation — diffs
ride the `input` payload lane instead: `applyDiff d where d = case input of
{ String s -> s; _ -> "" }` with the unified diff as the JSON string. `applyDiff`
is all-or-nothing (plan-first; zero writes on any conflict) and reports
conflicts/already-applied as DATA.

**Never hand-write hunk arithmetic** — `genPatchTo path newContent` reads the
current file and *generates* the unified diff for you (Myers O(ND) line diff,
3-line context, changes coalesced, counts/start-hints correct by construction,
output round-trips through `parsePatch`); an absent file yields a creation
patch, identical content yields `""`. Put the new file body on the `input` lane
(`genPatchTo "f" newC >>= applyDiff` where `newC = case input of {String s -> s;
_ -> ""}`) — generate then apply in one eval. `genPatch path old new ::
Either Text FilePatch` is the pure core; `diffFiles a b` diffs two existing
files. Hand-written hunk headers (when you must) still need count arithmetic
(`@@ -l,c +l,c @@`, c = ctx+del / ctx+ins line counts) or `parsePatch` rejects
loudly — but reach for `genPatchTo` first.

**Declarative small edits (line-range / anchor) — the `Edit` verbs.** When a
change is awkward as a diff (replace lines 10-15, insert after an anchor, an
edit that shouldn't have to reproduce surrounding context or the whole file),
name it with an `Edit` and let the engine lower it to a CONTEXT-anchored patch
that rides the same atomic apply: `applyEdits :: Text -> [Edit] -> M Value`
(in-eval) and `editsJ :: Value -> M Value` (input lane: `{file, edits:[{op,…}]}`).
`Edit` = `ReplaceLines lo hi [Text]` / `InsertAt n [Text]` / `ReplaceAnchor a
[Text]` / `InsertAfterAnchor a [Text]` / `InsertBeforeAnchor a [Text]` (1-based
lines; anchors are substring tests that must hit exactly one line). They INHERIT
the keystone discipline: `planEdits`/`planEditsJ` is a dry run returning the
rendered review `diff` (feed it to `applyDiff` after an `ask` — the edit
front-end and diff back-end meet at the patch text); `applyEdits` is
all-or-nothing (any conflict → zero writes); resolution problems come back as
DATA — `{"kind":"anchor-missing"|"anchor-ambiguous"|"range-out-of-bounds"|
"edits-overlap",…}`. **Line-number safety:** numbers resolve against the file
read in the SAME eval and bake into a context-anchored patch, so an in-eval
read+edit (e.g. line numbers straight from `grepGlob`/`rsFn`/`hsDef`) is safe;
passing numbers captured in a PRIOR eval is the footgun — prefer the anchor ops
for cross-eval edits (they are content-addressed and self-checking). The older
`patchFile`/`insertAfter` surgery verbs still work unchanged; the `Edit` verbs
are the keystone-integrated path and could later subsume them.

**checkDiff-first when a `[patch|]` pattern silently fails to match.** A no-match
is ambiguous: the INPUT might not parse at all, OR it parses but the pattern
shape differs. `checkDiff diffText` (pure, returns a `Value`) disambiguates:
`{"parses":false,"error":…}` means fix the *diff text*; `{"parses":true,"files":
[{path,create,hunks,oldLines,newLines}…]}` means fix the *pattern shape* against
that structure. The v1 pattern holes are: `$var` at a path; per-line content
holes ` $x`/`-$x`/`+$x` (each binds ONE line's `Text`); a bare `$var` in the
hunks position binds that file's whole `[Hunk]`; a trailing `...` allows extra
files. Line numbers in `@@` headers are HINTS — NOT matched (only body shape and
content are). There is no hole that binds a hunk's body lines as a list, and no
multi-hunk-with-structure binder — see the `qq_patch_pat_*` Suite fixtures for
the canonical, working shapes. Column-0 `[patch|]` literals work fine in helpers
and .tidepool/lib source files.

### Structural Search (MCP)

- `hsDef`/`hsSig`/`rsFn` recipes find function/signature definitions by name.
- `rHas`/`rInside` are deep by default (`stopBy: end`). Use `rHasChild`/`rInsideParent` for direct children.
- `grepGlob :: Text -> Text -> M [(Text, Int, Text)]` provides structured text-level search with regex and filename globbing.

### Adding new Prelude functions

Typeclass-dictionary polymorphism WORKS on the JIT — do not reflexively monomorphize.
(Verified live 2026-06-11: custom classes, multi-param classes, GADT construction +
type-indexed dispatch + dictionary use at refined types all pass. GHC specialization
is enabled; lazy poison closures defuse error-branch dictionary slots — the old
"dictionaries crash" rule is retired history.)

Shadow with a monomorphic version only for:
1. **Genuinely unsupported FFI** — `showDouble` (floatToDigits/Integer), `round`
   (rintDouble), GMP beyond the add/sub shims. The shadow works around the FFI gap,
   not the dictionary.
2. **Ergonomics** — Pack/Len/Null/Slice-style Text+list polymorphism by design.

A GADT-sibling-alt crash observed live on 2026-06-11 (dictionary method at two
different refined types: `data K a where { KInt :: K Int; KPrec :: Int -> K Double }`
with sibling alts `show n` / `show d`) RESOLVED without a targeted fix after the
2026-06-12 lib/table changes and was never reproducible in the test harness on any
build — prime suspect is a DataConTable stableVarId collision (56-bit hash; the
same class that evicted freer-simple's Union when the ghc package flooded the
table). If a GADT case crashes with no other explanation: re-run with
TIDEPOOL_VARID_AUDIT=1 and check for collisions FIRST, before suspecting emit.
