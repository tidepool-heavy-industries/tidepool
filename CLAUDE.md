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

## Project Structure

```
tidepool/
├── tidepool/              ← Facade crate (re-exports all public API)
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
│   ├── tide/              ← Demo: REPL
│   └── mcp-server/        ← Demo: MCP server with Console/Env handlers
├── haskell/               ← Haskell harness (tidepool-extract) + test suite
├── plans/                 ← Active plans and specs
│   └── README.md          ← Current active plan
├── flake.nix              ← Dev shell (Rust + GHC 9.12)
└── CLAUDE.md              ← YOU ARE HERE
```

### Build

```bash
nix develop              # Enter dev shell
cargo test --workspace   # Run all tests
cargo check --workspace  # Type check
```

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
