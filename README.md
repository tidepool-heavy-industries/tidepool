# Tidepool

Compile Haskell effect stacks into Cranelift-backed state machines drivable from Rust.

[![CI](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml/badge.svg)](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)

## What is Tidepool?

Tidepool compiles [freer-simple](https://hackage.haskell.org/package/freer-simple) effect stacks from Haskell into native code via Cranelift JIT, producing effect machines that can be driven step-by-step from Rust. Write your business logic as a pure Haskell effect program, compile it once, then run it with Rust-side effect handlers that provide IO, state, networking, or anything else.

Haskell expands (describes what to do). Rust collapses (does it). The language boundary is the hylo boundary.

## Getting Started

### 1. Install the MCP server

```bash
cargo install tidepool
```

### 2. Install the GHC toolchain (requires Nix)

The Haskell compiler (`tidepool-extract`) is needed to evaluate code. Install it via Nix:

```bash
# Install Nix (if needed):
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install

# Optional: use binary cache (skip 30min GHC build)
nix run nixpkgs#cachix -- use tidepool

# Install the tidepool GHC toolchain:
nix profile install github:tidepool-heavy-industries/tidepool#tidepool-extract
```

> **Note:** If `tidepool-extract` is not found, the server starts in setup mode and exposes only an `install_instructions` tool that tells the calling LLM what to install.

### 3. Configure your MCP client

The `tidepool` binary is an [MCP](https://modelcontextprotocol.io/) server that communicates over stdio.

**Claude Code** (`~/.claude/settings.json` or project `.claude/settings.json`):

```json
{
  "mcpServers": {
    "tidepool": {
      "command": "tidepool"
    }
  }
}
```

**Environment variables:**
- `TIDEPOOL_EXTRACT` — path to the `tidepool-extract` binary (falls back to `tidepool-extract` on `$PATH`)
- `TIDEPOOL_PRELUDE_DIR` — override the Haskell stdlib location (normally embedded in the binary)
- `TIDEPOOL_GHC_LIBDIR` — override GHC's lib directory (avoids calling `ghc --print-libdir`)
- `RUST_LOG` — set to `debug` or `info` for server diagnostics on stderr

### Verify

Once configured, your MCP client should see the `eval` tool. Try evaluating:

```haskell
pure (1 + 2 :: Int)
-- → 3
```

### Development (from source)

```bash
git clone https://github.com/tidepool-heavy-industries/tidepool.git
cd tidepool
nix develop          # Provides GHC 9.12 (fat interfaces) + Rust toolchain
cargo test --workspace
cargo install --path tidepool
```

## Architecture

```
tidepool/                   Facade crate + MCP server binary
tidepool-repr/              Core IR: CoreExpr, DataConTable, CBOR serialization
tidepool-eval/              Tree-walking interpreter: Value, Env, lazy evaluation
tidepool-heap/              Manual heap + copying GC for JIT runtime
tidepool-optimize/          Optimization passes: beta reduction, DCE, inlining, case reduction
tidepool-bridge/            FromCore/ToCore traits for Rust <-> Core value conversion
tidepool-bridge-derive/     Proc-macro: #[derive(FromCore)]
tidepool-macro/             Proc-macro: haskell_inline! { ... }
tidepool-effect/            Effect handling: EffectHandler trait, HList dispatch
tidepool-codegen/           Cranelift JIT compiler + effect machine
tidepool-runtime/           High-level API: compile_haskell, compile_and_run, caching
tidepool-mcp/               MCP server library (generic over effect handlers)
```

## How It Works

1. **Write Haskell** using `freer-simple` effects (e.g. `emit "hello" >> awaitInt`)
2. **Extract GHC Core** via `tidepool-extract`, which serializes to CBOR
3. **Load in Rust** as `CoreExpr` + `DataConTable` (the IR)
4. **Optimize** with configurable passes (beta reduction, inlining, dead code elimination)
5. **Compile to native** via Cranelift, producing a `JitEffectMachine`
6. **Run with handlers** — the machine yields effect requests; Rust handlers respond

## Examples

| Example | What it shows |
|---------|--------------|
| [`examples/guess/`](examples/guess/) | Number guessing game. Compile-time `haskell_inline!`, JIT, two effects (Console + Rng). The minimal "hello world". |
| [`examples/tide/`](examples/tide/) | Interactive REPL with 5 effects (Repl, Console, Env, Net, Fs). Multi-effect composition at scale. |

## Using as a Rust Library

### Defining effects

The core pattern is three steps:

**1. Haskell GADT defines the effect:**

```haskell
data Console a where
    Emit     :: String -> Console ()
    AwaitInt :: Console Int
```

**2. `#[derive(FromCore)]` Rust enum mirrors it:**

```rust
#[derive(FromCore)]
enum ConsoleReq {
    #[core(name = "Emit")]
    Emit(String),
    #[core(name = "AwaitInt")]
    AwaitInt,
}
```

**3. `impl EffectHandler` provides the implementation:**

```rust
impl EffectHandler for ConsoleHandler {
    type Request = ConsoleReq;
    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Emit(s) => { println!("{s}"); cx.respond(()) }
            ConsoleReq::AwaitInt => { /* read from stdin */ cx.respond(42i64) }
        }
    }
}
```

### Compile-time path (`haskell_inline!`)

```rust
use tidepool_macro::haskell_inline;
use tidepool_codegen::jit_machine::JitEffectMachine;

let (expr, table) = haskell_inline! {
    target = "greet",
    include = "haskell",
    r#"
greet :: Eff '[Console] ()
greet = emit "Hello from Haskell!"
    "#
};

let mut vm = JitEffectMachine::compile(&expr, &table, 1 << 20)?;
vm.run(&table, &mut frunk::hlist![ConsoleHandler], &())?;
```

### Runtime path (`compile_and_run`)

```rust
let result = tidepool_runtime::compile_and_run(
    &source, "result", &[], &mut handlers, &(),
)?;
println!("{}", result.to_json());
```

### Key crates

| Crate | Entry points |
|-------|-------------|
| [`tidepool-macro`](tidepool-macro/) | `haskell_inline!`, `haskell_eval!`, `haskell_expr!` — compile-time Haskell embedding |
| [`tidepool-effect`](tidepool-effect/) | `EffectHandler`, `EffectContext`, `DispatchEffect` — effect dispatch traits |
| [`tidepool-bridge-derive`](tidepool-bridge-derive/) | `#[derive(FromCore)]`, `#[derive(ToCore)]` — Haskell↔Rust value conversion |
| [`tidepool-runtime`](tidepool-runtime/) | `compile_and_run`, `compile_haskell`, `EvalResult` — high-level runtime API |
| [`tidepool-codegen`](tidepool-codegen/) | `JitEffectMachine` — Cranelift JIT compiler + effect machine |
| [`tidepool-mcp`](tidepool-mcp/) | `TidepoolMcpServer`, `DescribeEffect`, `EffectDecl` — MCP server library |

## MCP Server Effects

The `tidepool` binary provides these effect handlers:

| Effect | Operations |
|--------|-----------|
| **Console** | `Print :: Text -> Console ()` |
| **KV** | `KvGet`, `KvSet`, `KvDelete`, `KvKeys` — persistent key-value store |
| **Fs** | `FsRead`, `FsWrite`, `FsListDir`, `FsGlob`, `FsExists`, `FsMetadata` — sandboxed file I/O |
| **SG** | `SgFind`, `SgRuleFind` — structural code search via ast-grep |
| **Http** | `HttpGet`, `HttpPost` — outbound HTTP (no localhost) |
| **Exec** | `Run`, `RunIn` — shell command execution |
| **Llm** | `LlmChat`, `LlmStructured` — call a fast LLM for classification/extraction |
| **Ask** | `Ask :: Text -> Ask Value` — suspend execution and ask the calling LLM a question |

> **`--debug` flag**: Run `tidepool --debug` to enable the **Meta** effect (`MetaConstructors`, `MetaLookupCon`, `MetaPrimOps`, `MetaEffects`, `MetaDiagnostics`, `MetaVersion`, `MetaHelp`) for runtime introspection. For git operations, use `run "git ..."` via the Exec effect.

### MCP Server Usage Examples

Tidepool also supports live compilation, in cases where the user has the Haskell compiler available. To demonstrate this, we have provided an MCP server that provides this functionality. It's a bit like GHCi (Haskell's REPL), but specialized for the monadic composition of pure effects that are executed by Rust code.

#### Pure computation

The simplest case — pure Haskell compiled to native code and back:

```haskell
pure (1 + 2 :: Int)
-- → 3
```

#### Sequencing monadic effects

Each effect operation (`say`, `fsRead`, `run`, etc.) is a monadic action. Chain them with `do`-notation:

```haskell
content <- fsRead "Cargo.toml"
let lineCount = len (lines content)
say ("Cargo.toml has " <> pack (show lineCount) <> " lines")
pure lineCount
```
```
## Output
Cargo.toml has 29 lines

## Result
29
```

Effects compose freely — read files, run shell commands, query a KV store, all in one program:

```haskell
(_, rustc_out, _) <- run "rustc --version"
say ("Rust: " <> strip rustc_out)
kvSet "env" (object ["rustc" .= strip rustc_out])
v <- kvGet "env"
pure (case v of { Just val -> val; Nothing -> Null })
```
```json
{ "rustc": "rustc 1.93.0 (254b59607 2026-01-19)" }
```

#### Codebase census in a single eval

One eval replaces many tool calls. Glob for files, gather metadata, sort, return structured JSON:

```haskell
files <- fsGlob "tidepool-*/Cargo.toml"
sizes <- mapM (\f -> do
  (sz, _, _) <- fsMetadata f
  pure (object ["file" .= f, "bytes" .= sz])) files
pure (toJSON sizes)
```
```json
[
  {"bytes": 482, "file": "tidepool-bridge/Cargo.toml"},
  {"bytes": 921, "file": "tidepool-codegen/Cargo.toml"},
  ...
]
```

#### Free pagination via continuation

`ask` suspends the computation and returns control to the calling LLM. The LLM can do independent work (run other tools, think), then resume with an answer. The suspended eval is a coroutine checkpoint:

```haskell
files <- fsGlob "tidepool-*/src/lib.rs"
info <- mapM (\f -> do
  (sz, _, _) <- fsMetadata f
  pure (f <> " (" <> pack (show sz) <> " bytes)")) files
let numbered = map (\(i, f) -> pack (show i) <> ". " <> f) (zipWithIndex info)
answer <- ask ("Which file to inspect?\n" <> unlines numbered)
-- ← computation suspends here, LLM resumes with "9"
let chosen = head (sdrop (round (answer ^? _Number)) files)
content <- fsRead chosen
pure (toJSON (take 10 (lines content)))
```

The LLM sees a menu of 12 files with sizes, picks one, and the eval resumes to read it — all within a single logical computation.

#### Complex effect sequences

Combine structural code search (ast-grep), file I/O, and LLM classification in one program:

```haskell
-- Find all struct definitions in the codegen crate
matches <- sgFind Rust "struct $NAME { $$$FIELDS }" ["tidepool-codegen/src/"]
say ("Found " <> pack (show (length matches)) <> " structs")

-- Classify each one with a fast LLM
results <- mapM (\m -> do
  let text = case m of Match t _ _ _ _ -> t
  category <- llm ("Classify this Rust struct as 'data', 'config', or 'handler': " <> text)
  pure (object ["struct" .= text, "category" .= category])) (take 5 matches)

-- Persist results for later evals
kvSet "struct_analysis" (toJSON results)
pure (toJSON results)
```

This MCP server requires GHC (it uses GHC's intermediate representation Core, instead of reimplementing the type checker). Tidepool also supports baking Haskell code into Rust at compile time via `haskell_inline!`, such that GHC is not required at runtime.

## Development

```bash
nix develop              # Enter dev shell
cargo check --workspace  # Type check
cargo test --workspace   # Run all tests
```

## Known Limitations

- **Stack overflow at ~50+ recursion depth (eval):** The tree-walking interpreter (`tidepool-eval`) uses Rust's call stack for recursion. Deeply recursive Haskell functions (>~50 frames) may overflow. The JIT backend (`tidepool-codegen`) supports TCO and handles deep recursion.
- **`nub` crashes at ~31 elements with complex `Text`:** O(n²) equality comparisons on `Text` values can trigger "application of non-closure (tag=255)" around 31 elements. Use `nubBy` with simpler comparisons or shorter lists.
- **`Text`, not `String`:** The JIT evaluates eagerly, making `String` (`[Char]`) expensive. The Prelude standardizes on `Text` — use it everywhere. `show` returns `Text`, `pack` is polymorphic, `error` takes `Text`.
- **SIGILL = case trap, not missing primop:** All primop variants are implemented. `SIGILL` crashes come from Cranelift `trap` instructions on exhausted case branches (constructor tag mismatch, unexpected value shape). Check constructor tags and case coverage.
- **No JSON parsing in Haskell:** `encode`/`decode` are removed. Use the `httpGet` effect (parsed on the Rust side via serde_json) or `run` with external tools.

## License

Licensed under either of [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)
or [MIT license](http://opensource.org/licenses/MIT) at your option.
