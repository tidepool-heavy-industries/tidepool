# Self-writing Haskell actors

Status: active and incremental. The canonical landed-versus-pending inventory
and next delivery stage are in
[the implementation status](implementation.md#landed-architecture).

## Thesis

Tidepool should be an actor runtime whose actors are typed Haskell programs
paired with one serial agent context and a resident GHCi-style environment.
The agent context may be Tidepool-resident or a supervised interactive agent
application; the Haskell program remains the actor's policy in either form.

Rust owns execution mechanics. Haskell owns behavior. The Haskell program may
open a result-bearing agent session with `deliberate`; within an agent session,
the model can define new types and functions, return live values, replace
behavior, and construct new actor definitions.

The Haskell API optimizes for use by an LLM in a resident GHCi-style
environment, not for comprehensive exposure of runtime machinery. A small,
regular, success-shaped vocabulary is more valuable than a one-to-one Haskell
wrapper for every Rust state and failure. Rust keeps the complete operational
model and surfaces only distinctions that help an actor express typed behavior
or make a real policy choice.

The useful slogan is:

> Each actor is a Haskell program that can extend itself.

"Extend itself" may mean persistent declarations in a resident session or
editing and reloading an agent-backed actor's Haskell policy. A Tidepool-owned
provider loop reaches the workbench through fenced Haskell; an externally
hosted interactive agent reaches the same workbench through one actor-local,
GHCi-shaped hosted tool carrying raw Haskell. The transport differs, but the
language, persistent heap, interpreter, and typed completion contract do not.

At the system level, those actors form an adaptive unfold/execute/fold loop
over worktrees. The organization may begin as a detailed plan, a partial
subsystem sketch, or a discovered next step; actors recursively refine it into
heterogeneous child work, fold typed commits and evidence back upward, and
then unfold again from what integration revealed. The canonical contract is
[the architecture's self-hosting shape](architecture.md#self-hosting-shape-iterative-worktree-hylomorphisms).

The first production composition root is `shoal`: one host process owns the
resident Haskell machine and every actor, while external Codex TUIs occupy
tmux panes and connect to actor-scoped HTTP-over-UDS host dynamic tools. Local
actor scheduling, mailboxes, links, and supervision use Ractor. Tidepool adds
live Haskell execution, authority, provider sessions, and retained typed exits
above that substrate; it does not maintain a second actor scheduler.

## Accepted direction

- An actor combines one persistent Haskell environment, one serial control
  flow, one agent context, and one effect stack fixed for that exact identity
  under Rust-owned authority. Resident and interactive contexts are execution
  forms under the same identity and lifecycle, not separate actor systems.
- Ractor owns in-process task scheduling, sequential mailboxes, local
  addresses, linked ownership, and lifecycle notification. Same-machine
  messages move ordinary `Send + 'static` Rust values and are never serialized.
  Tidepool owns managed Haskell roots, exact execution context, effect
  authorization, model sessions, and repeatable typed exit observation.
- Experimental resident-Haskell effect profiles are named `ReadWrite` and `ReadOnly`. A `ReadWrite`
  actor may start either profile; a `ReadOnly` actor may start only `ReadOnly`.
  They test row selection and spawn attenuation; they are not Codex
  or operating-system sandboxes. Grants, worktrees, process policy, and opaque
  handles govern concrete runtime resources independently.
- The Haskell DSL stays small and `Member`-polymorphic. Same-machine protocols
  carry live typed values; JSON is only a durable or external boundary.
- Interactive sessions may return a live `AgentAction`. The hosted-tool call
  settles immediately; the resident actor runs and parks that action, and
  reactivates the same agent context only when the Haskell program asks for
  its next session. Ordinary `Functor`/`Applicative`/`Monad` composition is the
  orchestration vocabulary.
- Fresh spawn deploys an explicit program into a fresh context. Structural
  fork clones one exact model/Haskell/control point and applies registered
  capability and actor-linear-reference policy.
- `startActor` publishes only a ready exact reference. Exact calls
  never invent results or substitute actors; failure-prone jobs use
  `startActor`/`awaitExit` supervision and explicit Haskell control flow.
- A private sealed deployment hides an actor definition's concrete Haskell
  row. V0 runs one stack per actor and Rust authorizes nominal requests under
  the actor's principal and grants. Authored libraries remain
  `Member`-polymorphic; a later Haskell intent-to-kernel split must not require
  a reflected row ABI in Rust.
- One actor-turn admission spans a complete result-bearing agent session,
  while shorter machine checkouts serialize only its Haskell run segments.
  Lifecycle wake delivery is backend input, not a second session executor.
- Actor observability should converge on one neutral projection of runtime
  truth. Tracing, durable logs, owner notifications, and UI must not become a
  second scheduler or lifecycle registry.
- Moving a value does not move authority. Every child exit informs but does
  not kill its live agent-backed owner; owner termination recursively ends its
  subtree.
- Model-authored verification should normally run authoritative checks and an
  independent fresh-actor review before acceptance.
- Worker lifecycle and custody are live Rust interpreter state, not a
  model-visible registry snapshot threaded between Haskell calls.
  Haskell invokes typed batch lifecycle effects, receives structured results,
  and sees an immutable activation context containing correlated lifecycle
  wakes. Authored policy values remain ordinary immutable Haskell values.
- A worktree-backed worker submits a typed exit that combines an explicitly
  model-authored report with one Rust-observed repository state. Collection is
  repeatable until explicit acknowledgment; transport delivery never consumes
  the retained `ActorRef` implicitly.
- V0 result replay and worker correlation last only for the current root
  incarnation. `--recreate` starts new actor state and tells the resumed model
  that every old actor handle, worker binding, and pending result is dead;
  cross-incarnation recovery is later work.

## Documents

Each contract has one canonical home. Other documents link to it instead of
restating it unless an acceptance test needs the detail.

1. [Architecture](architecture.md) is canonical for runtime semantics,
   invariants, lifecycle, construction, and persistence.
2. [Live values and authority](live-values-and-authority.md) defines same-
   machine value transfer, caller identity, launch grants, and the
   distinction between invoking a closure and calling an actor.
3. [Haskell interaction surface](haskell-surface.md) is canonical for typed API
   consequences and the model-facing Haskell experience.
4. [Implementation plan](implementation.md) owns current status, delivery
   order, acceptance criteria, and retirement work.

## Vocabulary

This plan follows [the repository glossary](../../docs/GLOSSARY.md).

| Term | Meaning |
|---|---|
| actor | One exact identity, mailbox, Haskell program, persistent Haskell environment, and serial resident or interactive agent context |
| agent-backed actor | An actor whose conversation is owned by a supervised interactive agent application and whose actor-local hosted tool mounts the resident Haskell workbench |
| actor program | One installed authored `Eff` continuation with fixed row, protocol, and exit types |
| actor definition | An ordinary Haskell `ActorDefinition` containing typed startup, behavior, profile selection, and shutdown behavior |
| sealed deployment | The private exact-source/live-root representation produced inside `startActor`; never a model-facing value |
| agent session | One serialized, possibly multi-round resident or externally hosted agent interaction with a typed `Complete output` expectation |
| effect profile | An experimental named resident-Haskell row and spawn-attenuation class; initially `ReadWrite` or `ReadOnly`, and not a native-tool sandbox |
| machine session | The resident JIT machine, heap, declarations, bindings, and parked continuations |
| program image | The exact declarations, interface metadata, and live roots captured while `startActor` seals a definition |
| program snapshot | An immutable point in an actor's Haskell environment, suitable for structural sharing |
| execution principal | The runtime identity under whose authority Haskell is currently executing |
| capability | An opaque live value whose operations are authorized by Rust at use time |
| actor interpreter | Rust-owned nominal handlers and lifecycle policy enforcing requests under one actor principal |
| owner wake | Informational backend-native input derived from a child exit; typed results remain behind the exact `ActorRef` |

## Relationship to existing work

- [DevSwarm](../devswarm-haskell-dsl.md) remains the project-specific dogfood
  program. The actor model supplies its eventual execution substrate; DevSwarm
  still owns repository policy and roles.
- [`tidepool-runtime::session`](../../tidepool-runtime/src/session/mod.rs) and
  its [crate charter](../../tidepool-runtime/CLAUDE.md) own resident-machine
  suspension, checkout, roots, and frontend mounting seams. This plan consumes
  those mechanisms rather than introducing another continuation registry.
- [`ValueHandle`'s contract](../../docs/continuation-parking-contract.md)
  already proves that same-machine closure delivery is possible. The actor
  layer adds mailbox ownership and caller authorization above it.

## Non-goals for the first implementation

- Distributed actors or transparent cross-process live values.
- Restarting live closures, model contexts, or parked continuations after a
  process crash.
- A universal correctness score or fixed evidence ladder.
- A new durable database beside the existing get/put backend.
- Transferring runtime-issued continuation references through a fork.
- Detaching, reparenting, or adopting a live actor. These are desirable later
  extensions, but the initial ownership tree always terminates with its owner.
- Treating every scratch declaration as permanent actor-program state.
- Solving executable-code reclamation before real actor workloads establish
  the useful pressure. The design must expose growth and allow bounded
  rotation, but policy should follow measurement.

## Retirement

These are planning documents, not standing architecture. As phases land, their
load-bearing contracts move into the owning crate charters, public API docs,
and the glossary. This directory is deleted when the actor substrate and one
real Haskell actor have replaced the old `State`/`render`/`loop` path.
