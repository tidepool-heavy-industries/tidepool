# Adopting the standalone model harness in Exomonad

Companion to `~/dev/exomonad-harness/PRD.md` (the harness contract) and its
`docs/tree.md` (the dogfood wave that builds it). This file is the Tidepool
side: what Exomonad becomes once the harness exists, in what order, and what
is deleted. Written 2026-09-23 from the research notes of that day. It is a
plan, not standing architecture; when the adapter lands, the crate guides own
the description and this file goes.

## Why

The forked Codex backend has no async tool calls, gates effort changes on
Astra only, runs one subprocess per actor, relays host tools over a socket,
and reads usage by parsing rollouts. Every feature is a patch on a moving
upstream. GPT-6 makes tool calls non-blocking (`async: true`, late output on
the original `call_id`), so a Haskell cell, a child agent, or a long command
becomes a call that settles when it settles. The harness is built around that
and around the agent verbs GPT-6 was trained on. Exomonad supplies the
meaning of tools and hooks; the harness owns everything about the model.

## Dependency direction, and why a separate repository

`exomonad -> exomonad-harness`. The harness never depends on Tidepool and
never learns what a judgment service is. It offers typed hook points and
stores every decision with a provider-opaque evidence blob. Jev lives
entirely on this side, behind those hooks.

The separation is the build strategy. A Tidepool build carries the GHC
worker, Cranelift, and a many-crate workspace; a check-and-test cycle is
minutes and memory-heavy, which is the wrong inner loop for a swarm of models
building by many small commits. The harness crate checks and tests in
seconds, so it can be dogfooded into existence by the agents that will later
run inside it. Its PRD tells those agents exactly that: they are building
their next home, every rule carries its reason, and the reason wins when the
two disagree. Nothing in Tidepool changes until the adapter step below, and
the harness never imports from here.

## What Exomonad provides to the harness

One adapter crate implementing the harness `Provider` trait. It replaces the
`InteractiveAgentBackend` boundary; this is a breaking change, not a second
backend beside `codex/` and `mock.rs`.

- **Tools.** Derived from the protocol effect definitions, as today's tool
  records are. The cell tool becomes a freeform custom tool marked async:
  raw Haskell source arrives unescaped, the cell runs as a job, and its
  output settles on the call. Fast cells read as synchronous because a job
  that settles before the next request is delivered in that request.
- **Jobs.** A cell, a command, a form, a child's task. Each is a future with
  a cancellation token and a progress sink to the event stream. Cancel yields
  a typed `Cancelled` output, never silence.
- **Hooks.** See the placement table below.
- **Compactor.** The `Structured` strategy where the handoff tool is the cell
  tool itself: the cell must evaluate to a value of the summary type, so the
  typecheck is the strictness. Code then appends the deterministic state
  (live bindings with types, worktree OIDs, child paths with pending claims,
  contract clauses done and undone). Bindings live in the resident
  environment and survive compaction untouched.
- **State.** The per-conversation record copied on fork: the resident
  environment handle, the worktree binding, the contract record.

## What changes in the Haskell surface

The retired resident-actor design had one yield, `runLLMTurn`, parking a
typed continuation the model filled, and was dropped because a hosted
agent's prose could not safely be executed. Owning the loop removes that
reason. Cells as a freeform custom tool are the safe form of the same idea.

| Today | After |
|---|---|
| `unfold`, `errand`, `withContext inherited/selected`, two-pass activate | one effect `spawnAgent :: From -> Contract -> Eff es AgentRef`, `From = Prompt \| Here \| Checkpoint name`. N spawns in one cell are N siblings from one point; the child inherits the parent's claim on the pending cell call and starts from the cell's committed snapshot. |
| `watch` + end the turn + wake + `pollWatch` | the model calls `wait_agent`; the child's final answer arrives as a FINAL_ANSWER envelope, a settled job on its own `call_id`. No notification-and-wake protocol. |
| the `Await` applicative, `Watches` (register, groups, routes, observe, forget), the three-valued watch state, `ProgressCursor` revisions, the 30-second foreground handoff in `Command.hs`, `ToolRounds` | a cell blocks; the call settles late and the model resumes through `wait_agent`. The shape of the blocking surface is not decided; see "Blocking in cells" below. |
| `sendMessage`, `parentAgent` | `send_message` (no turn) and `followup_task` (starts a turn) on agent paths; parent path is a field of the actor context. |
| `afterTool` annotation | the `tool-result` hook, one of eleven. |
| labels validated as kebab segments | labels are path segments; model-facing form uses the trained grammar (lowercase, digits, underscore); both accepted, canonicalized. |
| one worktree per actor | one worktree per subtree lead; leaves share it with `owned`/`mustNot` paths from the contract record, vetoed at admission; the harness commits by pathspec for them. `tryMerge` on an exact OID stays the merge primitive. |
| `ForkContext`, fork groups | `From` above; fork strip list (settings items, annotations, dropped claims). |

| harness verbs as crate tools (`checkpoint`, `compact{keep_since}`, `set_effort`) | the same three as effects: `checkpoint :: Name -> Eff es Checkpoint`, `compact :: Checkpoint -> Eff es Compacted`, `setEffort :: Effort -> Eff es Effort`; a cell calls them, the adapter routes to the harness verb, the typed result (including `Refused{prefix_cold}`) comes back as a Haskell value. |
| user input, forms, notifications as separate mechanisms | one mailbox: the operator is the path `/operator`; a form is a `followup_task` to it; its answer is the operator's FINAL_ANSWER; job progress is an envelope from the agent path plus call handle. |

Kept as is: `startActor`/`call`/`awaitExit` for non-model actors, `reflect`,
the worktree crate, the glossary vocabulary. `checkpoint` keeps its name and
gains the harness meaning: fork point, cache breakpoint, compaction boundary,
and tree label at once.

Wave 1 of the harness is deliberately standalone and Exomonad-blind; the
tight integration described in this table is the adapter step, not the first
build. Dogfooding the harness well comes first.

## Hook placement (Jev behind typed hooks; the harness sees only decisions)

Measured rules from `~/jev-laptop`: Jev answers which, never what next;
packets over one state are applicative; the contract is data and every
packet compiles from it; answers are addresses.

| Harness hook | What Exomonad does there |
|---|---|
| spawn | validate and complete the contract record (clauses, acceptance, owned, mustNot, introduces, consumes, boundaries); split coverage and per-interface dependency Nouls before children start |
| tool-call-admission | code decides a boundary was crossed (path outside owned, network, destructive); Jev classifies which policy clause; three consecutive denials escalate to the parent |
| child-reply | compiled review joins over the parent-derived diff: clause to hunk, clause to test, four-valued claims on the report; gate as a screen; weak reply returns as a `followup_task` assembled from item ids with verbatim text |
| mailbox-message | attention score on the envelope mapped by code to Steer, AtBoundary, Hold |
| compaction `Select` filter | locate with per-item Nouls, keep top few plus floor, confirm with a Choice, accept only if the pruned answer agrees with the unpruned |
| tool-result | today's watchdog nudges (`docs/nudges.md` in the harness repo) |
| model-stopped | `Compact` when usage crosses the configured fraction; `Spawn` never (the model or the program spawns) |

Every Jev call is a `decision` row in the harness store with the packet and
answer as evidence. A replay provider answering from stored evidence makes
node logic a pure test.

## Reflect becomes fine-grained

Today `reflect n` returns the caller's last n turns as text because the
conversation lives in a Codex process the actor can only read back through
its rollout. With the harness and the swarm in one process, the store is the
conversation, and `reflect` becomes typed queries over it: items by address,
the envelopes sent and received, the hook decisions taken on this path with
their evidence, jobs with their timings, usage and cached fraction per
request, and, for a parent, the same views over its children. A Jev packet
can be compiled straight from a query result, since items already have
addresses. The old signature stays as a convenience over the new queries.

## The helper-to-tool pipeline (write this up after the first runs)

The self-improvement story the harness makes tellable, in one concrete
instance. A model writes an ad hoc Jev helper in a cell that trims context it
did not want from a large result. It is used again. Someone notices, from the
decision and job rows, that it is composed with file reads far more than
with anything else. It moves into a project module as a named helper. Then
into the tool-result hook for the read tool, as a `Pruned` annotation. Then,
when the pattern is stable, into the read tool itself. Four stages: ad hoc
helper, helper method, hook, tool modification. Each stage is visible in the
store, the "noticed" step is a query rather than a hunch, and nothing in the
crate changed until the last step. The harness ships the primitives that make
each promotion small; the model does the promoting. Record the first real
instance with its query.

## What the dogfood runs are showing (our notes, not the builders' task)

The harness waves run inside today's Exomonad on purpose: each run pays the
costs the harness is meant to remove, and the root's friction notes in the
harness repo's `docs/questions.md` are the primary evidence. Wave 0
(2026-09-23/24) showed: a registered watch gave no progress unless the child
published a stream, so the root polled and then read git instead; native
`spawn_agent` children could not use Bash, Haskell or status; a batched
`lookup` over agent lists failed in the compiler worker; the `afterTool`
watchdog was not installed for the root actor, so no nudge ledger could
exist; leaves got isolated worktrees and the parent merged by hand with no
veto before the edit; effort was fixed at spawn. Each maps to a mechanism in
the surface table above. Keep adding here per wave; this is the case for the
adoption, written from what the runs cost.

## One process

Today every actor is a Codex subprocess with its own conversation on disk,
so the swarm side has to manage a process tree: launch arguments per actor,
a relay socket per process for host tools, memory ceilings for N concurrent
Codex instances, rollout files as the only view of usage, wake-and-poll
protocols because a parked child is a parked process, and cleanup of the
tree when a parent dies. After adoption there are a few processes total: the
harness (all agent threads, the store, the web view), the GHC worker, and the
cells' machines. An agent is a row and a set of futures, not a process. A
parked node costs a row. Fork is a store operation, not a second process with
a copied rollout. Child shutdown is dropping futures under a subtree root.
Memory is the model window per live request plus the store, so the swarm's
own memory management collapses to the compaction policy in the hooks table.

The harness library ships no supervisor and no actor abstraction; it returns
one future per agent over a shared store, and its standalone binary has a
small driver that spawns them. Adoption replaces that driver with the actor
host here, which already owns supervision. `exomonad-actor`'s identity,
lifecycle and mailbox become views over harness rows or are deleted; the
merge is an Exomonad actor becoming a harness agent path plus a resident
environment, not a shared interface between two actor systems. Standalone
pieces built in wave 1 are built to be hosted this way, not rewritten.

## Blocking in cells (suggestions, not decisions)

Everything under `Tidepool/Agent/Watch/` and the foreground handoff in
`Command.hs` exists because a cell could not outlive the Codex turn. With
async tool calls the cell's job blocks and the call settles late, so the
question becomes what the blocking surface should look like. The following
are one model's answers to "what would you expect, sitting at the cell
prompt as a parent with children, jobs, and an operator", written
2026-09-23 before any of it is built. Each is a suggestion to test in
dogfooding, not a rule.

- **Events underneath, handles as sugar.** `awaitAgent h` is what a model
  types without reading docs, so keep it. But with two children the next
  thing wanted is "whichever finishes first", and with handles alone that
  is a special function to look up. If handles expose events (`settled h`,
  `progress h`, `envelopeFrom path`, the operator's reply) and there is one
  blocking primitive over events with `race` and `both`, then
  `awaitAgent h` is `await (settled h)` and a peek is
  `race (settled h) (after 30s)`, a value to pattern-match. `Tidepool/Event.hs`
  already has `awaitFirst` over `Event a`. No `awaitFor`: one composable
  primitive beats a second one to remember. This shape also leaves room for
  an applicative layer over events later, which is out of scope now.
- **Claims hold the duplicate.** A child's reply both settles the awaiting
  cell and is a FINAL_ANSWER envelope to the parent path. If a cell holds a
  claim on the child, the envelope is delivered as Hold and released when
  the claim ends (the cell returns, fails, or is cancelled), never dropped.
  The parent decided how to consume the reply when it wrote the cell;
  seeing it twice reads as the harness not trusting that. The claims rule
  already does this for calls.
- **Report only.** No interval notices that a cell is still waiting; they
  cost a turn boundary and carry nothing actionable. The cell calls `report`
  (the harness `JobVerbs::envelope`) where its author knows something
  happened. A child's `report` reaches the parent as an envelope from the
  child's path, so the parent's mailbox hook can Steer or leave it. The
  operator looks at the web view.
- **The child calls a tool; the parent picks the route.** A child given
  `askParent :: Question -> Decision` calls it and gets a typed answer,
  never knowing whether code, a cell, or the parent's model answered. Per
  tool the parent chooses a handler in its own environment (the existing
  `spawnAgentWithTools` idea, minus the round bound, because the handler
  runs as a job) or "ask my model", which is a `followup_task` to the
  parent's own path delivered as an ordinary envelope. Nothing new in the
  harness.
- **Ownership by origin.** A child spawned from a cell belongs to that
  cell; cancelling the cell cancels the child. A child spawned by the
  model's verb belongs to the conversation and is untouched by any cell's
  cancellation. Same split the claims rule makes between the two spawn
  origins. Retain-first: a cancelled child's last state stays queryable.

The harness-side implications (Hold released on claim end; cancellation
scoped by spawn origin) are recorded in the harness repo's
`docs/ideas-later.md` so wave 1 does not preclude them.

## What is deleted on this side

Per-actor process supervision and the resource accounting around it
(concurrent Codex instance limits, subprocess memory ceilings, orphan
cleanup). `exomonad/agent/src/backend/codex/` and the relay socket for host tools;
rollout parsing for usage; the `--disable multi_agent code_mode` launch
arguments; `Unfold.hs`'s free applicative and two-pass activation; the
watch-and-wake reactivation path for children; `Tidepool/Agent/Watch/` with
the `Watches` effect and its handler; the foreground-handoff and
observation-deadline paths in `Command.hs`; `ToolRounds`; `ForkContext`; the
notification presentation states. The retired `exomonad/harness/` and
`exomonad/web/` trees go with them, since the harness ships its own web view.

## Parity with Codex: what Exomonad takes from it today, and where each goes

Measured from `exomonad/agent/src/backend/codex/` (about 9.5k lines) on
2026-09-24. Exomonad drives Codex through `thread/start` with dynamic tools,
`turn/start` with a cwd and a workspace-write sandbox policy, `turn/interrupt`,
and `item/tool/call` relayed back over the commands socket; it reads
`config.toml` for model, effort and its own `model_instructions_file`; it
reads `auth.json` for the subscription credential that Codex refreshes; and
it reads rollouts for usage and for `reflect`. No MCP servers are handed to
Codex.

| Taken from Codex today | Covered by | Gap |
|---|---|---|
| model loop, streaming, retries | harness waves 0 to 1 | none |
| threads, fork, dynamic tools relayed to Exomonad | harness agent tree + `Provider` adapter (steps 2 and 3) | none |
| rollouts for usage and `reflect` | harness store queries | none |
| effort per thread from config | `configuration_update` (correction wave c) | none |
| compaction | `Compactor` (correction wave d, wave 1) | none |
| model instructions file | Exomonad already supplies its own; becomes the root developer item | none |
| subscription credential and refresh | harness reads Codex's `auth.json` read-only; **Codex stays installed only to refresh tokens** | login and refresh in the harness `Auth` trait. The retired `exomonad/harness/src/provider/oauth.rs` has a complete device-code and browser login with refresh (1.4k lines); port, do not redesign. Reference only, own code, per the Codex rule |
| `commandExecution` and `apply_patch` under a workspace-write, network-off sandbox | harness wave 2 gives the demo provider `run`, `read`, `edit` unsandboxed (operator's machine, trusted models) | sandboxing itself, deferred. Owner when it comes: the provider, since tool meaning is provider-owned; the harness supplies only the owned/mustNot veto |
| `turn/interrupt` | `interrupt_agent` declared, refused `not_available` | cancel in flight; needed for parity, not only for the operator |

The first three rows are the hard part and they are planned. The last three
are ordinary engineering that no wave owns yet.

## Follow-on waves toward replacing Codex

Harness-side first, Tidepool last. The harness stays pluggable and the demo
provider grows just enough that the operator can drive it alone for a few
runs before any adapter is written.

1. **Correction wave** and **wave 1** in the harness repo (its `docs/tree.md`).
2. **Wave 2, standalone and operator-drivable** (harness repo). Auth: port
   the device-code login and refresh from the retired
   `exomonad/harness/src/provider/oauth.rs` behind the `Auth` trait, so the
   run stops reading Codex's file. Coding tools in the demo provider,
   unsandboxed for now: `run` with cwd at the subtree worktree and a wall
   timeout, a `read` tool, `edit` owned-confined. The operator's machine,
   trusted models; sandboxing is deferred to a later wave and belongs to
   the provider when it comes. Tree mode by default, the operator
   drives root from the page. Gate: one real multi-file task in a scratch
   repo with Codex not installed and Tidepool untouched. Note for that later wave: Tidepool's own code has no subprocess
   resource limits today (checked 2026-09-24; the only `setrlimit` is in
   vendored Codex).
3. **Operator runs** of wave 2, several, before anything below. Friction
   notes from these go in the section above.
4. **Adapter wave** (Tidepool, steps 2 and 3 in Order): `Provider` for the
   cell tool and shell, `spawnAgent` and the contract record. Sandboxing, when
   it comes, covers cells and shell alike.
5. **Interrupt** (harness): `interrupt_agent` with the typed previous
   status, cancellation scoped by spawn origin. Later; nothing above needs
   it.
6. **Deletion**: steps 6 and 7 in Order. Codex is no longer on the machine.

## Order

1. Harness wave 1 lands in its own repository (see its `docs/tree.md`).
2. Adapter crate: `Provider` for the cell tool and shell only, toy-equivalent
   acceptance against a real resident environment.
3. `spawnAgent` effect and the contract record; retire `unfold` and friends.
4. Hooks in the order of the table, tool-result first (nudges already exist).
5. `Structured` compaction through the cell tool.
6. Delete the Codex backend once one dogfood run completes on the adapter.
7. Subscription auth in the harness; then the Codex fork is no longer needed
   for anything.

## Open

- Program-driven typed turns (Haskell asks the model for a value of type
  `a`). A cell can raise an effect that asks the model; whether a dedicated
  primitive is worth having is decided after wave 1.
- WebSocket lane (mid-turn steering, injecting a settled output into a
  running response, named lanes) once the HTTP path is byte-stable.
- Whether the server-compacted window retains an unanswered call
  (harness `docs/findings.md`).
