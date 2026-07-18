# Tidepool

Compile Haskell effect stacks into Cranelift-backed state machines drivable from Rust.

[![CI](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml/badge.svg)](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)

## What is Tidepool?

Tidepool compiles [freer-simple](https://hackage.haskell.org/package/freer-simple) effect stacks from Haskell into native code via Cranelift JIT, producing effect machines that can be driven step-by-step from Rust. Write your business logic as a pure Haskell effect program, compile it once, then run it with Rust-side effect handlers.

Haskell describes what to do. Rust does it.

## Why: bash++ for LLM agents

LLMs are near-natively fluent in two languages they've read for decades: shell
and Haskell. Tidepool turns the second into an agent tool surface — a
"basically GHCi" environment (a one-shot `eval` tool and a stateful repl)
where one Haskell expression replaces a dozen tool calls: grep + read +
transform + write as a single round trip, over typed effects (files,
processes, git, HTTP, LSP, LLM calls) instead of string-splicing.

Two principles follow:

- **The API is the prompt.** The surface mirrors canonical Haskell
  (`Data.Map`, aeson-shaped JSON, GHCi conventions) because models already
  know it — every deviation from standard Haskell is a fluency tax, every
  mirror is free fluency.
- **The interface evolves as an optimization loop, not a static design.**
  Clean-context models work real tasks; the cases where tidepool beats bash —
  and the frictions where it doesn't — drive each round of UX changes and new
  effects.

## The repl: typed working memory

Tidepool ships two MCP servers over the same effect surface: `tidepool`
(one-shot `eval` — every call is a fresh program) and `tidepool-repl` (a
GHCi-style session — one resident JIT machine whose bindings, declarations,
and heap persist across calls). The repl is where the model stops acting like
a tool-caller and starts acting like someone at a workbench:

- **Substrate once, interrogate for many turns.** Bind an expensive value (a
  parsed corpus, an API dump, a git history) in turn one; every later turn is
  a cheap typed fold over it. No re-fetch, no re-derive.
- **Intermediates stay out of the context window.** Big values live in the
  session heap, not the conversation. Fold them in-session and return the
  aggregate; a value too large to render is auto-stubbed but stays a live
  binding you can keep computing on.
- **A toolkit accumulates.** Function and `data` declarations persist with
  GHCi shadowing semantics — define a helper in turn two, refine it in turn
  nine; a reshaped `data` type coexists with its older generation.

A condensed real session — join every backtick-quoted claim in this repo's
docs against the tracked file tree, with no file ever entering the model's
context:

```haskell
-- turn 1: substrate (doc bodies + file list live in the session heap)
docs <- readGlob "*/CLAUDE.md"
Right ls <- run "git ls-files"
let tracked = Set.fromList (T.lines ls.stdout)

-- turn 2: every `claim` in every doc (only a count returns to the model)
let claims = [ (d.path, s) | d <- docs, Right t <- [d.contents]
             , (i, s) <- zip [(0::Int)..] (T.splitOn "`" t), odd i ]

-- turn 3: the verdict — only the misses cross back into context
pure [ c | c@(_, s) <- claims, T.count "/" s > 0, not (Set.member s tracked) ]
```

The thousand-file join runs in the session; a handful of tokens come back.
This exact fold found real drift in this repo's own docs (commit `b227fdc2`).

## Getting Started

### 1. Install the MCP servers

```bash
cargo install tidepool     # one-shot eval server
cargo install --git https://github.com/tidepool-heavy-industries/tidepool tidepool-repl
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
    "tidepool":      { "command": "tidepool" },
    "tidepool-repl": { "command": "tidepool-repl" }
  }
}
```

**Environment variables:**
- `TIDEPOOL_EXTRACT` — path to the `tidepool-extract` binary (falls back to `tidepool-extract` on `$PATH`)
- `TIDEPOOL_PRELUDE_DIR` — override the Haskell stdlib location (normally embedded in the binary)
- `TIDEPOOL_GHC_LIBDIR` — override GHC's lib directory (avoids calling `ghc --print-libdir`)
- `RUST_LOG` — set to `debug` or `info` for server diagnostics on stderr

## LLM provider

The `Llm` effect handles classification, extraction, and judgment requests via
the [genai](https://github.com/jeremychone/rust-genai) crate, which routes by
model name (`claude-*` → Anthropic, `gpt-*` → OpenAI, `gemini-*` → Google,
`ollama::<model>` → Ollama, …). Set the target provider's standard API-key env
var (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …).

**Environment variables:**
- `TIDEPOOL_LLM_MODEL` — model name (default: `gpt-4o-mini`); precedence is `--llm`/`TIDEPOOL_LLM_MODEL` > config file > default
- `TIDEPOOL_LLM_PROVIDER=openai` — deprecated: genai routes by model name; set `TIDEPOOL_LLM_MODEL` instead

> **Structured Outputs**: OpenAI-targeting models use `response_format: { type: "json_schema" }` with `strict: false` to accommodate optional schema fields generated by the Haskell `schemaToValue` helper.

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
tidepool-handlers/          Concrete effect handlers shared by both servers
tidepool-repl/              GHCi-style resident-session MCP server
tidepool-lsp/               LSP client + workspace daemon (call graph, hover, refs)
tidepool-bignum/            Native ghc-bignum shims (Integer arithmetic sans GMP)
tidepool-bridge-effects/    Bridged record types shared by handlers + test mocks
tidepool-testing/           Test utilities + property-based generators
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

### Cancelling runaway programs

`JitEffectMachine::cancel_handle()` returns a `Send + Sync + Clone`
`CancelHandle` that any other thread (typically a watchdog) can use to abort
a running program:

```rust,ignore
let mut vm = JitEffectMachine::compile(&expr, &table, 1 << 20)?;
let handle = vm.cancel_handle();

std::thread::spawn(move || {
    std::thread::sleep(std::time::Duration::from_secs(5));
    handle.cancel();
});

match vm.run_pure() {
    Err(JitError::Yield(YieldError::Cancelled)) => {
        // Watchdog fired — the program did not complete within the budget.
    }
    other => { /* normal result or different error */ }
}
```

Cancellation is observed at three safepoints, all of which promptly unwind
with `YieldError::Cancelled`:

1. **Tail-call trampoline iteration** — every iteration of a `letrec`-style
   tail loop. Fires constantly in tail-recursive Haskell.
2. **Effect-dispatch boundary** — checked in `JitEffectMachine::run` after
   each handler invocation, before resuming the JIT. Covers freer-simple
   effect loops, including handler-driven cancellation (a watchdog handler
   that flips its own machine's `CancelHandle`).
3. **GC heap-check safepoint** — fires on every non-trivial allocation. On
   cancel observation, `gc_trigger` skips GC and routes the failing
   allocation through `runtime_oom`'s poison path. Covers pure non-tail-call
   allocator loops (e.g. lazy `length [1..]`-style programs) where neither
   of the above safepoints is reachable.

The flag is per-machine, not per-run. Call `handle.reset()` between runs if
you intend to reuse the same `JitEffectMachine` after a cancellation.

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
| **Fs** | `FsRead`, `FsWrite`, `FsGlob`, `FsReadGlob` (batch read, per-file failure isolation), `FsGrep`, `FsListDir`, `FsExists`, `FsMetadata` — sandboxed file I/O + editing verbs |
| **Http** | `HttpGet`, `HttpPost` — outbound HTTP (no localhost) |
| **Exec** | `Run`, `RunIn` — shell commands returning typed `Proc` records |
| **Lsp** | `LspWhere`, `LspCallers`, `LspCallees`, `LspRefs`, `LspDef`, `LspHover`, `LspRename`, `LspDiagnostics` — semantic code graph via rust-analyzer |
| **Llm** | `LlmStructured` — schema-validated LLM call for classification/extraction |
| **Git** | `GitLog`, `GitStatus`, `GitDiffStat`, `GitShow` — read-only queries as typed records |
| **Time** | `TimeNow` — UTC clock (epoch millis; `getCurrentTime`, ISO-8601 helpers) |
| **Ask** | `AskWith :: Text -> Value -> Ask Value` — suspend and ask the calling LLM a schema-validated question |

> **`--debug` flag**: Run `tidepool --debug` to enable the **Meta** effect (`MetaConstructors`, `MetaLookupCon`, `MetaPrimOps`, `MetaEffects`, `MetaDiagnostics`, `MetaVersion`, `MetaHelp`) for runtime introspection. For git operations, use `run "git ..."` via the Exec effect.

### MCP Server Usage Examples

With GHC available, the servers compile live. The `eval` tool takes one Haskell expression per call; `tidepool-repl` keeps a resident session (see [The repl: typed working memory](#the-repl-typed-working-memory)). The examples below use the one-shot form; every one also works turn-by-turn in the repl, where bindings persist between them.

#### Pure computation

The simplest case — pure Haskell compiled to native code and back:

```haskell
pure (1 + 2 :: Int)
-- → 3
```

#### Sequencing monadic effects

Each effect operation (`say`, `readFile`, `run`, etc.) is a monadic action; failures are typed data, so `Right x <-` is the natural spelling. Chain them with `do`-notation:

```haskell
Right content <- readFile "Cargo.toml"
let lineCount = length (T.lines content)
say ("Cargo.toml has " <> show lineCount <> " lines")
pure lineCount
```
```
## Output
Cargo.toml has 57 lines

## Result
57
```

Effects compose freely — read files, run shell commands, query a KV store, all in one program:

```haskell
Right p <- run "rustc --version"
say ("Rust: " <> T.strip p.stdout)
kvSet "env" (object ["rustc" .= T.strip p.stdout])
v <- kvGet "env"
pure (fromMaybe Null v)
```
```json
{ "rustc": "rustc 1.93.0 (254b59607 2026-01-19)" }
```

#### Codebase census in a single eval

One eval replaces many tool calls. Batch-read matching files (per-file failure isolation — one binary file doesn't fail the sweep) and return structured JSON:

```haskell
rs <- readGlob "tidepool-*/Cargo.toml"
pure (toJSON [ object ["file" .= r.path, "lines" .= length (T.lines t)]
             | r <- rs, Right t <- [r.contents] ])
```
```json
[
  {"file": "tidepool-bignum/Cargo.toml", "lines": 12},
  {"file": "tidepool-bridge/Cargo.toml", "lines": 24},
  ...
]
```

#### Structured suspension via `ask`

`ask` suspends the computation and hands a schema-validated question to the calling LLM, which can do independent work (run other tools, think) before resuming. The suspended eval is a coroutine checkpoint, and an invalid reply does not consume the continuation:

```haskell
files <- glob "tidepool-*/src/lib.rs" >>= liftEither
v <- ask (SObj [("file", SEnum files)]) "Which lib.rs should I inspect?"
-- ← computation suspends here; the LLM resumes with {"file": "..."}
case v ^? key "file" . _String of
  Nothing -> pure Null
  Just chosen -> do
    content <- readFile chosen >>= liftEither
    pure (toJSON (take 5 (T.lines content)))
```

The LLM receives the question plus a JSON Schema whose enum is the live file list; the reply is validated server-side and the eval resumes exactly where it suspended — all one logical computation.

#### Complex effect sequences

Combine regex search, schema-validated LLM classification, and the KV store in one program:

```haskell
-- Find every TODO/FIXME marker in the workspace
hits <- grepGlob "TODO|FIXME" "**/*.rs" >>= liftEither
say (show (length hits) <> " markers found")

-- Classify a sample with a fast LLM (structured output, typed errors)
results <- for (take 5 hits) (\h -> do
  r <- llm (SObj [("category", SEnum ["bug", "cleanup", "feature"])])
           ("Classify this marker: " <> h.text)
  pure (object ["at" .= (h.path <> ":" <> show h.line), "class" .= either (const Null) id r]))

-- Persist results for later evals
kvSet "todo_audit" (toJSON results)
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

- **`Text`, not `String`:** The JIT evaluates eagerly, making `String` (`[Char]`) expensive. The Prelude standardizes on `Text` — use it everywhere. `show` returns `Text`, `pack` is polymorphic, `error` takes `Text`.
- **Deep recursion in the oracle interpreter:** the tree-walking `tidepool-eval` (the differential-testing oracle, not the serving path) recurses on the host stack and can overflow around ~50 frames. The JIT backend supports TCO and handles deep recursion.
- **Case traps abort cleanly:** an exhausted case branch (constructor tag mismatch, unexpected value shape) surfaces as a `runtime case trap` diagnostic with a breadcrumb — a poisoned eval result, not a process SIGILL.

## License

Licensed under either of [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)
or [MIT license](http://opensource.org/licenses/MIT) at your option.
