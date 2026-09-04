# Cache-preserving context unfold and typed result fold

Status: accepted and interaction-pressure-tested; implementation in progress.
This plan is the canonical design and linear handoff for context-preserving
forks of interactive agent applications. It refines the older process-fork-
shaped sketches in
[architecture.md](architecture.md) and
[haskell-surface.md](haskell-surface.md).

This file is the root document for the feature: product decisions, worked
LLM interaction, runtime boundaries, implementation ordering, and the durable
implementation checklist all live here. Supporting actor-model documents provide
landed substrate detail; when they disagree about interactive context forks,
this document wins until the feature lands and its contracts move into the
owning crate guides.

## Outcome

Tidepool should let an actor describe several independent branches in one
ordinary Haskell expression, fork its exact accumulated model and Haskell
context once, and receive typed handles immediately. Each child already sees
the complete `unfold` call and every branch plan in it. Its new prompt needs
only to identify the selected branch and state the effective role and resource
policy.

The parent later folds the children back through ordinary Haskell values,
`Response`s, applicative `Await`, and a durable `Watch`. A fork is actor
construction, not a one-shot async function. Replying completes one request;
it does not tear down the child or its useful learned context.

The interaction should feel like this:

```haskell
contextUnfold = campaign "context-unfold"

workers <- unfold (batch contextUnfold "implementation") $
  Campaign
    <$> child (coding @PatchReport "domain" domainTree domainPlan)
    <*> child (coding @TestReport "tests" testTree testPlan)
    <*> child (researching @ReviewReport "semantics" projectHead reviewPlan)
```

In the next hosted Haskell call:

```haskell
finished <- watch "implementation-results" $
  CampaignResult
    <$> awaitFork (domain workers)
    <*> awaitFork (tests workers)
    <*> awaitFork (semantics workers)
```

The exact function names remain provisional. The shape is not: one applicative unfold
at one frozen context boundary, immediate persistent actor handles, and an
explicit typed fold later.

## Accepted product decisions

1. **The hosted Haskell call is the fork point.** Every child inherits the
   provider transcript through the assistant's complete `unfold` tool call,
   including all branch inputs. Children do not receive separately generated
   summaries of that context.
2. **Only the branch selector is task-specific child prompt input.** A child
   receives a small activation such as “continue branch `semantics` from the
   shared unfold call.” Its selected Haskell input is also mounted as the
   ordinary typed `sessionInput`.
3. **The parent gets handles, not answers.** `unfold` returns after atomic
   admission and readiness, before child work completes.
4. **Children are persistent by default.** After replying they remain idle and
   addressable for follow-up requests. A backend may hibernate an idle process
   without changing the logical actor or its retained provider thread.
   `oneShot` is an explicit policy modifier, not the default.
5. **Admission is atomic.** No handle is published and no child assignment is
   activated until every branch in an unfold has passed preflight and reached
   queue readiness. A pre-publication failure cleans up all unpublished
   branches.
6. **Recursive unfold is supported.** Static effect narrowing, runtime grants,
   a shared descendant ceiling, depth limits, and concurrency limits bound it.
7. **Roles are semantic Haskell constructors.** Inspection, coding, and
   integration policies are not bags of JSON booleans. Their constructors set
   coherent effect, worktree, native-tool, and prompt defaults; typed
   modifiers refine them.
8. **Effect lists may narrow across a fork.** Shared context is information,
   not permission. A child can inherit the exact model and Haskell context
   while compiling new work against a smaller effect list and running under
   attenuated Rust authority.
9. **Authoritative worktree evidence accompanies results.** The child authors
   only its domain value. Tidepool observes the bound worktree while settlement
   is suspended and attaches the observed changes and custody facts to the
   requester-side result.
10. **The root remains permanent.** Neither unfold nor fold introduces a
    Haskell operation meaning “the model round is over.” Ordinary response
    termination ends a model round; the supervisor owns actor termination.
11. **The visible swarm is named, not hash-shaped.** One hierarchical branch
    path supplies the actor label, Git branch, status path, and default
    request/watch prefix. Opaque IDs remain authoritative, but operators and
    models normally see names such as `context-unfold/runtime/mailbox`; stable
    numeric suffixes resolve collisions.
12. **`unfold` ends its hosted Haskell call.** The provider sees the complete
    tool-call payload before its Haskell units execute. The unfolding unit must
    therefore be the final executable unit in that call; parent-only watch or
    integration code belongs in the next call after handles return.

## One exact split boundary

The provider and Haskell boundaries must describe the same moment:

```text
shared provider prefix
  ... conversation ...
  assistant: tidepool_actor.haskell { unfold with every branch plan }
                                  |
               +------------------+------------------+
               |                  |                  |
          parent suffix       child domain       child tests
          tool result:        developer delta:   developer delta:
          typed handles       branch=domain      branch=tests

shared Haskell snapshot
  declarations + binding tip + rooted branch inputs
                                  |
               +------------------+------------------+
               |                  |                  |
          parent result       child scope        child scope
          shaped handles      sessionInput       sessionInput
```

The children include the assistant tool call but exclude its tool result. If a
child saw the result, it would learn parent-only handles and break the simple
cache prefix. If it forked before the call, it would not see the plans that the
operator intends it to share.

Because the provider item contains the complete tool input, there is no honest
sub-call transcript boundary after one Haskell statement. The workbench must
reject an `unfold` followed by another executable input unit, or an `unfold`
nested inside a larger action with a post-unfold suffix, before admission. It
may allow declarations and bindings that execute before the final unfolding
unit; those are precisely the shared vocabulary the children should inherit.
The parent receives handles as that tool result, then uses a new Haskell call
to register watches or continue its fold.

The child does not resume through a public process-style `Parent`/`Child`
discriminator. The parent-side Haskell effect returns the shaped handles. Each
child begins a normal request activation with a new actor identity, mailbox,
typed input, typed reply authority, effective role, and workspace binding.
There is no public cloned parent continuation for the model to branch on and
no possibility of accidentally falling through into the parent's suffix.

This exact active-tool-call behavior needs a provider-backed executable spike
before the public API lands. Codex already exposes thread forking and active
turn boundaries; Tidepool must prove specifically that the hosted Haskell call
item is retained byte-for-byte while the parent tool result is excluded.

## Human-readable lineage and Git names

The same typed, hierarchical name should orient the model everywhere. A
permanent root may host many campaigns, so a reusable campaign namespace is
not the root actor's identity. A top-level `unfold` adds a batch/group segment,
and each `child` adds its branch segment:

```text
context-unfold
├── discovery
│   ├── repository
│   ├── provider-spike
│   └── surface-critic
└── implementation
    ├── replies
    ├── worktree-evidence
    └── runtime
        └── leaves
            ├── binding-snapshot
            └── atomic-admission
```

The runtime projects that path consistently:

```text
actor label:  context-unfold/implementation/runtime/leaves/binding-snapshot
Git branch:   refs/heads/shoal/context-unfold/implementation/runtime/leaves/binding-snapshot
status label: context-unfold/implementation/runtime/leaves/binding-snapshot
cwd alias:    /tmp/tidepool-actor-workspace
authority:    ActorId(17)@1, WorktreeId(42)
```

The readable path is presentation and provenance, not identity or authority.
Exact-incarnation actor and worktree IDs remain in receipts and checks, but
they appear after the name rather than replacing it with a hash-shaped UI.
Internal storage directories and provider thread IDs may remain opaque.

Use distinct validated types for a root `CampaignLabel`, an unfold
`ForkGroupLabel`, a leaf `BranchLabel`, and the assembled `ActorPath`. A small
pure naming DSL keeps the hierarchy visible in authored Haskell:

```haskell
data CampaignPath
data ForkGroupPath

campaign :: CampaignLabel -> CampaignPath
batch    :: CampaignPath -> ForkGroupLabel -> ForkGroupPath
subgroup :: ForkGroupLabel -> ForkGroupPath
```

`batch campaignPath label` names a top-level group under a reusable campaign;
`subgroup label` names a group relative to the executing actor's current
lineage. The actor registry resolves a requested campaign idempotently within
one permanent-root incarnation, so later batches using the same `CampaignPath`
retain the same allocated prefix. Another root or a retained external name may
force one stable numeric suffix, recorded in every receipt. A model that wants
a semantically new campaign should choose a new campaign label; repeated calls
do not silently mint unrelated namespaces.

These path values are naming intent, not authority or resource handles.
Segments
are short lowercase kebab-case Git-safe names. Invalid or overlong segments
fail preflight with a typed error; do not silently hash or lossy-normalize a
model-authored name. Fork admission is the one allocator for `ActorPath`; it
reserves the path in the actor registry, asks the worktree manager to validate
and reserve the exact Git projection when a worktree is requested, and only
then publishes either. Do not retain the current independent agent-label and
branch-label sanitizers as competing naming authorities.

Naming is deterministic within atomic admission. Unique sibling labels keep
their spelling. If a batch intentionally repeats one label, number the whole
repeated set in applicative order (`parser-1`, `parser-2`, ...). If an earlier
retained actor or branch already occupies the full path, reserve the lowest
available stable suffix. Receipts record both the requested and allocated
path. Retention means a later batch never silently reuses an old branch merely
because its actor is idle or stopped.

Requests and watches may add their own final labels for status, for example
`.../binding-snapshot:review-2`, without creating Git refs. A follow-up request
to a persistent actor keeps the same actor path and branch; it does not mint a
fake new worker. `:status`, logs, tmux presentation, worktree receipts, and
provider metrics must all render this one lineage projection.

## The Haskell unfold algebra

`Unfold` is a free applicative description, parameterized by the parent's
available effects. It is deliberately not a monad:

```haskell
data Unfold parent a

instance Functor (Unfold parent)
instance Applicative (Unfold parent)

data Branch child input result
data Forked result

child
  :: forall result child input parent
   . (KnownEffects child, Subset child parent)
  => Branch child input result
  -> Unfold parent (Forked result)

attemptUnfold
  :: Member Forks parent
  => ForkGroupPath
  -> Unfold parent a
  -> Eff parent (Either UnfoldError a)

unfold
  :: Member Forks parent
  => ForkGroupPath
  -> Unfold parent a
  -> Eff parent a
```

The two entry points have the same atomic admission semantics. `unfold` is the
ordinary interactive spelling: a rejected admission becomes a typed
workbench/effect failure and installs no result binding. `attemptUnfold`
returns the same `UnfoldError` as data when authored Haskell wants to recover
or choose another plan. This mirrors `reply`/`attemptReply` and avoids making
the happy path spell `let Right workers = ...` merely to unpack infrastructure
admission.

The workbench must still accept ordinary refutable Haskell bindings. A
mismatch such as `let WatchReady result = state` rejects that input unit as a
typed `PatternMatchFailure`; it does not kill the actor, discard earlier
successful units, or promote an exhaustiveness warning into a compile error.
Effects completed by the rejected unit remain completed, so examples should
bind an effect result first and project it in a later unit when retry matters.

In the interactive workbench, successful `unfold` must occur in tail position
of the final input unit. The only continuation after its effect site is the
trusted wrapper that materializes the returned binding and closes the hosted
tool call. Reject a user-authored `do` suffix before admission using compiled
effect-site/provenance information, not a string search for the name
`unfold`.

The applicative tree gives the interpreter every independent leaf before any
assignment starts. It can therefore:

- use one provider/Haskell snapshot for the whole batch;
- validate result and input types;
- reserve child identities, reply cells, worktrees, grants, and budgets;
- launch branches concurrently;
- roll back an unpublished partial admission; and
- reconstruct the caller's heterogeneous record of typed handles without
  serializing its shape through Rust.

There is intentionally no `Monad Unfold`. A branch whose plan depends on an
earlier result belongs to a later unfold after the parent has observed and
incorporated that result. This makes the model's causal structure match the
Haskell structure.

Homogeneous cases remain ordinary traversal:

```haskell
workers <- unfold (subgroup "parser-leaves") $
  traverse
    (\(label, plan) -> child (coding @PatchReport label boundHead plan))
    plans
```

The implementation should keep arbitrary branch inputs and result assemblers
as Haskell-owned live values. Rust needs stable branch IDs, role/resource
requests, actor identities, and scheduling state; it must not turn the
applicative tree into a JSON workflow language.

### Role constructors

Constructors establish valid policy combinations and fix the child's effect
list. A directionally correct surface is:

```haskell
researching
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch ResearchEffects input result

coding
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch CodingEffects input result

scaffolding
  :: forall result input
   . BranchLabel
  -> WorktreeSeed
  -> input
  -> Branch ScaffoldEffects input result

integrating
  :: forall result input
   . BranchLabel
  -> IntegrationWorktree
  -> input
  -> Branch IntegrationEffects input result

narrowed
  :: forall child parent result input
   . (KnownEffects child, Subset child parent)
  => Effects child
  -> RolePolicy child
  -> BranchLabel
  -> input
  -> Branch child input result
```

`narrowed` is the expressive escape hatch for new effect combinations. The
common constructors are not presets implemented as special actor kinds; they
are well-typed library values built over it.

`coding` is the ordinary leaf role. `scaffolding` is the interior development
role: it may establish shared code, recursively unfold children, inspect and
control those descendants, and fold their Git work back into its own bound
worktree. Making that distinction in the row prevents every small coding leaf
from carrying an unused recursive supervisor surface while keeping recursive
development a first-class, named role rather than a privileged root trick.

`researching` is likewise a leaf by default: it receives an independently
mounted, named, inspection-only checkout, but its row contains neither `Forks`
nor `AgentControl`. A recurring need for a read-only research coordinator can
earn a named constructor later; initially it is expressed explicitly with
`narrowed @ResearchCoordinatorEffects` and a read-only role policy. This keeps
the common researcher honest without making recursive read-only work
impossible.

`WorktreeSeed` is source-placement intent consumed during atomic admission,
not access authority and not necessarily a preallocated handle. The role
constructor decides whether the resulting named checkout is inspection-only
or writable. Its smart constructors cover an explicitly existing worktree, a
fresh branch from the current project/ref, and a fresh branch from the
executing actor's clean bound HEAD. The last form is the normal recursive-
scaffold path. All siblings that name the same bound source resolve one source
OID at preflight and start from that exact OID.

```haskell
data WorktreeSeed

existingWorktree :: WorktreeHandle -> WorktreeSeed
projectHead      :: WorktreeSeed
fromRef          :: GitRef -> WorktreeSeed
boundHead        :: WorktreeSeed
snapshotDirty    :: WorktreeSeed -> WorktreeSeed
```

The branch's hierarchical name supplies the worktree label and Git ref; it is
not repeated in `WorktreeSeed`. Fresh sources require clean working state by
default. `snapshotDirty` is deliberately visible at the branch call site and
reuses the existing Worktree `allowDirtySnapshot` semantics.

A dirty bound source is not silently reinterpreted. The actor either uses
ordinary Git in its isolated checkout to make a legible scaffold commit, or
selects an explicit dirty-snapshot placement whose receipt names the synthetic
snapshot commit and captured dirty/untracked paths. The clean-commit route is
the default for recursive implementation because the same worktree later
serves as the integration target. This is one place where Git's familiar CLI
is preferable to another Haskell workflow algebra.

Use lenses for refinements that preserve constructor invariants:

```haskell
review =
  researching @ReviewReport "semantics" projectHead reviewPlan
    & maximumRounds .~ 8
```

Do not expose a lens that can turn `researching` into “inspection-only, except
builds are allowed.” Type-changing role selection and workspace ownership stay
behind smart constructors. Labels, limits, optional guidance, deadlines, and
similar orthogonal fields are good lens targets. Plain record-dot accessors
remain available; optics are convenience, not a second data model.

### `Forked` is an actor handle bundle

`Forked result` should have a hidden constructor but exported immutable record
selectors, so ordinary record-dot reads stay pleasant:

```haskell
data Forked result = Forked
  { actor     :: AgentRef
  , response  :: Response result
  , launch    :: BranchReceipt
  , forkGroup :: ForkGroupId
  }
```

The module exports the selectors but not `Forked`'s constructor. Optional
`actorL`, `responseL`, and similar read-only optics may compose with lens-heavy
code, but the primary observation `worker.actor` must not require `view`.

The `Response result` parameter names the value the child is authorized to
reply with. Requester-side observation also carries trusted execution and
worktree evidence, described below. Keeping `AgentRef` means the parent may
send follow-up requests after the first result; keeping `Response` separate
means readiness composes with every other reply through `Await` and `Watch`.

The common fold spellings should not require peeling those accessors by hand:

```haskell
data Settlement result
  = ReplyAvailable (ResponseResult result)
  | ReplyUnavailable ResponseFailure

awaitFork
  :: Forked result
  -> Await (ResponseResult result)

awaitSettledFork
  :: Forked result
  -> Await (Settlement result)
```

`awaitFork` is strict: an unavailable child makes the containing watch
unavailable. `awaitSettledFork` treats both success and failure as readiness,
so an applicative fold can collect every sibling without scheduler flags or
exception-shaped short circuiting. Both preserve plan order regardless of
completion order.

### Labeled follow-up requests

Persistent actors make request identity part of the everyday surface. Keep the
common spelling small and typed:

```haskell
request
  :: forall result input effs
   . Member Replies effs
  => AgentRef
  -> RequestLabel
  -> input
  -> Eff effs (Response result)

requestWith
  :: forall result input effs
   . Member Replies effs
  => AgentRef
  -> RequestOptions input
  -> Eff effs (Response result)
```

`RequestOptions` adds optional concise guidance and a deadline to its typed
input and label. It is a Haskell record with invariant-preserving optics, not a
JSON options object. For an inherited-context actor, the ordinary activation
identifies the labeled request and mounts `sessionInput`; it does not paste a
full generated task. A genuinely fresh actor still needs a self-contained
initial brief.

Watch registration likewise takes a `WatchLabel`:

```haskell
watch
  :: Member Watches effs
  => WatchLabel
  -> Await result
  -> Eff effs (Watch result)
```

Status projects request and watch names beneath the actor path while retaining
their exact IDs. Labels never participate in settlement or authorization.

## Typed response results and worktree evidence

The target-facing contract remains minimal and general:

```haskell
sessionReply :: Reply result
respond      :: result -> Eff childEffects Void
```

In particular, a model never fabricates its own worktree receipt. While an
accepted `respond` is suspended, the trusted settlement path observes the
actor's bound worktree, constructs the requester result, fills the Haskell
result cell, publishes response readiness, reevaluates dependent watches, and
then performs the irreversible terminal transfer.

Requester-side observation should expose an envelope:

```haskell
data ResponseResult result = ResponseResult
  { value       :: result
  , execution   :: ExecutionReceipt
  , worktree    :: WorktreeResult
  }

data ResponseState result
  = ResponsePending
  | ResponseReady (ResponseResult result)
  | ResponseUnavailable ResponseFailure

pollResponse
  :: Member Replies effs
  => Response result
  -> Eff effs (ResponseState result)

awaitResponse
  :: Response result
  -> Await (ResponseResult result)

awaitValue
  :: Response result
  -> Await result
awaitValue response = (.value) <$> awaitResponse response
```

`awaitValue` is the intentional convenience for callers that do not need the
evidence. The full result must remain repeatably pollable from the same
`Response`; discarding the envelope in one Haskell projection does not consume
or erase it.

The worktree side is a sum because some actors have no bound worktree and an
observation can fail without destroying a successfully authored domain value:

```haskell
data WorktreeResult
  = NoBoundWorktree
  | WorktreeObserved WorktreeEvidence
  | WorktreeObservationFailed WorktreeError

data WorktreeEvidence = WorktreeEvidence
  { receipt     :: WorktreeReceipt
  , started     :: WorktreeSnapshot
  , submitted   :: WorktreeSnapshot
  , changes     :: WorktreeChanges
  , assessment  :: SubmissionAssessment
  }

data WorktreeChanges = WorktreeChanges
  { committed :: [PathChange]
  , staged    :: [PathChange]
  , unstaged  :: [PathChange]
  , untracked :: [Text]
  , ignoredExcluded :: Int
  }
```

`PathChange` should use typed Git change kinds, including rename/copy source
paths and an honest `OtherChange` case. `WorktreeSnapshot` retains typed HEAD,
working state, and in-progress operation. The start observation is taken when
that request becomes active; the submitted observation is taken while its
reply is suspended. That makes repeated requests to one persistent coding
actor report per-request deltas rather than continually diffing against the
worktree's original creation commit.

`SubmissionAssessment` classifies facts such as clean committed candidate,
unchanged head, dirty candidate, or in-progress operation. It does not decide
whether a project should accept the result. A dirty result still returns the
child's typed value and exact evidence; Haskell policy decides what to do.

The existing `SubmissionObservation` already owns base/head, staged,
unstaged, untracked, ignored-excluded, and operation facts. Extend that one
Worktree-owned observation path to include the committed path delta and
request-start snapshot. Do not add a second Git scanner in the actor or fork
interpreter.

This envelope should eventually apply to every request handled by a bound
interactive agent, not only the first request created by `unfold`. Persistent
actors make per-request evidence the stable abstraction.

### Git remains the integration language

Worktree evidence should make Git legible to the model, not hide it behind an
opaque workflow API. It exposes the exact repository, worktree, branch, base,
submitted commit, dirty state, changed paths, and dirty-snapshot provenance in
ordinary Git terms. Models already know the mature Git CLI and should use it
for rebases, selective cherry-picks, conflict resolution, history editing, and
unusual topology.

One conservative typed convenience is still worthwhile for the common fold:

```haskell
data MergeRequest = MergeRequest
  { sourceHead     :: GitOid
  , sourceBranch   :: Maybe BranchName
  , targetWorktree :: WorktreeId
  , message        :: Text
  }

tryMerge
  :: Member WorktreeIntegration effs
  => MergeRequest
  -> Eff effs (Either WorktreeError MergeOutcome)
```

`tryMerge` attempts the normal Git merge of the exact observed source into the
named target. Its result distinguishes already-contained, fast-forward, merge
commit, and `ManualGitRequired` with the exact source/target OIDs, reason, and
conflicted paths. A failed attempt aborts and proves that the target returned
to its starting HEAD and operation state.

Reply settlement never invokes `tryMerge` implicitly. The parent's typed fold
first sees the domain result and repository evidence, makes its acceptance or
review decision, and then calls the default merge operation explicitly. A
project library may package that sequence as an acceptance algebra, but the
runtime does not equate “the child replied” with “merge its branch.”

`ManualGitRequired` is the handoff boundary, not an invitation to add another
effect verb. Conflicts, ambiguous dirty-snapshot ancestry, or unsupported
topology move the work to a suitably bound integration actor using ordinary
Git tools. Tidepool observes the resulting repository state through the same
Worktree path afterward.

This evolves the existing one merge/abort primitive. It does not add generic
`rebase`, `cherryPick`, `transplant`, or conflict-resolution effects. Named
source and target fields remove `mergeBranchInto`'s directional ambiguity
without trying to replicate Git in Haskell.

## Worked interaction: a recursive three-batch campaign

This walkthrough is a pressure test of the target surface, not a transcript
golden. The Haskell is intentionally ordinary workbench Haskell. In
particular, `[fmt|...|]`, arbitrary expressions in its holes, `T.intercalate`,
record-dot access, typed declarations, and persistent bindings already work.
Only the actor/fork vocabulary in the example is prospective.

The example is a root implementing this feature itself. The root already has
the full operator conversation and repository guidance, so its first action is
to establish a small durable vocabulary—not to manufacture seven standalone
prompts that retell that context.

### Establish shared domain vocabulary once

The root declares values that should cross the first fork boundary:

```haskell
data CheckCost = Cheap | Broad
  deriving (Show, Eq)

newtype ShellCommand = ShellCommand
  { commandText :: Text
  }
  deriving (Show, Eq)

data Check = Check
  { checkName    :: Text
  , checkCommand :: ShellCommand
  , checkProves  :: Text
  , checkCost    :: CheckCost
  }
  deriving (Show, Eq)

data SlicePlan = SlicePlan
  { sliceSlug      :: Text
  , sliceObjective :: Text
  , sliceOwns      :: [Text]
  , sliceChecks    :: [Check]
  , sliceNotes     :: [Text]
  }
  deriving (Show, Eq)

data RepoSurvey = RepoSurvey
  { relevantOwners :: [Text]
  , invariants     :: [Text]
  , risks          :: [Text]
  }
  deriving (Show, Eq)

data ForkSpike = ForkSpike
  { callIncluded        :: Bool
  , resultExcluded      :: Bool
  , cachedInputTokens   :: Maybe Int
  , uncachedInputTokens :: Maybe Int
  , spikeNotes          :: [Text]
  }
  deriving (Show, Eq)

data SurfaceReview = SurfaceReview
  { keep       :: [Text]
  , change     :: [Text]
  , openRisks  :: [Text]
  }
  deriving (Show, Eq)

data PatchReport = PatchReport
  { patchSummary :: Text
  , checksRun    :: [Text]
  , caveats      :: [Text]
  }
  deriving (Show, Eq)

data SubsystemReport = SubsystemReport
  { subsystem       :: Text
  , integratedWork  :: [Text]
  , deferredWork    :: [Text]
  , subsystemChecks :: [Text]
  }
  deriving (Show, Eq)

data RevisionRequest = RevisionRequest
  { revisionFor :: Text
  , revisionWhy :: [Text]
  }
  deriving (Show, Eq)

data Discovery f = Discovery
  { repository    :: f RepoSurvey
  , providerSpike :: f ForkSpike
  , surfaceCritic :: f SurfaceReview
  }
```

`RevisionRequest` is declared early on purpose: a later request to an existing
fork may use vocabulary from the fork's shared prefix, but does not
retroactively import declarations the root invents after that actor diverges.

The root then defines concise semantic renderers. Rich records stay the source
of truth; rendered `Text` is only the seam where a model needs prose:

```haskell
bulletLines :: [Text] -> Text
bulletLines xs =
  T.intercalate "\n" [[fmt|  - {x}|] | x <- xs]

renderCheck :: Check -> Text
renderCheck c = [fmt|
  - {c.checkName}: `{c.checkCommand.commandText}`
    proves: {c.checkProves}; cost: {c.checkCost}
|]

renderChecks :: [Check] -> Text
renderChecks = T.intercalate "\n" . map renderCheck

cheapCheckChain :: [Check] -> Text
cheapCheckChain checks =
  case [c.checkCommand.commandText | c <- checks, c.checkCost == Cheap] of
    [] -> "No cheap check chain was authored."
    commands -> T.intercalate " && " commands

renderSlice :: SlicePlan -> Text
renderSlice p = [fmt|
  Slice: {p.sliceSlug}
  Objective: {p.sliceObjective}

  Owned paths:
{bulletLines p.sliceOwns}

  Checks:
{renderChecks p.sliceChecks}

  If their stated preconditions still hold, the cheap checks can be run as:
  {cheapCheckChain p.sliceChecks}

  Notes:
{bulletLines p.sliceNotes}
|]
```

`ShellCommand` distinguishes authored commands from arbitrary explanatory
text. `cheapCheckChain` renders a suggestion into a prompt; it does not ask the
Haskell effect interpreter to become a shell or silently execute the string.
The child uses its familiar native terminal, may split the chain while
diagnosing a failure, and reports what actually ran. Renderers should be
compact: relying on the fallback `Show` rendering of a large campaign value is
convenient for inspection but a poor default prompt grammar.

### Batch 1: discover in parallel, then learn

The root binds three typed inputs. Their details and all the prior conversation
are already in the Haskell/provider prefix:

```haskell
repositoryQuestion :: Text
repositoryQuestion = [fmt|
  Locate the existing owners for actor forking, binding snapshots, provider
  lineage, worktree observation, and prompt assembly. Return invariants and
  concrete file paths; do not build or edit.
|]

spikePlan :: SlicePlan
spikePlan = SlicePlan
  { sliceSlug = "provider-active-call-spike"
  , sliceObjective = "Prove the active hosted Haskell call is in each child prefix and its result is not."
  , sliceOwns = ["tidepool-agent", "tidepool-actor"]
  , sliceChecks = []
  , sliceNotes = ["Keep this a canary; do not build the public DSL around an assumed boundary."]
  }

criticQuestion :: Text
criticQuestion = [fmt|
  Adversarially review the proposed model-facing unfold/fold surface from the
  shared conversation. Concentrate on what would be awkward after three
  recursive batches. Do not edit or run build tools.
|]
```

One applicative call is one exact context boundary and one atomic admission:

```haskell
contextUnfold = campaign "context-unfold"

discovery <- unfold (batch contextUnfold "discovery") $
  Discovery
    <$> child
          (researching @RepoSurvey "repository" projectHead repositoryQuestion)
    <*> child
          (coding @ForkSpike "provider-spike" projectHead spikePlan)
    <*> child
          (researching @SurfaceReview "surface-critic" projectHead criticQuestion)
```

Every child sees that entire call, including all three plans. The selected
child additionally gets only a small role delta and its live typed input. For
the critic, the new provider suffix is conceptually:

```text
Fork branch: context-unfold/discovery/surface-critic
Continue branch surface-critic from the shared unfold call.
Role: inspection-only researcher; build and artifact-producing commands are unavailable.
Mounted input: sessionInput :: Text
```

The parent immediately has `discovery :: Discovery Forked`; no answer is
pretended to be ready. It creates one durable, collect-all fold:

```haskell
discoveryWatch <- watch "discovery-results" $
  Discovery
    <$> awaitSettledFork discovery.repository
    <*> awaitSettledFork discovery.providerSpike
    <*> awaitSettledFork discovery.surfaceCritic
```

The root then simply ends its model response. There is no `yield`, `park`, or
`complete`. When the watch changes, the runtime appends a durable sequenced
event and reactivates the root. A later Haskell unit observes it:

```haskell
discoveryState <- pollWatch discoveryWatch
let WatchReady discoveryFold = discoveryState
```

The refutable second line is legitimate interactive Haskell. If the model was
reactivated for another reason and the value is still pending, only that input
unit receives typed `PatternMatchFailure`; `discoveryState`, the watch, and all
earlier effects remain usable. `-Werror` must not reject the useful pattern
before it runs. An authored harness that needs total recovery instead uses an
exhaustive `case`.

The root now has new evidence in both its model context and live Haskell
bindings. It can define a new `ImplementationPlan` type, helper functions, and
values derived from `discoveryFold`. Those declarations did not need to be
guessed before the research completed.

### Batch 2: a heterogeneous implementation layer

Suppose the fold validates the provider boundary and reveals three separable
implementation branches. The root introduces the next layer's vocabulary:

```haskell
data ImplementationPlan = ImplementationPlan
  { implementationSlice :: SlicePlan
  , dependsOn            :: [Text]
  , reviewFocus          :: [Text]
  }
  deriving (Show, Eq)

data Implementation f = Implementation
  { replyRuntime     :: f PatchReport
  , worktreeEvidence :: f PatchReport
  , forkRuntime      :: f SubsystemReport
  , finalSemantics   :: f SurfaceReview
  }

implementationBrief :: ImplementationPlan -> Text
implementationBrief p = [fmt|
{renderSlice p.implementationSlice}

  Depends on:
{bulletLines p.dependsOn}

  Review especially:
{bulletLines p.reviewFocus}
|]
```

It binds four plan values using the first fold's findings, then launches:

```haskell
implementation <- unfold (batch contextUnfold "implementation") $
  Implementation
    <$> child
          (coding @PatchReport "replies" projectHead repliesPlan)
    <*> child
          (coding @PatchReport "worktree-evidence" projectHead evidencePlan)
    <*> child
          (scaffolding @SubsystemReport "runtime" projectHead runtimePlan
            & descendants .~ sharedDescendants 4
            & maximumDepth .~ 2)
    <*> child
          (researching @SurfaceReview "semantics" projectHead semanticsPlan)
```

After that call returns the handles, the root makes a second call:

```haskell
implementationWatch <- watch "implementation-results" $
  Implementation
    <$> awaitSettledFork implementation.replyRuntime
    <*> awaitSettledFork implementation.worktreeEvidence
    <*> awaitSettledFork implementation.forkRuntime
    <*> awaitSettledFork implementation.finalSemantics
```

This batch's descendants inherit the newer declaration/binding snapshot: the
discovery results, `ImplementationPlan`, `implementationBrief`, and all earlier
helpers are present without serialization. The older discovery actors do not
magically gain those later declarations.

`coding` allocates an isolated leaf worktree during admission. `scaffolding`
allocates the same kind of writable worktree but also receives the narrowed
effects and descendant budget needed to act as an interior node. Branches are
named, for example:

```text
shoal/context-unfold/implementation/replies
shoal/context-unfold/implementation/worktree-evidence
shoal/context-unfold/implementation/runtime
```

### Inside the runtime child: scaffold, recursively unfold, then fold

The runtime child starts with the exact root prefix through Batch 2 and
`sessionInput :: ImplementationPlan`. It can render the selected plan without
being handed a duplicate standalone prompt:

```haskell
implementationBrief sessionInput
```

After inspecting the repository it decides that two leaves can proceed after
one shared scaffold. It may define child-local types and helpers normally:

```haskell
data RuntimeLeafPlan = RuntimeLeafPlan
  { runtimeArea   :: Text
  , runtimeTask   :: Text
  , runtimePaths  :: [Text]
  , runtimeChecks :: [Check]
  }
  deriving (Show, Eq)

data RuntimeLeafReport = RuntimeLeafReport
  { runtimeAreaDone :: Text
  , runtimeSummary  :: Text
  , runtimeEvidence :: [Text]
  }
  deriving (Show, Eq)

data RuntimeLeaves f = RuntimeLeaves
  { bindingSnapshot :: f RuntimeLeafReport
  , atomicAdmission :: f RuntimeLeafReport
  }

renderRuntimeLeaf :: RuntimeLeafPlan -> Text
renderRuntimeLeaf p = [fmt|
  Runtime leaf: {p.runtimeArea}
  Task: {p.runtimeTask}
  Paths:
{bulletLines p.runtimePaths}
  Checks:
{renderChecks p.runtimeChecks}
  Suggested cheap chain: {cheapCheckChain p.runtimeChecks}
|]
```

These types exist only in the runtime actor's lineage. That is fine: its
grandchildren inherit them and return `RuntimeLeafReport` to it, while the
runtime actor eventually folds them into the `SubsystemReport` type that was
fixed at its reply boundary with the root.

The child authors the shared source scaffold in its bound checkout. Because
its descendants must share one immutable seed and the same checkout will be
their integration target, the normal route is familiar Git:

```text
git status --short
git add <the reviewed scaffold paths>
git commit -m 'scaffold: add fork runtime types'
```

It then recursively unfolds from that clean bound HEAD:

```haskell
runtimeLeaves <- unfold (subgroup "leaves") $
  RuntimeLeaves
    <$> child
          (coding @RuntimeLeafReport "binding-snapshot" boundHead bindingPlan)
    <*> child
          (coding @RuntimeLeafReport "atomic-admission" boundHead admissionPlan)
```

In the runtime actor's next Haskell call:

```haskell
runtimeWatch <- watch "runtime-leaves" $
  RuntimeLeaves
    <$> awaitSettledFork runtimeLeaves.bindingSnapshot
    <*> awaitSettledFork runtimeLeaves.atomicAdmission
```

Both grandchildren start at the exact scaffold commit. Their names extend the
actual execution lineage:

```text
shoal/context-unfold/implementation/runtime/leaves/binding-snapshot
shoal/context-unfold/implementation/runtime/leaves/atomic-admission
```

The runtime actor ends this model round without settling `sessionReply`. Its original
request remains pending, the watch is durable, and the actor becomes idle. The
runtime should warn only when a pending request has no plausible durable wake
source; it must not require a fake lifecycle call or turn an omitted reply into
failure. Cancellation and deadlines remain the cleanup tools for genuinely
forgotten work.

When the grandchildren settle, one later activation observes the whole typed
layer. The runtime actor reviews each `Settlement`, may send a concise
`RevisionRequest` to a still-persistent grandchild, and integrates accepted
heads into its own branch with `tryMerge`. Straightforward cases return typed
Git outcomes. On `ManualGitRequired`, the same actor uses ordinary `git
status`, `git log`, `git diff`, merge/rebase/cherry-pick, and conflict tools in
the named checkout rather than asking Tidepool for a second-rate Git DSL.

After its native checks pass, it replies:

```haskell
respond SubsystemReport
  { subsystem = "fork-runtime"
  , integratedWork = ["binding snapshots", "atomic applicative admission"]
  , deferredWork = []
  , subsystemChecks = ["focused runtime tests", "git diff --check"]
  }
```

Settlement observes and attaches the runtime actor's authoritative worktree
delta before waking the root. Grandchild evidence remains visible through the
runtime actor's retained handles and actor inspection, but is not flattened
into an enormous untyped root transcript. The domain fold decides what summary
crosses each edge; the runtime always preserves immediate response/worktree
receipts.

### Batch 3: revise with retained actors, judge with a fresh one

Back at the root, `implementationWatch` yields an `Implementation Settlement`
in plan order. The root can:

- send `RevisionRequest` to an existing child when retained context is an
  advantage;
- start a new context fork from the now-richer root for another dependent
  implementation batch; or
- use `startAgent` for a genuinely fresh reviewer when inherited assumptions
  would bias the judgment.

That distinction should remain visible in code. A follow-up resembles:

```haskell
replyRevision <-
  request @PatchReport
    implementation.replyRuntime.actor
    "revision-2"
    RevisionRequest
      { revisionFor = "reply publication ordering"
      , revisionWhy = ["review found readiness could precede worktree observation"]
      }
```

The actor and Git branch keep their original readable path; the request label
becomes `.../replies:revision-2`. The input type works because
`RevisionRequest` existed before Batch 2 forked.

By contrast, the final independent review is a fresh actor with no inherited
model suffix, so it receives a fuller `[fmt|...|]` brief containing the exact
submitted heads, checks, and decision rubric. Its typed response can still be
joined through the same `Response`/`Await`/`Watch` machinery. Context fork is
for faithful parallel continuation; fresh launch is for independent cognition.

The root integrates accepted named branches one at a time, observes the target
after every merge, and runs the broad boundary gate once. It does not merge
model contexts, trust prose in place of receipts, or tear down useful idle
actors merely because the first campaign completed.

### What the walkthrough fixes as contract

The interaction above makes several otherwise easy-to-miss rules explicit:

1. **Declarations flow down one lineage, never sideways or backward.** A
   declaration made before an unfold is available to every descendant of that
   snapshot. A declaration made afterward is available only to later forks
   from that actor. A child's local declaration reaches its descendants but
   not its parent or siblings.
2. **Every cross-edge result type is fixed before that edge forks.** A child is
   free to invent rich local types and use them in deeper unfolds, but it folds
   them into the already-known type of its own `Reply`. V1 does not attempt
   declaration grafting between diverged sessions.
3. **One applicative call is one independent layer.** A result-dependent branch
   belongs in a later batch after the prior fold. There is no `Monad Unfold` and
   no hidden speculative tree.
4. **Interior actors own their immediate fold.** Each scaffolding actor reviews
   and integrates its children, then reports upward. The root need not retain a
   materialized global result tree, although typed inspection can traverse the
   actor lineage for diagnosis.
5. **A clean Git commit is the preferred recursive seed.** Explicit dirty
   snapshots remain supported and receipted, but they are not the invisible
   default for a worktree that will also integrate its children.
6. **Ending a model round with a pending reply is normal orchestration.** Watches and
   mailbox events wake the actor later. There is no turn-ending Haskell
   operation and no automatic `ReplyOmitted` failure.
7. **Prompt helpers are normal persistent Haskell.** Use `[fmt|...|]` to render
   typed plans, observations, check lists, metrics (including format specs such
   as `{ratio:.1%}`), and concise follow-ups. Do not serialize the orchestration
   tree to JSON or repeat the shared context in every child prompt.
8. **Readable names and opaque authority coexist.** The model navigates
   `campaign/group/worker` paths and ordinary Git refs; exact IDs in the same
   receipts prevent ABA and authorization mistakes.
9. **Cache inheritance follows the actor tree.** A grandchild reuses its
   immediate parent's accumulated prefix, including the parent's local
   declarations and scaffold discussion. A later root batch reuses the root's
   prefix and does not somehow absorb descendant-only context. Metrics report
   each fork edge and aggregate a group without pretending contexts merged.

## Effect vocabulary for role narrowing

The current interactive row

```haskell
'[Replies, Watches, Actor, Worktree]
```

is too coarse for meaningful child roles. The low-level `Actor`, `ActorLocal`,
and `AgentSession` effects should remain trusted substrate rather than appear
in the normal Shoal facade. Split the model-facing capabilities along actual
permission decisions while keeping their Rust mechanisms with the existing
owners.

| Effect | Purpose | Typical holders |
|---|---|---|
| `Replies` | Request, typed reply settlement, repeatable polling, cancellation/deadline extensions | Every interactive actor |
| `Watches` | Durable applicative readiness subscriptions | Every orchestrating actor |
| `Forks` | Exact-context `unfold`, recursive fork admission, fork observation | Roles allowed to context-fork |
| `ActorContext` | Read-only self, activation, role, lineage, budget, prompt-profile, and bound-worktree facts | Every interactive actor |
| `AgentLaunch` | Start a genuinely fresh-context agent | Supervisors allowed fresh judgment |
| `AgentInspection` | Observe/list authorized agents without controlling them | Supervisors and reviewers as granted |
| `AgentControl` | Stop/cancel authorized descendants and obtain typed outcomes | Owning supervisors |
| `BoundWorktree` | Observe only the executing actor's bound worktree | Coding and worktree-backed research roles |
| `WorktreeRegistry` | Look up and filtered-list authorized managed worktrees | Roots and workspace coordinators |
| `WorktreeAllocation` | Create a managed worktree from an explicit source | Roles allowed to allocate custody |
| `WorktreeIntegration` | Conservative default merge/abort with typed manual-Git handoff | Roots and explicit integrators |

This is granularity with a user-visible purpose, not one effect per Rust
method. For example, `AgentInspection` must be distinct from `AgentControl`
because a reviewer may be allowed to see lifecycle state without being able to
stop anything. `BoundWorktree` must be distinct from `WorktreeRegistry`
because a worker should inspect its own candidate without enumerating or
allocating arbitrary worktrees. Allocation is separate from registry
inspection because a diagnostic console or reviewer may need the latter
without authority to create retained resources.

`Forks` may internally reserve exactly the worktree placements declared by
its child branches without granting the caller general `WorktreeAllocation`.
That is part of atomic child admission under the descendant grant. A scaffold
actor can therefore create isolated child branches from its bound HEAD but cannot
allocate unrelated arbitrary worktrees or enumerate the global registry.

Possible initial rows are:

```haskell
type CoreEffects =
  '[Replies, Watches, ActorContext]

type ResearchEffects =
  '[Replies, Watches, ActorContext, BoundWorktree]

type ResearchCoordinatorEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl, BoundWorktree
   ]

type CodingEffects =
  '[Replies, Watches, ActorContext, BoundWorktree]

type ScaffoldEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentInspection, AgentControl
   , BoundWorktree, WorktreeIntegration
   ]

type IntegrationEffects =
  '[ Replies, Watches, ActorContext
   , AgentInspection, BoundWorktree, WorktreeIntegration
   ]

type RootEffects =
  '[ Replies, Watches, Forks, ActorContext
   , AgentLaunch, AgentInspection, AgentControl
   , BoundWorktree, WorktreeRegistry, WorktreeAllocation
   , WorktreeIntegration
   ]
```

Research and coding still differ in native-process authority as well as in
their rows. Effects describe the typed language available to a model; they are
not a dishonest proxy for shell or mount permissions. A read-only coordinator
may explicitly select `ResearchCoordinatorEffects`; a coding branch that
really should retain recursive `Forks` selects `ScaffoldEffects` or a narrower
project-defined subset deliberately. The common leaf constructors do not carry
unused supervisor operations that runtime policy merely promises to reject.

### One typed residual-row witness

Dynamic row narrowing needs one honest bridge from the Haskell type list to
the child facade and resident interpreter. Do not pass `[Text]` effect names or
maintain a second hand-written Rust profile table. Generate abstract witnesses
with the effect declarations themselves:

```haskell
data EffectWitness effect
data Effects effects where
  ENil  :: Effects '[]
  ECons :: EffectWitness effect -> Effects effects -> Effects (effect ': effects)

class KnownEffect effect where
  effectWitness :: EffectWitness effect

class KnownEffects effects where
  knownEffects :: Effects effects
```

Common role constructors retain their exact `Effects ChildEffects` value;
`narrowed` may accept `knownEffects @child`. Constructors stay hidden, so a
model cannot claim an unregistered effect by inventing its spelling. The
witness preserves row order for compiling the exact `ActorEffects` alias and
installing the compatible residual handler surface. Stable nominal effect keys
come from the protocol owner, not rendered Haskell type strings.

This witness is descriptive, not authority. `Subset child parent` proves the
static attenuation edge; the parent/supervisor policy validates the witnessed
keys; principals and grants still authorize every concrete request. A purely
Haskell-local effect is welcome, but it must be eliminated before the actor's
residual boundary and therefore does not appear in `Effects`.

The list is extensible. A custom branch may choose any statically valid subset
of the parent's permitted child effects. Adding a new effect later should
require adding its handler-owned fork policy and, when relevant, its typed
role projection—not editing a global closed “capabilities” enum.

The forked facade compiles every new child input against its narrowed
`ActorEffects`. Shared declarations whose functions are `Member`-polymorphic
remain usable when the smaller list satisfies their constraints. A shared
value whose type is a concrete `Eff ParentEffects a` remains an immutable value
but does not typecheck as the child's `Eff ChildEffects a`; it is not silently
coerced or reinterpreted. Runtime checks remain necessary for copied opaque
handles and for every concrete resource operation.

Do not add generic shell, filesystem, build, test-runner, or prompt effects.
Those are native agent tools or launch metadata. Do not add a second
`Submission` effect: reply settlement composes existing Replies and
Worktree-owned observation. Do not add a generic event-stream effect until a
consumer needs more than targeted `Watch` and typed inspection.

## Static effects, runtime grants, and native tools

Role conversion has three coordinated layers. They primarily prevent
accidental interference, misleading affordances, and wasted compute inside a
cooperating development swarm; V1 is not trying to turn locally forked models
into mutually hostile security tenants.

1. **Static effect narrowing.** The child facade defines its exact
   `ActorEffects`; GHC rejects new computations requiring an absent effect.
2. **Runtime authority attenuation.** Opaque handles, actor principals,
   worktree bindings, and handler-owned grants decide which concrete resources
   an admitted operation may touch.
3. **Native-tool policy.** The process launcher and tool broker enforce
   workspace write access, process classes, builds, network, credentials, and
   resource budgets that do not live in the Haskell effect machine.

All three are projected from one accepted `EffectiveRole` record. The child
developer message renders that record; prose is not a fourth authority layer.
`:status` and `ActorContext` expose the same typed facts.

Each runtime mechanism declares how it crosses a fork:

| State or authority | Fork policy |
|---|---|
| Provider transcript | Share the exact frozen prefix |
| Haskell declarations and ordinary values | Share an immutable snapshot and root leases |
| Actor identity, mailbox, activation | Rebind to a new child incarnation |
| Child effect list | Narrow through a proved `Subset` |
| Parent `Reply`, `Response`, `Watch`, and continuation handles | Withhold; copied use fails `InvalidAfterFork` |
| Bound worktree | Explicitly create, inspect-bind, edit-bind, or omit |
| Worktree integration and supervisor authority | Withhold unless explicitly granted by the parent and supervisor |
| Native command/build/network policy | Attenuate by intersection |
| Descendant budget | Share a subtree ceiling and optionally reserve a child cap |
| Provider model/configuration | Preserve by default; an explicit override reports likely cache consequences |

The effective policy is monotone:

```text
effective child policy
  = requested branch policy
  intersect parent inheritable policy
  intersect supervisor policy
```

A branch can narrow itself to a researcher. It cannot regain build, network,
integration, secret, or fork authority its parent did not have. A copied
memory that says “I can merge” conveys no merge grant.

### Named native roles

The initial native policies should be coherent, inspectable values:

- `InspectionOnly`: read files and repository facts; no source or artifact
  writes, builds, tests, formatters, generators, package installation, or
  unrestricted subprocess escape.
- `Coding`: writable assigned worktree, ordinary editing, normal Git commands
  scoped to that checkout, and an explicit build/process budget. Source and
  sibling working files remain read-only.
- `Integration`: writable integration target plus the narrow typed integration
  effect and normal Git tools. It is not an ambient right to mutate every
  worktree.
- `Inherited`: exact parent native policy subject to supervisor attenuation;
  useful when a fork is a parallel copy of the same role.

If a backend cannot enforce a requested native policy, admission must fail
with a typed policy error or report an explicitly weaker guarantee. Tidepool
must never label a prompt-only instruction “inspection-only enforcement.”

Network and process credentials remain optional policy dimensions, but they
should not dominate the first dev-swarm surface. Context forks operate inside
one information-trust boundary and normally inherit the project's development
environment. `offline` and `withoutCredentials` are useful explicit
attenuations; stricter deployment profiles can require them. Context already
present in the shared transcript cannot be made secret from a child, so no
role message should imply otherwise.

### Recursive budgets

Do not clone a numeric allowance into every child; that multiplies authority
exponentially. Use a hybrid:

- one supervisor-owned hard ceiling for the whole descendant subtree;
- a decreasing maximum depth;
- one active-child/concurrency ceiling; and
- an optional per-branch reservation or cap for deterministic planning.

Both conditions are necessary for recursive unfold: `Forks` must be in the
child effect list, and its effective runtime fork allowance must be nonzero.
Removing either gives a clear static or typed dynamic denial.

## Prompt and context preloading

Prompt construction should become an explicit, versioned projection owned by
the agent launch path. It is not a Haskell effect and not an arbitrary text
field in every branch.

Use distinct prompt layers:

| Layer | Contents | Fork behavior |
|---|---|---|
| Provider system instructions | Provider/tool safety contract | Preserved exactly |
| Tidepool root or worker Developer prelude | Stable lifecycle, Haskell, reply/watch, custody, and delegation guidance | Shared and cacheable |
| Repository guidance | Applicable contributor instructions and stable project context | Shared when already present |
| Effective-role Developer delta | Branch selector, narrowed effects, workspace/tool policy, budgets | Appended per child |
| Typed activation | `sessionInput`, `sessionReply`, request identity | Mounted per child, not summarized as JSON |

Keep stable concepts early and volatile state typed. Current HEAD, remaining
budget, queue state, and other changing facts belong in `ActorContext`, typed
receipts, and `:status`; copying them into a supposedly stable prelude would
make the prompt stale and reduce cache reuse.

### Root prelude

The root should start with a concise stable developer prelude containing:

- the permanent-application lifecycle contract;
- the GHCi-style Haskell workbench mental model;
- `Replies`, `Watches`, and the unfold/fold pattern;
- the distinction among `request` (same actor), `unfold` (exact inherited
  context), and `startAgent` (fresh independent context);
- the rule that typed handles and observations, not prose, carry authority;
- worktree custody and automatic response evidence;
- current repository guidance and the stable actor-relative workspace path;
- a calibrated delegation rule: fork bounded independent work when inherited
  understanding matters, start fresh actors for independent judgment, and
  keep integration or conversation-dependent synthesis in the root.

The current absolute instruction to start agents instead of implementing is
too strong and should be removed. Tool syntax details belong in the hosted
tool description; the root prelude should teach judgment and lifecycle rather
than duplicate a manual.

Because this prelude is installed before useful work, it becomes part of the
shared provider-cache prefix for every descendant.

### Fork role delta

After the shared tool-call prefix, the child receives one generated Developer
message derived from its accepted role receipt. Conceptually:

```text
Fork branch: context-unfold/discovery/semantics
Continue branch semantics from the shared unfold call.
The unfold result binding is parent-only and is not present in your Haskell scope.
Role: inspection-only researcher.
Available Haskell effects: ResearchEffects.
Workspace: inspect-only, branch shoal/context-unfold/discovery/semantics,
           /tmp/tidepool-actor-workspace, WorktreeId ..., head ...
Build/test/formatter/generator/package commands are unavailable.
Descendant allowance: depth 1, at most 3 total, at most 2 active.
Reply with the mounted sessionReply result type; replying does not terminate you.
```

Only the first two lines select work. The rest state operational truth and do
not repeat the plan. The message, `:status`, and `ActorContext` must be rendered
from the same `EffectiveRole` and workspace receipts.

### Fresh-agent prelude

A fresh agent receives the common worker prelude plus its role projection and
explicit task because it has no inherited conversation. This is deliberately
different from context unfold. “Fresh reviewer” remains meaningful when the
parent wants uncorrelated judgment or wishes to avoid carrying a contaminated
context.

Prompt profiles should have stable IDs and versions in status and tracing.
Tests assert required semantic clauses and projections, not the byte-for-byte
rendering of an entire generated prompt.

## Admission, settlement, and failure

### Atomic unfold admission

One unfold is admitted in this order:

1. Prove the `unfold` effect is tail-positioned in the hosted call's final
   executable input unit.
2. Evaluate the pure applicative plan and assign stable branch IDs.
3. Freeze the provider prefix at the active hosted Haskell call.
4. Freeze the declaration generation and persistent binding tip; lease every
   live value reachable by branch inputs and shared bindings.
5. Validate child effect subsets and result/input type identities.
6. Resolve requested roles against parent and supervisor policy.
7. Reserve the shared descendant budget, actor identities, typed request/reply
   cells, workspace placements, and backend capacity.
8. Create every provider-native fork and prove each application queue-ready,
   without yet activating its assignment.
9. On any failure, terminate and release all unpublished branches and return
   one typed `UnfoldError` with per-branch diagnostics.
10. Publish the parent-side applicative result, enqueue each child role delta
   and typed input, and let parent and children proceed independently.

Effects known to have happened before a later Haskell unit rejects remain
non-transactional, as elsewhere in the workbench. The atomicity above belongs
inside the single owning `unfold` effect, not around arbitrary neighboring
effects in a fenced block.

### Post-publication failure

After publication, each child is an ordinary persistent actor. Startup death,
cancellation, deadline, backend failure, or an unavailable reply settles only
that child's response and any dependent watch according to existing typed
rules. It does not retroactively erase sibling handles or the fork group.

Add a settled-observation combinator so a parent can choose fail-fast or
collect-all in ordinary Haskell:

```haskell
awaitSettled
  :: Response a
  -> Await (Settlement a)
```

Strict `awaitResponse` may retain its current unavailable-watch behavior;
`awaitSettled` makes partial campaign folds explicit without adding scheduler
policy to `Watch`.

### Haskell snapshot prerequisite

The declaration environment already freezes a parent's generation when a
child scope is minted. The persistent binding store currently resolves names
by walking mutable ancestor frames, so a later parent rebinding can leak into
a nominal child scope. Haskell values are immutable; the evolving
name-to-current-value map is the missing snapshot.

Before claiming exact context fork, add an immutable binding-resolution tip
with root leases. Tests must prove:

- a child sees every binding current at the unfold call;
- later parent bindings and rebindings are invisible;
- sibling bindings are invisible;
- parent retirement cannot release roots leased by a living child; and
- parent and child may force shared lazy values safely under the resident
  machine's serialization discipline.

## Observability

Immediate receipts and later provider measurements are different facts.

`BranchReceipt` is available when `unfold` returns and should include:

- campaign, fork-group, branch, parent, child actor, and exact incarnation IDs;
- label and selected role/profile versions;
- provider source thread/turn/tool-call boundary;
- Haskell declaration and binding snapshot identities;
- effective effect list and runtime policy receipt;
- worktree ID, starting HEAD, visible path, and access mode; and
- reserved descendant and compute budgets.

`ForkObservation` becomes richer as children execute:

- application state and queue/request state;
- cached and uncached input tokens reported by the provider;
- output and reasoning tokens when reported;
- fork-to-first-token and request duration;
- active, idle, hibernated, stopped, and failed child counts; and
- worktree submitted HEAD and response/watch standing.

Missing provider usage is `Nothing`, never zero. A static fork receipt proves
lineage; provider token accounting proves economic cache reuse. Acceptance
uses a live canary and metrics, not an exact cache-ratio golden test.

`:status` should display role, effective effects, parent/fork group/branch,
remaining descendant budget, prompt profile, and `bound_worktree` rather than
the misleading bare `worktrees`. The identical visible path
`/tmp/tidepool-actor-workspace` is always paired with actor incarnation,
worktree ID, and HEAD.

## Runtime ownership

No new mechanism receives a second owner:

| Concern | Owner |
|---|---|
| Actor identity, parentage, human lineage/path allocation, mailbox, lifecycle, request activation, fork group | `tidepool-actor` |
| Provider-native fresh/resume/fork launch and token usage | `tidepool-agent` |
| Process, mount, native-tool policy, durable activation delivery | `tidepool-node` and the existing launch boundary |
| Persistent declarations, immutable binding tips, machine checkout, live roots | `tidepool-runtime` plus `tidepool-codegen`'s binding store |
| Haskell effect declarations, `Unfold`, role DSL, optics, result combinators | `haskell/` and the existing protocol/codegen path |
| Worktree Git-ref projection, snapshots, stable observations, changed paths, merge boundary | `tidepool-worktree` |
| Effect decoding and principal/grant enforcement | Existing actor/handler interpreters |
| Prompt assembly | Existing Shoal actor launch/application source path |

The free applicative and typed result assembler live in Haskell. Rust
interpreters provide arbitrary mechanics through the existing effect
machinery. Do not create a Rust workflow AST, a second actor scheduler, a
parallel result registry, or a second worktree scanner.

## Linear implementation handoff

This section is written for a later single-session, lower-effort implementation
loop. The interaction decisions above are input, not invitations to redesign
the product between compiler errors. When code contradicts an assumed
mechanism, extend the named owner and record the discrepancy; do not add a
parallel shortcut merely to keep moving.

### Implementation checklist

This is the durable todo list for the feature. Update this checklist in the same
commit that changes a gate's state; the detailed gate table below owns its
exit evidence. A checked planning item means the decision is ready to
implement, not that production support has landed.

- [x] Fix the permanent-application, typed reply/watch, and non-fatal
  interactive pattern contracts from live Shoal use.
- [x] Pressure-test the unfold/fold surface through a recursive three-batch
  interaction, including local types, `[fmt|...|]`, persistent children,
  named Git lineage, and a fresh-review boundary.
- [x] Record one canonical root plan, owner map, hard blockers, semantic-test
  policy, and linear low-effort handoff.
- [ ] Gate 0: prove the exact active-provider-call fork boundary, measurable
  prefix-cache reuse, tail-position rejection, and enforced inspection-only
  native policy.
  - 2026-09-04 provider canary: forked Codex thread
    `01a06b3a-6c67-7d91-a4f3-5fc8eb981b96` while its parent was suspended in
    an active shell call. The child observed the request, assistant preamble,
    and in-progress command, but neither the command result nor a post-result
    marker. Usage reported 11,264 cached of 16,107 input tokens.
  - 2026-09-04 policy slice: inspection-only interactive actors now receive a
    process-private Codex execution-policy overlay. Focused adapter and mount
    tests passed; `codex execpolicy check` classified `cargo`, `nix develop`,
    and `git commit` as forbidden while leaving `rg` and `git status`
    unmatched. A live Codex process inside the same Bubblewrap overlay rejected
    `/run/current-system/sw/bin/zsh -lc 'cargo --version'` before execution with
    the inspection-only rationale. Tail-position enforcement remains for the
    `Forks` effect slice.
- [x] Gate 1: add immutable Haskell binding tips and descendant root leases.
  - 2026-09-04: `BindingTable` now captures a flattened immutable inherited
    tip at scope mint, retains each referenced `SessionVarId` through a
    counted root lease, and defers release of retired-owner entries until the
    final tip disappears. Focused binding tests cover parent progress,
    sibling isolation, and deferred release. The JIT
    `binding_tip_lazy_sharing` test forces one shared tenured thunk from both
    sides of a parent/child tip. Runtime declaration-scope and root-retirement
    suites passed in the repository Nix toolchain; `tidepool-actor --lib`
    passed against the new compile view.
- [x] Gate 2: add campaign/actor-path allocation, granular residual effects,
  typed row witnesses, and one authoritative effective-role projection.
  - 2026-09-04: `ActorPath` is the single validated allocator input and exact
    `shoal/<path>` Git projection. `KnownEffects`/`Subset`, semantic branch
    constructors, role-specific workbench aliases, native-tool class,
    workspace access, prompt profile, and descendant policy all project from
    one accepted `EffectiveRole`. Actor/protocol invariants and the public
    Shoal compile fixture passed.
- [x] Gate 3: attach authoritative per-request execution and worktree evidence
  to requester-side response results.
  - 2026-09-04: every `ResponseResult a` now carries the exact request/actor
    execution receipt and either no worktree, an observation failure, or the
    bound worktree receipt plus request-start HEAD and stable submitted
    observation. The settlement path fills that Haskell cell before publishing
    response/watch readiness; the provider-boundary integration test observed
    the envelope repeatedly.
- [x] Gate 4: land one cache-preserving persistent child with typed follow-up.
  - 2026-09-04: the actor-host integration test forks the active parent thread
    and immutable Haskell tip, reads a parent-declared type and bound value,
    replies, remains attached, and accepts a second differently labeled typed
    request against the same actor incarnation.
- [x] Gate 5: land heterogeneous applicative batch admission, rollback, and
  result reconstruction without JSON or unchecked casts.
  - 2026-09-04: `Unfold` is a Haskell-owned free applicative with no `Monad`;
    its two-pass interpreter constructs all children before publishing any
    request and reconstructs a heterogeneous pair of typed handles. One
    actor-owned fork-group ledger reserves the whole sibling name set, gates
    publication on every provider queue-ready signal and the final Haskell
    commit, and releases paths/stops unpublished children on rejection or
    failure. Unit tests cover barrier/abort/collision behavior; the public
    compile fixture covers heterogeneous and homogeneous traversal; the
    provider-boundary test covers a two-type fan-in watch.
- [ ] Gate 6: land recursive scaffold/unfold/watch/fold from one clean named
  Git seed.
- [ ] Gate 7: land prompt profiles, status/metrics, hibernation, and the full
  three-batch dogfood run.
- [ ] Run the final relevant broad checks once, move stable contracts to
  owning crate docs and the glossary, and retire this plan.

Do not check a gate merely because its happy-path code exists. Record the
focused commands and failure/cleanup evidence immediately beneath the item or
in its commit message, and leave the box open when the gate's exit evidence is
partial.

### Starting audit

Before changing production code:

1. Read the root and nearest subsystem `AGENTS.md` files and the mechanism
   index in `CLAUDE.md`.
2. Record `git status --short` and preserve every pre-existing modification.
3. Confirm the landed permanent-application, `Replies`, `Watches`, projected
   pattern-binding, and non-fatal-warning behavior with their existing focused
   tests. Do not reimplement them from the older completion-era plans.
4. Identify the exact slow monolithic surface/golden tests already
   quarantined. Keep them quarantined while the API moves; add adjacent
   semantic Haskell fixtures instead of repeatedly rewriting generated source
   strings.
5. Make one reviewable commit after each green gate below. A gate that is not
   green remains an explicit working-tree checkpoint with its failed command
   recorded; do not accumulate the entire cross-crate feature as one
   unreviewable diff.

The provider active-call proof is a hard feasibility gate. If the backend
cannot fork a thread including the in-progress hosted Haskell call while
excluding its result, stop and report that evidence. Transcript replay,
summary injection, or forking the previous completed turn is not an acceptable
substitute under the name `unfold`.

### Gate map

| Gate | Production owners and likely first touchpoints | Exit evidence |
|---|---|---|
| 0. Provider/policy canaries | `tidepool-agent/src/interactive.rs`, Codex backend `node.rs`/`transport.rs`/recorded fixtures; `tidepool-node::process_boundary`; actor hosted-tool correlation | Two siblings contain the exact active call and not its result; a non-final or non-tail unfold is rejected before launch; lineage and reported cache tokens correlate; an inspection-only child is denied representative build/artifact commands before execution |
| 1. Immutable Haskell snapshot | `tidepool-codegen/src/binding_table.rs`; `tidepool-runtime/src/session/persistent.rs`, `registry.rs`, `resident.rs` | Child resolves the pre-fork binding tip; parent rebind, sibling bind, scope retirement, and shared lazy forcing satisfy the snapshot/root-lease tests |
| 2. Names, effects, and roles | actor identity/descriptor/start plus a single new lineage owner; `tidepool-worktree/src/label.rs`, `create.rs`, `registry.rs`; protocol effect definitions; Haskell Shoal facade; node launch policy | Typed hierarchical paths and numeric collision allocation; generated effect rows compile/deny as intended; one `EffectiveRole` agrees across prompt, status, grants, workspace, and native tools |
| 3. Per-request evidence | `tidepool-worktree/src/submission.rs`, `snapshot.rs`; actor request/settlement path; Haskell Reply/Watch modules | `Reply a` accepts only `a`; requester gets `ResponseResult a`; start/submitted heads plus committed/staged/unstaged/untracked facts precede response/watch readiness; observation failure preserves `a` |
| 4. One persistent context child | actor registry/kernel/resident-interactive path; provider `InteractiveLaunchMode::Fork`; process mounting; one internal `Forks` request; Haskell `Branch`/`Forked` | One child inherits provider and Haskell snapshots, gets its selected input/role/worktree, replies, remains idle, and accepts a typed follow-up request |
| 5. Applicative atomic batch | Haskell `Unfold` normalization/rebuilder; one actor-owned batch admission transaction; worktree/budget reservation | Heterogeneous record and homogeneous traversal both rebuild without serialization; all preflight failure paths publish zero children; post-publication failure affects only its leaf |
| 6. Recursive scaffold/fold | descendant budget owner, `ScaffoldEffects`, bound-HEAD placement, conservative merge, watches | An interior child commits a scaffold, forks two named leaves from one OID, ends a turn with its reply pending, wakes once, integrates immediate children, and replies upward |
| 7. Prompt/status/dogfood | actor prompt catalog and activation renderer; `:status`; structured tracing/metrics; Shoal Console | Root/fork/fresh profiles state truthful facts once; status uses readable lineage plus exact IDs; a three-batch live campaign confirms cache reuse, retained actors, recursive worktrees, and one independent fresh review |

Run the smallest owning test at every gate. Compile every changed target before
committing it, run formatting for changed languages and `git diff --check`, and
reserve `just verify` (plus `just fixtures-check` if extractor/serialization
changed) for the coherent final boundary. Do not run broad batteries in every
inner loop.

### Haskell free-applicative bridge

Keep the typed tree and its reconstruction out of Rust. A concrete
implementation can use an internal shape equivalent to:

```haskell
data Unfold effs a where
  PureU :: a -> Unfold effs a
  ApU   :: Unfold effs (a -> b) -> Unfold effs a -> Unfold effs b
  LeafU :: Branch child input result -> Unfold effs (Forked result)

data SomeBranch parent where
  SomeBranch
    :: (KnownEffects child, Subset child parent)
    => Branch child input result
    -> SomeBranch parent

data FlatUnfold parent a = FlatUnfold
  { leaves  :: [SomeBranch parent]
  , rebuild :: [RawForkReceipt] -> Either InternalUnfoldError a
  }
```

The public constructors and actual free-applicative representation may differ,
but the ownership split does not:

1. Pure Haskell normalization records leaves in applicative order and retains
   a typed reconstruction closure.
2. One `Forks` effect transfers the existential leaf inputs plus closed
   operational metadata under the existing live-value custody mechanism.
   Arbitrary inputs are never JSON encoded.
3. Rust preflights and admits the ordered leaves atomically and returns only
   raw request/actor/worktree/role receipts in that same order.
4. Haskell applies the retained closure to construct each correctly indexed
   `Forked result` and the caller's original result shape.
5. Wrong arity/order is an internal invariant failure with the fork-group
   correlation ID. It must not be patched with `unsafeCoerce`, a universal
   `Value`, or result-type strings.

Reuse the current request cell/root path. A leaf's typed reconstruction step
creates or completes the requester-side `Response result`; the target receives
the dual `Reply result` through its mounted activation. Rust owns stable IDs
and custody transfers but never needs to know the domain result's layout.

Write pure Haskell tests for normalization and rebuilding before wiring the
effect. Include nested `(<$>)`/`(<*>)`, `traverse`, empty/pure plans, repeated
labels, and deliberate raw-receipt arity mismatch. Keep the production
consumer—the actor batch handler—in the same slice so the free applicative is
not a test-only abstraction.

### Atomic admission transaction

Implement one actor-owned admission object with explicit phases rather than a
series of public operations and compensating caller code:

```text
planned -> preflighted -> reserved -> provider-ready -> published
             |              |              |
             +----------- rollback --------+
```

Preflight resolves and freezes:

- the workbench proof that no authored unit or effectful suffix follows this
  unfold in the provider-visible tool call;
- provider source thread/turn/tool-call boundary;
- Haskell declaration generation and immutable binding tip;
- requested and allocated hierarchical paths;
- input/result identities and child effect subsets;
- effective roles/native policy;
- one source OID per shared worktree placement;
- actor, request/reply, worktree, descendant, and backend capacity; and
- every branch-specific diagnostic needed for one `UnfoldError`.

Reservations are unpublished and owned by this object. Its drop/rollback path
terminates provider forks, closes actor/application reservations, releases
Haskell roots and budgets, and records provisional worktree failures using the
existing owners. Publication linearizes the whole group: first make all
handles observable, then enqueue every role delta and typed activation. Never
activate the first child while later siblings are still capable of rejecting
admission.

After publication, ordinary actor supervision takes over. The transaction no
longer pretends it can roll back model output or a visible child. A leaf's
failure settles only its response and dependent watches.

### Immutable binding implementation direction

Declarations already have generations; value lookup must stop walking mutable
ancestor heads. Extend the existing append-only binding owner with an
immutable resolution tip, for example a frame/version pair captured at fork.
A child has:

- a frozen chain for inherited lookup;
- a new mutable append-only head for its own later bindings; and
- root leases for every inherited live value reachable through the frozen
  chain.

Parent rebindings append beyond the captured tip and are therefore invisible.
Sibling writes occur on sibling heads. Retirement drains roots only after the
last descendant lease. Keep checkout/settlement inside `SessionRegistry` and
live root mechanics in `tidepool-codegen`; do not add a fork-specific root
registry in the actor crate.

The first tests should use tiny values and rebindings. Add closure and lazy
forcing cases only after lookup identity is correct, then add retirement races.
If sharing a lazy thunk concurrently would violate the current serialized
machine discipline, serialize forcing at the existing session owner and state
that limitation; do not deep-copy arbitrary values and call it immutable
sharing.

### Lineage and branch allocation

Replace the current two independent sanitizers with one validated path model
for new actors. Old durable receipts and their existing branch strings remain
readable; there is no reason to rename historical Git refs.

Admission allocates in this order:

1. validate campaign/group/branch segments without lossy rewriting;
2. for a top-level `batch`, resolve or reserve the permanent root's requested-
   to-allocated campaign mapping; for a `subgroup`, use the executing actor's
   allocated path;
3. append the group and child segments;
4. identify repeated sibling requests in applicative order;
5. reserve candidate actor paths under the actor registry lock;
6. ask `tidepool-worktree` through its existing `GitCli`/registry path whether
   each required `refs/heads/shoal/<actor-path>` can be created;
7. on a retained/external collision, retry the colliding segment or repeated
   sibling set with the lowest available numeric suffix; and
8. record requested and allocated campaign/actor paths, actor ID, worktree ID,
   and Git ref in the branch receipt before publication.

Do not pin another exhaustive string-sanitization corpus. Test validation
classes, hierarchy preservation, deterministic repeated-sibling numbering,
concurrent collision allocation, existing external Git refs, retained old
actors, and the property that every allocated projection passes Git's own
`check-ref-format`.

### Effect and role projection

Add model-facing effects through the protocol source and generated bridge;
never hand-edit generated modules. Keep effect operations with the production
owner named in the effect table. `KnownEffects`/`Subset` evidence is Haskell's
static authoring layer; `Effects effs` is the single generated residual-row
witness used for facade/handler alignment; opaque principals and grants remain
runtime authority. Do not create a parallel Rust enum whose variants must be
kept in sync with those witnesses.

One accepted `EffectiveRole` value must be produced before provider launch and
then projected into:

- the exact child effect facade compiled for its incarnation;
- actor/resource grants;
- bound-worktree placement and access;
- native process/build/network policy;
- descendant budget;
- role Developer delta;
- `ActorContext`, `:status`, and tracing receipts.

No projection may parse another projection's rendered text. A backend that
cannot enforce `InspectionOnly` must reject that role or report an explicitly
weaker policy; the prompt must never claim build denial while the native tool
still permits it. Conversely, this trusted development swarm does not need a
large security theater layer: ordinary coding and scaffolding roles should
retain normal Git and build ergonomics inside their named worktrees.

Compile fixtures should prove useful polymorphic helpers work under any row
with the required `Member`s, a coding leaf cannot call `unfold` or
`tryMerge`, a researcher cannot acquire coding effects, a scaffold actor can
fork and integrate, and no copied opaque handle bypasses runtime authority.

### Response-envelope settlement order

Extend the existing request state machine, not `Watch`, with one terminal
success transaction:

1. accept and fence the target's `Reply a` exactly once;
2. retain the live `a` while settlement is in progress;
3. observe the request-start and submitted bound-worktree states through
   `tidepool-worktree`;
4. construct `ResponseResult a`, using `WorktreeObservationFailed` when that
   observation fails;
5. fill the requester Haskell cell;
6. mark `pollResponse` ready;
7. reevaluate and publish durable watch transitions; and
8. perform the target-side irreversible terminal transfer for this request.

Tests should stop or cancel both actors at every seam and prove a single final
state, no overwritten cell, no lost wake, and eventual root release. The
worktree observation is authoritative state, not an event-log reconstruction.
Committed path changes are the diff from request-start HEAD to submitted HEAD;
staged, unstaged, untracked, ignored-excluded, and in-progress operation facts
come from the submitted working state.

### Recursive dogfood script

The first live acceptance is the worked interaction above at smaller scale:

1. root declares one `Plan`, two result types, an HKD result record, and an
   `[fmt|...|]` renderer;
2. root unfolds one researcher and one scaffolding child in a named campaign
   batch;
3. scaffolding child makes and commits a tiny shared change, declares a local
   leaf result type, and unfolds two coding leaves from its clean HEAD;
4. both levels register labeled applicative watches and end their turns with
   replies still pending;
5. one leaf receives a typed follow-up request after its first reply;
6. scaffold actor merges both named child branches, using native Git only if
   `tryMerge` returns `ManualGitRequired`, then replies upward;
7. root observes typed worktree evidence, launches one genuinely fresh
   reviewer, and performs the final merge; and
8. status/logs prove exact context lineage, Haskell snapshot IDs, readable
   actor/Git names, and reported cached versus uncached tokens.

Retain the session and worktrees after success for inspection. Teardown is a
separate supervisor action, not a hidden test epilogue.

### Do-not-improvise boundaries

Pause the overnight loop and leave a concise blocker if any of these occurs:

- the provider active-call boundary cannot meet the exact-prefix contract;
- reconstructing heterogeneous results appears to require `unsafeCoerce`,
  JSON, or Rust interpretation of the user's applicative tree;
- immutable binding snapshots would require a second live-root owner;
- inspection-only build denial cannot be made truthful with the available
  backend/process boundary;
- atomic publication would activate any child before all siblings are ready;
- a worktree change would rename, delete, or overwrite retained user branches
  or dirty state; or
- a broad generated-string golden is the only test keeping progress possible.

In the last case, quarantine the tedious test with an explicit replacement
fixture and continue. In the others, preserve the last green commit and report
the exact invariant and evidence rather than silently weakening the surface.

## Delivery plan

### Slice 0 — executable feasibility proofs

- Prove a Codex child forked while the parent is suspended on the hosted
  Haskell call sees that exact call and not its result.
- Record provider thread lineage and cached/uncached token usage for at least
  two siblings.
- Prove an inspection-only launch policy refuses representative build,
  formatter, generator, package, and artifact-writing commands before they
  execute while retaining useful read/search commands.
- Keep these as focused canaries; do not build the public DSL on an assumed
  provider behavior.

### Slice 1 — immutable Haskell environment snapshots

- Add persistent binding tips and live-root leases alongside the existing
  declaration generation snapshot.
- Give forked scopes immutable ancestor lookup while preserving ordinary
  mutable progress within each new child scope.
- Add focused binding, rebinding, sibling, retirement, and lazy-value tests.

### Slice 2 — granular interactive effects and role projection

- Replace independent sanitized agent/worktree labels for new launches with
  validated hierarchical `ActorPath` allocation and exact `shoal/<path>` Git
  projection; preserve historical receipts and refs as-is.
- Introduce `ActorContext`, `AgentLaunch`, `AgentInspection`, `AgentControl`,
  `BoundWorktree`, `WorktreeRegistry`, `WorktreeAllocation`, and
  `WorktreeIntegration` facades over existing owners.
- Remove low-level `Actor`, `ActorLocal`, and `AgentSession` from the public
  Shoal row.
- Add type-level effect subset evidence and dynamically generated child
  facades.
- Implement named role policies and one accepted `EffectiveRole` projection
  used by runtime admission, `:status`, and developer prompts.
- Distinguish leaf `CodingEffects` from recursive `ScaffoldEffects`; keep
  ordinary Git/build tools available to both writable roles and enforce
  inspection-only compute denial before process execution.
- Land compile-pass and compile-fail fixtures for representative narrowed
  rows.

### Slice 3 — authoritative per-response evidence

- Extend the one Worktree observation owner with request-start snapshots and
  committed/staged/unstaged/untracked path changes.
- Generalize request settlement so `Reply a` still accepts exactly `a` while
  requester polling returns `ResponseResult a`.
- Capture evidence before publishing readiness and preserve the authored value
  when observation itself fails.
- Add `awaitValue` and `awaitSettled` as ordinary Haskell projections.

### Slice 4 — one context-forked persistent child

- Add the internal `Forks` effect request and actor fork-group state.
- Require the interactive unfold site to be the tail effect of the hosted
  call's final executable Haskell unit; reject parent-authored suffixes before
  reserving anything.
- Reuse the provider's native fork launch mode with an exact active-call
  boundary rather than transcript reconstruction.
- Fork one immutable Haskell snapshot, mount one typed input/reply scope, apply
  a narrowed role, and return one `Forked result` after readiness.
- Verify follow-up requests reuse the child after its initial reply.

### Slice 5 — applicative atomic unfold and recursion

- Implement the Haskell-owned free applicative and heterogeneous result
  assembler.
- Flatten and preflight a whole plan at one snapshot, admit atomically, and
  reconstruct its shape with typed handles.
- Add shared subtree/depth/concurrency budgets and explicit child caps.
- Support nested unfold under both static `Forks` membership and runtime
  allowance.
- Resolve every recursive sibling worktree from one clean bound HEAD (or one
  explicitly receipted dirty snapshot) and preserve the hierarchical branch
  path through its fold.

### Slice 6 — prompt profiles, hibernation, and observability

- Replace the absolute root delegation instruction with the calibrated root
  prelude.
- Add role-derived child and fresh-agent prompt projections.
- Extend typed context/agent/fork inspection and `:status`.
- Surface provider cache measurements honestly and add idle backend
  hibernation without logical actor teardown.

### Slice 7 — retirement and dogfood

- Delete or rename the old blocking `Tidepool.Fork`/`forkAll`/`forkCata`
  surface so it cannot be confused with persistent actor unfold.
- Update older architecture sections that require exact effect-profile or
  public continuation cloning.
- Run a real campaign with heterogeneous coding and research children,
  recursive unfold, per-request worktree evidence, a second request to a
  replied child, and an independent fresh reviewer.
- Move stable contracts into the owning crate guides and public Haskell docs,
  then retire this plan.

## Verification style

Prefer semantic fixtures and properties over transcripts and generated-text
goldens:

- compile small adjacent Haskell fixtures proving each public signature and
  effect-row denial;
- test free-applicative shape and result reconstruction in pure Haskell;
- test actor, response, watch, budget, and rollback transitions against typed
  states and IDs;
- test worktree evidence in real temporary Git repositories;
- assert prompt projections contain required semantic facts and agree with the
  effective policy, not every rendered byte;
- keep provider/cache tests as narrow live or recorded protocol canaries; and
- temporarily quarantine the existing slow monolithic surface fixture while
  the API is moving, recording exactly what replaces it and re-enabling a
  boundary test once the surface stabilizes.

Critical scenarios include:

- every child sees all branch plans but only its own selector;
- no child sees the parent's unfold result;
- a later Haskell input unit or user-authored post-unfold `do` suffix rejects
  before any provider child, actor, request, or worktree is reserved;
- all siblings share one provider/Haskell snapshot identity;
- a later parent binding is invisible in every child;
- child-local declarations are visible to its later descendants but not its
  parent or siblings, and local results fold into the predeclared parent-edge
  reply type;
- actor labels, Git refs, status, and receipts preserve one hierarchical path,
  with deterministic numeric suffixes under repeated and concurrent names;
- a researcher lacks build effects where applicable and is denied build
  commands before execution;
- a narrower child cannot escalate through custom role syntax or recursive
  unfold;
- one pre-publication launch failure leaves no visible child or worktree
  binding;
- one post-publication child failure leaves sibling handles usable;
- response readiness is published only after worktree evidence is observable;
- untracked paths and committed path changes survive in the typed result;
- a straightforward merge reports its exact Git outcome, while conflict or
  ambiguous snapshot ancestry aborts cleanly and returns `ManualGitRequired`
  with enough facts for an integration actor to continue using native Git;
- replying leaves the child idle and a later typed request succeeds;
- ending a model round with an unsettled request and a registered watch leaves the
  reply pending and wakes the same actor when the watch settles;
- several child settlements coalesce activation while all results remain
  individually pollable; and
- provider metrics distinguish “not reported” from a measured cache miss.

## Rejected shapes

- No `complete`, `yield`, `park`, or replacement operation for ending a model
  turn.
- No `fork :: Eff effs (Either Parent Child)` in the model-facing workbench.
- No blocking `forkAll :: [Prompt] -> Eff effs [result]` as the primary actor
  API.
- No fresh prompt per child containing a generated summary of shared context.
- No automatic merge of child model contexts into the parent.
- No retroactive declaration grafting from a later parent into an already
  diverged child in V1.
- No implicit copying of parent reply, watch, integration, secret, or
  supervisor authority.
- No universal JSON branch specification or Rust-owned workflow tree.
- No role name that is only prose while the process retains contradictory
  tools.
- No hash- or opaque-ID-shaped actor/branch name as the primary model-facing
  navigation surface, and no silent lossy normalization of a bad label.
- No Haskell replica of Git's rebase, cherry-pick, history-editing, or
  conflict-resolution interface; the one default merge helper hands complex
  cases back to ordinary Git.
- No eager child teardown after the first reply.
- No exact full-prompt or full-transcript golden that must be rewritten for
  every surface refinement.

The compact rule is:

> Fork context exactly, narrow authority explicitly, and fold typed evidence.
