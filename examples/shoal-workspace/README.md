# A project-specific Shoal workspace

The authored `.shoal` directory contains a complete planned-Sol package and a
concrete `shoal-repl` application plan: selectable creation, supervision and
context relationship views. Start with [.shoal/plans/README.md](.shoal/plans/README.md)
and [the run/RSI guide](.shoal/plans/run.md). Copy it into that app or adapt the
terminology, plan paths, acceptance and helpers for another project. TOML is the configuration entry point. Paths
are relative to `.shoal/config.toml`. The normal Codex TUI remains the interface
for every worker. A swarm captures selected inputs once; restart explicitly to
activate edits. No import launches work.

Worker instructions live in `prompts/*.md`, selected by `[prompts.files]` in
TOML. The captured `Shoal.Workspace` module exposes `workspacePrompt`, returning
`Maybe Text`, plus the selection identity and configured module names.
`Project.Work` chooses instructions and combines them with typed task evidence;
missing required project prompts fail explicitly. Editing a prompt affects the
next swarm, including when an existing swarm launches a worker later.

The original workspace root has the swarm’s one authoritative `.shoal`. Every
actor uses its frozen selection, even when a managed checkout contains a copy.
Treat edits in those checkouts as candidates to integrate into the authoritative
directory for the next swarm. Keep these files in the main repository for now;
a nested Git repository is not required.

Astra writes a Markdown plan tree. Each branch identifies its source baseline,
result type, acceptance, recipients and any tagged Astra specialist obligation.
Sol executes the branch with short task-focused contexts. Haskell recipes bind
repeated choices; they are machine coordination code, so use concise ordinary
functions rather than narrative boilerplate or a workflow framework.

The planner can launch cooperating roots with
`withLifetime SwarmOwned (solTask label seed task)` through ordinary `unfold`.
These workers keep their selected context and normal TUI after the planner exits.
Use `shareObservation rsi scope` to let a later RSI worker inspect a declared
creation tree; this shares observations without giving it stop authority.
Default workers remain supervised by their creator. Swarm shutdown still owns
all independent roots, and configuration remains frozen across their lifetimes.

A Sol component lead receives a `DeliveryLane` as `sessionInput` and, after its
declared prerequisites are satisfied, installs it with:

```haskell
flow <- deliverLane sessionInput sessionReply
```

`DeliveryLane` carries the assignment, branch/group labels and source
seeds chosen by the planner. The lead's request returns `Delivery`. The recipe
starts implementation, routes its candidate to independent review, starts
integration only after `Accepted`, and replies to the lead's requester with the
integration result. The lead can end its turn after installing the flow; it does
not wake just to relay routine success. Independent leads settle independently.

Reviewers use `requestRepair` with their retained implementer and watch the
returned response while keeping their review request pending. They inspect the
revised candidate before accepting. `ReviewBlocked`, `DesignBlocked`, and
`ExecutionUnavailable` preserve exceptional outcomes for the recipient. For a
tagged design obligation, `consultDesign slot question` starts the declared
specialist and returns its retained handle and answer watch. The requesting
review stays open while the specialist answers against the exact revised source.
An `AmendPlan` answer carries a `PlanAmendment`: exact base and proposed commit,
changed paths, affected obligations, rationale and evidence. After the owning
decision accepts it, `requestIncorporation` sends a separate request to the
retained implementer. Watch that response while keeping the review pending.
`Incorporated` records the original amendment, resulting head and checks;
`IncorporationBlocked` preserves a failed premise or check. Inspect the resulting
revision before accepting it. No receipt silently changes another worker's plan
or the swarm's frozen modules. Do not queue back to a lead waiting on this review.
`Project.Plan` declares the contract, projection and controls components, their
seeds and the tagged expert slot. Its Markdown tree supplies dependencies,
ownership, discretion and acceptance. `Project.Observe` relates each existing
lane handle to its plan, definition identity, response state and creation tree;
`rsiBranch` uses a selected high-level packet for an ordinary requested expert.

Callback failures emit exceptional attention to their owner. Recover handles with
`listRoutes` when they were created inside a recipe, then inspect `pollRoute`
and the retained effects before acting; replaying the whole chain could duplicate
already-started work. A callback may reply only to its owner's active request;
cancellation and update fences still apply. Such a reply ends the callback and
resumes the request through the ordinary actor scheduler.

The recipe modules and their selected Markdown prompts are checked through the
real resident workbench. Focused execution covers candidate/review evidence,
retained repair, planned specialist answers, exact plan incorporation, direct callback replies and cancellation. The delivery-lane check
uses real commits and an integration checkout; it does not claim live model or
fresh-context application acceptance.

For an ordinary human-started Astra RSI session:

```haskell
observed <- snapshot
inspectFull observed
inspectFull (swarmUsage observed)
inspectFull (usageByRequestedModel observed)
```

Keep a snapshot before a wave segment, bind `later <- snapshot` afterwards, and
inspect `usageDelta observed later`. Its comparable totals exclude newly visible
thread histories and discontinuities, which remain explicit in the same result.

Read selected plan branches and outcomes, identify repetitive context or routing
work, then edit prompts/helpers for the next swarm. Keep raw event streams out of
Astra's context unless diagnosing a specific failure. There is no special RSI
lifecycle or budget-enforcement service.

Validate an authored selection before starting a run or proposing an RSI change:

```sh
shoal check --workspace /path/to/project
```

This uses startup's frozen-module compiler without creating actors, contacting
providers or replacing a live swarm. It reports the definition identity and
imports. It checks source/configuration; it does not prove provider readiness.
The deterministic delivery test now drives the planned component constructor,
local implementation/review/integration, an RSI packet and customization commit,
and compilation of the next frozen selection while the old prompt stays fixed.
Fresh-model application usability and measured savings remain live-run evidence.
