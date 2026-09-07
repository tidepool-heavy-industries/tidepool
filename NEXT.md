# Shoal implementation handoff

## Objective and execution mode

Use **one Astra at Medium, working sequentially in the repository**, to build a
small, expressive Haskell coordination surface with project-specific
customization. Do not delegate implementation or review or use a Shoal tree to
build the system. The same agent reviews its changes and runs focused checks.
Keep one concise checkpoint here as implementation proceeds.

The product operating mode is: Astra authors a tree of Markdown plans and shared
project language; Sol leads execute the declared tasks through Haskell recipes,
with Astra specialists at explicitly tagged obligations. The human starts an
ordinary Astra session for RSI when wanted. Markdown remains the plan; no plan
compiler or mandatory executable workstream description is required.

Read the [vision](plans/next/planned-swarm.md), then the
[workspace/context design](plans/next/workspace-pilot.md). Consult the
[Haskell reference](plans/next/sol-worker-routing.md) only for the current
implementation slice. These documents retain the broader design and illustrative interfaces. The
[shipped API guide](prompts/shoal/api-guide.md) and
[compiled workspace example](examples/shoal-workspace/README.md) describe the
implemented surface; use their exact signatures.

## Settled boundaries

- **Keep the existing interactive Codex TUI execution path.** The human opens a
  worker's pane and talks to it normally. No replacement viewer, new operator UI,
  operator-input protocol, or service/controller migration belongs to this build.
- **TOML owns core configuration.** Extend `.shoal/config.toml` for module/source
  lists, prompt references, and necessary metadata. Preserve existing defaults
  and explicit CLI overrides. Haskell owns typed worker specifications, context
  builders, project types, and coordination recipes; Markdown owns prompt prose.
- **Edit ordinary repository files.** Track authored `.shoal` content and exclude
  only private runtime artifacts. Do not introduce a second configuration loader.
- **One authoritative `.shoal` per swarm, at the original workspace root.**
  Managed-checkout copies are candidate source, never independent configuration
  authorities. Keep authored files in the main repository for now; a nested Git
  repository is optional future storage, not a requirement. Freeze the selected TOML, prompt, and module
  inputs at startup, using existing run/source owners. Later roots, children, and
  fresh task contexts use that selection. Changes activate at an explicit swarm
  teardown/restart boundary, never by live reload.
- **Keep working state fluent.** New task data, evidence, local bindings, and
  Haskell compositions remain usable over the fixed shared interfaces.
- **Build strong primitives and project recipes.** Rust owns runtime mechanics;
  model-facing Haskell exposes useful operations, outcomes, and inspection.
  Retain existing actor/request/watch/value ownership instead of adding registries.
- **Worker roles compose the same mechanisms.** Implementers, leads, reviewers,
  specialists, and RSI sessions use normal Codex TUIs and the same underlying
  actor/request primitives. Their model, context, typed task/result, and actual
  permissions can differ; a role name does not require a new UI or runtime path.
- **Observation and steering compose.** Provide topology, activity, received
  message/event, compaction, and usage observations. Steer through ordinary Codex
  conversations and existing Haskell operations. No new budget governor,
  specialist-admission policy subsystem, or dedicated checkpoint protocol.
- **RSI is an ordinary Astra engagement.** Supply useful context and projections;
  edit/check source for the next swarm. No RSI-specific actor lifecycle, request
  type, approval pipeline, or adoption swarm.
- Preserve native-goals-disabled policy and useful in-flight specialists.
  Fable and a comparative model evaluation campaign are outside scope.

## Implementation sequence

### 1. Preserve the TUI and verify failure containment

Tidepool now pins Codex `4372d1a1cf9952178aff25bafdb7e3a6de49b491`, published
on the fork branch `shoal-async-completion`. Completion acknowledgments use
60-second attempts on an ordered background queue; exhausted failures disable
hosted tools while retaining the TUI conversation and other tools. No running
service or installed binary has been replaced as part of implementation.

Verify the selected build and actual failure path. Check whether awaited completion
handling blocks normal TUI input/event processing; if it does, repair that
asynchronous boundary in its existing owner. Preserve ordered completion and
uncertain-effect evidence. Retry idempotent acknowledgments, not original effects.
Do not turn slow or unavailable hosted coordination into fatal TUI exit.

Native controller and read-only observer code exists in the fork. Its existence
does not make migration a requirement. Fix concrete defects in the current path.
Do not launch, stop, or replace existing user services incidentally.

### 2. Establish TOML configuration, frozen modules, and prompts

Extend the current Shoal configuration and prompt composition owners. TOML selects
source roots/import modules and prompt files. Resolve paths relative to the
workspace configuration, not an actor's incidental checkout directory.
Use the existing compiler/workbench to load configured Haskell; loading definitions
alone must not spawn agents.

Materialize selected editable sources and prompt bytes under existing run storage.
Retain their identity and pinned library/build identity. Compile later actors from
that same selection, including configured project-module dependencies, rather than
rereading mutable sources. Keep candidate source checks separate from active state.
No generalized configuration-version service or hot-reload machinery is needed.

Allow replacement of authored core prompt prose and role guidance while retaining
truthful tool/API and authority information. Keep the common prompt/tool prefix
stable within the swarm; append relevant role/task context afterward. Expose the
resolved configuration and context through focused inspection.

Narrow the existing blanket `/.shoal/` local exclusion to runtime artifacts;
preserve existing files and unrelated Git exclusions. Update owning guidance
and prompt checks when the per-workspace behavior becomes real.

### 3. Make model and context selection real

Carry explicit model/effort and selected-versus-inherited context through worker
specifications, actor admission, and the current Codex TUI launch boundary.
An Astra parent must be able to spawn Sol without accidentally inheriting Astra.
Exact-context forks preserve the actual completed-call boundary; selected contexts
receive their relevant definitions and typed input without the parent's transcript.

Context builders supply task, current source/owners, rationale, acceptance,
relevant recipes, and result/question recipients. Their advertised definitions
must exist in the receiving Haskell environment. Model, context ancestry,
supervision, reply authority, and pane placement remain separate.
Use existing support for multiple top-level actors; no new global manager.

### 4. Complete one typed coordination path

Implement owned asynchronous result routing over existing `Forked`, `Response`,
`Await`, and `Settlement` mechanisms. A route installs one continuation, returns
promptly, and executes known forwarding without a model relay. Retain callback
failures and unavailable outcomes at the actual owner. Never synchronously wait
for children inside the tool block that admits them.

Build project Haskell recipes for implementation, independent review/direct repair,
checked integration, and tagged architectural questions. Bind repetitive launch,
context, and recipient choices once. Preserve exact candidate/review/integration
evidence and partial acceptance. Return answers to the waiting obligation without
queueing behind it or manufacturing authority from a captured handle.

Use ordinary functions and extensible effect constraints. Do not require a new
domain effect family or universal workflow state machine. The first useful
consumer determines the small surface; compile its normal and failure examples.

### 5. Add observation, teaching, and ordinary RSI usage

Project existing actor/provider observations into typed Haskell snapshots.
Include creation, supervision and context relationships, actual model, obligation,
received messages/events, compactions, and own/subtree usage. Add missing counters
at their owners. Distinguish received from presented, deduplicate provider usage,
and retain incomplete coverage instead of showing unknown values as zero.
Snapshots read state; they do not wake agents to compose reports.

Ship a compact common guide plus focused project recipes. Keep signatures,
explanations, and examples aligned with source; offer deeper inspection on demand.
Ordinary Haskell functions can assemble a useful RSI input from plan, definitions,
outcomes, and selected observations. Demonstrate source editing/checking and use
of the revision after the next explicit swarm boundary, without a dedicated RSI
protocol or new control subsystem.

## Verification and completion

Use repository Nix/toolchain commands and the smallest owning checks. Compile
changed consumers, inspect failure/cleanup paths, and review the final diff in
the same implementing session. Broaden checks at final integration; run
`just fixtures-check` if extractor translation or serialization changes.

Decisive coverage:

- Existing TUI input, steering, and normal launch remain usable. Delayed/failed
  completion acknowledgments preserve responsiveness and conversation; disabled
  hosted calls fail visibly while other tools remain usable.
- Existing TOML defaults work; configured modules/prompts load; invalid input fails
  before launch. Disk edits affect neither active actors nor late spawns; a new
  swarm uses the revised inputs.
- Sol model selection survives Astra ancestry. Selected context and exact forks
  preserve their distinct contracts and actual authority.
- A candidate reaches review, repair, and checked integration. Questions resume
  the right obligation; independent deliveries do not wait for unrelated work.
  Lost workers, route failures, and cancellation retain honest outcomes.
- Observation counts preserve provenance and avoid inherited-history double
  counting. Reading snapshots or previewing candidate context causes no model call.
- A checked project prompt/helper change is usable in the next swarm.

Prefer deterministic owning fixtures during development. Any paid live-agent
acceptance is a separately scoped exercise, not a recursive implementation run.
Report what ran, what only compiled, and what remains unverified. Useful partial
commits are acceptable; they do not close outstanding acceptance requirements.

## Current checkpoint and historical evidence

The implementation and focused acceptance are complete in Tidepool.
The Codex fix is committed and published; Tidepool selects that exact revision.
Existing user services have not been restarted.

- Ordered, bounded Codex completion settlement runs off the TUI loop. Six focused
  tests and scoped Clippy pass under Rust 1.95. The full TUI suite ran 4,282 tests:
  4,262 passed and 20 failed outside the focused completion tests. This is not a
  clean full-suite result. The exact pinned Nix release build and host-tool contract also pass.
- TOML-selected module roots/imports and core/legacy-role prompts are frozen in
  run storage, with source integrity and library identity checked on reuse.
  Authored `.shoal` files are trackable. Configuration/prompt tests and actual
  configured-module compilation pass, including reuse after mutable source edits.
- `withModel` and `withContext` (`selected renderer` or `inherited`) reach normal
  TUI launch independently. Resident acceptance proves Sol selection, common
  supervision, inherited binding visibility and selected isolation. Both modes
  preserve the completion startup gate.
- Watch-owned `route`, `pollRoute`, and `forgetRoute` retain callbacks/outcomes on
  the existing actor queue. Acceptance covers typed forwarding, selected reviewer
  admission, callback failure, unavailable replies and explicit route cleanup.
  Callback child cleanup is restricted to that callback's admitted groups.
- `snapshot`, `subtree`, and `swarmUsage` project the existing roster and provider
  observations. Received request and coordination-event counters have explicit
  owner-local meanings; compaction parsing excludes inherited history. Missing
  coverage stays unknown. Usage is observed thread usage, not a billing ledger;
  inspect the roster's stale/provenance fields when judging freshness.
- The project example's types and implementation/review/integration recipes
  compile in a real frozen resident workbench. The guide's signatures compile.
  No paid live model has executed the example's full repository delivery chain.
- Embedded Haskell uses captured source bytes rather than stale absolute paths
  from a shared build directory; build provenance follows the current checkout.
- Named Rust role-to-prompt mappings remain a tolerated transitional boundary.
  Project roles and recipes belong in Haskell; no new role runtime was introduced.

Final verification: 9 resident acceptance tests, 12 lineage/request-counter tests,
19 rollout-usage tests and 6 protocol contracts pass. Earlier focused config,
prompt and ownership checks also passed. Production `cargo check -p tidepool
--bins` passes. Fixture regeneration changes only the source fingerprint; all
217 fixture semantic tests pass. Formatting and `git diff --check` pass.

Strict Clippy is not clean: dependency-inclusive checking stops at an existing
`tidepool-node` conversion warning; `--no-deps` reports five existing
unwrap/expect/boolean diagnostics in host retirement/launch code. Those exact
statements predate this implementation. No live paid swarm acceptance has run.
The exact pinned Nix Codex release build and host-tool contract pass
(`nix build .#checks.x86_64-linux.codex-host-tools-contract --no-link`).
The final two workspace packaging checks also pass. Runtime build directories
remain ignored; authored example modules are included in the repository.

The [previous-wave closeout](plans/next/evidence/wave-closeout.md) records merged
source, checks, and resource history. Its service migration and observer
acceptance goals are historical, not current gates. Previous dispatch instructions
remain in Git history; do not redispatch that tree or assume its handles exist.
