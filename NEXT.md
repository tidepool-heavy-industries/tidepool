# Shoal implementation handoff

## Objective and execution mode

Build a powerful, composable Shoal foundation and a usable orchestration package
in workspace Haskell and Markdown. Astra authors the architecture, plan tree,
shared language, and working recipes. Sol leads execute declared components and
activate Astra specialists at explicitly tagged obligations. The human requests
ordinary Astra RSI engagements to improve the next swarm's working definitions.

Use **one Astra at Medium working sequentially** to implement, review, and check
the system. Do not delegate its implementation or review or build it through a
Shoal development tree. The builder's user-directed goal loop is distinct from
the product actors: native Codex goals remain disabled on all Shoal nodes.

The [vision](plans/next/planned-swarm.md) owns the operating model;
the [workspace design](plans/next/workspace-pilot.md) owns customization and
context packaging; the [Haskell reference](plans/next/sol-worker-routing.md)
contains broader interface sketches. Exact implemented signatures belong in the
[shipped API guide](prompts/shoal/api-guide.md) and checked source.
The [workspace example](examples/shoal-workspace/README.md) is currently a partial
consumer, not the completed orchestration package.

## Architecture and settled boundaries

| Layer | Responsibility |
|---|---|
| Rust runtime | Processes, provider integration, scheduling, authority, custody, persistence, observations |
| General Haskell surface | Worker construction, typed interaction, dependencies, routing, inspection |
| Workspace Haskell and prompts | Context builders, model placement, review/repair, questions, reporting, project behavior |
| Markdown plan tree | Current architecture, decomposition, assignments, dependencies, acceptance, amendments |

A substantial change in collaboration should normally be expressible by editing
workspace Haskell and prompts. Add runtime behavior when a real composition
exposes a missing capability or ownership invariant. Build the Haskell consumer
alongside each primitive extension.

- Keep normal interactive Codex TUIs for every worker and for human steering.
  No replacement viewer, new operator-input protocol, or controller/service
  migration is required. Preserve conversation when hosted coordination fails.
- TOML owns core configuration: module/source lists, prompt references, metadata,
  defaults, and explicit CLI overrides. Haskell composes behavior; Markdown
  supplies authored prose. Do not introduce a second configuration loader.
- One authoritative `.shoal` at the original workspace root supplies the swarm.
  Managed-checkout copies are candidate source. Track authored files in the
  main repository and exclude runtime artifacts; nested Git is optional future
  storage, not a prerequisite.
- Freeze selected configuration, prompt bytes, modules and library identity for
  all collaborating roots and descendants. Activate edits only at an explicit
  swarm boundary. Task data, plan amendments and local Haskell compositions
  remain live over the fixed interfaces.
- Project role names and behavioral prompts belong in Haskell compositions.
  Existing Rust role presets are a tolerated compatibility boundary. Runtime
  capability checks remain authoritative; no new Rust role for each workflow.
- Keep model selection, context ancestry, supervision, result ownership,
  workspace access and pane placement distinct. Use existing owners for each.
- Markdown remains the plan. Ordinary project functions connect declared work;
  no plan compiler, second scheduler, universal workflow engine or duplicate
  task/response registry is required.
- Make spending visible and steerable. Do not kill useful in-flight specialists
  at a token target or introduce a new budget governor.
- RSI is an ordinary human-requested Astra engagement that edits/checks source.
  No RSI lifecycle, approval/adoption subsystem, or mandatory implementation tree.
- Fable, comparative model evaluations and self-hosting acceptance campaigns are
  outside this work. Do not replace or restart unrelated user services.

## Current implementation

The implemented foundation includes:

- Frozen TOML-selected modules and Markdown prompts, source integrity checks,
  stable selection identity and pinned Haskell library identity. `Shoal.Workspace`
  exposes captured resources to Haskell. Authored `.shoal` files are trackable.
- `withInstructions` selects persistent behavior in Haskell independently of
  model, request guidance and runtime authority. The example uses Markdown
  prompts selected by `[prompts.files]`; Rust retains the shared authority facts.
- Independent `withModel` and `withContext` selection through TUI launch.
  Selected contexts have fresh transcripts and isolated local bindings; inherited
  contexts preserve the completed-call boundary. Both load the frozen modules.
- Watch-owned `route`, `pollRoute` and `forgetRoute`, with automatic callbacks,
  selected-worker admission, retained failures and scoped callback child cleanup.
  Failed callbacks emit one exceptional owner notification; successful callbacks
  do not wake the model merely to relay success.
- `snapshot`, `subtree` and `swarmUsage` over existing observations, including
  requested/observed models, received request/coordination-event counts and
  compactions when known. Usage is observed thread usage, not a billing ledger.
- Codex `4372d1a1cf9952178aff25bafdb7e3a6de49b491`: completion acknowledgments run on
  an ordered background queue with 60-second attempts. Exhausted failures disable
  hosted tools while preserving the TUI conversation and other tools.

This does **not** complete the planned Sol operating mode. The remaining work is
both general capability and a complete authored orchestration package:

- Composable project behavior and responsibility-specific context packets, with
  actual launch previews. Existing `previewBranch` reports authority/capacity,
  not the resolved prompt, model, context and definition selection.
- Complete repair and specialist-answer compositions preserving request ownership,
  the waiting obligation, exact evidence and useful retained workers.
- A normal control path for cooperating independent roots and scoped observation.
  Rust forest support exists; a complete model-facing usage has not been established.
- Host-level failure containment. Codex ACK degradation does not cover Shoal's
  current whole-tmux cleanup on host error in `tidepool/src/shoal.rs`.
- A usable plan-tree package and aligned prompts. Existing defaults still teach
  inherited recursive work. The example leaves repair/question handlers undefined;
  review now retains candidate checks/gates and the exact implementer, and local
  repair uses a distinct request without closing the review or replacing the
  original candidate response. Integration accepts a distinct reviewed input;
  the routed implementation/review/integration chain now executes under a Sol
  lead and settles its original request automatically. Specialist consultation,
  amendments and the complete plan package remain outstanding.
- Project observations connecting the plan, actual work, usage and RSI. Complete
  delivery/failure examples must execute, not merely typecheck recipe signatures.

## Acceptance on a separate application

Use `shoal-repl` (the standalone TUI application) or another non-self-hosting
project for the next live runs. The test team changes that application using a
fixed Shoal build; it does not implement or repair the orchestrator running it.

Fresh application actors receive the authored prompts/API guide, project modules,
selected plan branches and relevant target-project source. They must not depend
on the builder's transcript, implementation handoff, or knowledge accumulated
while reading Shoal internals. Ordinary investigation of the target application
is expected. A need to inspect the harness implementation merely to learn how to
use it is a guidance/API finding, not a successful substitute for the package.

Distill discovered usage knowledge into the owning prompt, API or helper. Check
it, then supply the revision through the explicit next-swarm boundary. Do not
make an acceptance run succeed by privately tutoring it with implementation lore.
Specific runtime diagnosis remains ordinary engineering work outside the
application team's product obligation.

Product acceptance requires a useful multi-lane application change: Sol leads
execute an Astra-authored plan, a tagged Astra specialist contributes hard work,
review and repair retain exact evidence, and partial results integrate while
independent work continues. Questions and failed routes reach the right owner
without polling or routine Astra relay. An ordinary requested RSI engagement
then prepares a checked workspace improvement consumed by the next swarm.

## Verification and checkpoint discipline

Use the smallest owning checks in the repository Nix/toolchain environment;
compile changed consumers and execute normal, unavailable, failure and cleanup
paths. Exercise the actual project recipes through deterministic resident fixtures.
Use focused checks during development and relevant broad boundaries at integration.
Run `just fixtures-check` after extractor translation or serialization changes.
Run appropriate formatting and `git diff --check`; inspect the final diff for
duplicated policy, stale callers, magic-string control flow and obsolete comments.

Separate implementation readiness from live product acceptance. A compile-only
recipe does not establish a working delivery chain, and deterministic fixtures
do not prove that fresh Sols can use the supplied guidance. Keep unperformed
acceptance visible. Follow the user's goal scope for implementation and later
application runs; this handoff does not itself start a paid swarm.

Current focused checks: frozen selection and resident recipe checks passed,
including selected Markdown instructions, preserved authority, direct retained
repair and original response identity. Callback replies share the ordinary
settlement path and preserve cancellation. The delivery-lane test integrates an
actual commit in an integration checkout and preserves a partial product gate
without waking the lead to relay results. Non-root committed-ref admission
requires exact active custody; stale/unbound/released principals and root dirty
snapshots are rejected by the owning worktree handler. `just fixtures-check` passed all 217
semantic tests after the instruction protocol extension. The complete package
and host failure containment remain outstanding.

Evidence recorded for `42e27421`: focused resident acceptance, ownership,
configuration, protocol and usage checks passed; production compilation and all
217 fixture semantic tests passed. The exact pinned Codex release build and
host-tools contract passed. Codex's six focused completion tests and scoped
Clippy passed; its full TUI suite had 20 failures out of 4,282 executed tests.
Tidepool strict Clippy retained known pre-existing diagnostics. These are
baseline results, not checks of future edits. No live planned-Sol application
acceptance has run.

The [previous-wave closeout](plans/next/evidence/wave-closeout.md) retains older
source/check evidence. Its service migration, dispatch tree and observer goals
are not current implementation requirements. Git retains earlier detailed logs
and superseded handoffs.
