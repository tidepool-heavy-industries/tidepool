# Tidepool

Compile Haskell effect stacks into Cranelift-backed state machines drivable from Rust.

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

You can also use `mcp-wrapper.py` (from the repo) to add a `__mcp_restart` tool for hot-restarting the server:

```json
{
  "mcpServers": {
    "tidepool": {
      "command": "python3",
      "args": ["/path/to/tidepool/tools/mcp-wrapper.py", "tidepool"]
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
| **SG** | `SgFind`, `SgPreview`, `SgReplace`, `SgRuleFind`, `SgRuleReplace` — structural code search via ast-grep |
| **Http** | `HttpGet`, `HttpPost`, `HttpRequest` — outbound HTTP (no localhost) |
| **Exec** | `Run`, `RunIn`, `RunJson` — shell command execution |
| **Meta** | `MetaConstructors`, `MetaLookupCon`, `MetaPrimOps`, `MetaEffects`, `MetaDiagnostics`, `MetaVersion`, `MetaHelp` |
| **Git** | `GitLog`, `GitShow`, `GitDiff`, `GitBlame`, `GitTree`, `GitBranches` — native git access via libgit2 |
| **Ask** | `Ask :: Text -> Ask Value` — suspend execution and ask the calling LLM a question |

## Development

```bash
nix develop              # Enter dev shell
cargo check --workspace  # Type check
cargo test --workspace   # Run all tests
```

## License

Licensed under either of [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)
or [MIT license](http://opensource.org/licenses/MIT) at your option.
