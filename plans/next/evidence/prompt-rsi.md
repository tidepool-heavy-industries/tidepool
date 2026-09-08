# Prompt RSI from the paused live workbench

This pass prepares the canonical operating package in
`examples/shoal-workspace/.shoal`. It changes Markdown guidance and the TOML-selected
planner resource. Rust, Haskell implementation, runtime protocols and existing
coordination recipes are unchanged. The live application remains clean at
`f6d90983e2ad34b7fed3e0bd0a3884b8c9a7299c` and on the explicit operator hold.

## Working model

The prompts teach **recursive fork/join inside local integration loops**. Going
down the responsibility tree divides ownership; going around a loop advances the
same obligation. Each coding fork pairs exact source with useful reasoning.
Integration joins checked artifacts and consequential knowledge, retaining detailed
debugging with its owners. A broad ready frontier expresses real independence;
dependency edges determine which results must join. Every substantial node can
own several waves, and partial source acceptance preserves the encompassing goal.

The [composition guide](../../../examples/shoal-workspace/.shoal/plans/composition.md)
uses Git, graph relationships and control flow, with a concrete worked example.
It distinguishes responsibility, dependencies, Git ancestry, context ancestry and
runtime supervision. It does not require a recursion-scheme taxonomy or invent
new join/fold commands. The [operating guide](../../../examples/shoal-workspace/.shoal/plans/operating.md)
connects that model to the existing asynchronous Haskell boundaries.

## Findings and changes

| Observation or human steering | Change |
|---|---|
| Execution initially treated a supplied plan as ready for fan-out; the human wanted collaborative initial planning. | On-demand planner prompt: interview for concrete normal/awkward behavior, establish acceptance, declare model placements and connected later waves. |
| Sol readbacks exposed useful misunderstandings and objections. | Owner/lead prompts retain the open Delivery through a concrete own-words readback, with original-planner recipient, corrections and explicit release scope. |
| Partial contract work was liable to be confused with feature completion. | Each substantial owner keeps a continuing obligation across recursive local waves and checked partial integrations. |
| Shared UI wiring reserved to root could delay actual consumer acceptance. | The seam owner owns timely delivery; ready frontiers gate only their actual dependencies. |
| Transcript/history orientation and routine signature inventories consumed attention. | Start from the selected task and existing invocations; preserve the useful common fork point and return compact decisions/evidence instead of descendant transcripts. |
| A missing client mirror field was interpreted as missing host capability. | Generic prompts require checking consequential premises at their owning boundary. Concrete creator/identity distinctions remain in referenced examples and review evidence. |
| A snapshot recipe bound an action instead of executing it; generator review also found a name-capture risk. | Reviews distinguish text, compilation and execution. Detailed Haskell examples live in the operating guide rather than being repeated in every generic role prompt. |
| Prose in taskSource failed admission; withDecision could replace a newer source with older decisionSource. | Explicit exact-Git-source semantics and checked integration source for accepted decisions, with current assignments retained through review/repair. |
| Active update delivery failed, while operator holds worked. | Inspect delivery receipts, preserve pending obligations, and distinguish failed transport from intentional suspension. No retry loop or implicit release. |
| The next run may target an unrelated project or Tidepool itself. | Default plan index is project-neutral; the graph feature moves to an optional worked example. A generic Task/lead/watch invocation precedes example allocation. |
| The user requires one authoring location in Tidepool for now. | Curate only the canonical package here. Next-run preparation must copy the selected definitions to the target automatically; do not hand-maintain installed variants. |

The concrete four-lane product corrections remain in
[live-workbench-review.md](live-workbench-review.md). They are pending planner
steering, not active implementation instructions or accepted product decisions.
The existing-worker versus create-worker messaging preference remains unanswered.

## Responsibility walkthrough

| Starting responsibility | Useful next action and preserved boundary |
|---|---|
| Initial Astra planner | Resolve the next consequential human choice, plan the near frontier and later outcomes, review Sol's actual understanding, then hand off routine execution. |
| Sol owner | Use the agreed project goal, commission substantive leads, collect readbacks and coupled questions, establish early shared integration, then accept coherent deliveries. Root has no invented sessionInput/respond. |
| Sol lead | Keep Delivery open at the readback checkpoint; after release pair source/context at useful fork points, run local integration loops and reuse the reviewer or retained implementer for repairs. |
| Sol implementer | Start directly on the actual Task; recurse for substantive independent work, keep small leaves simple, and return the exact checked candidate with remaining gates. |
| Declared Astra expert | Verify the premise, resolve the bounded uncertainty, explain affected consumers and evidence, and leave incorporation/product scope with the owning decision. |
| Reviewer | Use current ReviewTask and candidate after amendments, exercise owning behavior, and select local versus retained repair without queuing behind the waiting owner. |
| Requested RSI | Inspect selected evidence, improve canonical source within scope, and deliver a checked candidate for a later explicit selection. Do not curate installed copies or turn improvement into a management tree. |

These are ordinary existing actors and typed requests with selected instructions,
not new Rust roles. The planner resource is selected with withInstructions or used
in the human's planning conversation. Default config remains the Sol execution
owner; merely loading the package does not imply the initial planning took place.

## Prompt footprint

Whitespace word counts against source before this pass, not tokenizer or billing
measurements:

| Prompt | Before | After |
|---|---:|---:|
| Shared core | 409 | 430 |
| Sol owner | 353 | 432 |
| Sol lead | 465 | 469 |
| Implementer | 230 | 226 |
| Specialist | 217 | 234 |
| Review | 363 | 374 |
| Repair | 110 | 99 |
| Incorporation | 131 | 117 |
| RSI | 283 | 356 |
| Initial planner, on demand | — | 508 |

The existing nine prompts increase by 176 words in total. Core plus implementer
increases by 17 words; core plus lead by 25. The new planner and longer composition/
operating guides are on demand, not added to every role's shared prefix. This
trades a little instruction space for less repeated orientation, relay and repair;
no usage savings or improved live behavior are claimed before observation.

## Validation

The final configured package compiled and passed all **43 existing resident recipe
assertions**: 10 workbench, 20 collaboration and 13 routing checks. These exercise
review/reuse, repair ownership, cumulative questions, current source decisions,
failed/cancelled routing and next-swarm frozen prompt adoption. Total check time
was 207.64 seconds, using the fixed packaged Shoal and its matching compiler.

Command:

```sh
/nix/store/53g4rdbi08b87v9149p2yn9pbp399720-shoal/bin/shoal check --workspace /home/inanna/dev/tidepool/examples/shoal-workspace --recipes
```

Checked definition identity:
`744dde8553684c52d41a06431a435a659ff8b97bba935e38b27efaa7d4b40ef1`.
All inherited TIDEPOOL_* overrides were removed; only the check-owned matching
compiler socket was supplied. That compiler was stopped afterward. No native
workers or providers were launched. The existing unforced Typeable diagnostics
and intentional cancellation-path diagnostic remain visible in the logs.

The new generic Task/lead/watch invocation, paired-branch example, readback value
construction and planner prompt selection were also compiled in an isolated
scratch module, using the Markdown snippets rather than separately rewritten
calls. This is compile evidence; it does not execute model decisions or prove
that fresh workers will follow the instructions. The original scripted resident
recipes provide the actual execution evidence for existing coordination mechanics.

All ten TOML prompt references resolve. Package/updated-document links, Markdown
fences and whitespace passed checks, along with `git diff --check`. Haskell
implementation and recipe source files have no changes. Private local command
logs and results are retained under
`/tmp/nix-shell.2gCQwE/shoal-prompt-rsi-final-x8qg4ma6`.

## Activation and remaining work

The canonical package is the authoring source. Its target copy is a runtime
materialization selected at startup, not another place to iterate. This prompt-only
pass does not implement a new installer or change startup mechanics. The next run's
preparation must copy the canonical selection and its task-specific plan, check
that selection, and use the explicitly chosen executable. No installed edits
remain from this pass. No provider launch, live tool submission or restart was
performed.

A deliberate Tidepool dogfood task is permitted when the human selects it; the
paused shoal-repl run's source restrictions and task list do not become generic
worker policy. Its current hold is still independent of this package's readiness.

Future checkpointContext, stronger source types in Haskell and startup/compiler
performance improvements remain separate implementation work. The messaging fix
has a separate owner and source commit; checking prompts with the previous fixed
binary does not accept a newly built messaging substrate. Full child usage/trace
coverage was not established in the paused run. Preserve that limit for later
visualization and cost analysis instead of inferring it from requested models.

## Readiness for the proposed Tidepool dogfood run

The human's next intended targets are the already-authored STG engine plan
(`plans/haskell-engine-stg.md`) and interactive application/process plan
(`plans/interactive-applications/README.md`).
The portable prompt package is ready for commissioning. Actual launch still needs:

- A fixed matched runner containing Tidepool `5194426d` and pinned Codex
  `d760c5cb8c`, with a focused owning-TUI steering check. The current
  `target/shoal-standalone-ready` still selects the old `53g4rdb...` wrapper bundling
  Codex `4372d1a`; successful prompt checks on it do not validate the new repair.
- Automatic canonical-package preparation for this run. The current root
  `.shoal/config.toml` contains only Astra/high defaults; `scripts/shoal-init.sh`
  builds and launches the local binary but does not copy this operating package.
  Do not treat that command as already selecting the planned Sol configuration.
- Committed exact plan/source baselines. At this review the two design plans and
  engine regression additions remain other owners' uncommitted work. Do not omit
  them from fresh worktrees or indiscriminately stage them with prompt curation.

The separately authored allocation landed during this review as `f5f4999d`, under
[plans/parallel-dogfood](../../parallel-dogfood/README.md). It already supplies the
two Sol leads, declared Astra questions, local frontiers, initial readback/release
checkpoint, explicit Codex checkout owner and shared integration seam map. It also
explicitly supersedes the process plan's old sequential/non-Shoal restriction.
Use that allocation; no further orchestration design is needed before commissioning.
Its detailed referenced designs still need to be present in the committed launch
seed. Launch authorization is separate from this readiness assessment.
