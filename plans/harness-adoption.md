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

## What is deleted on this side

`exomonad/agent/src/backend/codex/` and the relay socket for host tools;
rollout parsing for usage; the `--disable multi_agent code_mode` launch
arguments; `Unfold.hs`'s free applicative and two-pass activation; the
watch-and-wake reactivation path for children; `ForkContext`; the
notification presentation states. The retired `exomonad/harness/` and
`exomonad/web/` trees go with them, since the harness ships its own web view.

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
