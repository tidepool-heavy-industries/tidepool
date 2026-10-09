# Compositional agent library delivery

Status: implementing from `82a847e541c708578c57a748e19ab4f1c25e00f6`.
The original engine session owns main; this wave integrates in its own worktree.
Implementation checkouts and evidence live in
`/srv/swarm/evidence/inanna/compositional-agent-library-20261006/`.

## Accepted contract

- One operation: `spawnSubagent context workspace (defaultSpawnOptions actualSpec)`.
  Context is `ForkCtx checkpoint | FreshCtx prompt`; workspace is
  `SameDir | ExistingWorkspace opaqueHandle | ForkWorktree seed`.
- Spawn options hold the real typed AgentSpec, model, effort, instructions,
  optional ordinary Text label, lifetime and limits. No role discriminator,
  group label, batch combinator or mandatory Assignment. Agent identities and
  workspace/storage names are independent of labels; duplicate labels work.
- Spawn returns `Either SpawnError AgentRef` after tools, workspace, captured
  context and provider attachment are ready. The actor is idle. Fresh context
  seed is not activating input. First request or human message activates it.
- `request @Answer agent input defaultRequestOptions` accepts raw typed input.
  `requestWithProgress` additionally provides a typed independent progress
  handle. Request handles retain singular control identity. Admission,
  execution and authored domain errors remain distinct typed outcomes.
- Spawn defaults to parent actor ownership; request defaults to caller actor
  ownership. Explicit invocation, run and scope lifetimes remain possible.
  Returning a handle does not transfer ownership. Waiting cancellation does
  not cancel requests; request cancellation does not retire its target actor.
- One observation path: `result :: Request a -> Await a` and
  `await :: Member Watches effects => Await a -> Eff effects (Either AwaitError a)`.
  Await supports Functor, Applicative, traverse, eitherOf and progress.
  Settlement projections expose failures as values for collecting outcomes.
- Either chooses first terminal branch (including failure); initial ties are
  left-biased. All fails on a required branch failure. Choices latch even
  inside an unfinished parent All. Losing branches never poison winners.
- Readiness remains an expression (Ready/Leaf/All/Either), not distributed CNF.
  One watch owner retains immutable decisions and selected response/progress
  leases. Haskell projection follows that decision without repolling branches.
  Forgetting a response does not revoke a previously retained terminal snapshot.
- Actual typed spec installation retains and transfers compiled installer code
  and exact dependencies. Use the existing startup installation owner, no
  source-expression reconstruction or per-child installer recompilation.
  Explicit same-surface replacement publishes atomically; in-flight calls pin
  their installation. Surface changes require a new incarnation. Filesystem
  reload cannot silently replace an explicitly supplied spec.
- `withScope` is a runtime-owned delimiter, registered before its callback runs.
  Its callback receives an opaque Scope. Resources choose `InScope scope`
  explicitly; ambient spawn/request defaults do not change. ScopeOutcome keeps
  body and cleanup outcomes separately. Return live values into parent custody
  before retiring body roots. Failure, cancellation, nesting and incomplete
  cleanup use the retained finalization path, never trailing Haskell cleanup.
- Extend existing invocation cleanup membership for nested scopes; do not add
  resource registries or a scheduler. Authority, cleanup ownership and operation
  construction provenance are independent. Transfers linearize against closure.
- Workspaces have one registered backing and independent actor attachments.
  SameDir shares actual files/index/HEAD. ExistingWorkspace reuses backing via
  an opaque run-issued grant. Root registration admits dirty/detached checkouts.
  ForkWorktree resolves a selected committed seed once. Retiring an attachment
  does not retire siblings or delete directories. Partial failures retain
  concrete actor/workspace identities and cleanup uncertainty.
- Directory selection does not replace compiled source. Shared mutable files,
  immutable source selection and installed tools remain distinct. FreshCtx
  excludes ambient lexical bindings, while explicit closure dependencies remain.
  Preserve compiler product sharing and CompletedOriginal acquisition.
- Remove Branch/Unfold/unfoldDeferred, vocation constructors/profiles, group
  admission, mandatory label grammar, role-selected grants/prompts/installers,
  role-specific preparation coverage, and associated generated/persisted paths.
  Project/Contrib remains ordinary authored composition, not a hidden framework.
- Backward compatibility may break. Old runs remain evidence, not resumable
  compatibility mode. Regenerate prepared products from their declared owners.

## Parcels and edit ownership

| Parcel | Owner | Primary paths |
|---|---|---|
| Workspace identity/membership | shared_workspace_design | exomonad/worktree; workspace/backing attachment code; handler/worktree |
| Readiness graph/decisions | spawn_library_design | actor request watch owner; Haskell Agent/Watch; watch protocol |
| Runtime scopes/ownership | spawn_lifecycle_design | invocation_work; rooted scope execution; request/command/actor cleanup |
| Explicit spec installation | installer worker | actor agent_spec/resident_workbench installed tool state; startup installer |
| Independent idle spawn | spawn worker | actor start/lineage/child_launch; facade attachment and spawn admission |
| Public Haskell API and schemas | surface worker | Actors/Spawn; Internal/Agent; Launch/Assignment; effect schemas and compiler sites |
| Project/Contrib migration | consumers worker | authored Project/Contrib modules and workflow fixtures |
| Role/preparation deletion | roles worker | role selection/config/preparation/frozen manifests |
| Documentation migration | docs worker | prompts, skills, glossary and guides |
| Integration/build/review | root | this plan, joins, generated manifests and accepted verification |

Owners coordinate shared-file edits directly before changing another parcel's
methods. Publish concrete schema/signature changes early. No worker writes main
or claims source checks as executed runtime evidence. Each worker compiles its
affected targets and runs focused tests through the admitted repository commands.
Use measured resource headroom to run independent checks concurrently; coordinate
concrete checkout, configuration and output conflicts. Each owner commits its
parcel with focused tests and records unexecuted gates and their blockers
explicitly.

## Acceptance

1. Every changed target and public example compiles; legacy production consumers
   and stale generated/prepared paths are removed.
2. Context/workspace combinations, dirty/shared directories, duplicate labels,
   independent source selection and attachment retirement execute correctly.
3. Idle spawn causes no inference; first request executes through real Haskell.
   Explicit captured spec works in FreshCtx/ForkCtx and replacement preserves
   admitted calls. Repeated spawns do not compile an installer per child.
4. Stateful property tests independently model readiness histories, ownership
   transfer, scope cleanup and workspace memberships. Test readiness/projection
   races with release, progress supersession and producer retirement.
5. Scope tests cover normal return, evaluation failure, cancellation, partial
   admission, nested scopes, escaped tokens, returned lazy values and uncertain
   cleanup. No replay of body effects during cleanup retry.
6. M2: A parks while B publishes; A resumes without erasing B; two children use a
   capture before the parent finishes and survive later parent failure.
7. Freeze and qualify one matched new bundle. Human exploration uses an Astra
   Medium root and Luna children, with Jev, progress and shared/forked directories.
   Retain actual timing and failure evidence; do not prescribe a project.
