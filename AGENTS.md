# Tidepool contributor guide

Tidepool compiles typed Haskell effect programs into Cranelift state machines
and services them from Rust. Haskell is the high-level language for authored
programs and agent harnesses; Rust owns runtime mechanics such as processes,
providers, scheduling, resources, persistence, and argument parsing.

## How to work here

- Prefer one clear owner and one implementation for each mechanism. Before
  adding a cache, registry, path resolver, process launcher, durable log, ID
  issuer, parser, or supervision primitive, consult the ownership map below
  and extend the owner.
- Search for the production consumer before adding public surface. A helper
  used only by its new test is usually a sketch, not an abstraction.
- Put invariants in the owning entry point. Do not require every caller to
  remember a pre-check or reproduce policy.
- Treat surprising or needlessly complicated code as a finding. Fix it when
  it is in scope; otherwise report it plainly. Prefer deleting obsolete paths
  to preserving them through adapters.
- Backward compatibility is not a goal when it would preserve a bad internal
  boundary. Serialized formats and externally consumed APIs still require an
  explicit migration decision.
- Use types for control flow. If a string has a closed or partly closed set of
  meanings, represent it with an enum (an `Other(String)` case is fine).
  Rendered error text and labels must not secretly drive behavior.
- Comments explain current contracts, invariants, or non-obvious reasons.
  Delete incident narratives, archaeology, stale morphology, and comments
  that merely narrate the code. Git is the history.
- Keep imports and warnings clean. When a test needs a substantial Haskell
  program, put it in an adjacent fixture file and use `include_str!` instead
  of maintaining an escaped Rust string.

## Architecture and language boundary

- Keep model-facing Haskell familiar, typed, and small. Optimize it as an LLM
  interaction surface, not as a comprehensive mirror of low-level runtime
  failures.
- GHCi-style fenced Haskell is the primary resident-agent interaction surface.
  Do not add JSON/tool-call ceremony where ordinary Haskell syntax suffices.
- Prefer `Member Effect effects` constraints to naming or depending on a
  concrete effect-stack order. Effects are ordinary extensible API, not a
  hidden frozen list.
- Rust interpreters enforce runtime authority. Haskell definitions and effect
  membership express intent; opaque handles, principals, and grants authorize
  concrete resources.
- Do not put CLI parsing, subprocess lifecycle, provider protocol details,
  resource registries, or actor scheduling into Haskell.
- Preserve the IR boundary: GHC types, casts, and ticks are erased before Rust
  CBOR; `CoreExpr` remains a recursive tree; union tags are unboxed indices in
  the effect list.

## Repository navigation

- `haskell/`: extractor worker and the model-facing `Tidepool` library.
- `tidepool-repr`, `tidepool-eval`, `tidepool-heap`, `tidepool-codegen`: IR,
  reference semantics, heap, and JIT/effect machine.
- `tidepool-toolchain`, `tidepool-runtime`: compilation policy and resident
  machine/session substrate.
- `tidepool-protocol`, `tidepool-mcp`, `tidepool-handlers`: effect schemas,
  generated bridge, and concrete interpreters.
- `tidepool-model`, `tidepool-model-output`, `tidepool-agent`: provider-neutral
  conversations, model-output parsing, and coding-agent backends.
- `tidepool-actor`: actor identity, lifecycle, mailbox, and actor sessions.
- `tidepool-repl`, `tidepool-harness`, `tidepool-web`: user-facing runtimes.
- `tidepool-worktree`: managed coding checkouts and repository observation.

Read the nearest nested `AGENTS.md` before editing a subsystem. Use
`docs/GLOSSARY.md` for names, especially model-facing text. `plans/README.md`
lists active designs; plan documents are temporary scaffolding, not standing
architecture. Root and nested `AGENTS.md` files are the contributor guidance;
legacy `CLAUDE.md` files may be stale and are not a prerequisite for starting work.
Verify architectural claims against owning source and production consumers.
Keep detailed design references out of always-loaded instructions.

## Shared-context development

- Scaffold the shared types, semantics, source baseline, and integration owner
  before forking independent obligations. Use resident Haskell `unfold` for
  Shoal work; native tools operate on the assigned checkout.
- Fork around meaningful shared decisions, not a headcount target. Leads can
  recursively delegate implementation and fresh-context review. Prefer Low
  effort for bounded work; escalate explicitly for consequential uncertainty.
- Reuse the exact parent prefix rather than reconstructing it through long task
  briefs. Keep one shared superset API guide and stable tool definitions across
  roles; put changing assignments and authority observations after the shared
  prefix. Start from the guide, not a ritual `:bindings` inventory.
- Fork inheritance is a snapshot, not shared ongoing knowledge. A delivered
  baseline, its acknowledgment, incorporation, and checks are distinct evidence.
  Review concrete commits and failure paths; integrate and verify the resulting
  revision. Retain specialists for repairs without importing all their history.
- Shoal owns continuation: native Codex goals are disabled on every node,
  including root. Do not restore role-dependent goal-tool exposure.
- Judge cache preservation using actual normalized provider requests and usage,
  not rollout metadata alone. Reuse existing opt-in tracing; keep full-context
  captures private and bounded, and report incomplete evidence explicitly.

## Ownership map

| Mechanism | Owning source |
|---|---|
| Shoal CLI, actor launch composition, prompt assembly | `tidepool/src/shoal.rs`, `tidepool/src/actor_host.rs`, `tidepool/src/actor_host/prompt_catalog.rs` |
| Shipped resident instructions and shared API guide | `prompts/shoal/` |
| Actor identity, lifecycle, mailbox, resident actor workbench | `tidepool-actor` |
| Backend protocols, interactive launch and active-update transport | `tidepool-agent/src/backend/codex/` |
| Process mount boundary and durable inbox | `tidepool-node/src/process_boundary.rs`, `tidepool-node/src/inbox.rs` |
| Git invocation and managed checkout registry | `tidepool-worktree/src/git.rs`, `tidepool-worktree/src/registry.rs` |
| Discovery, artifact cache, paths and toolchain fingerprints | `tidepool-toolchain`; extractor process/daemon invocation: `tidepool-extract-cmd` |
| Machine-session checkout, supervision and source sequencing | `tidepool-runtime/src/session/{registry,supervisor,workbench}.rs` |
| Durable JSONL and version migrations | `tidepool-repr/src/{jsonl,version_ladder}.rs` |
| Effect schemas and generated bridge | `tidepool-protocol`; generated consumers in `tidepool-mcp` and `haskell` |

## Verification

- Use the smallest check that proves the changed behavior and compile every
  changed build or test target. Include important failure and cleanup paths.
- In parallel worktree batches, do not run broad batteries. Follow
  `scripts/codex-worktree-guidance.md` and report exactly what ran, what only
  compiled, and what remains unverified.
- The `justfile` enters the Nix environment. Prefer `just quick`,
  `just test-lib <crate> 'test(<name>)'` or
  `just test-target <crate> <target> 'test(<name>)'`, and the owning crate's
  documented focused command. Haskell/extractor-backed tests must use the repository's Nix/toolchain
  setup rather than assuming ambient `cargo` is sufficient.
- Spot checks are appropriate during ordinary work. Run the relevant broad
  boundary check at major integration or release points, not after every edit.
- After extractor translation or serialization changes, run
  `just fixtures-check`; use `just fixtures-update` only when the corpus should
  intentionally change.
- Always run formatting appropriate to changed languages and
  `git diff --check`. Review the final diff for stale callers, duplicated
  policy, unused surface, magic-string control flow, and obsolete comments.
