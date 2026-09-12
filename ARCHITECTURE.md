# Tidepool Architecture

Tidepool is a system for compiling Haskell effect programs into high-performance, JIT-compiled state machines that can be driven from Rust. The core philosophy is: **Haskell expands, Rust collapses.**

## Compile Pipeline

The transition from Haskell source to native execution goes through five transformations, with no configurable optimization stage in between:

1.  **Haskell Source**: Business logic is written in Haskell using `freer-simple` effect stacks. This allows for pure, composable descriptions of side-effecting operations.
2.  **GHC Core**: The `tidepool-extract` tool (a GHC frontend plugin) intercepts the compilation process to extract GHC's intermediate representation (Core). During this phase, types are erased, and casts/ticks are stripped.
3.  **CBOR Serialization**: The GHC Core is serialized into CBOR (Concise Binary Object Representation). This serves as the language-agnostic boundary between the Haskell toolchain and the Rust runtime.
4.  **Rust IR (`tidepool-repr`)**: The Rust side deserializes CBOR into a simplified Core IR, primarily consisting of `CoreExpr` (a `RecursiveTree` of `CoreFrame` variants) and a `DataConTable` for constructor metadata.
5.  **Cranelift JIT (`tidepool-codegen`)**: The IR is compiled directly into native machine code using the Cranelift JIT compiler, with no optimization pass in between. This produces a `JitEffectMachine` that manages its own heap and stack.

`tidepool-optimize` (beta reduction, DCE, inlining, case reduction) is a real crate with real passes, but the production compile path above never calls it. Its callers today are `tidepool-testing`'s differential generators, its own matrix/stack-safety tests, and `tidepool-runtime`'s differential/GC-pressure test suites — optimize-then-evaluate is a way to cross-check the optimizer against the oracle interpreter, not a step production code runs. See "Facade surface" below for how this is reflected in the public API.

## The Hylo Boundary

The hylomorphism's unfold/fold split falls on the language boundary — the structural relationship between the two sides of the system:
- **Haskell Expands**: The Haskell code builds up a recursive description of a computation (the "ana" phase). It defines the *what*—the sequence of effects and the logic connecting them.
- **Rust Collapses**: The Rust side interprets or JIT-compiles this description into a concrete execution that performs side effects and produces a final value (the "cata" phase). It defines the *how*—how a `FileRead` effect actually interacts with the OS.

## Effect Machine Model

Tidepool transforms `freer-simple` continuations into a state machine:
- In Haskell, a `freer-simple` program is a tree of `Leaf` (pure value) or `Node` (effect request + continuation).
- The JIT compiler transforms this into an **Effect Machine**.
- When the machine encounters an effect, it suspends execution, yields control to Rust with an effect request, and waits for a response.
- Rust handlers process the request and resume the machine with the result.
- The machine uses a custom **Copying GC** (`tidepool-heap`) to manage memory during execution, with a specialized stack walker for JIT frames.

## Surfaces

The compiled effect machine is used by three live entry points:

- **One-shot eval (`tidepool`)** compiles and runs a fresh program.
- **Resident REPL (`tidepool-repl`)** preserves declarations, bindings, and the heap across calls.
- **Shoal (`tidepool` actor host)** runs persistent typed actors. Haskell describes requests, watches, actor records, and context unfolds; Rust owns actor identity, scheduling, providers, worktrees, authority, and persistence.

Each entry point services only the effects installed by its Rust interpreter. Declaring an effect in Haskell does not install runtime authority or a handler.

## Facade surface

The `tidepool` library crate re-exports the crates a Rust consumer needs to compile and run a Haskell program (`compile_haskell`, `JitEffectMachine`, `EffectHandler`, …). `tidepool-optimize` is deliberately NOT among them: it is a workspace member for its test/research consumers (see "Compile Pipeline" above), not a facade-level pipeline stage, so it stays out of the crate a downstream consumer installs to run programs.

## Crate Responsibilities

- **`tidepool-repr`**: Defines the extracted Core IR (`CoreExpr`, `DataConTable`) and handles CBOR serialization/deserialization.
- **`tidepool-eval`**: A tree-walking interpreter for evaluating Core expressions without JIT overhead, used for testing and as a reference implementation. Owns the runtime `Value` type (`tidepool-eval/src/value.rs`).
- **`tidepool-heap`**: Implements the manual memory layout (raw byte buffers) and the copying garbage collector used by the JIT runtime.
- **`tidepool-bignum`**: Native `ghc-bignum` shims — `Integer` arithmetic without GMP.
- **`tidepool-optimize`**: Optimization passes (beta reduction, DCE, inlining, case reduction). Test/research crate ONLY — see "Compile Pipeline" above; the production compile path never calls it, and the public facade does not depend on or re-export it.
- **`tidepool-codegen`**: The Cranelift-based compiler that generates native code and manages the `JitEffectMachine` lifecycle.
- **`tidepool-extract-cmd`**: The public extractor boundary: CLI parsing, typed requests, process resolution, and resident-daemon transport/lifecycle. It sends a versioned request to the internal Haskell compiler worker, so Haskell owns GHC and Core translation rather than application infrastructure. `std`-only, zero deps, so `tidepool-macro` can depend on it without pulling in the runtime graph.
- **`tidepool-atomic-write`**: The one atomic write-then-rename helper, shared by every durable on-disk store in the workspace (worktree registry, agent binding table, checkpoints, compile cache, toolchain stamp).
- **`tidepool-toolchain`**: Toolchain discovery, fingerprints, paths, and the compiled-artifact cache.
- **`tidepool-runtime`**: The high-level orchestration layer for compiling and running programs. Also owns the `SessionEngine`/`PersistentSession` machinery shared by the one-shot and REPL surfaces (see "Surfaces" above).
- **`tidepool-effect`**: Core traits and logic for effect dispatch and handling (`EffectHandler`, `DispatchEffect`).
- **`tidepool-protocol`**: The effect contract as data — one schema (verbs, records, errors, field types) that generates the macro DSL strings, wire mirrors, extractor verb tables, and harness classification lists. A `std`-only leaf with no runtime component; effects migrate here one at a time from `tidepool-mcp/src/effect_defs.rs`.
- **`tidepool-macro`**: Procedural macros embedding Haskell source as CBOR at build time (`haskell_eval!` for whole programs, `haskell_inline!` for inline snippets).
- **`tidepool-bridge`**: Provides `FromCore` and `ToCore` traits for seamless data conversion between Rust types and Tidepool `Value`s.
- **`tidepool-bridge-derive`**: Procedural macro crate providing `#[derive(FromCore)]` and `#[derive(ToCore)]`.
- **`tidepool-bridge-effects`**: Single-source bridged-record types (e.g. `Proc`, `Hit`, `Commit`) shared by handlers and test mocks.
- **`tidepool-handlers`**: Central effect-request handler arms — the Rust side of the effect contract (`<Eff>Req` matches, sandbox enforcement).
- **`tidepool-mcp`**: MCP server library, generic over effect handlers.
- **`tidepool-repl`**: GHCi-style resident-session MCP server (declarations and heap persist across calls). See "Surfaces" above for what it actually services.
- **`tidepool-worktree`**: Managed git worktrees, a durable registry, and typed repository events. It includes one typed merge-and-abort primitive; conflict resolution and other git workflow remain authored policy.
- **`tidepool-agent`**: Typed headless coding subagents — the containment boundary and the one place a coding backend (Codex today) is named. Spawns a subagent into a managed `tidepool-worktree` worktree.
- **`tidepool`**: Facade crate + the `tidepool` MCP server binary. See "Facade surface" above.
- **`tidepool-testing`**: Internal utilities and property-based generators for testing the compiler and runtime — the primary consumer of `tidepool-optimize`.

## Data Flow

1.  User provides Haskell code (or it's generated/inlined).
2.  `tidepool-runtime` invokes `tidepool-extract` to get CBOR.
3.  `tidepool-repr` parses CBOR into `CoreExpr`.
4.  `tidepool-codegen` emits Cranelift IR directly from that `CoreExpr` (no optimization pass), compiles to machine code, and constructs a `JitEffectMachine`, which owns and manages its own heap (built on `tidepool-heap`'s copying GC).
5.  `vm.run()` executes the machine, yielding effects to `EffectHandler`s until completion or parking a resident continuation for later resumption.
