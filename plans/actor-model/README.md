# Self-writing Haskell actors

Status: implementation is incremental. The Rust actor registry, ownership
tree, exact waits, live-value mailboxes, actor-local model sessions, execution
principal mounting, and the first typed Haskell `awaitExit` vertical have
landed. Program images, `ActorSpec`, startup, actor-local effect-stack
interpreters, and the full resident actor loop remain planned here.

## Thesis

Tidepool should be an actor runtime whose actors are typed Haskell programs
with a resident model context and a resident GHCi-style environment.

Rust owns execution mechanics. Haskell owns behavior. The model works inside
typed deliberation requests opened by the Haskell program, where it can define
new types and functions, return live values, replace behavior, and construct
new actor specifications.

The Haskell API optimizes for use by an LLM in a resident GHCi-style
environment, not for comprehensive exposure of runtime machinery. A small,
regular, success-shaped vocabulary is more valuable than a one-to-one Haskell
wrapper for every Rust state and failure. Rust keeps the complete operational
model and surfaces only distinctions that help an actor express typed behavior
or make a real policy choice.

The useful slogan is:

> Each actor is a Haskell program that can extend itself.

“Extend itself” does not mean rewriting source files. The primary interaction
surface is deliberately fenced Haskell in ordinary assistant responses: each
fenced block is compiled into the actor's persistent Haskell environment, and
may produce typed values—often closures or actor
specifications—which the fixed program can test, install, invoke, send, or
retain for rollback.

## Accepted direction

- An actor combines one accumulating model conversation, one persistent
  Haskell environment, one serial Haskell control flow, and one fixed
  per-incarnation effect stack interpreted by Rust.
- Rust owns mechanics and authority; `Member`-polymorphic Haskell owns typed
  protocols, state machines, behavior, deliberation, and composition.
- The model-facing DSL is intentionally smaller than the Rust kernel. New
  operations must earn their place by improving agent reasoning or typed
  composition, not merely by exposing an internal capability.
- Same-machine messages may carry arbitrary live Haskell values. JSON is only
  a durable or external boundary; durable facts use explicit get/put.
- Fresh spawn deploys an explicit program into a fresh context. Fork clones the
  exact model prefix, Haskell snapshot, and control continuation, while
  actor-linear continuation, reply, join, and parked-request references remain
  invalid in children. Capabilities follow their registered fork policies, and
  ordinary `AgentRef` values remain portable where authority permits.
- `startActor` performs specification-authored typed startup deliberation and
  publishes only an exact-incarnation reference that has crossed the readiness
  linearization point. A non-prompted constructor waits for a concrete use.
- Actors are logically concurrent and individually serial. Shared-machine
  Haskell execution remains globally serialized through existing checkout.
- Typed call, cast, `awaitExit`, and startup surfaces expose only their success types.
  Rust mechanically handles conditions it can resolve; an unsatisfied exact
  operation terminates the actor with a retained structured failure. Exact
  references are never silently replaced and authoritative operations never
  invent results. Failure-prone work uses explicit `startActor`/`awaitExit`
  supervision and may start a new child after observing an exit. Each reference
  retains its terminal exit for repeatable `awaitExit`; abnormal exits not already
  observed by a wait or call start advisory model turns rather than entering a
  heterogeneous Haskell inbox.
- Abnormal or unexpected child exit settles an already-observing obligation or
  creates one keyed advisory; it never kills its owner.
- Deliberation, startup, and advisory share one Rust-owned
  agent-session executor; their completion contracts differ, not their
  provider/Haskell orchestration. Assistant responses may contain several
  fenced Haskell blocks; all such blocks run in order against the same
  persistent environment, without JSON tool-call encoding.
- The existing durable harness journal is the seed of actor observability, not
  disposable scaffolding. The actor kernel emits neutral lifecycle, causality,
  model-turn, Haskell-execution, suspension, and resource events; log folding,
  streaming, and presentation remain outside authoritative runtime state.
- Owner termination delivers typed shutdown and recursively terminates the
  owned subtree.
- Moving a value does not move its creator's authority. Rust checks operations
  against the current actor principal.
- Model-authored verification should normally run authoritative checks and an
  independent fresh-actor review before acceptance.

## Documents

Each contract has one canonical home. Other documents link to it instead of
restating it unless an acceptance test needs the detail.

1. [Architecture](architecture.md) is canonical for runtime semantics,
   invariants, lifecycle, construction, and persistence.
2. [Live values and authority](live-values-and-authority.md) defines same-
   machine value transfer, caller identity, capability delegation, and the
   distinction between invoking a closure and calling an actor.
3. [Haskell interaction surface](haskell-surface.md) is canonical for typed API
   consequences and the model-facing Haskell experience.
4. [Implementation plan](implementation.md) contains only delivery order,
   dependencies, acceptance criteria, and retirement work.

## Vocabulary

This plan follows [the repository glossary](../../docs/GLOSSARY.md).

| Term | Meaning |
|---|---|
| actor | One actor identity, mailbox, Haskell program, persistent Haskell environment, and accumulating model context |
| actor program | The fixed authored harness plus currently installed typed behavior |
| deliberation | A typed request from the Haskell program to its resident model context |
| agent session | The possibly multi-round interaction that answers one deliberation |
| machine session | The resident JIT machine, heap, declarations, bindings, and parked continuations |
| program image | The declarations, interface metadata, and live roots required to deploy an actor specification |
| program snapshot | An immutable point in an actor's Haskell environment, suitable for structural sharing |
| execution principal | The runtime identity under whose authority Haskell is currently executing |
| capability | An opaque live value whose operations are authorized by Rust at use time |
| effect policy | The actor-local allowed request families, handlers, grants, and suspension policy |
| actor interpreter | One actor-local Rust handler instance enforcing an effect policy by nominal request identity |
| advisory turn | A Developer-triggered model session for an abnormal runtime fact with no parked Haskell result obligation |

## Relationship to existing work

- [DevSwarm](../devswarm-haskell-dsl.md) remains the project-specific dogfood
  program. The actor model supplies its eventual execution substrate; DevSwarm
  still owns repository policy and roles.
- [Resident-session kernel](../resident-session-kernel-design.md) and
  [session-crate design](../session-crate-design.md) own lower-level machine
  suspension and frontend mounting decisions. This plan consumes those
  mechanisms rather than introducing another continuation registry.
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
- Automatically promoting every scratch declaration into permanent program
  state.
- Solving executable-code reclamation before the semantic spikes establish the
  useful workload. The design must expose growth and allow bounded rotation,
  but policy should follow measurement.

## Retirement

These are planning documents, not standing architecture. As phases land, their
load-bearing contracts move into the owning crate charters, public API docs,
and the glossary. This directory is deleted when the actor substrate and one
real Haskell actor have replaced the old `State`/`render`/`loop` path.
