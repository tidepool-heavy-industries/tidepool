# Tidepool

Compile freer-simple effect stacks into Cranelift-backed state machines drivable from Rust. Haskell expands, Rust collapses. The language boundary is the hylo boundary.

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

Everything MCP users need in one import:
- **Types**: Int, Double, Char, Bool, Text, String, Maybe, Either, Map, Set, Value
- **Text ops**: pack/unpack, toUpper/toLower, strip, splitOn, replace, words/lines/unwords/unlines, isPrefixOf/isSuffixOf/isInfixOf
- **List ops**: map, filter, foldl/foldr/foldl', sort/sortBy, nub/nubBy, groupBy, partition, transpose, intercalate, intersperse, zip/zip3/unzip/unzip3, elemIndex/findIndex, find, span/break/takeWhile/dropWhile, tails, unfoldr, mapAccumL, concatMap, reverse, splitAt, replicate, head/tail/last/init, zipWithIndex, imap, enumFromTo
- **Char**: isDigit, isAlpha, isAlphaNum, isSpace, isUpper, isLower, digitToInt, toLowerChar, toUpperChar, ord, chr
- **Numeric**: even/odd, abs'/signum'/min'/max' (monomorphic Int), round (Double→Int), parseIntM/parseInt, parseDoubleM/parseDouble
- **JSON**: Value(..), encode/decode/eitherDecode, toJSON, (.=), object, lenses (key/nth/_String/_Number/_Bool/_Array/_Object, ^?/^../preview/toListOf), helpers (?./lookupKey/asText/asInt)
- **Map**: fromList/toList, insert/delete/adjust, union/intersection/difference/unionWith/intersectionWith, singleton/empty, findWithDefault, foldlWithKey'/foldrWithKey, mapKeys/mapWithKey/filterWithKey
- **Monadic**: mapM/forM/foldM, when/unless/void/join/guard, (>=>)/(<=<)

### Tidepool.Text (import explicitly)

`camelToSnake`, `snakeToCamel`, `capitalize`, `titleCase`, `center`, `padLeft`, `padRight`, `indent`, `dedent`, `wrap`, `slugify`, `truncateText`

### Tidepool.Table (import explicitly)

`parseCsv`, `parseTsv`, `parseDelimited`, `renderTable`, `renderTableWith`, `column`, `sortByColumn`, `filterByColumn`

### Adding new Prelude functions

Polymorphic base functions going through typeclass dictionaries often crash — the JIT eagerly evaluates error branches in dictionary records. Shadow with monomorphic versions using primops directly (e.g., `rem` instead of `Integral` dict). Avoid `maximum`/`minimum` from base (use manual `foldl'` with comparison).

**Infinite lists crash**: The JIT evaluates data constructor fields eagerly (no thunks). Infinite list producers like `[0..]` or `myFrom n = n : myFrom (n+1)` cause SIGSEGV unless GHC fuses them away (e.g., `take 5 [0..]` works via build/foldr fusion, but `zipWith f xs [0..]` does not fuse). Use `zipWithIndex`, `imap`, or `enumFromTo` instead.
