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
inspectFull (fmap settledValue result)
```

`fmap` preserves `WatchPending` and `WatchUnavailable`; inside `WatchReady`,
`settledValue` preserves a failed child as `Left`, rather than pretending it
returned a report. A successful report is not review, acceptance, or integration.
Keep the original result when execution/worktree evidence matters.

`Unfold effects` and `Await` are applicative: combine independent work with
`(,) <$> child branchA <*> child branchB`, and dependencies with
`(,) <$> awaitSettledFork workerA <*> awaitSettledFork workerB`.
Register separate watches when results can be integrated independently.

## Commands

The `bash` tool accepts literal Bash and displays output directly.
`exec_command` adds memory/cwd/environment/PTY options; `write_stdin` sends input
or polls a returned `session_id`; `close_stdin: true` closes piped input after any
final chars. A verified rejection states that neither input nor EOF was submitted;
correct that call safely. A partial write/close receipt requires close-only recovery, never
replaying acknowledged bytes. Backend acknowledgment does not prove child consumption.
`cancel_command` requests cancellation; its receipt distinguishes terminal outcome
and cleanup from an outstanding request. Terminal receipts always show cleanup.
`read_output` returns contiguous retained output with `next_offset`; its default
display budget is 8 KiB (`max_output_bytes` selects 1024–32768 bytes).
All use `Cmd`'s execution/resource owner. Ordinary shell work needs no Haskell
binding. Haskell job bindings additionally support composition and recovery.
Tool names are flat; native shell implementations are disabled, while `apply_patch`
remains available. Use the schemas directly for ordinary commands.

`Cmd` is `Tidepool.Command`; `bash`, `withMemory`, `MiB` and `GiB` are loaded.
```haskell
result <- Cmd.run [bash|git status --short|]
```

Output appears automatically; `result` remains data. `Cmd.quiet action` suppresses
routine display within that action. Commands are reusable values; `Cmd.describe`
inspects intent. Ordinary commands use 256 MiB; use `withMemory` for builds/tests.
`print value` inside an effectful block emits bounded `Display` output through
Console in execution order; output before a later failure remains visible.
It differs from Prelude's `Show`-based IO print. State-machine actors log it without
waking a model; use projections for large values.

`Cmd.run` and `Cmd.await job` wait up to 30 seconds for completion. An overrun
stops the current computation; the interactive workbench names a retained
`jobN :: Cmd.Job` binding in its receipt. The enclosing result and subsequent
statements do not run. Use that binding to inspect, read output or await later;
never rerun for diagnostics. Haskell handlers fail without an interactive binding;
use `Cmd.start` plus completion routing for their long-running work.

`Cmd.stdout result` purely extracts complete successful stdout or an explicit
issue. Use `T.lines` or `Cmd.decodeWith (Cmd.asJSON @Value)` for data consumption.
`Cmd.readStdout (Cmd.job result)` explicitly reads more retained complete stdout;
repeated `await` does not enlarge capture. `Cmd.output job` starts stdout paging,
`Cmd.next page` advances, and `Cmd.tailOutput Cmd.Stderr job` reads diagnostics.
`Cmd.status job` includes cleanup; `Cmd.cancel job` requests cancellation.
`Cmd.completion job` supplies terminal metadata to a record actor.
Load `shoal-command` for safe arguments, retention, paging and interactive jobs.

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
externally supplied labels. `projectHead` selects the project checkout;
`boundHead` requires an allocated bound checkout, not merely write access.
Ordinary `unfold` inherits its working files and index, including untracked and
ignored project files. Busy or unavailable capture falls back to the selected
checkout's committed HEAD and reports the omission. Explicit Git refs remain
committed seeds; `snapshotDirty` is unnecessary for ordinary managed unfolding.

Build caches follow the creator independently of source/context selection. A useful
shared build before forking helps descendants; finish edits and leave source/build
files idle when convenient. Active builds continue, and forks use the latest
completed cache or an empty private cache. Keep durable work in Git: a Shoal host
crash ends the wave, and a new wave starts from committed branches.

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
is independent of context inheritance. Prefer inherited context for related
implementation children after shared reasoning; select fresh contexts explicitly for independent plan branches and
reviews. Workspace helpers must preserve that choice rather than silently reset
it. Route callbacks can launch selected-context workers; they have no provider transcript
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
awaitAnyProgress :: [(Progress progress, ProgressCursor)] -> Await [ProgressState progress]
settledValue     :: Settlement result -> Either ResponseFailure result
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
`awaitAnyProgress` wakes when any input advances or closes; captured results retain
input order, with unchanged inputs Pending. Applicative combinations still wait
for all combined dependencies. Empty input is immediately ready with [].
A terminal watch can be polled again without consuming it. A notice is not the
result: poll its retained handle, and do not repeat the original work.

Lifecycle observations may print only `WatchReady`; the payload still exists.
Bind the observation, then apply a projection or `inspectFull` to that saved
value. `inspectFull expression` is standalone workbench display syntax, not an
`Eff` action to compose with `>>=`. It does not poll again. `:info ResponseFailure` or
`:info WatchFailure` supplies detailed constructors only when needed.

## Persistent typed coordination

Use finite watches for finite obligations. For ongoing routing, define one generic
actor record: exactly one `mode :- State s`, typed `Call input NoReply` or
`Call input (R.Reply output)` routes, and `Event input` source handlers. `R` is the
preloaded `Tidepool.Actor.Record` alias; `Actor` names the lower-level actor API.
Load the workspace's `shoal-define-actors` skill for executable definitions.

The same record supplies initial state and handlers. Handlers use ordinary
`get`, `gets`, `put`, `modify'`. `R.start definition` returns an `ActorHandle api`;
`R.client handle` supplies endpoint values for `R.send` and `R.call`. State and
event fields expose no remote capability. `R.self @Api` supplies narrow send-only
return endpoints; it cannot make synchronous calls to itself.

An event field contains `R.on source handler`. `R.progress progressHandle`,
`R.settlement response` and `R.lifecycle actorHandle` select actual sources;
`fmap` and `(<>)` compose them. Fixed sources capture retained current state and
follow later publications in mailbox order. Handlers receive one event at a time,
without model rearming or batching. Cross-source order is acceptance order, not a
claim of global causality. Original result/request evidence remains attached.

Closures capture immutable values. Effects run under the receiving actor's
principal, not its creator's authority. `R.sender @Api` identifies the current
input origin; a forwarding actor is not the original result producer. The curated
`coordinationActor` supplies the project's `Replies`, `Actor`, `Notifications` row.
It does not impersonate the owner or grant captured worktree handles.

Use `Project.Routing.followWork` for ordinary named request/progress collection.
`readWork` obtains its retained state; `workSnapshotSummary` shows outstanding
candidates, questions and outcomes. Full publications stay in `workHistory`;
notices contain deltas and receipt references. Explicit `incorporatedWork` messages
remove exactly the handled evidence from the brief, preserving history.

A failed handler keeps committed state and queued work; its external effects are
not rolled back. `R.replace handle newDefinition` preserves the state/schema and
fixed source positions, skips the failed input, and returns the successor's exact
handle. Old client endpoints remain stale. Retain request handles before admission
when failure could otherwise strand them; never replay uncertain work.

Source completion leaves the actor alive through useful repairs. `R.finish handle`
drains accepted work and returns the retained exit/state (`finishWork` for project
collectors). Release native workers separately through their existing cleanup
owner. Whole-run recovery is from Git checkpoints.

## Requests and replies

```text
request :: Member Replies effects
        => AgentRef -> RequestLabel -> input -> Eff effects (Response result)
updateRequest :: Member Replies effects
              => Response result -> Text -> Eff effects (Either ReplyError RequestUpdate)
pollRequestUpdate :: Member Replies effects
                  => RequestUpdate -> Eff effects (Either ReplyError RequestUpdateState)
sendMessage :: (MessageRecipient recipient, Member Notifications effects)
            => recipient -> Text -> Eff effects (Either NotificationError NotificationReceipt)
pollNotification :: Member Notifications effects
                 => NotificationReceipt -> Eff effects (Either NotificationError NotificationState)
stopAgent :: Member AgentControl effects => AgentRef -> Eff effects StopOutcome

data RequestUpdateState
  = UpdateQueued | UpdatePresented | UpdateTooLate
  | UpdateUnconfirmed Text | UpdateNotPresented Text
```

`sendMessage` accepts an AgentRef or a captured ActorContextInfo. To have a
router message your TUI, bind `owner <- actorContext` in your model turn and capture
owner in the handler's `sendMessage owner ...`. The handler runs as the router,
not the capturing model. Both forms use the same authority-checked inbox;
a context observation grants no extra permissions.

Use `request @Report actor label assignment` for a new assignment to a retained
specialist; it queues when busy. `updateRequest` clarifies the exact owned,
active response without replacing its reply obligation. Handle its `Left` and
retain a returned `Right` handle; poll when delivery affects the next decision.
Presentation is not incorporation. Queued work
is not yet steerable; unconfirmed delivery can fence settlement. See
`:doc request` for delivery/recovery policy, and `:doc refinement` for review.

Use `sendMessage actor "e434: digest mutable; privatize fields; reject mismatch"`
for ordinary steering without a new reply obligation. It reaches the existing TUI
at its normal input boundary, waking it if idle. `Right receipt` means durable
admission; it does not mean incorporation. Keep the receipt; inspect delivery only
when that distinction affects the next action. Unconfirmed delivery must be
reconciled before retrying. `updateRequest` remains the request-bound operation
when the correction must participate in that request's settlement fence.

Actor messages are machine coordination: use fragments, exact refs and established
names. Send only post-fork changes, ambiguous constraints and the next needed
result. Do not repeat the recipient's task or shared instructions. Preserve
identity, authority, uncertain submission and acceptance scope when consequential.
Fresh contexts still need the meaning behind their refs. No mandatory format.

For an active request, activation supplies `sessionInput`'s type and the reply
type/declaration. Select or apply opaque input; use `inspectFull sessionInput`
only for explicitly omitted prose. `sessionReply` identifies this request's
reply obligation; `respond value` accepts its specified reply type and settles
it. It is terminal control transfer, not an ordinary `()` result to sequence
past. A root outside a request has no `sessionInput`, `sessionReply`, or
`respond` binding. Ending a model turn does not settle a request.

When progress was requested, activation also supplies `reportProgress`; call
it with the declared progress type to publish while retaining the reply.
Publish cumulative state for current-state capture. An attached `progressSource`
receives every subsequent publication; polling and finite watches observe
snapshots. Use `:doc watch` for `childWithProgress @Progress @Report`,
`requestWithProgress`, and source/watch choices. Do not guess a progress type or
create a duplicate store.

Inside a stateful actor, use `requestWithProgressInto @Progress @Result agent options retain`
when admission must survive a subsequent handler failure. Before submission,
`retain` receives `(Response Result, Progress Progress)`; enqueue those exact handles
to a typed `Self` route. That route can attach the result collector without repeating
the request. The defining-actors skill and project continuation example show this flow.

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
