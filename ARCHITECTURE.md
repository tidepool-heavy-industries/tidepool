# Tidepool Architecture

Tidepool is a system for compiling Haskell effect programs into high-performance, JIT-compiled state machines that can be driven from Rust. The core philosophy is: **Haskell expands, Rust collapses.**

## The 5-Layer Pipeline

The transition from Haskell source to native execution follows a structured 5-layer pipeline:

1.  **Haskell Source**: Business logic is written in Haskell using `freer-simple` effect stacks. This allows for pure, composable descriptions of side-effecting operations.
2.  **GHC Core**: The `tidepool-extract` tool (a GHC frontend plugin) intercepts the compilation process to extract GHC's intermediate representation (Core). During this phase, types are erased, and casts/ticks are stripped.
3.  **CBOR Serialization**: The GHC Core is serialized into CBOR (Concise Binary Object Representation). This serves as the language-agnostic boundary between the Haskell toolchain and the Rust runtime.
4.  **Rust IR (`tidepool-repr`)**: The Rust side deserializes CBOR into a simplified Core IR, primarily consisting of `CoreExpr` (a `RecursiveTree` of `CoreFrame` variants) and a `DataConTable` for constructor metadata.
5.  **Cranelift JIT (`tidepool-codegen`)**: The IR is optimized and then compiled into native machine code using the Cranelift JIT compiler. This produces a `JitEffectMachine` that manages its own heap and stack.

## The Hylo Boundary

The "hylo boundary" (short for hylomorphism) refers to the structural relationship between the two sides of the system:
- **Haskell Expands**: The Haskell code builds up a recursive description of a computation (the "ana" phase). It defines the *what*—the sequence of effects and the logic connecting them.
- **Rust Collapses**: The Rust side interprets or JIT-compiles this description into a concrete execution that performs side effects and produces a final value (the "cata" phase). It defines the *how*—how a `FileRead` effect actually interacts with the OS.

## Effect Machine Model

Tidepool transforms `freer-simple` continuations into a state machine:
- In Haskell, a `freer-simple` program is a tree of `Leaf` (pure value) or `Node` (effect request + continuation).
- The JIT compiler transforms this into an **Effect Machine**.
- When the machine encounters an effect, it suspends execution, yields control to Rust with an effect request, and waits for a response.
- Rust handlers process the request and resume the machine with the result.
- The machine uses a custom **Copying GC** (`tidepool-heap`) to manage memory during execution, with a specialized stack walker for JIT frames.

## Crate Responsibilities

- **`tidepool-repr`**: Defines the Core IR, `Value` types, and handles CBOR serialization/deserialization.
- **`tidepool-eval`**: A tree-walking interpreter for evaluating Core expressions without JIT overhead, used for testing and as a reference implementation.
- **`tidepool-heap`**: Implements the manual memory layout (raw byte buffers) and the copying garbage collector used by the JIT runtime.
- **`tidepool-optimize`**: Contains optimization passes like beta reduction, dead code elimination (DCE), inlining, and case reduction.
- **`tidepool-codegen`**: The Cranelift-based compiler that generates native code and manages the `JitEffectMachine` lifecycle.
- **`tidepool-runtime`**: The high-level orchestration layer that handles Haskell compilation (via `tidepool-extract`), caching, and running programs.
- **`tidepool-effect`**: Core traits and logic for effect dispatch and handling (`EffectHandler`, `DispatchEffect`).
- **`tidepool-macro`**: Procedural macros for inlining Haskell code directly into Rust using `haskell_inline!`.
- **`tidepool-bridge`**: Provides `FromCore` and `ToCore` traits for seamless data conversion between Rust types and Tidepool `Value`s.
- **`tidepool-bridge-derive`**: Procedural macro crate providing `#[derive(FromCore)]` and `#[derive(ToCore)]`.
- **`tidepool-testing`**: Internal utilities and property-based generators for testing the compiler and runtime.

## Data Flow

1.  User provides Haskell code (or it's generated/inlined).
2.  `tidepool-runtime` invokes `tidepool-extract` to get CBOR.
3.  `tidepool-repr` parses CBOR into `CoreExpr`.
4.  `tidepool-optimize` simplifies the `CoreExpr`.
5.  `tidepool-codegen` emits Cranelift IR, compiles to machine code, and allocates a `JitEffectMachine` on the `tidepool-heap`.
6.  `vm.run()` executes the machine, yielding effects to `EffectHandler`s until completion.
