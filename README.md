# Tidepool

Compile Haskell effect stacks into Cranelift-backed state machines drivable from Rust.

[![CI](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml/badge.svg)](https://github.com/tidepool-heavy-industries/tidepool/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)](LICENSE.md)

## What is Tidepool?

Tidepool compiles [freer-simple](https://hackage.haskell.org/package/freer-simple) effect stacks from Haskell into native code via Cranelift JIT, producing effect machines that can be driven step-by-step from Rust. Write your business logic as a pure Haskell effect program, compile it once, then run it with Rust-side effect handlers.

Haskell describes what to do. Rust does it.

## Why: bash++ for LLM agents

LLMs are near-natively fluent in two languages they've read for decades: shell
and Haskell. Tidepool turns the second into an agent tool surface — a
"basically GHCi" environment (a one-shot `eval` tool and a stateful repl)
where one Haskell expression replaces a dozen tool calls: grep + read +
transform + write as a single round trip, over typed effects (files,
processes, git, HTTP, LLM calls) instead of string-splicing.

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

### 4. Start an actor ensemble with Shoal

`cargo install tidepool` also installs `shoal`. With tmux, Codex, and the GHC
toolchain available, start the bundled typed DevSwarm policy from any project:

```bash
shoal init
```

Shoal creates a `shoal-<project>` tmux session. One host process owns every
resident Haskell actor; each interactive actor is an ordinary Codex TUI in its
own pane. Worker actors receive retained managed worktrees; the root remains in
the source checkout and is instructed to orchestrate rather than implement.
The fresh root opens ready and idle without spending an inference turn. Its
conversation identity binds on the first real prompt, after which Shoal can
deliver lifecycle wakes through Codex's native queue.
Detach with `Ctrl-b d`. `shoal init --recreate` starts a new actor incarnation
while retaining the root Codex conversation. It fails if the retained binding
is unavailable and tells the resumed model explicitly that prior handles,
workers, inbox rows, exits, and resident state were not restored.

Use `--no-attach` for headless startup; it prints the exact attach, host-log,
status-file, and teardown commands.

**Environment variables:**
- `TIDEPOOL_EXTRACT` — path to the `tidepool-extract` binary (falls back to `tidepool-extract` on `$PATH`)
- `TIDEPOOL_PRELUDE_DIR` — override the Haskell stdlib source root (normally embedded in the binary). Must point at a directory containing `Tidepool/Prelude.hs` — a set-but-invalid value is a hard startup error, not a silent fall-through.
- `TIDEPOOL_GHC_LIBDIR` — override GHC's lib directory (avoids calling `ghc --print-libdir`)
- `TIDEPOOL_TOOLCHAIN_STAMP` — override the deploy-stamp path (default: under the cache dir, `toolchain-stamp.json`). The stamp records what `scripts/redeploy.sh` last deployed; servers compare their resolved extract + stdlib against it at startup.
- `TIDEPOOL_TOOLCHAIN_HANDSHAKE` — deploy-stamp check severity: `error` (default — an extract/stdlib mismatch aborts startup), `warn` (log and continue), or `off` (skip the check).
- `RUST_LOG` — set to `debug` or `info` for server diagnostics on stderr

## LLM provider

The `Llm` effect handles classification, extraction, and judgment requests via
the [genai](https://github.com/jeremychone/rust-genai) crate, which routes by
model name (`claude-*` → Anthropic, `gpt-*` → OpenAI, `gemini-*` → Google,
`ollama::<model>` → Ollama, …). Set the target provider's standard API-key env
var (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …).

**Environment variables:**
- `TIDEPOOL_LLM_MODEL` — model name (default: `gpt-4o-mini`); precedence is `--llm`/`TIDEPOOL_LLM_MODEL` > config file > default

> **Structured Outputs**: OpenAI-targeting models use `response_format: { type: "json_schema" }` with `strict: false` to accommodate optional schema fields generated by the Haskell `schemaToValue` helper.

> **Migration note**: `TIDEPOOL_LLM_PROVIDER=openai` is still accepted but is compat-only — genai already routes by model name. Set `TIDEPOOL_LLM_MODEL` instead of relying on it.

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
nix develop --command just quick  # First run; supplies Just and the toolchain
just check           # Format, strict clippy, and the default test tier
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
tidepool-extract-cmd/       The one `tidepool-extract` invocation builder: bin resolution, typed args, the spawn
tidepool-atomic-write/      Atomic write-then-rename, shared by every durable on-disk store
tidepool-protocol/          Effect contract as data: one schema generating macro DSL, wire mirrors, extractor tables, harness classification
tidepool-codegen/           Cranelift JIT compiler + effect machine
tidepool-toolchain/         Toolchain discovery, fingerprints, paths, and compile cache
tidepool-runtime/           High-level API: compile_haskell, compile_and_run, sessions
tidepool-mcp/               MCP server library (generic over effect handlers)
tidepool-handlers/          Concrete effect handlers shared by both servers
tidepool-repl/              GHCi-style resident-session MCP server
tidepool-harness/           Resident harness: session-tree turn lifecycle, the selfharness driver
tidepool-worktree/          Managed git worktrees, durable registry, typed repository events
tidepool-agent/             Typed headless subagents: the backend seam + the Codex adapter
tidepool-web/               Operator GUI (HTTP+SSE, Datastar) for the self-iterating harness
tidepool-bignum/            Native ghc-bignum shims (Integer arithmetic sans GMP)
tidepool-bridge-effects/    Bridged record types shared by handlers + test mocks
tidepool-testing/           Test utilities + property-based generators (internal)
```

## How It Works

1. **Write Haskell** using `freer-simple` effects (e.g. `emit "hello" >> awaitInt`)
2. **Extract GHC Core** via `tidepool-extract`, which serializes to CBOR
3. **Load in Rust** as `CoreExpr` + `DataConTable` (the IR)
4. **Compile to native** via Cranelift, producing a `JitEffectMachine`
5. **Run with handlers** — the machine yields effect requests; Rust handlers respond

The production path does not run `tidepool-optimize`; that crate is used for
differential and optimizer testing.

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
    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Response, EffectError> {
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
    Err(JitError::Yield(YieldError::Runtime(RuntimeError::Cancelled))) => {
        // Watchdog fired — the program did not complete within the budget.
    }
    other => { /* normal result or different error */ }
}
```

Cancellation is represented as `YieldError::Runtime(RuntimeError::Cancelled)`
(cancellation shares the same `RuntimeError` cause cell as other runtime
faults). It is observed at safepoints throughout the JIT, all of which
promptly unwind with that error:

- **Tail-call trampoline iteration** — every iteration of a `letrec`-style
  tail loop. Fires constantly in tail-recursive Haskell.
- **Effect-dispatch boundary** — checked in `JitEffectMachine::run` after
  each handler invocation, before resuming the JIT. Covers freer-simple
  effect loops, including handler-driven cancellation (a watchdog handler
  that flips its own machine's `CancelHandle`).
- **GC heap-check safepoint** — fires on every non-trivial allocation. On
  cancel observation, `gc_trigger` skips GC and routes the failing
  allocation through `runtime_oom`'s poison path. Covers pure non-tail-call
  allocator loops (e.g. lazy `length [1..]`-style programs) where neither
  of the above safepoints is reachable.
- **Recursive join back-edge** — a GHC-loopified non-tail, non-allocating
  spin (a Cranelift `jump` closing a recursive join) reaches none of the
  above; a dedicated check before the back-edge `jump` catches it.

The flag is per-machine, not per-run. Call `handle.reset()` between runs if
you intend to reuse the same `JitEffectMachine` after a cancellation.

The MCP server (`tidepool`, `tidepool-repl`) layers its own per-call timeout
on top of the above: an eval that reaches an effect boundary within the grace
timeout interval parks as a resumable continuation; a pure computation that
reaches no safepoint before the timeout interval expires has its thread
**detached** — left
running in the background rather than killed — and the call returns a
timeout error. This is deliberate: it avoids tearing down a thread mid-unsafe
operation.

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
| **FsRead** | `FsRead`, `FsGlob`, `FsReadGlob` (batch read, per-file failure isolation), `FsGrep`, `FsListDir`, `FsExists`, `FsMetadata` — sandboxed filesystem queries |
| **FsWrite** | `FsWrite`, `FsWriteCas` — sandboxed writes and compare-and-swap edits |
| **Http** | `HttpGet`, `HttpPost` — outbound HTTP (no localhost) |
| **Exec** | `Run`, `RunIn` — shell commands returning typed `Proc` records |
| **Llm** | `LlmStructured` — schema-validated LLM call for classification/extraction |
| **Git** | `GitLog`, `GitStatus`, `GitDiffStat`, `GitShow` — read-only queries as typed records |
| **Time** | `TimeNow` — UTC clock (epoch millis; `getCurrentTime`, ISO-8601 helpers) |
| **Ask** | `AskWith :: Text -> Value -> Ask Value` — suspend and ask the calling LLM a schema-validated question |
| **RunLLMTurn** | `RunLLMTurnWith`, `RunLLMTurnFreezeWith` — interposed, like `Ask`: `runLLMTurn`/`runLLMTurnFork`/`runLLMTurnFanout` open a clean-context model sub-turn and deliver a typed answer; `freezeContext` snapshots the calling context for later branching |
| **Fork** | `ForkWith`, `ForkAllWith` — `fork @T brief`/`forkAll @T briefs` delegate to one or more sub-answerers, each producing a typed `T`; the resident harness supports recursively forked child sessions within configured budgets |

> **`--debug` flag**: Run `tidepool --debug` to enable the **Meta** effect (`MetaConstructors`, `MetaLookupCon`, `MetaPrimOps`, `MetaEffects`, `MetaDiagnostics`, `MetaVersion`, `MetaHelp`) for runtime introspection. For git operations, use `run "git ..."` via the Exec effect.

> **Effect declaration is not effect servicing.** Every constructor above compiles in the base stack on `tidepool` and `tidepool-repl`, but those two servers share one request parser that recognizes only `AskWith` and `RunLLMTurnWith` as suspensions it can answer. `RunLLMTurnFreezeWith`, `ForkWith`, and `ForkAllWith` compile and suspend the machine like any other interposed effect, then fail at request-extraction time — there is no scheduler behind them on these two surfaces. The resident harness (`tidepool-harness`) is the one surface that implements the fuller vocabulary: freezing a context, forking one or many sub-answerers, and finalizing a node all route through its own session-tree turn engine instead.

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
just --list  # Discover development workflows
just quick   # Fast process-isolated unit tests
just check   # Format, strict clippy, and the default test tier
just verify  # Pre-review gate, including fixture freshness
```

## Known Limitations

- **`Text`, not `String`:** The JIT evaluates eagerly, making `String` (`[Char]`) expensive. The Prelude standardizes on `Text` — use it everywhere. `show` returns `Text`, `pack` is polymorphic, `error` takes `Text`.
- **Deep recursion in the oracle interpreter:** the tree-walking `tidepool-eval` (the differential-testing oracle, not the serving path) recurses on the host stack and can overflow around ~50 frames. The JIT backend supports TCO and handles deep recursion.
- **Case traps abort cleanly:** an exhausted case branch (constructor tag mismatch, unexpected value shape) surfaces as a `runtime case trap` diagnostic with a breadcrumb — a poisoned eval result, not a process SIGILL.

## License

Licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE.md):
free to use, modify, and share for any noncommercial purpose (personal
projects, research, education, charitable and government use included).
Commercial use requires a separate license — open an issue or contact the
author.

Versions published before this license change remain available under
their original MIT/Apache-2.0 terms via git history.
