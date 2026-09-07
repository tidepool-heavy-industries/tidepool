# Shoal core API guide

This is one shared API superset, not a grant of authority or an inventory of
this actor's state. Use the assignment and runtime authority already supplied.
Start useful work from these call shapes; do not begin with `:bindings` or
re-query the signatures below. Look up only a genuinely missing detail.

## Fork, watch, fold

This small example needs a clean project checkout and authority to fork a
coding actor. It is a syntax example, not a recommended task size. Replace the
assignment with a bounded obligation against the actual shared scaffold.

```haskell
:{
reportOnly :: Settlement a -> Either ResponseFailure a
reportOnly (ReplyAvailable result) = Right (responseValue result)
reportOnly (ReplyUnavailable failure) = Left failure
:}
let Right campaign = campaignLabel "api-guide"
let Right wave = forkGroupLabel "checks"
let Right checkBranch = branchLabel "hit-targets"
let task = "Check the hit targets." :: Text
worker <- unfold (batch campaign wave) (child (withEffort Medium (coding @Text checkBranch projectHead task)))
let Right readyLabel = watchLabel "hit-targets-ready"
ready <- watch readyLabel (awaitSettledFork worker)
```

Return from that tool call so the child can start. End the model turn when
waiting on the watch. On its wake, use the retained handle in a later call:

```haskell
result <- pollWatch ready
inspectFull (fmap reportOnly result)
```

`fmap` preserves `WatchPending` and `WatchUnavailable`; inside `WatchReady`,
`reportOnly` preserves a failed child as `Left`, rather than pretending it
returned a report. A successful report is not review, acceptance, or integration.
Keep the original result when execution/worktree evidence matters.

`Unfold effects` and `Await` are applicative: combine independent work with
`(,) <$> child branchA <*> child branchB`, and dependencies with
`(,) <$> awaitSettledFork workerA <*> awaitSettledFork workerB`.
Register separate watches when results can be integrated independently.

## Construction and handles

The following are API reference signatures, not declarations to paste into the
workbench. `result` is the first visible type argument of branch constructors:
`coding @Report label seed assignment` requests a `Report` reply.

```text
campaignLabel  :: Text -> Either NameError CampaignLabel
forkGroupLabel :: Text -> Either NameError ForkGroupLabel
branchLabel    :: Text -> Either NameError BranchLabel
watchLabel     :: Text -> Either WatchLabelError WatchLabel
requestLabel   :: Text -> Either RequestLabelError RequestLabel
batch          :: CampaignLabel -> ForkGroupLabel -> ForkGroupPath

projectHead   :: WorktreeSeed
boundHead     :: WorktreeSeed
snapshotDirty :: WorktreeSeed -> WorktreeSeed

coding          :: BranchLabel -> WorktreeSeed -> input -> Branch CodingEffects input result
scaffolding     :: BranchLabel -> WorktreeSeed -> input -> Branch ScaffoldEffects input result
integrating     :: BranchLabel -> WorktreeSeed -> input -> Branch IntegrationEffects input result
researching     :: BranchLabel -> WorktreeSeed -> input -> Branch ResearchEffects input result
researchingLeaf :: BranchLabel -> WorktreeSeed -> input -> Branch ResearchLeafEffects input result

data ForkEffort = Low | Medium | High
withEffort :: ForkEffort -> Branch child input result -> Branch child input result
withModel :: Text -> Branch child input result -> Branch child input result
withInstructions :: Text -> Branch child input result -> Branch child input result
data WorkerLifetime = ParentOwned | SwarmOwned
withLifetime :: WorkerLifetime -> Branch child input result -> Branch child input result
selected :: (input -> Text) -> WorkerContext input
inherited :: WorkerContext input
withContext :: WorkerContext input -> Branch child input result -> Branch child input result

child :: (KnownEffects child, Subset child effects)
      => Branch child input result -> Unfold effects (Forked result)
unfold :: (Member Forks effects, Member Replies effects, Member AgentInspection effects)
       => ForkGroupPath -> Unfold effects result -> Eff effects result

forkedActor    :: Forked result -> AgentRef
forkedResponse :: Forked result -> Response result
forkedLaunch   :: Forked result -> BranchReceipt
```

Labels use nonempty lowercase letters/digits separated by single hyphens, at
most 48 characters. The example uses validated literals; handle `Left` for
externally supplied labels. `projectHead` selects the project repository's
committed head; `boundHead` requires an allocated bound checkout, not merely
write access. Both require clean source unless wrapped in `snapshotDirty`.

Coding/scaffolding/integration branches have coding worktrees; research branches
are inspection-only. `researchingLeaf` omits delegation. Available effects and
runtime depth/width still limit admission. For recursive budget proposals use
`:doc unfold` and `previewBranch`; do not infer permission from visible handles.

`previewBranch proposed` resolves the branch's effective authority and static
launch settings without allocating a checkout or starting a provider. Inspect
`previewSource`, `previewContext`, `previewLifetime`, `previewGuidance` and
`previewLaunch`.
The host launch value contains `launchModel`, `launchEffort`,
`launchInstructions` (selected behavior plus effective authority),
`launchBaseFingerprint`, `launchWorkspaceIdentity` and `launchModules`.
`launchModel = Nothing` means preserve the inherited parent model at the completed
call boundary; a fresh worker resolves the configured default. `previewLaunch =
Nothing` means this embedding has no host resolver, not an instruction-free launch.
The common base is identified by content hash instead of copied into each preview.
Runtime workspace paths, request IDs and orientation are appended at admission.
A preview is not a capacity reservation or a proof of provider acceptance.

Workers default to `ParentOwned`. A top-level actor can select `SwarmOwned` with
a selected context to create a cooperating independent root through the same
`unfold` path. It retains its normal TUI and can receive followups after its creator
retires. Swarm shutdown still owns its retirement. A supervised actor cannot use
this selector to escape its owner's lifetime. Creation, supervision, and context
inheritance are separate observed relationships; lifetime does not widen effects
or worktree authority.
Unspecified fork effort defaults to Low, not the parent setting.
`withEffort` requests initial effort, not a change to a running actor or proof
of provider application.

Children start after the enclosing tool block completes. Default inherited
context includes its final committed ambient bindings. `withContext (selected
renderer)` starts a fresh conversation and isolated local binding scope; the
renderer supplies task guidance and the typed input remains available. Frozen
project modules are available in both modes. Captured assignments/closures keep
capture-time meanings; later parent calls do not refresh an existing child.

Set `withModel "gpt-5.6-sol"` explicitly in reusable worker recipes; model selection
is independent of context inheritance. Prefer selected contexts for independent
plan branches, and inherit when the actual shared reasoning is useful. Route
callbacks can launch selected-context workers; they have no provider transcript
to inherit. Specialist tasks use the model tagged by the plan.

`withInstructions body` selects persistent worker behavior independently of the
task input, context ancestry and runtime permissions. It replaces legacy role
prose while retaining the shared guide and host-supplied authority facts. Select
authored Markdown through the frozen `Shoal.Workspace.workspacePrompt` when a
workspace is configured; compose project behavior in Haskell. Request guidance
still applies to the individual obligation, including later repair requests.

## Read results without another discovery round

```text
awaitSettledFork :: Forked result -> Await (Settlement result)
awaitFork        :: Forked result -> Await (ResponseResult result)
awaitSettled     :: Response result -> Await (Settlement result)
awaitResponse    :: Response result -> Await (ResponseResult result)
watch     :: Member Watches effects => WatchLabel -> Await result -> Eff effects (Watch result)
pollWatch :: Member Watches effects => Watch result -> Eff effects (WatchState result)

data WatchState result
  = WatchPending
  | WatchReady result
  | WatchUnavailable WatchFailure

data Settlement result
  = ReplyAvailable (ResponseResult result)
  | ReplyUnavailable ResponseFailure

responseValue     :: ResponseResult result -> result
responseExecution :: ResponseResult result -> ExecutionReceipt
responseWorktree  :: ResponseResult result -> WorktreeEvidence
```

`Await` describes dependencies; `watch` registers a wake subscription.
`awaitSettledFork`/`awaitSettled` retain dependency failures inside `Settlement`;
`awaitFork`/`awaitResponse` propagate unavailability to the watch instead.
A terminal watch can be polled again without consuming it. A notice is not the
result: poll its retained handle, and do not repeat the original work.

Lifecycle observations may print only `WatchReady`; the payload still exists.
Bind the observation, then apply a projection or `inspectFull` to that saved
value. Full inspection does not poll again. `:info ResponseFailure` or
`:info WatchFailure` supplies detailed constructors only when needed.

## Requests and replies

```text
request :: Member Replies effects
        => AgentRef -> RequestLabel -> input -> Eff effects (Response result)
updateRequest :: Member Replies effects
              => Response result -> Text -> Eff effects (Either ReplyError RequestUpdate)
pollRequestUpdate :: Member Replies effects
                  => RequestUpdate -> Eff effects (Either ReplyError RequestUpdateState)
stopAgent :: Member AgentControl effects => AgentRef -> Eff effects StopOutcome

data RequestUpdateState
  = UpdateQueued | UpdatePresented | UpdateTooLate
  | UpdateUnconfirmed Text | UpdateNotPresented Text
```

Use `request @Report actor label assignment` for a new assignment to a retained
specialist; it queues when busy. `updateRequest` clarifies the exact owned,
active response without replacing its reply obligation. Handle its `Left` and
poll a returned `Right` handle. Presentation is not incorporation. Queued work
is not yet steerable; unconfirmed delivery can fence settlement. See
`:doc request` for delivery/recovery policy, and `:doc refinement` for review.

For an active request, activation supplies `sessionInput`'s type and the reply
type/declaration. Select or apply opaque input; use `inspectFull sessionInput`
only for explicitly omitted prose. `sessionReply` identifies this request's
reply obligation; `respond value` accepts its specified reply type and settles
it. It is terminal control transfer, not an ordinary `()` result to sequence
past. A root outside a request has no `sessionInput`, `sessionReply`, or
`respond` binding. Ending a model turn does not settle a request.

When progress was requested, activation also supplies `reportProgress`; call
it with the declared progress type to publish while retaining the reply.
Progress is cumulative latest-value state, not a lossless message stream.
To request/watch progress, use `:doc watch` for
`childWithProgress @Progress @Report`, `requestWithProgress`, and
`awaitProgressAfter`. Do not guess a progress type or create a duplicate store.

## Targeted discovery and cleanup

Use `:type name` for an omitted signature, `:info Type` for missing constructors,
and focused `:doc request`, `:doc unfold`, `:doc watch`, `:doc refinement`, or
`:doc cleanup` for their less-common operations. `:doc topics` lists the rest.
Use `:bindings` only to locate a needed live value whose name is missing, not
to collect every inherited name at startup. Visibility does not transfer
ownership or authority. Use `:status!` for lifecycle/provider uncertainty and
`:recovery` after recreation; conversation names do not restore lost handles.

`stopAgent` retires an actor when authorized. It is separate from accepting its result;
retain useful specialists and inspect the outcome and cleanup evidence.

## Automatic routing and inexpensive observation

```text
route :: Member Watches effects
      => Await result -> (result -> Eff effects ()) -> Eff effects Route
pollRoute :: Member Watches effects => Route -> Eff effects RouteState
listRoutes :: Member Watches effects => Eff effects [Route]
snapshot :: Member AgentInspection effects => Eff effects SwarmSnapshot
subtree :: (Int, Int) -> SwarmSnapshot -> SwarmSnapshot
creationTree :: (Int, Int) -> SwarmSnapshot -> SwarmSnapshot
shareObservation :: Member AgentInspection effects
                 => AgentRef -> AgentRef -> Eff effects ObservationShareResult
swarmUsage :: SwarmSnapshot -> UsageTotal
usageByRequestedModel :: SwarmSnapshot -> [(Maybe Text, UsageTotal)]
usageDelta :: SwarmSnapshot -> SwarmSnapshot -> UsageDelta
```

A route installs one callback, returns promptly, and runs it on its owning actor
when the typed dependency is ready. It accepts ordinary applicative `Await`
values, including settlements and `awaitProgressAfter` observations. Known
forwarding needs no model relay. For settlements handle both `ReplyAvailable`
and `ReplyUnavailable`; for progress preserve its cursor, cumulative unresolved
facts, closure and rejection. Rearming from the captured cursor waits for a newer
observation; it does not turn progress into a lossless event stream.
Callback effects have the owner's
permissions; captured handles never confer someone else's authority. Keep callbacks
short: submit work or install the next route, then return. `pollRoute` reports
waiting, running, completed, or retained failure. A failed callback is not retried
automatically because earlier effects may have happened. Callback failure sends
one exceptional notification to its owner; ordinary successful routing does not
wake the model. Inspect the failed route before deciding how to continue.
`listRoutes` recovers this actor's retained handles, including routes created
inside callbacks. It excludes other actors' routes and forgotten routes. Bind
the list, then use `traverse pollRoute` to inspect the current states.

A callback can `reply` through a captured `Reply` owned by this same actor to
finish its active obligation. This is a terminal transfer: later callback code
does not run. Exact request ownership, cancellation and update fences still
apply. Use this to deliver a completed chain directly to its original requester.

Snapshots read existing observations without asking models to report. They include
creation, supervision and context ancestry, actual/requested model, current requests, received
request and coordination-event counts, compactions when known, and provider usage.
Received coordination events count watch/cancellation notifications issued by the
request owner; they do not claim to count all provider/TUI events or presentation.
`shareObservation recipient scope` grants visibility into that exact actor's
creation tree, including later workers. The caller must already observe that
scope. Inspect `ObservationShareResult`: the recipient/scope may be unavailable,
or sharing may be unauthorized. Sharing is observation-only; it never permits
stopping workers or mutating their resources. Snapshots, host graph reads and
`:status`/`:lineage` use the same visibility policy. Use `creationTree` to group
planner-created work independently of supervision lifetimes.
Usage totals deduplicate provider threads, choose a compatible cumulative
observation across resumed actors, and expose unknown actors, inconsistent sources
and partial coverage. `usageByRequestedModel` groups those totals by requested
launch model; mixed/unspecified selections share `Nothing`. It is not attribution
to the model billed for every response. `usageDelta before after` separates
comparable increases, newly observed thread histories, lost observations and
discontinuities. Newly visible history may predate the first snapshot.
Token counts are observations, not a kill budget. Use ordinary TUI
conversations to steer workers and request high-leverage decisions.

The original workspace root has the swarm’s one authoritative `.shoal`. Copies
in managed checkouts are candidate source, not per-worker configuration. Integrate
customization changes into that authoritative directory for the next swarm.
Project prompt/module edits activate at the next explicit swarm restart. The
selected workspace inputs are recorded under the run's `workspace/selection.json`.
Define local task data and functions normally during a wave; do not expect edits
to imported module files or core prompts to reload mid-wave. The example at
`examples/shoal-workspace` shows a small TOML-selected project vocabulary and recipes.
