# Tidepool contributor guide

Tidepool compiles typed Haskell effect programs into Cranelift state machines
and services them from Rust. Haskell is the high-level language for authored
programs and agent harnesses; Rust owns runtime mechanics such as processes,
providers, scheduling, resources, persistence, and argument parsing.

## Start here

1. Read this file, then `docs/GLOSSARY.md` for names. `README.md` says what the
   system is for.
2. Before editing a subsystem, read the nearest nested `AGENTS.md` or
   `CLAUDE.md`. They state that crate's boundaries and invariants.
3. Build and test through the `justfile`, a thin frontend to declared Buck targets.
   Prepare and retain this revision's selected pinned Nix tools with
   `bash scripts/buck2-configure.sh --tests`, which publishes Buck configuration
   only after realizing and rooting its selected outputs. Include `--browser`
   when building or testing the `exomonad-host` package or assembling a runtime bundle:
   the host test target declares browser inputs even when the selected case
   does not open a browser. A test filter does not narrow build dependencies. See
   `docs/swarm-builds.md` for the source outputs, bind mount and resource
   admission requirements.
   `just test-lib PACKAGE --exact FULL_NAME --expected-count 1` runs one unit
   test through its native counted runner. Compile-only target builds do not
   establish execution evidence. `just verify` is a broad integration gate;
   do not run it as a routine spot check.
4. To run it, follow `exomonad/docs/getting-started.md`: `exomonad new` creates a
   project workspace with a pinned shared-source submodule, while `exomonad init`
   starts a run. In this repository `just exomonad-init DESCRIPTOR REPORT`
   uses the descriptor's frozen native bundle and records the operation. Build,
   freeze and qualify it through `build/package/README.md` before delivery.
   To see the system from the model's side, read `exomonad/prompts/base.md`
   and the skills under `exomonad/examples/workspace/.exomonad/skills/`, and
   drive a session from a
   terminal with `exomonad proxy` (`exomonad/docs/operator-http.md`).

`just exomonad-build` builds the native runtime bundle without starting a run.
Freeze and qualify that exact bundle through `build/package/qualification.py`;
see `build/package/README.md`. Run with an explicit frozen descriptor and report
path. The descriptor selects the bundle that owns the host, libtest, compiler,
stdlib, assets and build source/profile contract. Independently supplied
compiler or libtest paths do not establish matched release evidence.

Most changes touch one of three layers, and it helps to know which:

- **The engine**: prepared STG to Cranelift, the heap and collector, the effect
  machine (`tidepool-repr`, `tidepool-heap`, `tidepool-codegen`). Correctness
  is differential against GHC. Changes here are rare and carefully tested.
- **The resident runtime**: sessions, the workbench, actors, effects and their
  handlers (`tidepool-runtime`, `exomonad-actor`, `tidepool-protocol`,
  `tidepool-handlers`). Most feature work lands here.
- **The model-facing surface**: the Haskell library in `bridge/haskell/lib`, the
  shipped prompts in `exomonad/prompts/`, and the skills. Text here is read by a
  model on every turn: use the glossary, retain concepts that change recognition
  or decisions, and remove duplication. Judge wording through resulting decisions
  and artifacts; prompt size alone is not a quality criterion. A Haskell snippet
  in a prompt or skill must be one that has been compiled.

## How to work here

- Prefer one clear owner and one implementation for each mechanism. Before
  adding a cache, registry, path resolver, process launcher, durable log, ID
  issuer, parser, or supervision primitive, consult the ownership map below
  and extend the owner.
- Search for the production consumer before adding public surface. A helper
  used only by its new test is usually a sketch, not an abstraction.
- Put invariants in the owning entry point. Do not require every caller to
  remember a pre-check or reproduce policy.
- At orchestration boundaries, declare the services a consumer needs through
  named `HasX` traits with associated behavior contracts. A service implementation
  can require its own `HasY + HasZ` context without exposing those dependencies
  to its callers. Wire concrete services at the owning composition root; prefer
  owned, cheaply cloneable service contexts for spawned work. Keep operation
  authority and exclusive permits in separately issued domain handles.
  Follow [capability boundaries](docs/capability-boundaries.md) for contextual
  composition, ownership and incremental migration. A contract can justify a
  trait with one implementation; do not turn every local helper into a service.
- Treat surprising or needlessly complicated code as a finding. Fix it when
  it is in scope; otherwise report it plainly. Prefer deleting obsolete paths
  to preserving them through adapters.
- Backward compatibility is not a goal when it would preserve a bad internal
  boundary. Serialized formats and externally consumed APIs still require an
  explicit migration decision.
- Carry authoritative facts from their issuing owner through consumers and
  recovery. Do not reconstruct exact selections from broader inventories, infer
  ownership from names, or treat missing state as proof of successful cleanup.
  Preserve distinctions such as available versus selected and completed versus
  released in the types and their production constructors. Share immutable
  evidence through exact references rather than embedding complete snapshots
  at every consumer. Preserve typed failure distinctions, including missing
  versus inaccessible storage; a failed observation does not establish absence.
- Keep a value coupled to the metadata required to interpret it, through return
  values, storage and presentation. Audit downstream consumers when admission
  enriches metadata; correct execution with an older rendering table is still
  incorrect. Prefer one complete result over parallel raw and enriched APIs.
  Preserve typed error causes across actor replies; render them at the reporting
  boundary rather than stringify and reclassify them between owners.
- Use types for control flow. If a string has a closed or partly closed set of
  meanings, represent it with an enum (an `Other(String)` case is fine).
  Rendered error text and labels must not secretly drive behavior.
- Comments explain current contracts, invariants, or non-obvious reasons.
  Delete incident narratives, archaeology, stale morphology, and comments
  that merely narrate the code. Git is the history.
- Keep imports and warnings clean. When a test needs a substantial Haskell
  program, put it in an adjacent fixture file and use `include_str!` instead
  of maintaining an escaped Rust string.

## Choose the investigation and evidence

Recover the intended outcome, constraints, available tools, and unresolved
decisions from the task. Preserve user corrections as distinctions the result
must maintain. Define acceptance through something a recipient can inspect,
reproduce, or decide from: an integrated change, a minimal counterexample, a
measured comparison, or a recommendation with discriminating evidence. A broad
investigation can have a small independently verifiable handoff.

Recognize targets by relationships, not just names. Use the relevant lenses;
they are starting points for extrapolation, not a checklist for every change:

| Relationship | Useful methods and evidence |
|---|---|
| Derived state maintained from primary facts | Full recomputation as an independent oracle; stateful histories for invalidation, aliasing, replacement and retirement |
| Several representations of one contract | Differential testing, refinement and metamorphic relations; check common-mode failures when producer and verifier share assumptions |
| Ownership or authority crossing a boundary | Trace issuance, transfer, use and release; test refusal, partial effects, cancellation and stale settlement through the real consumer |
| Many symptoms sharing a dependency | Trace the dependency graph and critical path; compare deleting work, moving responsibility and improving the mechanism |
| A claimed improvement visible only through measurement | Controlled comparisons, sensitivity checks and cost decomposition; separate workload outcomes from failures of the measurement |

Use value of information: choose observations likely to change a consequential
decision relative to their cost. Inspect a consumer or minimize a reproducer
before building a large abstraction. Amortize repeated exploration with generators,
reference models and analyzers when actual reuse justifies them. A known local
defect may need only a direct repair and a focused regression.

On an anomaly, retain its inputs, source and construction history; distinguish
competing explanations with a controlled change. Search for analogous consumers
once the mechanism is established, and name the missing relationship when an
analogy does not apply. A quiet search establishes only what its inputs and
observations could reveal. Separate a product finding, a broken investigative
procedure and unresolved uncertainty. Check the failing history against the
pre-change implementation before calling it a regression; record an unknown
origin explicitly. Verify that relevant events were enabled and retained before
using their absence as evidence that a path did not execute.
Follow [property testing](docs/property-testing.md),
[compiler profiling](docs/compiler-profiling.md), or [workflow audits](docs/rsi-loop.md)
for the evidence needed by that method.

Treat bugs and suspiciously awkward code as signals to strengthen property
coverage around the mechanism and its immediate conceptual neighbors, including
related producers, consumers and implementations of the same invariant. Follow
[property testing](docs/property-testing.md#expand-from-a-finding) for the bounded
search and evidence; a code smell is a lead, not a confirmed defect.

When a finding exposes a reusable failure category, delegate a bounded independent
search while its owner repairs the instance. Include related producers, consumers
and test paths, and revisit earlier one-off repairs for missed analogues. Give the
searcher the violated invariant, known counterexample and current owners so work
does not collide. Seek a shared typed boundary, owner-derived generation or
executable property that prevents or detects the category; another reminder or a
clean text search is not such a guarantee. Record confirmed findings, intentional
exceptions and untested boundaries separately, and compile and test each repair.

Learn from each mistake, including mistakes in tests, tooling and our own
reasoning. Ask both why it happened and why the existing checks missed it;
repair the mechanism and the detection gap. Keep retrospectives blameless:
examine contracts, available evidence, workload assumptions and integration
feedback. Distinguish latent defects exposed by a new workload from regressions
introduced by a change; do not infer causes from worker count or chronology.
Prefer an enforced invariant or
executable check over another reminder. Update the nearest owning guidance
when a reusable decision rule is missing, and replace obsolete advice rather
than accumulating incident checklists. See [learning from mistakes](docs/rsi-loop.md#learning-from-mistakes).

## Planning improvements

For every RSI pass and audit, reconstruct the full workflow before tuning the
mechanism where friction appeared: intended outcome, actual steps, participant
knowledge, ownership, dependencies and continuation. Consider how the flow could
work differently, including removing steps or changing responsibility and
admission order. Compare alternatives against retained wave evidence before
choosing a repair. See [the RSI method](docs/rsi-loop.md#method-reconstruct-the-workflow-before-tuning-its-mechanisms).

After removing a subsystem or specializing a boundary, investigate each axis:
runtime latency and throughput; allocation and retained memory; generated code
size; artifact and scratch-disk use; build and test time; dependency fanout;
API and control-flow simplicity; code volume; documentation and prompt clarity;
correctness and failure isolation; observability; and maintenance cost.
Look for newly impossible states, duplicate owners, needless conversions,
repeated fixed-fixture compilation, and tests of deleted behavior. Preserve
real failure cases. Measure costs before proposing larger caches or dispatch
redesigns, and distinguish structural savings from measured speedups.

For persistent state, compare the size of a logical change with the work it
triggers. Investigate whole-graph encoding, hashing, validation and writes for
small metadata updates. Measure unique facts, references, encoded bytes and
expanded memory separately; compact serialization alone does not establish
cheap decoding or incremental publication.

Give each resource limit an explicit purpose: external format constraint,
operational budget, cache retention policy or runaway safeguard. Record its
workload/resource basis and report observed usage and the limit on refusal.
Do not turn an inherited constant into an unexplained limit on valid work.
Check before expensive allocation where feasible; a check after serialization
does not bound serialization memory. Review limits against current resources
without weakening identity checks or reflexively raising every limit.

## Architecture and language boundary

- Keep model-facing Haskell familiar, typed, and small. Optimize it as an LLM
  interaction surface, not as a comprehensive mirror of low-level runtime
  failures.
- Notebook cells of raw Haskell are the primary resident-agent interaction surface.
  Do not add JSON/tool-call ceremony where ordinary Haskell syntax suffices.
- Prefer `Member Effect effects` constraints to naming or depending on a
  concrete effect-stack order. Effects are ordinary extensible API, not a
  hidden frozen list.
- Rust interpreters enforce runtime authority. Haskell definitions and effect
  membership express intent; opaque handles, principals, and grants authorize
  concrete resources.
- Do not put CLI parsing, subprocess lifecycle, provider protocol details,
  resource registries, or actor scheduling into Haskell.
- Preserve the IR boundary: GHC types, casts, and ticks are erased before the
  prepared-STG wire program; union tags are unboxed indices in the effect list.

## Repository navigation

- `bridge/haskell/`: extractor worker and the model-facing `Tidepool` library.
- `tidepool-repr`, `tidepool-heap`, `tidepool-codegen`: IR, heap, and
  JIT/effect machine. GHC is the independent language oracle for
  differential testing; there is no separate Rust reference interpreter.
- `tidepool-toolchain`, `tidepool-runtime`: compilation policy and resident
  machine/session substrate.
- `tidepool-protocol`, `tidepool-mcp`, `tidepool-handlers`: effect schemas,
  generated bridge, and concrete interpreters.
- `exomonad-model`: provider-neutral conversations. Provider tool-call parsing
  belongs to the pinned Harness runtime.
- `exomonad-actor`: actor identity, lifecycle, mailbox, and resident workbench.
- `tidepool`: the generic public facade, standard library and utility binaries.
- `exomonad-host`: Exomonad host composition, operator interfaces and binaries.
- `exomonad-worktree`: managed coding checkouts and repository observation.

Read the nearest nested `AGENTS.md` before editing a subsystem. Use
`docs/GLOSSARY.md` for names, especially model-facing text. Keep plans focused
on open decisions and actionable work, verified against source and execution
evidence. Remove completed handoffs and checkpoint history; Git retains them.
The root `CLAUDE.md` is a short repository overview;
this file owns contributor rules and the cross-crate ownership map. A nested
`CLAUDE.md` governs its own crate. Verify architectural claims against owning
source and production consumers.
Keep detailed design references out of always-loaded instructions.

## Shared-context development

Maintain one integration `main` per repository and integrate reviewed parcels regularly.
Keep qualification at exact tags and bundle identities; an integration tip is not
release evidence. Preserve active branches and WIP when transferring work.
Before integration or delivery, run `python3 scripts/integration-status.py --strict`
through the admitted project environment. It reads cached tracking refs without
fetching; fetch deliberately before claiming remote freshness. Use repeated
`--candidate REF` arguments to inspect containment and patch uniqueness, and
`--harness-repo PATH --required-harness-ancestor EXACT_OID` to check an explicitly
required Harness change against Tidepool's selected pin. This status command
reports evidence; it does not merge, prune, move branches or block pushes.

- Delegate only when an independent result justifies another agent. A spawned
  Exomonad agent is idle until it receives a typed request. Choose its captured
  or fresh context, actual `AgentSpec`, and workspace explicitly; `SameDir`
  shares the writable files, index, and HEAD. Labels are optional descriptive
  text, not identities or grouping controls.
- Give each assignment its source revision, owned paths, dependencies,
  acceptance evidence, and escalation condition. Keep shared contracts and
  accepted decisions visible in the parent workspace; a child's captured
  context does not receive later changes automatically. Review the concrete
  result and failure paths, then verify the integrated revision.
- Use an authored project workflow when Git delivery benefits from recursive
  implementation, exact-candidate review, repair, and integration. It is an
  optional composition, not a required agent role or hierarchy. General
  exploration and investigation need no project workflow.
- Preserve the evidence trail that changes decisions: distinguish hypotheses,
  findings, accepted decisions, and unresolved questions. Reassign work when
  evidence changes the critical path; local acceptance does not replace
  combined acceptance.
- Close each lane with its checkout/daemon owner, active commands, output and
  runtime dependencies, retained evidence, and next expected build. After a
  review or qualification handoff, the coordinator releases the lane's idle Buck
  daemon when no near-term rebuild is planned; keep active delivery caches warm.
  Artifact reclamation has a separate retention decision. Preserve source,
  Git and WIP, and follow [resource closeout](docs/swarm-builds.md#resource-closeout).
- Exomonad owns continuation: native Codex goals are disabled on every node,
  including root.
- Judge cache preservation using actual normalized provider requests and usage,
  not rollout metadata alone. Reuse existing opt-in tracing; keep full-context
  captures private and bounded, and report incomplete evidence explicitly.

## Ownership map

| Mechanism | Owning source |
|---|---|
| Exomonad CLI, actor launch composition, prompt assembly | `exomonad/host/src/exomonad.rs`, `exomonad/host/src/actor_host.rs`, `exomonad/host/src/actor_host/prompt_catalog.rs` |
| Shipped resident instructions and shared API guide | `exomonad/prompts/` |
| Workspace skills, and the links a client loads them through | `exomonad/examples/workspace/.exomonad/skills/`, `.agents/skills/` |
| Agent spec discovery, reload, and the after-tool hook | `exomonad/actor/src/{agent_spec,after_tool}.rs`, reload publication in `exomonad/actor/src/resident_actor/owned_workbench/reload.rs`; Haskell side in `bridge/haskell/lib/Tidepool/Reload.hs` and `bridge/haskell/lib/Tidepool/Agent/Contract.hs` |
| Comparing two declared tool surfaces | `exomonad/tool/src/surface.rs` |
| Source layers: capture, typecheck, atomic publication, drift | `exomonad/host/src/exomonad/source.rs` |
| Jev operators | the pinned `jev-dsl` flake input, fronted per workspace by `.exomonad/Jev/Operators.hs` |
| Run trace (structured JSONL under `.exomonad/logs/`) | `exomonad/host/src/exomonad.rs` |
| Actor identity, lifecycle, mailbox, resident actor workbench | `exomonad-actor` |
| Native provider transport and actor attachment | `exomonad/host/src/actor_host/{embedded_service,embedded_harness,provider_attachment}.rs`; provider transport and tool-call parsing in pinned Harness |
| Process mount boundary and durable inbox | `exomonad/node/src/process_boundary.rs`, `exomonad/node/src/inbox.rs` |
| Git invocation and managed checkout registry | `exomonad/worktree/src/git.rs`, `exomonad/worktree/src/registry.rs` |
| Discovery, artifact cache, paths and toolchain fingerprints | `tidepool-toolchain`; extractor process/daemon invocation: `tidepool-extract-cmd` |
| Machine-session checkout, parked continuations and source sequencing | `tidepool/runtime/src/session/{registry,resident,workbench}.rs` |
| Durable JSONL append/read mechanism | `tidepool/repr/src/jsonl.rs`; format versions belong to each durable owner |
| Effect schemas and generated bridge | `tidepool-protocol`; generated consumers in `tidepool-mcp` and `haskell` |

## Verification

- Keep maintained property tests at component and component-cluster boundaries,
  with independent oracles, shrinking and observable coverage. See
  [property testing](docs/property-testing.md); end-to-end gates complement these tests.
  Exercise accumulation over publication, branching, reuse and recovery histories.
  For shared snapshots, cover both repeated identical references and distinct
  snapshots with overlapping contents. Check structural growth and restored
  sharing alongside semantic round trips through the production writer/reader.
  Compare retained snapshots with independently saved model contents, not with
  values just read from that same snapshot. Include interrupted construction and
  unpublished reservations; allocation ceilings are not published inventories.
- Construct behavioral fixtures through the owning production APIs. Build the
  ordered history through ordinary request, update and admission calls; issue
  claims and captures at the same points as production. Use the real issuer for
  authority and provenance. Do not bypass those APIs with SQL,
  private-field writes or forged records, then repair positions or metadata to
  make the scenario pass. Reuse small helpers that compose the real APIs, and
  remove obsolete fixture paths instead of adding compatibility machinery.
  Trace callers when retiring a fixture and remove otherwise-unused production
  escape hatches that existed only to manufacture its unsupported state.
  Direct storage manipulation belongs in storage, migration or deliberate
  corruption tests whose purpose requires it; identify that boundary and keep
  a valid control constructed through the appropriate owner.
- Assert an independently observable contract. Check the exact scripted message
  through its parsed structure, attachment access after owner retirement, or an
  execution witness from the test body. Do not substitute a substring search
  over serialized JSON, incidental reference counts or generated names,
  arithmetic tautologies, or comparison against the same constant or algorithm
  that produced the result. Exact representations belong in tests of that
  representation's contract. For a negative case, establish the valid path
  first and assert the intended typed refusal plus unchanged state where
  atomicity is promised; an unrelated setup error must not count as success.
- For a changed ownership or evidence boundary, review the downstream journey,
  not only the newly constructed value. Exercise issuance, handoff and use by
  the next real consumer; include another handoff when the result is reusable.
  Ask which distinction the change preserves and which executable case fails
  if it is collapsed. Component checks complement, rather than replace, this
  representative joined history.
- Compile every changed or directly affected target and run the smallest tests
  that prove its behavior, including meaningful refusal and cleanup controls.
  Each worker runs these checks through the admitted repository commands;
  ordinary builds do not require a build owner's approval. Compile affected
  test targets early, including `cfg(test)` and integration callers of changed
  APIs, before describing a parcel as integration-ready. Formatting, metadata
  checks and source review do not replace compilation. Compile joined changes
  incrementally rather than deferring the first compile to release assembly.
  Choose the joined compile set from changed owning crates and direct consumers,
  including each owner's test target; a familiar top-level package list can
  omit the changed crate's `cfg(test)` code even when its library builds.
  An owner library build does not compile downstream test callers. Name any
  uncompiled consumer explicitly in the handoff instead of calling the change
  integration-ready.
- Keep the ordinary feedback path cheap: use the selected fast-development
  profile, with unoptimized first-party tests and optimized dependencies or
  explicitly justified hot libraries. Optimization belongs to profile and
  target policy, not unconditional compiler-toolchain flags. Preserve the
  production profile for release and performance qualification. Check effective
  compiler arguments across repository/cell boundaries; a profile name alone
  does not prove which policy ran. A timed rustc action includes optimization
  and code generation as well as linking; do not call it linker time without
  separate evidence.
- Give each retained test a distinct behavioral obligation. Consolidate repeated
  setup and assertions, retire obsolete cases and their otherwise-unused escape
  hatches, and move pure checks to their owning component. Keep independent
  oracles and real boundary/recovery checks. Test counts are not a quality goal.
  Declare test-only sources and resources only on consumers that need them, so
  editing a fixture does not rebuild production libraries or browser assets.
  Reuse immutable build products, never shared mutable test state.
- Run a small representative history through the joined production path early,
  using its declared compiler and runtime resources. For a changed boundary,
  follow the same issued facts through publication, reuse, recovery and final
  observation where applicable. Test through the public consumer, not only a
  new private helper. Integrate and replay that history before accumulating
  further dependent changes. Component passes and a
  shrinking defect list do not establish release readiness. Final acceptance
  belongs to the exact integrated bundle. Separate implementation, qualification
  and deployment in estimates, and state the unresolved uncertainty rather than
  predicting completion from the number of known bugs.
  For changes spanning the pinned Harness browser and native host, build
  `//web:dist` through `just build` before full host assembly. This exercises
  the matched schema, dependency and sealing graph; it is not a substitute for
  native asset admission or browser execution.
- Use actual process peaks, enclosing cgroup limits and host headroom to admit
  independent builds and tests in parallel. Coordinate concrete conflicts over
  checkout/configuration/output ownership or insufficient resources, not every
  command. CPU count alone is not a safe heavy-test process count. Follow
  `docs/swarm-builds.md`; do not retain a blanket single-build policy from a
  smaller machine. When a check must be deferred, record the observed blocker,
  exact pending command and unqualified status, preserving source and evidence.
  Use the smallest affected targets for feedback; reserve full bundle assembly
  and freezing for release qualification.
- `just test-lib PACKAGE --exact FULL_NAME --expected-count N` selects unit
  cases; `just test-target PACKAGE TARGET ...` selects an integration target.
  `just test-bin PACKAGE BINARY ...` selects binary tests. `just test-list`
  lists the owning libtest executable; listing and linking are not test passes.
- `just test-native LABEL --pattern PATTERN` forwards arguments to a named
  Haskell Tasty target. Native runners reject empty selection and preserve
  declared compiler, source and runtime resources. Rust cases run in separate
  bounded processes; Tasty selection runs only selected leaves, serially.
- `build/native-targets.json` is generated from Cargo targets and the reviewed
  module ownership walk. Cargo metadata owns package/target discovery;
  `scripts/buck2-first-party.py` enforces integration registration. The source
  walk establishes ownership, not a proof that a harness contains zero tests.
  Actual libtest discovery owns execution counts. Empty harnesses remain
  compile-only and cannot be accepted as executed suites.
- `just suite PACKAGE` runs its registered integration targets. `just check`
  compiles native consumers without executing tests. `just quick` selects the
  explicit engine unit packages; fixed fixture build actions may need GHC on
  a cold build. `just lint` uses declared formatting/Clippy inputs.
- After compiler translation or serialization changes, run
  `just fixtures-check`. Optional cohort names select existing native corpus
  targets. Prepared fixture/corpus producers emit immutable runtime resources
  from declared compiler, source, ordered-root, target and package inputs.
  Corpus validation checks nonzero programs and typed outcomes against the
  independent oracle. Generated prepared artifacts are not checked in;
  change their owning sources/targets and let Buck rebuild their producers.
- Use `tidepool-test-data` for representation-only fixtures, `tidepool-testing`
  for evaluation, and `tidepool-prepared-corpus` for corpus execution. Test-only
  dependency edges do not become production features or certificate issuers.
  No local development feature union or `test-support` authority factory is
  supported. Reuse immutable compiled fixtures with fresh mutable machines.
- Keep GHC API and package dependencies in their owning native actions. The
  production worker retains its production package closure; Tasty dependencies
  belong to the separate host test toolchain. Tests must fail on missing or
  broken required compiler inputs, rather than return a passing skip.
  `bridge/haskell/tidepool-extract.cabal` declares the Tasty suites. The pinned
  Cabal metadata producer and `scripts/buck2-haskell-components.py` own their
  native component projection; discovery owns case counts and execution owns
  passing evidence.
- Retain the source OID, actual artifact/hash, command, selected/executed counts,
  exit status and log/report. Report compilation, execution and unqualified
  work separately. Frozen release acceptance must use one bundle-owned source,
  profile, compiler and asset contract through the central qualification owner.
- Run language-appropriate formatting and `git diff --check`. Review the final
  diff for stale callers, duplicated policy, unused surface, rendered-string
  control flow and obsolete comments. Keep the open migration obligations in
  `plans/typed-build-and-test-delivery.md`; qualification requires actual runs.
