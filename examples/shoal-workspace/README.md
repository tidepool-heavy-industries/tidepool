# A project-specific Shoal workbench

This is the extended Tidepool development workspace: orchestration modules,
prompts, recipe checks and usage guides beyond the minimal package created for
a new project. It is not a directory to copy as a starter. Run `shoal new` in
the target repository instead.

The minimal scaffold is deliberately assembled from maintained sources:
`tidepool/src/shoal/scaffold.rs` owns its configuration; the repository-root
`.shoal` owns the starter agent spec, tools and watchdog; and this example owns
the Jev facade and skills. Tests compare the embedded Haskell byte for byte
with those owners. The extended modules, prompts and checks in this directory
are not all installed by `shoal new`.

For current development, iterate on extended prompts, helpers and usage guides
here. Next-run preparation must copy the selected canonical package into the
target workspace automatically before startup; do not hand-edit an installed
copy to curate another variant. The target's original-root `.shoal` is the
runtime materialization selected for that swarm. This source ownership is
separate from its frozen runtime authority. Updating this package does not
authorize a paused run to resume.

`shoal new` writes the skills and accompanying `.agents/skills` links from this
example into a project. The links use Codex's
ordinary repository skill discovery and point into the canonical `.shoal/skills`;
no Shoal-specific loader is involved. This repository already tracks those links
at its root. Skills cover forking,
coordination, review, actor definitions and commands. Their Markdown examples are
executed by the coordination recipes and resident command acceptance checks.
Keep installed skills unchanged during a wave.

This authored .shoal package supplies a programmable working style for the current
project, with a separate relationship-view example for shoal-repl. Begin with the human and
[initial Astra planner](.shoal/prompts/planner.md), then the
[plan tree](.shoal/plans/README.md), [working pattern](.shoal/plans/operating.md)
and relevant [invocation examples](.shoal/plans/run.md).
Astra interviews the human and owns the finished feature across waves. Sol leads
write their own execution plans for planner review, then own substantial recursive
scaffold/fork/integrate waves and independent review. An ordinary human-requested
Astra engagement improves the next wave. No worker is created merely because a
pipeline names a stage.

The [composition model](.shoal/plans/composition.md) explains the what and why:
recursive fork/join inside local integration loops. Shared scaffolds make child
results fit together, context forks share their reasoning, and checked integration
produces source and understanding for the next ready frontier.

The Haskell modules are tools for resident GHCi use. Bind a Task, apply an operation,
keep its real handles, inspect a useful projection and compose the next action.
No import starts work and no Markdown parser schedules a plan. The constituent
unfold/request/watch/route operations remain available; a helper need not own an
entire workflow to be useful.

- Project.Types carries source, scope, rationale, accepted decisions and evidence.
  Reviewed source has one authority; resulting integrated source is a separate fact.
- Project.Work supplies selected workers, independent/reused review, local or
  retained repair, declared design consultations, context and cumulative attention.
  OwnerRepairs avoids queuing work behind the owner's pending delivery. A separate
  retained implementer can receive direct repairs after its original reply.
- Project.Plan declares the concrete components, source constructors and Astra slot.
  Leads implement and integrate substantial work while opening useful parallel
  subtrees through multiple local waves. Each delivery retains its remaining gates.
- Project.Observe connects existing handles to outcomes, unresolved questions,
  identities, lifecycle/provider observations and usage with explicit coverage.
  RSI receives selected evidence, not a routinely summarized raw event stream.

TOML is the configuration entry point; Haskell expresses behavior and Markdown
supplies guidance. Paths are relative to .shoal/config.toml. Prompt resources are
ordinary authored names selected by Project.Work, not Rust workflow roles.
Shoal.Workspace exposes the captured prompts, module names and definition identity.
Every worker has the normal Codex TUI for engineering and direct human steering.

A project can also compile Haskell that lives in another repository, pinned
through its own `flake.nix`. `[haskell.flake_sources]` names, for one flake
input, the directories inside it that hold modules:

```toml
[haskell]
source_roots = ["."]
modules = ["Project.Work"]

[haskell.flake_sources]
jev-dsl = ["core"]
```

with the matching input in the project's `flake.nix`:

```nix
inputs.jev-dsl = { url = "github:inanna-malick/jev-dsl/<rev>"; flake = false; };
```

This package pins jev-dsl that way and supplies `Jev/Operators.hs`, the front
that fixes the library's JSON type to Tidepool's own `Value`. It is the one
Haskell file here that specialises rather than decides: every declaration in it
is a type alias or a name bound to its generic counterpart, so drift from the
pinned revision is a compile error on the line that drifted. `shoal new` writes
the same revision and the same front into a project that has no `flake.nix`;
into one that has its own it writes neither, and prints the input line and the
lock command instead.

Those directories become ordinary source roots. Shoal captures them into the
run's frozen workspace, compiles them through the same pipeline as authored
source, imports the configured modules into every actor, and fingerprints them
in the compile cache by content. There is no package build and no separate
dependency list: `flake.lock` is the pin, and updating it is what moves the
dependency. Authored roots are searched before pinned ones.

While a dependency is being developed, `[haskell.flake_overrides]` substitutes
a working directory for one input:

```toml
[haskell.flake_overrides]
jev-dsl = "../../jev-dsl"
```

Override paths follow the usual rule — relative to `.shoal/config.toml`, or
absolute for a checkout elsewhere. An overridden run leaves `flake.lock`
untouched, so editing the dependency in place stays a working change rather
than a re-pin; each edit gives the next run a different cache key and a fresh
compile. Remove the override to go back to the pin.

Both forms ask `nix` to resolve the project the way any other `nix` command
would, so `flake.nix` must be a tracked file in the project's Git tree; its
uncommitted edits are read as written.

`[launch].systemd_slice` selects the shared systemd user slice (default
`swarm.slice`). Configure its finite RAM and swap limits on the machine before
launching. Shoal places compiler, host, native panes and shared command resources
there; the tmux server stays outside. Placement is checked before payload execution.
`resource-budget.json` in the run directory records the effective aggregate limits.

The target's original repository root has one authoritative runtime .shoal. A swarm
captures it once; later workers use that selection even when their checkout has
newer files. For this package, incorporate changes in the Tidepool authoring source
and let next-run preparation copy them before explicit swarm selection. More general
project-owned customization uses the same source/frozen-runtime distinction with
that project's chosen authoring owner. Preserve unfinished work and runtime artifacts.

The default root prompt selects the planner. Launch that root with Astra; worker
defaults remain Sol. The coordinator prompt selects an ordinary hosted Sol actor
using withInstructions; no new runtime role is needed. The original planner reviews readbacks and consequential amendments.
An operator hold is not a failed continuation or permission to resume.

The run guide shows general first-turn and continuation expressions: commission a
lead, review/repair without a queue cycle and return a decision to pending work.
The optional graph walkthrough adds a concrete allocation and requested RSI.
Typed results distinguish product blockers from execution unavailability. Progress
uses WorkProgress for candidate evidence and current unresolved questions. One
Project.Routing.followWork actor retains progress, terminal receipts and notification
outcomes for each local wave. Its default messages contain actionable deltas;
typed casts connect subtrees without a model relay. Callback failures retain evidence; inspect routes
and effects before replaying a launch that may already have admitted useful work.

Validate a candidate selection without providers or a swarm restart:

```sh
shoal check --workspace /path/to/project
```

This compiles configuration and selected worker modules. Add `--recipes` to run
the `[haskell].checks` entry points against the candidate's own code and fixtures.
The model-free driver uses the same resident admission, workbench, request and
worktree owners as normal execution. Ordinary Haskell in the package chooses the
workflow; Rust knows neither its project roles nor its stage sequence.

The prepared checks cover direct/reused review, delegated repair, expert plan
incorporation, cumulative questions and owning steering, independent progress,
callback success/cancellation/unavailability, typed subtree handoff of later final
heads, retained notification failures, automatic review by an available specialist,
and explicit next-swarm customization.
They use temporary repositories seeded with authored .shoal files and their own
source fixtures. These are executable coordination examples, not application tests
or evidence of live model usability and savings. Run the application's own checks
for its product changes; the subsequent application wave supplies live-use evidence.
