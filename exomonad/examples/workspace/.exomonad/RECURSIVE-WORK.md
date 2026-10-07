# Optional Git project delivery

Use this authored workflow when a Git project benefits from independent
implementation, exact-source review, repair, integration, and executed checks.
General exploration and actor programming do not require a project workflow,
agent hierarchy, or group naming convention.

Keep one integration owner accountable for the accepted outcome. Before
assigning independent work, establish the shared types, minimum consumer wiring,
source revision, and integration contract that the next work depends on. Each
assignment names its objective, source, owned paths, dependencies, acceptance
evidence, and escalation condition. A child result is local evidence; the owner
still verifies combined behavior on the integrated source.

## Admit independent work

Create each agent explicitly with its actual `AgentSpec`, context, workspace, and
spawn options. A spawned agent is idle until a typed request activates it. Use
`SameDir` when the child should share actual writable files, index, and HEAD. Use
`ForkWorktree seed` when it should start from one selected committed source.
Context, workspace, and tool installation are separate choices. Spawn labels are
ordinary optional text, not identity or grouping controls.

Send the actual typed assignment as raw request input. Use
`requestWithProgress` when intermediate questions or checkpoints affect the
owner's next decisions; otherwise use `request`. A request has one control
identity and a typed terminal result. Compose independent `result` projections
as `Await` values, or use `followWork` when an authored event-source collector
adds useful progress routing for already-created requests. Collector source IDs
are ordinary domain labels. The collector does not create agents or batches.

For a collector, inspect retained state and original receipts before acting on a
notice. `acknowledgeWork` records inspected publications; it does not prove
incorporation. `finishWork` closes the collector and returns a typed actor exit;
`Completed` contains the retained state. It does not retire the model agents.
Agent retirement and workspace cleanup are separate operations, covered by the
cleanup skill. Keep unfinished work under a
named owner and retain its actual terminal evidence.

## Review and integrate

Keep authored `reportedChecks` and `reviewNotes` separate from executed check
receipts and review findings. Admit a reviewer against the exact candidate source.
A new candidate requires new review evidence; a request to an existing reviewer
does not move that actor's workspace. A repair must stay inside the accepted
contract, preserve the review basis, and pass the required checks again. Unknown
evidence, scope uncertainty, failed admission, or a changed integration head
returns to the owner for a decision.

Integrate reviewed work in coherent slices when dependencies allow. Check the
resulting integration head, including combined acceptance; a child check does
not establish that result. Keep publication, acknowledgment, incorporation,
review, integration, executed checks, and resource cleanup as distinct facts.

Use the cleanup skill before retiring agents or actor services. Return unfinished
ownership to a named owner rather than treating a finished request or collector
as a cleanup signal.
