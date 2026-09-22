# Plan the current goal, then execute it

This package supplies an operating style, typed Haskell tools and worked examples.
The human's current task and the target repository determine the product. No
example component, old run name or imported module is an assignment to build it.
The same model applies to a standalone application or an explicitly chosen
Tidepool dogfood task; obey the current source scope and concurrent owners.

Start from an existing agreed goal/plan when one is supplied. Otherwise, the
initial Astra planner collaborates with the human to establish the intended
outcome, important examples and acceptance. A Sol execution owner can orient the
current project and collect concrete questions, but should not treat a missing
planner decision as permission for broad implementation. Use normal Codex TUI
conversations; selecting an actor's model does not select its instructions.

A root's instructions come from the shipped `base.md` and `root.md`, not from a
copy kept here. This workspace used to override both, and the overrides drifted
until they told a root to commission a coordinator and go idle while the shipped
prompts told it to stay engaged. What this repository needs on top of the shared
prompts lives in this package, which they can be pointed at.

## A plan that can be executed

Put the current goal in a focused Markdown tree, following this shape where useful:

```text
goal/
  README.md              # Human intent, finished walkthrough, overall acceptance
  shared.md              # Shared contracts, language, source/decision basis
  component-a/
    README.md            # Owned outcome, next frontier, later local waves
    hard-question.md     # Declared Astra task and its release condition, if needed
  component-b/
    README.md            # Another substantial Sol-owned outcome
```

Each substantive branch needs its scope, current source, shared prerequisites,
next independently useful children, integration owner and acceptance evidence.
Describe later waves by the behavior they unlock, refining details as results land.
A child can recursively own several such waves. Keep current intent concise;
retain older evidence by reference instead of appending every conversation here.

Before broad execution, the designated initial Sol leads write their interpretation with a normal and
awkward consumer example, concrete interfaces, dependency/fork structure, checks,
assumptions, objections and questions. The original planner reviews coupled choices
and returns explicit corrections and release scope. Incorporate those decisions
before dependent implementation. Later local work within the agreement proceeds
without repeating this initial interview or inventing more approval stages.

## Use the workbench

- [composition.md](composition.md): paired source/context forks, dependency frontiers,
  checked integration and a continuation at every substantial node.
- [operating.md](operating.md): readback progress, source-bearing decisions, parallel
  branch expressions and the actual asynchronous control boundaries.
- [run.md](run.md): concrete admission, watch, review and retained repair recipes.
- [launch.md](launch.md): operator-only launch and package validation.
- [language.md](language.md): compact shared distinctions; add project vocabulary
  where it helps collaborators make the same consequential choices.
- [examples/README.md](examples/README.md): eight worked Jev cells with the
  real fixtures they read. Reference to copy from, not modules to import.

## What is installed

Eleven modules are compiled into every session here. `doc topics` lists them and
`lookup` on a name browses its declarations, which is authoritative over this
paragraph.

`Project.Types` is the shared vocabulary — tasks, candidates, decisions,
questions. `Project.Work` builds the implement/review/repair/incorporate
requests; `Project.Routing` and `Project.Actors` carry the collection patterns;
`Project.Observe` reports on work in flight without consuming it.

`Project.Reflex` classifies compiler, lint and test output from a
precedence-ordered table, with no model turn. `Project.Evidence` types what a
check actually established, keeping what a child reported separate from what was
run here. `Project.Contract` carries a task agreement. `Project.Investigate`
reads a failed build. `Project.Merge` holds an integration worktree and checks a
merged head before publishing, rolling a red one back. The caller supplies the
check command for the change under review.
`Project.Review` runs the loop around them.

There is no `Project.Plan`: it generated assignments for one graph-UI feature and
was retired rather than carried forward as if it were a general tool. Its
campaign tree is in Git history.

Use the actual task/result contracts and callable signatures. Importing a module
or binding a composition starts no worker.

There is one original-root runtime .exomonad and one frozen selection per swarm.
This package is tracked in the repository, so a worktree gets it from git and
there is one maintained copy to edit; it is not copied forward by hand between
runs. A changed prompt, completed check or watch notification does not release an
explicit operator hold. Preserve remaining obligations when handing off across a
new swarm.

## Working in this repository

Shared mechanics — commands, quieting output, notebook cells, Jev packets — are
in the installed guide and the skills, and are not repeated here. What follows is
true of Tidepool specifically.

**Checks are focused, never the gate.** `just test-lib CRATE 'test(name)'` and
`just test-target CRATE SUITE 'test(name)'` run exactly what you name.
`just verify` is the pre-review gate and is budgeted at up to two hours; do not
run it as part of ordinary work, and do not run `just check` or a whole-crate
`just suite` without agreeing it first — other work shares this machine.

**Integration tests live inside suite entry points** under `tests/suites/*.rs`
with `autotests = false`, so `--test <file>` is not a valid target and fails
confusingly. Name the suite.

**`nix-shell` does not work here.** Everything goes through `just`, which enters
the shell itself, or `bash scripts/dev-shell.sh bash -c '…'`.

**A change to the extractor needs a rebuild, not a redeploy.**
`cd haskell && cabal build tidepool-extract-bin` is incremental and the launcher
picks it up. `scripts/redeploy.sh` rebuilds from scratch with the full Haskell
suite and takes the better part of an hour; it refreshes long-running MCP
servers and is not part of testing a change.

**This repository has 2,612 tracked files.** Bound every search — a pathspec on
`git grep`, a pattern on `git ls-files` — rather than walking the tree.

**`checkpointContext` remains proposed work**, not an installed capability.

## Opening a repository session

Start with the human's current task and the selected workspace's plans. Use a
real change to explore a small Haskell program that combines available commands,
evidence and Jev judgments, then keep its runnable example and observed result.
The productivity ambition is 10×; it is not a measured result or a target quota.
Use actual run evidence to choose the next friction point, and preserve failures
with their source and output so the program can improve from what happened.
