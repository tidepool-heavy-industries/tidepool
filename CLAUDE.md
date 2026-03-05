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
│   ├── guess-interpreted/ ← Demo: interpreted version
│   └── tide/              ← Demo: REPL
├── haskell/               ← Haskell harness (tidepool-extract) + test suite + stdlib
│   └── lib/Tidepool/      ← Haskell stdlib (auto-imported in MCP)
├── tools/
│   └── mcp-wrapper.py     ← MCP stdio proxy with __mcp_restart tool
├── flake.nix              ← Dev shell (Rust + GHC 9.12 with fat interfaces)
└── CLAUDE.md              ← YOU ARE HERE
```

### Build

```bash
nix develop              # Enter dev shell (provides Rust + GHC 9.12)
cargo test --workspace   # Run all tests
cargo check --workspace  # Type check
```

### MCP Server

The `tidepool` binary is an MCP server. See README.md for full setup instructions.

```bash
cargo install --path tidepool   # Install the binary
tidepool                        # Run (expects MCP JSON-RPC on stdio)
```

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

MCP users get `import Tidepool.Prelude hiding (error)` auto-imported. Additional modules available via the `imports` field.

### Tidepool.Prelude (auto-imported)

Everything MCP users need in one import.

> **Text, not String — the #1 usability trap.** The Prelude standardizes on `Text`. `show` returns `Text` (not `String`), and `pack` is polymorphic (identity on Text, `T.pack` on String), so `pack (show x)` works fine. `lines`/`words`/`unlines`/`unwords` all operate on `Text`. `error` takes `Text`. String literals are `Text` via `OverloadedStrings`. The underlying `String` type (`[Char]`) exists but is expensive under the JIT's eager evaluation. Stick to `Text` everywhere.

- **Types**: Int, Double, Char, Bool, Text, String, Maybe, Either, Map, Set, Value
- **Text ops**: pack/unpack, toUpper/toLower, strip, splitOn, replace, words/lines/unwords/unlines, isPrefixOf/isSuffixOf/isInfixOf, intercalate (Text→[Text]→Text), joinText, tReverse
- **Polymorphic ops**: `pack` (String→Text or Text→Text identity), `len` (Text or [a] → Int), `isNull` (Text or [a] → Bool), `stake`/`sdrop` (like take/drop on both Text and [a])
- **List ops**: map, filter, foldl/foldr/foldl', sort/sortBy, nub/nubBy, groupBy, partition, transpose, intersperse, zip/zip3/unzip/unzip3, elemIndex/findIndex, find, span/break/takeWhile/dropWhile, tails, unfoldr, mapAccumL, concatMap, reverse, splitAt, replicate, head/tail/last/init, zipWithIndex, imap, enumFromTo, length, take, drop, null (list-only versions still available)
- **Char**: isDigit, isAlpha, isAlphaNum, isSpace, isUpper, isLower, digitToInt, toLowerChar, toUpperChar, ord, chr
- **Numeric**: even/odd, abs'/signum'/min'/max' (monomorphic Int), round (Double→Int), parseIntM/parseInt, parseDoubleM/parseDouble
- **JSON**: Value(..), toJSON, (.=), object, lenses (key/nth/_String/_Number/_Bool/_Array/_Object, ^?/^../preview/toListOf), helpers (?./lookupKey/asText/asInt). **No JSON parsing in Haskell** — `encode`/`decode` removed; use `runJson`/`httpGet` (parsed on Rust side)
- **Map**: fromList/toList, insert/delete/adjust, union/intersection/difference/unionWith/intersectionWith, singleton/empty, findWithDefault, foldlWithKey'/foldrWithKey, mapKeys/mapWithKey/filterWithKey
- **Monadic**: mapM/forM/foldM, when/unless/void/join/guard, (>=>)/(<=<)

### Tidepool.Text (import explicitly)

`camelToSnake`, `snakeToCamel`, `capitalize`, `titleCase`, `center`, `padLeft`, `padRight`, `indent`, `dedent`, `wrap`, `slugify`, `truncateText`

### Tidepool.Table (import explicitly)

`parseCsv`, `parseTsv`, `parseDelimited`, `renderTable`, `renderTableWith`, `column`, `sortByColumn`, `filterByColumn`

### Heuristic Combinators — `Q a` (in preamble, auto-available)

First-class questions: `Q a` bundles schema + parser + confidence threshold.
- `pick cats ?? prompt` — classify. `yn ?? prompt` — judge. `obj schema ?? prompt` — extract.
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

### Dangerous Patterns (silent crash → SIGILL/SIGSEGV)

The JIT runs a strict subset of Haskell. These core idioms crash without useful error messages:

- **Infinite lists**: `[0..]`, `repeat x`, `cycle xs`, `iterate f x` — the JIT evaluates data constructor fields eagerly (no thunks). Use `enumFromTo`, `replicate`, `zipWithIndex` instead.
- **~~`maximum`/`minimum`/`sum`/`product`/`genericLength`~~**: Now work (lazy poison closure fix defers error branches).
- **`read`/`reads`**: Read typeclass crashes. Use `parseInt`/`parseDouble` from Prelude.
- **~~Floating-point math~~**: `sqrt`, `sin`, `cos`, `exp`, `log` now work (lazy poison closure fix).
- **`zipWith f xs [0..]`**: Doesn't fuse, infinite list crashes. Use `zipWithIndex` or `imap`.
- **~~Large Value trees~~**: Effect responses now capped at 10K Value nodes. RunJson also has a 512KB output cap.

When you hit SIGILL or SIGSEGV, the error message will be opaque ("null pointer", "signal 4", etc.). The root cause is almost always one of the above patterns.

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

### Adding new Prelude functions

Polymorphic base functions going through typeclass dictionaries often crash — the JIT eagerly evaluates error branches in dictionary records. Shadow with monomorphic versions using primops directly (e.g., `rem` instead of `Integral` dict). Note: `sum`/`product`/`maximum`/`minimum`/`genericLength` now work (lazy poison closure fix), and `Floating` math now works too (lazy poison closure fix).

**Infinite lists crash**: The JIT evaluates data constructor fields eagerly (no thunks). Infinite list producers like `[0..]` or `myFrom n = n : myFrom (n+1)` cause SIGSEGV unless GHC fuses them away (e.g., `take 5 [0..]` works via build/foldr fusion, but `zipWith f xs [0..]` does not fuse). Use `zipWithIndex`, `imap`, or `enumFromTo` instead.
