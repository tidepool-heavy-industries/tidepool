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
conversations; selecting an actor's model does not select its planner instructions.
The on-demand [planner prompt](../prompts/planner.md) supplies that behavior.

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

Before broad execution, Sol owners write their interpretation with a normal and
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
- [run.md](run.md): concrete admission, watch, review, retained repair and RSI examples.
- [language.md](language.md): compact shared distinctions; add project vocabulary
  where it helps collaborators make the same consequential choices.
- [graph/README.md](graph/README.md): a worked graph-view feature allocation using
  Project.Plan. It is an example, not the default goal or next-run instruction.

Project.Types, Project.Work and Project.Observe support ordinary project Tasks.
Project.Plan includes the checked graph allocation and a reusable componentLead;
its example constructors do not restrict which project can use the workbench.
Use the actual task/result contracts and callable signatures. Importing a module
or binding a composition starts no worker.

There is one original-root runtime .shoal and one frozen selection per swarm.
Follow the assigned authoring owner for customization; current package development
lives in Tidepool and is copied during next-run preparation. A changed prompt,
completed check or watch notification does not release an explicit operator hold.
Preserve remaining obligations when handing off across a new swarm.
