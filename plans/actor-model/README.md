# Self-writing Haskell actors

Status: implementation is incremental. The Rust actor registry, ownership
tree, exact waits, live-value mailboxes, actor-local model sessions, execution
principal mounting, exact source-import membrane, nominal effect routing,
provider-neutral model seam, and the first typed Haskell `awaitExit` vertical
have landed. Program images, `ActorSpec`, startup, actor-local effect-stack
interpreters, and the full resident actor workbench remain planned here.

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
  Haskell environment, one serial control flow, and one fixed effect stack
  interpreted under Rust-owned authority.
- The Haskell DSL stays small and `Member`-polymorphic. Same-machine protocols
  carry live typed values; JSON is only a durable or external boundary.
- Fresh spawn deploys an explicit program into a fresh context. Structural
  fork clones one exact model/Haskell/control point and applies registered
  capability and actor-linear-reference policy.
- `startActor` publishes only a ready exact-incarnation reference. Exact calls
  never invent results or substitute actors; failure-prone jobs use
  `startActor`/`awaitExit` supervision and explicit Haskell control flow.
- One actor-turn admission spans a complete agent session, while shorter
  machine checkouts serialize only its Haskell run segments. Startup,
  deliberation, and advisory share the same provider/fenced-Haskell executor.
- One neutral actor event stream records runtime truth. Views, durable logs,
  Developer advisories, and UI are projections; none is a second registry.
- Moving a value does not move authority. Unexpected child failure informs but
  does not kill its owner; owner termination recursively ends its subtree.
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
