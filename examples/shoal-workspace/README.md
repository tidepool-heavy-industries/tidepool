# A project-specific Shoal workspace

Copy the authored `.shoal` directory into a project and adapt the terminology,
plan paths, acceptance and helpers. TOML is the configuration entry point. Paths
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

A Sol lead installs its declared lane with:

```haskell
flow <- deliverLane lane sessionReply
```

`lane :: DeliveryLane` carries the assignment, branch/group labels and source
seeds chosen by the planner. The lead's request returns `Delivery`. The recipe
starts implementation, routes its candidate to independent review, starts
integration only after `Accepted`, and replies to the lead's requester with the
integration result. The lead can end its turn after installing the flow; it does
not wake just to relay routine success. Independent leads settle independently.

Reviewers use `requestRepair` with their retained implementer and watch the
returned response while keeping their review request pending. They inspect the
revised candidate before accepting. `ReviewBlocked`, `DesignBlocked`, and
`ExecutionUnavailable` preserve exceptional outcomes for the recipient. A tagged
specialist/question workflow and a complete authored plan tree are still required
before treating this example as the finished planned-Sol package.

Callback failures emit exceptional attention to their owner. Inspect `pollRoute`
and the retained effects before acting; replaying the whole chain could duplicate
already-started work. A callback may reply only to its owner's active request;
cancellation and update fences still apply. Such a reply ends the callback and
resumes the request through the ordinary actor scheduler.

The recipe modules and their selected Markdown prompts are checked through the
real resident workbench. Focused execution covers candidate/review evidence,
retained repair, direct callback replies and cancellation. The delivery-lane check
uses real commits and an integration checkout; it does not claim live model or
fresh-context application acceptance.

For an ordinary human-started Astra RSI session:

```haskell
observed <- snapshot
inspectFull observed
inspectFull (swarmUsage observed)
```

Read selected plan branches and outcomes, identify repetitive context or routing
work, then edit prompts/helpers for the next swarm. Keep raw event streams out of
Astra's context unless diagnosing a specific failure. There is no special RSI
lifecycle or budget-enforcement service.
