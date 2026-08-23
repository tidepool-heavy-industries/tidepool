# Tidepool Architecture

Tidepool is a system for compiling Haskell effect programs into high-performance, JIT-compiled state machines that can be driven from Rust. The core philosophy is: **Haskell expands, Rust collapses.**

## Compile Pipeline

The transition from Haskell source to native execution goes through four transformations, with no configurable optimization stage in between:

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

The same compiled effect machine is driven from three different server shapes, each with its own request vocabulary — an effect *declaring* is not the same as a surface *servicing* it:

- **One-shot eval (`tidepool`)**: every `eval` call compiles and runs a fresh program to completion or a single suspension. Its `SessionEngine` request parser (`tidepool-runtime/src/session/engine.rs`) recognizes exactly two suspension shapes by constructor name: `AskWith` and `RunLLMTurnWith`. A program that suspends on anything else (`RunLLMTurnFreezeWith`, `ForkWith`, `ForkAllWith`) compiles and suspends the machine, then fails at request-extraction time — there is no handler behind those constructors on this surface.
- **Resident REPL (`tidepool-repl`)**: a GHCi-style session (bindings, declarations, and heap persist across calls) built on the same `PersistentSession`/`SessionEngine` machinery, and it reuses the identical request parser — so it services the same `AskWith`/`RunLLMTurnWith` subset and rejects the same constructors as the one-shot surface.
- **Resident harness (`tidepool-harness`)**: a session-TREE runtime, not a single session. `NodeTree`/`SessionRegistry` (`tidepool-harness/src/tree.rs`, `registry.rs`) own a set of nodes, each independently checked out, suspended, and resumed; a `pending_holes` map tracks every parked hole across the whole tree, keyed by session+hole. The harness's own turn engine classifies suspensions by constructor name across the FULL interposed vocabulary — `AskWith`, `AskUserWith`, `RunLLMTurnWith`, `RunLLMTurnFreezeWith`, `ForkWith`, `ForkAllWith`, `FinalizeWith` — and services all of them: freezing a context for later branching, forking one or many sub-answerers, finalizing a node. This is the only surface that runs an authored `State`/`render`/`loop` program indefinitely (the `selfharness` driver) rather than to a single completion.

Effect *declaration* (what `standard_decls()` puts in the compiled row) is shared across all three surfaces; effect *servicing* is not — only the harness implements the fuller interposed control vocabulary.

## Facade surface

The `tidepool` library crate re-exports the crates a Rust consumer needs to compile and run a Haskell program (`compile_haskell`, `JitEffectMachine`, `EffectHandler`, …). `tidepool-optimize` is deliberately NOT among them: it is a workspace member for its test/research consumers (see "Compile Pipeline" above), not a facade-level pipeline stage, so it stays out of the crate a downstream consumer installs to run programs.

## Crate Responsibilities

- **`tidepool-repr`**: Defines the extracted Core IR (`CoreExpr`, `DataConTable`) and handles CBOR serialization/deserialization.
- **`tidepool-eval`**: A tree-walking interpreter for evaluating Core expressions without JIT overhead, used for testing and as a reference implementation. Owns the runtime `Value` type (`tidepool-eval/src/value.rs`).
- **`tidepool-heap`**: Implements the manual memory layout (raw byte buffers) and the copying garbage collector used by the JIT runtime.
- **`tidepool-bignum`**: Native `ghc-bignum` shims — `Integer` arithmetic without GMP.
- **`tidepool-optimize`**: Optimization passes (beta reduction, DCE, inlining, case reduction). Test/research crate ONLY — see "Compile Pipeline" above; the production compile path never calls it, and the public facade does not depend on or re-export it.
- **`tidepool-codegen`**: The Cranelift-based compiler that generates native code and manages the `JitEffectMachine` lifecycle.
- **`tidepool-extract-cmd`**: The one `tidepool-extract` invocation builder (bin resolution, typed args, the spawn) that every caller of the Haskell toolchain goes through. `std`-only, zero deps, so `tidepool-macro` can depend on it without pulling in the runtime graph.
- **`tidepool-atomic-write`**: The one atomic write-then-rename helper, shared by every durable on-disk store in the workspace (worktree registry, agent binding table, checkpoints, compile cache, toolchain stamp).
- **`tidepool-runtime`**: The high-level orchestration layer that handles Haskell compilation (via `tidepool-extract-cmd`), caching (via `tidepool-atomic-write`), and running programs. Also owns the `SessionEngine`/`PersistentSession` machinery shared by the one-shot and REPL surfaces (see "Surfaces" above).
- **`tidepool-effect`**: Core traits and logic for effect dispatch and handling (`EffectHandler`, `DispatchEffect`).
- **`tidepool-protocol`**: The effect contract as data — one schema (verbs, records, errors, field types) that generates the macro DSL strings, wire mirrors, extractor verb tables, and harness classification lists that used to be hand-maintained separately. A `std`-only leaf with no runtime component; effects migrate here one at a time from `tidepool-mcp/src/effect_defs.rs`.
- **`tidepool-macro`**: Procedural macros embedding Haskell source as CBOR at build time (`haskell_eval!` for whole programs, `haskell_inline!` for inline snippets).
- **`tidepool-bridge`**: Provides `FromCore` and `ToCore` traits for seamless data conversion between Rust types and Tidepool `Value`s.
- **`tidepool-bridge-derive`**: Procedural macro crate providing `#[derive(FromCore)]` and `#[derive(ToCore)]`.
- **`tidepool-bridge-effects`**: Single-source bridged-record types (e.g. `Proc`, `Hit`, `Commit`) shared by handlers and test mocks.
- **`tidepool-handlers`**: Central effect-request handler arms — the Rust side of the effect contract (`<Eff>Req` matches, sandbox enforcement).
- **`tidepool-mcp`**: MCP server library, generic over effect handlers.
- **`tidepool-repl`**: GHCi-style resident-session MCP server (declarations and heap persist across calls). See "Surfaces" above for what it actually services.
- **`tidepool-lsp`**: DEPRECATED (2026-08-20, unused in practice, will not be fixed further — see `tidepool-lsp/CLAUDE.md`). The `tidepool-lsp-daemon` sidecar plus the `LspHandler` client. The `Lsp` effect stays in the canonical base row and every server still advertises `LspWhere`/`LspHover`/etc., but there is no in-repo process that starts the daemon — a normal Tidepool stack advertises LSP operations whose backing service is a separately launched, deprecated executable. The practical replacement is the ordinary file/search/`Exec` tool surface.
- **`tidepool-worktree`**: Managed git worktrees, a durable registry, and typed repository events — the runtime observes git state but has no git-workflow verbs of its own (no merge, no rebase).
- **`tidepool-agent`**: Typed headless coding subagents — the containment boundary and the one place a coding backend (Codex today) is named. Spawns a subagent into a managed `tidepool-worktree` worktree.
- **`tidepool-harness`**: Resident harness runtime — session-tree turn lifecycle, `SessionRegistry` checkout ownership, the selfharness driver that drives an authored `State`/`render`/`loop` program forever. See "Surfaces" above.
- **`tidepool-web`**: Operator GUI (HTTP+SSE, Datastar/d3) for the self-iterating harness.
- **`tidepool`**: Facade crate + the `tidepool` MCP server binary. See "Facade surface" above.
- **`tidepool-testing`**: Internal utilities and property-based generators for testing the compiler and runtime — the primary consumer of `tidepool-optimize`.

## Data Flow

1.  User provides Haskell code (or it's generated/inlined).
2.  `tidepool-runtime` invokes `tidepool-extract` to get CBOR.
3.  `tidepool-repr` parses CBOR into `CoreExpr`.
4.  `tidepool-codegen` emits Cranelift IR directly from that `CoreExpr` (no optimization pass), compiles to machine code, and constructs a `JitEffectMachine`, which owns and manages its own heap (built on `tidepool-heap`'s copying GC).
5.  `vm.run()` executes the machine, yielding effects to `EffectHandler`s until completion — or, on the harness surface, until the machine parks in the multi-hole `SessionRegistry` for later resumption.
