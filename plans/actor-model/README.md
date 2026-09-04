# Self-writing Haskell actors

Status: active and incremental. The canonical landed-versus-pending inventory
and next delivery stage are in
[the implementation status](implementation.md#landed-architecture).

## Thesis

Tidepool should be an actor runtime whose actors are typed Haskell programs
paired with one serial agent context and a resident GHCi-style environment.
The agent context may be Tidepool-resident or a supervised interactive agent
application; the Haskell program remains the actor's policy in either form.

Rust owns execution mechanics. Haskell owns behavior. A long-lived agent actor
receives ad hoc typed requests through its mailbox; within each Codex-backed
session, the model can define new types and functions, return live values,
replace behavior, and construct new actor definitions.

The Haskell API optimizes for use by an LLM in a resident GHCi-style
environment, not for comprehensive exposure of runtime machinery. A small,
regular, success-shaped vocabulary is more valuable than a one-to-one Haskell
wrapper for every Rust state and failure. Rust keeps the complete operational
model and surfaces only distinctions that help an actor express typed behavior
or make a real policy choice.

The useful slogan is:

> Each actor is a Haskell program that can extend itself.

"Extend itself" may mean persistent declarations or editing and reloading an
agent-backed actor's Haskell policy. Codex reaches the workbench through one
actor-local, GHCi-shaped hosted tool carrying raw Haskell. The persistent heap,
interpreter, and typed completion contract stay under Tidepool ownership.

At the system level, those actors form an adaptive unfold/execute/fold loop
over worktrees. The organization may begin as a detailed plan, a partial
subsystem sketch, or a discovered next step; actors recursively refine it into
heterogeneous child work, fold typed commits and evidence back upward, and
then unfold again from what integration revealed. The canonical contract is
[the architecture's self-hosting shape](architecture.md#self-hosting-shape-iterative-worktree-hylomorphisms).

The first production composition root is `shoal`: one host process owns the
resident Haskell machine and every actor, while Codex TUIs occupy tmux panes
and connect to actor-scoped HTTP-over-UDS host dynamic tools. Local
actor scheduling, mailboxes, links, and supervision use Ractor. Tidepool adds
live Haskell execution, authority, Codex sessions, and retained typed exits
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
- Shoal's ordinary unit is a long-lived agent-backed actor in the recursive
  ownership tree, not a one-shot model invocation disguised as an actor.
  Haskell starts the actor once, sends any number of ad hoc typed requests to
  that exact incarnation, and stops it separately. One-shot work is a small
  composition over those operations rather than a second actor model.
- An ad hoc request fixes its result visibly at the sending boundary, normally
  as `request @ReviewReport agent prompt input`. Dispatch returns a requester-
  side `Response ReviewReport`; the target receives a distinct one-shot
  `Reply ReviewReport`. The result index belongs to those dual capabilities
  and the fixed-row `Replies` effect, not a result-indexed effect stack.
- Interactive applications remain attached for their actor incarnation. A
  model turn ends when the model stops producing output; no Haskell operation
  completes, yields, or parks a turn. Request and watch transitions reactivate
  the application through typed durable events, while only the supervisor may
  intentionally terminate the permanent root.
- Typed `Watch result` subscriptions recover useful `Functor`/`Applicative`
  readiness composition without becoming model-turn continuations or
  executable values returned through completion. A data-dependent `Monad`
  waits for a proven consumer because it adds continuation custody.
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
- One admitted request remains current across any number of model turns until
  typed settlement, cancellation, deadline, or target termination. Shorter
  machine checkouts serialize its Haskell run segments. Durable activation
  delivery is backend input, not another session executor.
- Actor observability should converge on one neutral projection of runtime
  truth. Tracing, durable logs, owner notifications, and UI must not become a
  second scheduler or lifecycle registry.
- Moving a value does not move authority. Every child exit informs but does
  not kill its live agent-backed owner; owner termination recursively ends its
  subtree.
- Model-authored verification should normally run authoritative checks and an
  independent fresh-actor review before acceptance.
- Actor lifecycle and custody are live Rust interpreter state, not a
  model-visible registry snapshot threaded between Haskell calls. Haskell
  invokes typed actor effects and keeps exact `ActorRef` values as ordinary
  live values.
- V0 live values and actor references last only for the current root
  incarnation. `--recreate` starts new actor state and tells the resumed model
  that old bindings and pending results are dead; cross-incarnation recovery is
  later work.

## Documents

Each contract has one canonical home. Other documents link to it instead of
restating it unless an acceptance test needs the detail.

1. [Architecture](architecture.md) records runtime semantics, invariants,
   construction, and persistence; completion-era sections are superseded.
2. [Live values and authority](live-values-and-authority.md) defines same-
   machine value transfer, caller identity, launch grants, and the
   distinction between invoking a closure and calling an actor.
3. [Haskell interaction surface](haskell-surface.md) records typed API
   consequences and older alternatives; the current Shoal vocabulary is in
   the reply/watch plan and field guide.
4. [Implementation plan](implementation.md) owns current status, delivery
   order, acceptance criteria, and retirement work.
5. [Shoal workbench correctness wave](shoal-workbench-correctness-wave.md)
   records the superseded completion-era migration baseline and the still-live
   GHCi/workbench findings.
6. [Persistent applications, typed replies, and watches](persistent-applications-replies-and-watches.md)
   supersedes that wave's root-completion and interactive `AgentAction`
   direction after live Shoal Console use, and owns the current
   request/activation contract.

## Vocabulary

This plan follows [the repository glossary](../../docs/GLOSSARY.md).

| Term | Meaning |
|---|---|
| actor | One exact identity, mailbox, Haskell program, persistent Haskell environment, and serial resident or interactive agent context |
| agent-backed actor | An actor whose conversation is owned by a supervised interactive agent application and whose actor-local hosted tool mounts the resident Haskell workbench |
| actor program | One installed authored `Eff` continuation with fixed row, protocol, and exit types |
| actor definition | An ordinary Haskell `ActorDefinition` containing typed startup, behavior, profile selection, and shutdown behavior |
| sealed deployment | The private exact-source/live-root representation produced inside `startActor`; never a model-facing value |
| interactive application | The persistent model context and hosted workbench attached to one actor incarnation |
| request scope | The typed input and one-shot reply authority retained across model turns until one request settles or becomes unavailable |
| effect profile | An experimental named resident-Haskell row and spawn-attenuation class; initially `ReadWrite` or `ReadOnly`, and not a native-tool sandbox |
| machine session | The resident JIT machine, heap, declarations, bindings, and parked continuations |
| program image | The exact declarations, interface metadata, and live roots captured while `startActor` seals a definition |
| program snapshot | An immutable point in an actor's Haskell environment, suitable for structural sharing |
| execution principal | The runtime identity under whose authority Haskell is currently executing |
| capability | An opaque live value whose operations are authorized by Rust at use time |
| actor interpreter | Rust-owned nominal handlers and lifecycle policy enforcing requests under one actor principal |
| owner wake | Informational backend-native input derived from a child exit; typed results remain behind the exact `ActorRef` |
| agent request | One ad hoc, caller-typed interaction with a long-lived agent-backed actor; it does not define or end that actor |
| response | The requester's typed observation capability for one request and the owner of its ready live-result reachability |
| reply | The target's typed, one-shot settlement capability for one request; settlement is separate from model-turn and actor lifecycle |
| watch | An actor-owned typed readiness subscription over one or more response handles; it notifies without completing a model turn |

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
