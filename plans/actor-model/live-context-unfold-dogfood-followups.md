# Live context-unfold dogfood follow-ups

The current tool-completion boundary is specified in [SHOAL.md](../../SHOAL.md#cache-preserving-context-unfold).
It supersedes this record's final-input-unit restriction: admitted children now
start after the real enclosing tool result and inherit its final committed scope.


Status: implemented through the reviewed live-process hardening wave; a fresh
provider canary is the final acceptance gate. Host-restart reconstruction is a
separately gated successor phase, not a partially landed durability claim.
This is the root plan and implementation record for hardening the landed
cache-preserving context-unfold surface from live Shoal Console use. It does
not reopen the accepted architecture in
[cache-preserving context unfold](cache-preserving-context-unfold.md).

## Implementation outcome (2026-09-04)

The original checklists below are retained as the design and review record;
they are not an assertion that every aspirational R4 item became part of this
wave. The following ledger is the authoritative implementation status.

| Area | Outcome | Primary commits |
|---|---|---|
| child/cleanup fault containment | landed; child failures no longer collapse the permanent host, cleanup reports a successful prefix | `b1316b8b` |
| reply and activation failure containment | landed; an accepted reply never reopens, failed activations settle locally | `b8b51aa5`, `0ca864c1` |
| dimensional time and typed paths | landed; raw model-facing millisecond and branch-prefix guesses removed | `af65318b` |
| build posture and actor observations | landed; inspection actors are denied build-like processes before launch, coding actors hold lifetime leases | `71cc7a32` |
| activation, campaign, lineage, and cache projection | landed; observations preserve exact incarnation, watermark, measurement scope, and prompt identity | `9f8f9c86`, `d7efe4a1`, `b9af76bb`, `9bff1978` |
| final integration custody and campaign cleanup | landed; source checkout is a validated target and cleanup is typed, dependency ordered, and retryable while the host lives | `fb02d96e`, `1eb719ec` |
| resident recovery | landed for diagnostic isolation and honest root successor declaration replay; effects and arbitrary live values are never replayed or serialized | `d00db7e6`, `cc5222b0` |
| workbench ergonomics and retry | landed; diagnostics continue, patterns fail locally, bindings/operations/terminal transfers are structured, exact-call retries are actor-owned | `0ca864c1`, `9bfb51f3`, `9c150a83`, `9bff1978` |
| prompt and executable guidance | landed and versioned; role deltas, inspection posture, lifecycle, cache, recovery, and docs match the mounted surface | `b9af76bb`, `ceb92f9d`, `9bff1978` |

The retry ledger deliberately lives with the resident actor that owns the
effects. It proves exact retry only while that actor/machine incarnation is
alive. A composition-root journal cannot honestly close the crash gap between
an owner commit and an observer append, so this wave did not add one and does
not claim pre/post-host-death exactly-once behavior.

### Final boundary review (2026-09-04)

- Deadline decoding must follow erased Core representation: Haskell's
  `newtype RequestDeadline = RequestDeadline Duration` has no runtime wrapper.
  The Rust request decoder consumes `Maybe Duration`. The real reply/watch
  fixture now submits `after (minutes 5)` and checks the authored unit in status;
  synthetic values cannot establish the extractor/bridge representation contract.
- A matched constructor with an invalid nested field must produce `FieldDecode`,
  retaining the nested cause. It must not escape as a top-level constructor miss
  that effect dispatch can skip. Derived decoders enforce this at field entry.
- Rust deadline construction owns unit conversion and range validation. Its
  fields are private and the live runtime type has no unchecked deserializer.
- Observable Haskell lexical scope IDs are named scopes. Forked actors have
  distinct scopes; inherited declarations/bindings plus context/provider lineage
  establish inheritance. Scope-ID equality is not a cache-reuse requirement.
- The earlier second-`Just`/duplicate-constructor explanation was unproven.
  The speculative `Maybe` lookup workaround has been removed; verification must
  demonstrate the deadline correction using the existing lookup implementation.

The boundary corrections are committed as `1e51bf4a`; scope naming is
`48956e0b`; obsolete full-protocol snapshots were removed in `3471d602`.
`just verify` passed: 2,421 default-tier tests, strict workspace/all-target
Clippy and formatting, suite-manifest validation, and 217/217 Haskell fixtures.
Focused checks also passed for actor request dispatch, bridge enum/record
failure propagation, and the nine retained protocol compatibility checks.
The first real recursive reply/watch/follow-up/cleanup run passed in 556 seconds;
a daemon-backed run checks the final code after removing the speculative
`Maybe` lookup change and adding the authored-deadline status assertion.

The fresh provider canary is `shoal-hardened-canary-low-5`, run
`559fead3-45f5-44b4-9a4c-a436b67d83f4`, using `gpt-5.6-sol` at low effort.
Its root is actor `0@1`, provider thread
`01a06ef8-e202-7f71-99ac-395a21b7b84e`. The previous failed canary was idle with
cancelled children and was stopped to release the repository's single-owner
binding registry; its logs and worktrees were preserved.

The final-code recursive regression passed (426.974 seconds, nextest run
`754c3a1d-bb39-4e2b-8272-68b9a3cf637c`). Canary 5 then exposed two additional
live concurrency/environment defects, so release acceptance remains open:

- Read-only inspection batches held the shared Haskell machine while GHC ran.
  The root's normal `watch` operation failed the implicit 30-second checkout
  deadline while children were inspecting. Fix at the owners: capture the
  immutable `ActorCompileView` under checkout, run inspections after settlement,
  and use the registry's cancellation-safe FIFO queue for ordinary admission.
  Explicit shutdown admission deadlines remain bounded. No replacement timer
  or polling/retry loop is introduced.
- The coding child inherited the host Cargo configuration's `sccache` wrapper.
  Its daemon resolved `/tmp/tidepool-actor-workspace` outside the child's mount
  namespace and failed writing dependency files. The actor launch environment
  now overrides both Cargo compiler-wrapper variables to empty whenever it
  assigns private build output. A future cache daemon must be owned inside the
  actor's mount namespace; inherited host daemons are not workspace-safe.

Canary 5 did prove deadline submission, role/worktree separation, custom type and
`fmt` inheritance, and provider forking. Both child histories reference the root
with exclusive ordinal 89; the actual fork call is parent ordinal 87. This proves
the inherited prefix includes the complete call, beyond merely trusting a pane
label. Research/coding observed cached/uncached input counts were respectively
30,592/2,265 and 30,336/373. Watch/fold/follow-up/cleanup acceptance is still missing.

The admission/inspection fix is committed as `54c156a1`; the compiler namespace
fix is `82ae06b5`. Focused evidence: all 17 registry tests passed, including
120 seconds of simulated ownership with a cancelled queue predecessor; a real
offline Cargo compile inside bubblewrap passed with unusable Cargo-config
wrappers, writing output only into its private overlay. `just verify` passed
again (2,422 tests, 217 fixtures, strict lint/format and manifest checks).
The old canary root successfully resumed diagnostic inspection and polled both
retained responses after contention ended; its live state was not lost.
It then completed typed cleanup (`cleanupReceiptComplete = True`), stopped and
forgot both children, retired group 1, and evaluated `actorContext` afterward.
The source, worktrees, and Git history remained intact. That completed test
session was stopped. A replacement named `shoal-hardened-canary-low-6` exposed
another composition-root defect before useful testing: omitting `--model`
inherited the interactive client's globally last-used `gpt-6-astra`. The
expensive canary was killed immediately. Shoal now materializes explicit
project-local model/effort defaults in `.shoal/config.toml`, permits deliberate
per-run CLI overrides, requires the resolved pair at the private host boundary,
and records it in run status. No fresh acceptance evidence is attributed to
canary 6.

Canary 7 (`shoal-hardened-canary-sol-low-7`, run
`c6e13866-5c3a-4539-9c4d-4489f78d528f`) launched without CLI model overrides.
Project configuration, status v4, host arguments, and provider arguments all
selected `gpt-5.6-sol` / `low`. It completed an applicative research/coding fork,
typed watch/fold, retained coding follow-up, and successful typed cleanup. Both
children shared provider parent `01a06f18-f4dc-7662-b610-e3baa077c54c` and reported
`CacheForkedPrefix`. Cached/uncached tokens were 48,512/556 for research,
51,712/531 for initial coding, and 55,424/388 for the retained follow-up; each
sample describes its latest provider response, not cumulative campaign usage.
Cleanup forgot two watches and three responses, stopped and forgot both
children, and retired group 2. Worktrees and candidate commit
`b4d1ee698dc132cd559ef7b20728e6cfcfa30e74` remained intact. The root's untracked
`docs/TIDEPOOL_HASKELL_EXERCISE.md` was preserved and the candidate was not merged.
The coding actor manually overrode `CARGO_TARGET_DIR`, so its successful test
does not establish the default private build-directory behavior; that remains
covered by the deterministic bubblewrap check above.

The live run also exposed a workbench import inconsistency: ordinary units
had a text-triggered `[fmt|...|]` import, but `:type` did not receive that
import and `:info fmt` failed. The correction removes conditional imports and
mounts the promised quasiquoter vocabulary through the shared workbench import
path. Verification must cover discovery and execution together.

Remaining release review: resolve the confirmed inspected-cleanup scope,
deadline bounded-wait, exact campaign membership, and executable-help findings
from the context-tree UX review. Reconcile acceptance against focused and live
evidence; this successful two-child canary alone is not full acceptance.

### Gated successor work

- [ ] Reattach a failed external child application to the same still-live
  actor/machine incarnation only after the provider exposes a durable idle and
  reattachment contract that makes duplicate turns impossible.
- [ ] Generalize root source-only successor recovery to independently owned
  child machines when Shoal introduces that machine boundary; report every
  lost live binding/handle explicitly and issue fresh exact incarnations.
- [ ] Implement R4 only as owner-emitted, versioned actor/request/watch/fork
  transitions with replay tests at every commit point. Do not infer this
  ledger from composition-root observations or serialize actor tasks.
- [ ] Add handle-filtered `:trace`/campaign renderers only when another live
  campaign shows that the existing typed `observeCampaign`, `:lineage`,
  `:status!`, and `:trace` views are insufficient; no renderer may own state.
- [x] Complete the fresh low-effort provider canary and record its spot-check
  evidence below.

These are explicit architecture gates, not hidden incomplete behavior in the
landed API. The first three require new owner/provider prerequisites and are
not completion criteria for the live-process hardening wave.

## Handoff contract

This document is the implementation handoff for the next Shoal hardening wave.
The intended executor is one lower-effort model working linearly, one checked
slice at a time. It should not have to rediscover product decisions from the
incident transcript or invent architecture while editing.

Read the implemented unfold decision record above, the root contributor guide,
the root mechanism index in `CLAUDE.md`, and the nearest subsystem guide before
each slice. Then follow the ordered handoff near the end of this document.

The executor should:

- preserve every frozen decision below unless a named implementation spike
  disproves a prerequisite;
- extend the existing owner of identity, scheduling, persistence, mounts,
  compilation, or delivery rather than creating a parallel mechanism;
- complete and commit one coherent slice before starting the next;
- run the smallest owning check after each slice and the broad Shoal boundary
  gate only at the final integration point;
- record an unexpected architectural blocker in this plan before changing the
  public interaction model; and
- leave no temporary ignored test at the final gate.

This is deliberately more than a bug list. The target is the resident
environment an LLM should be able to inhabit for a long campaign without
turning orchestration into its main intellectual task.

## North star: exact forks, typed folds, local failure

The product sentence is:

> Fork context exactly, narrow authority explicitly, and fold typed evidence;
> ordinary mistakes stay local and durable runtime truth is always queryable.

From the permanent root's perspective, the good path should require only five
ideas:

1. Define useful domain types and helpers in ordinary Haskell.
2. Describe one independent frontier with applicative `Unfold`.
3. Let each persistent child continue one named branch from the exact shared
   model and Haskell prefix.
4. Register a typed `Watch`, end the model response naturally, then fold
   `ResponseResult` values and `WorktreeEvidence` when reactivated.
5. Refine retained actors or unfold another frontier from what the fold taught
   the parent.

Everything else—provider thread forking, prompt-cache reuse, actor scheduling,
workspace mounts, result-cell custody, wakeup delivery, resource leases, and
logs—is runtime machinery. It must be observable when useful, but it must not
be ritual the model performs on every branch.

The design optimizes for these model outcomes:

- almost no restatement of accumulated intent when work fans out;
- readable names and typed handles instead of copied hashes;
- compile-time guidance for the happy path and structured recovery for the
  unhappy path;
- no uncertainty about whether an effect happened after a tool failure;
- no global host failure from one child, one reply, one bad pattern, or one
  cleanup step;
- no need to scrape tmux, prose notifications, or physical paths to learn
  authoritative state; and
- no forced teardown of expensive learned contexts merely because one request
  has settled.

## Frozen product decisions

These decisions are inputs to implementation, not questions for the executor.

1. **A model response ending is the turn boundary.** There is no root-facing
   `complete`, `yield`, `park`, `nextTurn`, or equivalent Haskell operation.
   The root is permanent and only its supervisor can terminate it.
2. **Replies settle requests, not actors.** `Replies` remains one neutral,
   fixed effect. A typed `Reply a` authorizes exactly one `a`; successful
   settlement performs an irreversible terminal transfer. Actors remain
   addressable afterward.
3. **An unanswered request remains pending.** Ending a child response without
   `respond` does not fail or abandon its request. This supports multi-turn
   child orchestration. The workbench reports the still-open request, and an
   optional typed deadline or explicit cancellation bounds forgotten work.
   Do not add `retainReply` ceremony.
4. **Request bindings exist only in a request scope.** `sessionInput` is the
   typed request payload and says nothing about a reply/output type;
   `sessionReply`/`respond` are the separate typed settlement authority. Root
   startup and event/watch reactivations do not invent `sessionInput :: ()`.
   Their reasons are typed activation events in `ActorContext`.
5. **Waking on result readiness is opt-in.** A ready `Response a` is durable
   and pollable but does not spend an inference turn by itself. Registering a
   `Watch a` is the explicit typed subscription that makes its terminal
   condition reactivating.
6. **No deadline is the default.** Persistent actors should not expire merely
   because the caller omitted policy. When supplied, time is dimensional;
   ordinary examples use seconds or minutes and never a bare millisecond
   integer.
7. **Every child inherits the exact fork prefix.** It sees the complete parent
   conversation and hosted `unfold` call containing every sibling input, but
   not the parent-only tool result. Its incremental prompt is a small branch
   selector plus an authoritative role/resource delta.
8. **Context inheritance is not authority inheritance.** The child's Haskell
   row is statically narrowed and its Rust grants/native-tool policy are
   independently attenuated. Remembering a parent capability never grants it.
9. **Roles are coherent defaults.** Research means inspect without builds,
   tests, formatters, generators, installers, or artifact-producing commands.
   Coding receives one writable worktree and stable build resources.
   Scaffolding may recursively unfold its subtree. Integration may use the
   conservative typed merge path. Custom rows remain possible.
10. **Actors are handles, not one-shot futures.** An unfold returns persistent
   `Forked a` values immediately. The parent may request follow-ups from the
   same learned context and branch.
11. **Each folded value carries custody evidence.** User-defined child values
   stay user-defined; the runtime wraps them in `ResponseResult a` with an
   `ExecutionReceipt` and `WorktreeEvidence`. Do not force every domain result
   to hand-encode Git facts.
12. **Haskell is the orchestration language.** Keep the GHCi-shaped tool,
    ordinary declarations, functions, lenses, quasiquotes, applicative
    composition, and typed handles. Do not replace them with JSON workflow
    specifications or an opaque campaign engine.
13. **Git remains visible and useful.** Typed worktree allocation, evidence,
    and conservative merge cover the safe common path. If a merge conflicts or
    requires history surgery, the appropriate writable actor uses ordinary
    Git. Tidepool must not recreate Git's full API.
14. **One canonical workspace path is a feature.** Every actor sees
    `/tmp/tidepool-actor-workspace`. The runtime changes the backing mount, not
    the prompt. Physical cache/worktree paths appear only in deep diagnostics.
15. **Human labels are not idempotency keys.** Labels aid cognition and status.
    Runtime-generated operation identity makes transport retry safe. Reusing a
    readable label in a genuinely new Haskell call means new intent unless the
    domain operation itself says otherwise.
16. **Rust owns runtime truth.** Haskell is the typed semantic surface; status,
    lineage, trace, and notifications are projections. Rendered strings never
    decide lifecycle, authorization, retry, or custody.
17. **The live Haskell heap is not a serialization format.** Functions,
    closures, existential values, user-defined sums, typed cells, and arbitrary
    bindings stay live and rooted in the resident machine. Tidepool must not
    impose `ToJSON`, `Checkpointable`, or a lowest-common-denominator wire
    schema on the ordinary actor API. Durable operational facts and replayable
    source are useful; pretending an arbitrary heap can be reconstructed is
    not. Losing a machine terminally ends every exact actor incarnation whose
    semantic roots it owns; recovered actors are successor incarnations with
    fresh handles.
18. **Failures are scoped to the smallest owner.** A bad observational command
    is one diagnostic. A Haskell unit failure stops that unit's suffix. A
    request/JIT failure settles that request unavailable. A child process or
    cleanup failure degrades that actor. Only a root/core invariant breach may
    end the fleet host.
19. **Automatic context merge is not a goal.** Exact understanding fans out;
    typed results, receipts, reasoning deltas, commits, and explicit parent
    decisions fold back in.

## The interaction is an iterative hylomorphism

Shoal should expose the structure without forcing the model to name the
recursion scheme on every use:

```text
current understanding + typed seed
                |
                | coalgebra: choose one independent frontier
                v
       applicative Unfold of persistent actors
          /             |              \
 exact context      exact context      exact context
 + branch delta     + branch delta     + branch delta
       |                  |                  |
 typed result +      recursive Unfold   typed result +
 worktree evidence   then local fold    worktree evidence
          \             |              /
           applicative Await / durable Watch
                            |
                            | algebra: integrate evidence
                            v
              refined understanding + next seed
                            |
                       another wave
```

`Unfold` is one free-applicative frontier, not a new traversal runtime. Its
shape lets the parent declare all independent children before any selected
branch runs, which gives atomic admission, exact prefix sharing, and result
order independent of completion order. `Await` is the corresponding
applicative fold description; `Watch` makes readiness durable and
reactivating. A data-dependent next wave is ordinary later Haskell after the
first fold.

Do not add a JSON-shaped `SplitPolicy` with `WaitAll`, `FailFast`, or
`CollectAvailable` modes. Hard descendant depth/concurrency ceilings remain
runtime role policy. Semantic wait/failure behavior is expressed by composing
`awaitFork`, `awaitSettledFork`, cancellation, several watches, and ordinary
Haskell functions. That is both more expressive and one less policy enum for a
model to memorize.

For recursively computed trees, authored code may use `Tidepool.Swarm`'s
`PlanF`, `hyloM`, or `hyloConcurrentM`, but Shoal must not bake a universal
workflow AST into the host. The model remains free to define new node, result,
review, budget, and integration types as it learns. Each recursive actor can
run its own applicative frontier because it inherited the declarations and
plan that motivated it.

### Worked multi-wave root interaction

The exact domain records belong to the campaign. A representative root might
define:

```haskell
data PatchReport = PatchReport
  { patchSummary :: Text
  , patchChecks :: [Text]
  , patchCaveats :: [Text]
  } deriving (Show, Eq)

data Verdict = Accept | Revise | Reject
  deriving (Show, Eq)

data ReviewReport = ReviewReport
  { reviewVerdict :: Verdict
  , reviewFindings :: [Text]
  } deriving (Show, Eq)

data FirstWave = FirstWave
  { domainWorker :: Forked PatchReport
  , interfaceWorker :: Forked PatchReport
  , semanticsWorker :: Forked ReviewReport
  }

let Right campaignName = campaignLabel "pinned-references"
let Right buildGroup = forkGroupLabel "implementation"
let Right domainName = branchLabel "domain"
let Right interfaceName = branchLabel "interface"
let Right semanticsName = branchLabel "semantics"

workers <- unfold (batch campaignName buildGroup) $
  FirstWave
    <$> child
      (coding @PatchReport domainName projectHead domainPlan
        & withBranchGuidance [fmt|Own domain semantics; preserve the agreed invariants.|])
    <*> child
      (coding @PatchReport interfaceName projectHead interfacePlan
        & withBranchGuidance [fmt|Own the model-facing presentation only.|])
    <*> child
      (researching @ReviewReport semanticsName projectHead reviewPlan)
```

This is the final executable unit in its hosted call. All children inherit the
whole call, including the three plans and shared declarations. The parent gets
typed handles in the tool result; it does not append watches after the unfold
commit in the same call.

In the next call the root describes the fold:

```haskell
data FirstFold = FirstFold
  { domainResult :: ResponseResult PatchReport
  , interfaceResult :: ResponseResult PatchReport
  , semanticsResult :: ResponseResult ReviewReport
  }

let Right firstWaveLabel = watchLabel "pinned-references/implementation"
firstWaveWatch <- watch firstWaveLabel $
  FirstFold
    <$> awaitFork (domainWorker workers)
    <*> awaitFork (interfaceWorker workers)
    <*> awaitFork (semanticsWorker workers)
```

The response ends normally. A durable, labelled transition wakes the root when
the fold can be observed. `pollWatch firstWaveWatch` returns typed evidence;
`:campaign` and `:lineage` explain the operational state without changing it.

If the review calls for a refinement, the root keeps the original interface
actor and sends a new typed request rather than recreating its context:

```haskell
let Right revisionLabel = requestLabel "interface/accessibility-revision"
revision <- requestWith @PatchReport
  (forkedActor (interfaceWorker workers))
  (requestOptions revisionLabel revisionPlan
    & withRequestGuidance "Address only the accepted review findings.")
```

The eventual `ResponseResult PatchReport` carries fresh execution and
worktree evidence from the same actor and named branch. The root or retained
coordinator uses `tryMerge` for the clean, conservative case and ordinary Git
only when the typed outcome says human/model judgment is required.

### Recursive coordinator interaction

A scaffolding child selected as `implementation/coordinator` already remembers
the root's entire approved plan. Its role delta tells it that its selected
typed input is `sessionInput`, that it may recursively unfold, and which
effects/native resources it actually has. It can define a local result shape
and run:

```haskell
data Leaves = Leaves
  { runtimeLeaf :: Forked PatchReport
  , workbenchLeaf :: Forked PatchReport
  , adversaryLeaf :: Forked ReviewReport
  }

let Right leavesName = forkGroupLabel "leaves"
leaves <- unfold (subgroup leavesName) $
  Leaves
    <$> child (coding @PatchReport runtimeName boundHead runtimePlan)
    <*> child (coding @PatchReport workbenchName boundHead workbenchPlan)
    <*> child (researching @ReviewReport adversaryName boundHead adversaryPlan)
```

The descendants share the coordinator's exact provider prefix and immutable
Haskell declaration/binding snapshot. They receive only “continue branch
`<allocated-path>`” plus their effective role delta. The coordinator watches,
folds, integrates, and replies with one typed coordinator report. The root sees
both the domain result and authoritative integrated worktree evidence.

### What the root should wake up to

A reactivation should begin with a compact structured notice, for example:

```text
watch pinned-references/implementation (watch 12) became ready
event sequence 418; actor shoal-root activation 31
3/3 dependencies terminal; pollWatch firstWaveWatch is authoritative
```

It should not dump all actor state into the prompt, invent path changes, or
claim that a pushed notification was consumed merely because the backend
accepted it. The root can ask for detail through typed handles, `:status`,
`:campaign`, `:lineage`, or `:trace`.

## Three planes, one truth

The implementation should keep three planes explicit:

| Plane | Owns | Must not own |
|---|---|---|
| Haskell semantic plane | domain types, helpers, `Unfold`, `Await`, `Watch`, opaque handles, typed effect intent | actor scheduling, process paths, provider protocols, durable IDs |
| Rust runtime-truth plane | exact identities, lifecycle, authority, request/watch/fork states, resource leases, event ordering, provider and worktree correlation | campaign policy or model-authored result schemas |
| Projection plane | Haskell observations, workbench receipts, status/lineage/trace text, notifications, logs | independent state or control decisions inferred from rendered text |

Every projection carries stable IDs and a sequence/watermark back to its owner.
No UI, tmux pane, prompt sentence, or log parser becomes authoritative. The
same owner transition feeds all projections so they cannot disagree by
construction.

The planes deliberately have different durability. The Rust ledger can retain
that actor 7 accepted request 12, that worktree 4 points at commit `abc...`, or
that a reply became unavailable. A replayable declaration log can retain the
source of `data Review = ...`. Neither can serialize a closure captured in
`reviewPolicy`, a polymorphic lens composition, an arbitrary
`ResponseResult Review`, or the Haskell projection stored inside a
`Watch Review`. Those values
survive by keeping the resident machine alive. If that machine is genuinely
lost, recovery reports the semantic loss instead of fabricating a value from
operational metadata.

## Model-facing effect surface

The existing split is already close to the right abstraction:

| Effect | Model intent | Ordinary roles |
|---|---|---|
| `Replies` | request, poll, cancel, abandon/forget, and settle typed replies | all |
| `Watches` | name and observe applicative readiness | all |
| `ActorContext` | inspect this activation, role, lineage identity, and effective resource posture | all |
| `BoundWorktree` | inspect the actor's one mounted checkout | research, coding, scaffold, integration |
| `Forks` | atomically admit/abort/commit an exact-context frontier | root, scaffold, research coordinator |
| `AgentInspection` | observe exact actors and a derived subtree/campaign snapshot | root and coordinators |
| `AgentControl` | stop/forget owned descendants | root and coordinators |
| worktree registry/allocation/integration effects | manage the safe common custody path | only roles that need each operation |

Do not add `Capabilities`, `ResourcePolicy`, `RuntimeDiagnostics`, generic
filesystem/shell, Git-command, journaling, or model-turn lifecycle effects in
this wave. Static role rows, typed `ActorContext` facts, Rust grants, native
tool policy, ordinary coding tools, and meta diagnostics already cover those
needs with clearer owners.

Effect membership still does not prove runtime authority. Every denied actor,
worktree, fork, merge, or process operation returns a typed domain error that
contains a shared `AuthorityFailure` shape (principal, requested operation,
resource identity when discloseable, and denial class). It must not surface as
an effect-level infrastructure failure or require matching rendered text. Keep
that record embedded in the owning error ADT rather than adding a generic
authorization effect.

Add a new effect only if a production Haskell consumer needs a permission
boundary that the table cannot express. In particular:

- whole-campaign observation should initially be a read-only operation under
  `AgentInspection`, derived from causally tagged actor/fork/request facts and
  observed provider/worktree samples;
- deep runtime traces are workbench meta commands, not values a domain program
  branches on;
- cleanup composes existing control/forget primitives and has a supervisor
  entrypoint; it is not a second lifecycle effect; and
- do not add a generic checkpoint effect over Haskell values. If a future
  campaign proves that intermediate progress needs a first-class channel, add
  the narrowest typed progress effect then; replies, requests, watches, and Git
  commits are sufficient for this wave.

`ActorContext` should grow only self-local, stable facts the model otherwise
has to infer: exact actor incarnation, readable path, role, activation/request
identity, context parent, supervisor, provider parent, bound worktree/access,
and the effective compute posture (`InspectionOnly`, `BuildAllowed`, or the
smallest useful equivalent). It must not expose a pretend authorization bitset
that can drift from the Rust checks.

### Target public deltas

These signatures are the intended shape. Exact record field names may follow
existing generated-style conventions, but the types and effect placement are
decided.

```haskell
-- Unit-safe time. Constructors do not expose stored milliseconds.
data Duration
milliseconds :: Natural -> Duration
seconds      :: Natural -> Duration
minutes      :: Natural -> Duration
after        :: Duration -> RequestDeadline

-- Readable namespaces remain distinct from authority and Git object identity.
data ActorPath
data GitBranchPrefix
data ObservedAt

data ActivationKind
  = RootStarted
  | RequestActivated RequestId
  | EventsActivated [ActorEventRef]
  | SupervisorActivated SupervisorEventRef

actorGitPrefix :: ActorPath -> GitBranchPrefix
withGitBranchPrefix :: GitBranchPrefix -> WorktreeQuery -> WorktreeQuery
withinForkGroup :: ForkGroupHandle -> WorktreeQuery -> WorktreeQuery
createdAfter :: ObservedAt -> WorktreeQuery -> WorktreeQuery

-- Read-only projections over existing owners.
observeFork
  :: Member AgentInspection effs
  => Forked result
  -> Eff effs ForkObservation

observeCampaign
  :: Member AgentInspection effs
  => ForkGroupHandle
  -> Eff effs CampaignSnapshot

-- Mechanical dependency cleanup, never Git/worktree deletion.
planCleanup
  :: Member AgentInspection effs
  => ForkGroupHandle
  -> Eff effs CleanupPlan

executeCleanup
  :: ( Member AgentInspection effs
     , Member AgentControl effs
     , Member Replies effs
     , Member Watches effs
     , Member Forks effs
     )
  => CleanupPlan
  -> Eff effs CleanupReceipt
```

`ForkObservation` combines the immutable `BranchReceipt` with later
application/provider/cache/request/worktree observations and their times.
`CampaignSnapshot` is a recursively shaped read-only projection keyed by the
existing exact fork-group identity. `CleanupPlan` embeds those exact identities
and preconditions, not only readable labels. `CleanupReceipt` has one outcome
per step and can be used as the input to a retry/resume helper.

Do not expose `OperationId` in these ordinary signatures. It appears in
structured receipts and deep tracing, and it is accepted optionally by the
supervisor retry path. The Haskell model should not manufacture or thread
correlation tokens during healthy work.

`BranchReceipt` should be completed to the contract already recorded in the
implemented unfold plan: requested/allocated typed paths, fork group and exact
actor identity, supervisor/context/provider source, Haskell declaration and
binding snapshot, role/profile/effect row, worktree/start head/access, and
descendant reservation. Later provider binding/usage stays in
`ForkObservation`; do not mutate the supposedly immutable admission receipt.

The existing `forkGroupHandle :: Forked result -> ForkGroupHandle` is adequate
for now: a named record gives the root an obvious branch from which to obtain
the group handle. Do not wrap every successful unfold in another envelope
unless a second dogfood run shows that this is real friction.

`ActorContextInfo` includes `ActivationKind` and the current durable-event
watermark. `sessionInput`, `sessionReply`, and `respond` are generated only for
`RequestActivated`; the other constructors expose their event references
through `actorContext`/inspection. Tool instructions must describe the mounted
names actually present, never infer an output contract from an input binding.

## Prompt and context architecture

Prompt construction is part of cache behavior and model correctness. Build it
as four explicit layers:

1. **Stable Shoal prelude.** A versioned, highly cacheable system/developer
   prefix explaining the permanent actor model, Haskell workbench, exact
   context forks, typed replies/watches, Git custody, canonical workspace, and
   how to ask for status/docs.
2. **Project guidance.** The relevant contributor instructions and the
   checkout's `SHOAL.md`/`AGENTS.md` guidance, loaded without rewriting the
   stable prelude for every activation.
3. **Authoritative role delta.** Appended after a context fork. It explicitly
   supersedes inherited parent-role instructions, identifies the selected
   branch, states the narrowed effect row and native resource posture, and for
   inspection roles says not to run build-like commands at all.
4. **Activation facts.** A tiny typed request/event notice. Domain payload is
   `sessionInput` and reply authority is `sessionReply` only for request
   activations. Root/event activations expose typed event references through
   `ActorContext`. Dynamic fleet state is queried, not pasted into every
   prompt.

The root prelude should encourage delegation when a bounded assignment and its
evidence transfer faithfully. It should keep synthesis in the root when the
full conversation, operator taste, or cross-branch decision history is the
essential input. “Always delegate implementation” is too absolute.

The child delta should resemble:

```text
You are the retained actor at pinned-references/implementation/semantics.
This activation is an exact context fork at snapshot <typed-id>; all inherited
conversation is context, not authority. Continue only branch `semantics` from
the visible unfold call. Your typed payload is sessionInput and only this actor
owns sessionReply. Effective role: inspection. Inspect existing evidence; do
not run builds, tests, formatters, generators, installers, or artifact-producing
commands. The workspace alias remains /tmp/tidepool-actor-workspace.
```

The role delta, prompt-profile identity, selected branch, and provider-fork
receipt must be logged as hashes/typed fields so a cache miss can be explained.
Do not make the ordinary model transcript discuss physical backing paths or
cache implementation.

## Scope and evidence

The evidence source is the 2026-09-04 read-only observation of Shoal run
`c7580ee6-f6e8-4fa7-a4ab-a11b714d7132` in `shoal-console`. The campaign used a
root discovery unfold, retained-actor recovery, a recursive coordinator-owned
implementation unfold, typed worktree folds and merges, independent reviews,
and a retained-child refinement wave.

The feature campaign itself completed: `shoal-console/main` fast-forwarded to
verified integration head `57e57ffe3e5956174c6c718e57a45fe08bb1a877`, and
the source checkout's pre-existing untracked exercise note remained untouched.
The subsequent lifecycle-cleanup campaign did not complete because stopping
the first recursive leaf was followed by loss of the dynamic-tool host.

The core interaction model worked:

- a dirty source checkout rejected the whole first unfold before partial
  publication; retrying from clean `HEAD` launched the group;
- root-launched children inherited the root provider thread and Haskell
  snapshot, while recursively launched children named the retained coordinator
  as supervisor, context parent, and provider-thread parent;
- hierarchical actor paths projected to readable `shoal/<path>/branches/<leaf>`
  Git branches and distinct managed worktrees;
- every actor used the same actor-relative
  `/tmp/tidepool-actor-workspace` path without path-change instructions;
- narrowed research actors had inspection-only native tools and no build or
  mutation behavior; coding leaves had writable bound worktrees but no launch
  or integration effects;
- retained follow-up requests reused typed values and model context instead of
  replacing actors;
- `ResponseResult` plus `WorktreeEvidence` carried exact request, actor,
  worktree, base, head, committed paths, and dirty-state evidence;
- typed merges integrated a fast-forward followed by two merge commits without
  requiring a manual Git mutation fallback;
- the coordinator and root independently reproduced formatting, 48 tests,
  strict Clippy, legacy smoke compatibility, and the new smoke contract;
- review findings flowed root -> retained coordinator -> retained presentation
  child, producing and merging a focused follow-up commit without spawning a
  replacement.

Cache reuse was not merely assumed. Reported cached/uncached input tokens were:

| actor/activation | cached | uncached | cached share |
|---|---:|---:|---:|
| retained product recovery | 57,984 | 384 | 99.3% |
| retained architecture recovery | 66,688 | 1,501 | 97.8% |
| retained coordinator recovery | 57,472 | 316 | 99.5% |
| recursive domain child | 89,600 | 615 | 99.3% |
| recursive presentation child | 107,136 | 675 | 99.4% |
| recursive smoke/failure child | 94,464 | 742 | 99.2% |
| retained adversarial reviewer | 90,240 | 229 | 99.7% |
| retained integration reviewer | 85,632 | 215 | 99.7% |
| retained coordinator final verification | 99,712 | 606 | 99.4% |

These measurements, provider-parent thread identities, fork-group identities,
and inherited transcripts together establish that the run exercised context
forking with prefix-cache reuse rather than unrelated fresh sessions.

## Preserve these boundaries

- Keep the canonical actor-relative workspace path. Models should not receive
  relocation prose or physical cache paths when actors fork or resume.
- Keep context inheritance exact and authority inheritance explicit. Effect-row
  narrowing and Rust grants remain separate mechanisms.
- Keep actors persistent. A reply settles one request; it does not terminate
  the actor or discard its context, bindings, or worktree.
- Keep Git legible. Typed worktree and merge operations provide the safe common
  path and authoritative receipts; ordinary Git remains available for review
  and exceptional conflict handling.
- Keep the GHCi-shaped Haskell surface. Improve its types, diagnostics, and
  prelude rather than replacing it with serialized tool-call records.
- Keep root synthesis and integration explicit. Contexts fork exactly; evidence
  rejoins through typed results and receipts.

## Runtime contracts to implement

### Failure domains

Use a closed failure classification at the host boundary. Names may follow the
owning crate's vocabulary, but the behavior is fixed:

| Failure class | Example | Required effect |
|---|---|---|
| diagnostic | unknown `:info` name, incomplete-pattern warning | record on that input unit; continue independent later diagnostics |
| unit rejection | type error, forced pattern-match failure, rejected effect | preserve the committed prefix; mark the remaining units not run |
| activation failure | bad reply continuation, JIT trap scoped to one request | settle that request unavailable; keep actor and fleet inspectable when the machine is safe |
| actor degradation | pane exit, delivery failure, build lease loss, retirement cleanup error | transition that actor to a typed degraded/failed state; notify its supervisor |
| fleet-fatal invariant | root kernel ownership corrupted, actor registry internally inconsistent | emit the full structured cause, preserve durable ledger, then stop the host |

Do not turn an actor-scoped task `JoinError` into a fleet-fatal error merely
because it was awaited by the composition-root `JoinSet`. Conversely, do not
launder a genuine registry or machine-custody invariant into a child failure.
The enum boundary should force the caller to choose.

Retirement is a state machine, not a fallible destructor:

```text
Running -> StopRequested -> Retiring -> Retired
                                 \-> CleanupDegraded { completed, failed }
```

Every cleanup component is attempted exactly once per operation and returns a
structured per-component receipt. A pane-kill error must not skip delivery
shutdown, socket retirement, binding settlement, or the supervisor notice.
Retry targets only incomplete components. `CleanupDegraded` remains observable
and never takes the permanent root host with it.

### Request admission is one logical transaction

The current Haskell implementation exposes `reserveRequest` followed by
`submitRequest`. That split lets a failure between the effects leave a runtime
reservation whose publication status the model cannot determine. Replace the
public/internal call boundary with one logical request-admission operation:

```text
validate caller + exact target + result fingerprint + deadline
  -> allocate RequestId and root custody
  -> enqueue request activation
  -> publish Response handle/receipt
```

The owner may retain private prepare/commit mechanics if needed for live-value
construction, but rollback is handler-owned and no reserved ID is observable
until submission is committed. The preferred implementation is one internal
effect request carrying a live request blueprint/cell and returning the
committed ID. If invoking that blueprint safely from the handler would violate
the existing evaluator boundary, the acceptable fallback is an
operation-scoped reservation guard: the two private effect steps share one
`OperationId`, the workbench automatically aborts the reservation when the
unit exits without commit, and neither constructor is exported or browseable.
In either implementation, `request`/`requestWith` are one semantic transaction
and a submission refusal is a typed `RequestAdmissionError` rather than an
apparently pending `Response`. The ordinary convenience may reject that input
unit, matching `unfold`; add an `attemptRequest` returning `Either` only if a
real authored caller needs to branch and recover in the same unit.

The request record pins:

- owner and target exact incarnations;
- request label and runtime operation identity;
- input and result type fingerprints;
- activation and Haskell generation at admission;
- optional dimensional deadline;
- live-root custody for input, reply, response cell, and later result; and
- bound-worktree identity/base observation needed for result evidence.

This state remains in the existing actor request owner. Do not add a second
response store or a Haskell-visible registry snapshot.

### Reply settlement has an irreversible commit point

The target-side state and requester-side state are related but not identical:

```text
reply authority: Open -> Claimed -> Closed
                                  \-> ClosedSettlementFailed

response: Pending -> Settling -> Ready
                               \-> Unavailable ResponseSettlementFailed
          Pending -> CancellationRequested -> Unavailable ...
```

`attemptReply :: Reply a -> a -> Eff effs (Either ReplyError Void)` remains the
recoverable API. A stale, duplicate, unauthorized, wrong-incarnation, or
already-cancelled attempt returns `Left` and leaves the caller active. Once the
runtime returns acceptance internally, success never returns to Haskell and
the first value wins.

The settlement sequence is fixed:

1. Validate exact reply authority and claim the open request atomically.
2. Root the live result and run the private requester-side settlement path
   against the pinned result fingerprint/generation.
3. Observe the bound worktree and attach `WorktreeEvidence`; an observation
   error is itself typed evidence, not an excuse to discard the domain value.
4. Fill the Haskell result cell.
5. Publish `ResponseReady` in the request owner.
6. Record the response transition in the observation ledger and append only
   newly terminal subscribed-watch (or explicit cancellation/supervisor)
   events to durable activation inboxes. An unwatched ready response does not
   wake its owner.
7. Schedule idle subscribed owners or mark one later activation pending for
   active owners.
8. Release transient roots and transfer terminal custody to the response/watch
   handles.

If steps 2–4 trap after the reply was claimed, mark both reply and response
terminal with a typed settlement failure and preserve diagnostic correlation.
Never call `rollback_reply` back to `Open`; the child cannot safely guess
whether the first value crossed the commit point. An old reply handle can never
target a restarted incarnation.

The strongest ordering assertion is: whenever a notification says a response
or watch is ready, `pollResponse`/`pollWatch` already observes that state and
the typed result cell is already filled.

### Watches are durable readiness, not continuations

`Await` remains a pure applicative dependency description and typed
projection. `Watch a` registers that description with the request owner and
retains the Haskell projection/root needed to produce `a`. It is not a model
turn, actor lifecycle operation, or serializable workflow node.

For every terminal request transition, the request/watch owner atomically:

1. commits authoritative request state;
2. recomputes affected watches once;
3. commits each newly terminal watch exactly once;
4. appends sequenced events to the existing `DurableInbox`; and
5. schedules at most one future activation per currently active application.

This ordering closes the poll-then-idle lost-wakeup race. Several events that
arrive during one active model response may be batched into its next
activation, but every event keeps its sequence and every handle stays
independently pollable. A delayed notification includes its event sequence and
the handle's current state, making staleness obvious rather than silently
misleading.

Backend `push` acceptance advances the delivery cursor under the existing
`DurableInbox` contract. It does not prove the model read or acted on the
message; UI text and metrics must use those words precisely.

### Runtime operation identity and safe retry

Every hosted Haskell effect receives an internal `OperationId` derived from
stable execution coordinates, conceptually:

```text
RunId
  + ActorRef exact incarnation
  + ActivationId
  + HostedToolCallId
  + InputUnitIndex
  + EffectOrdinalWithinUnit
```

The concrete representation belongs with monotonic IDs and actor activation
metadata; do not stringify this tuple and parse it later. The workbench records
the completed operation IDs alongside each unit receipt. A durable operation
receipt stores kind, targets, commit disposition, and runtime-minted IDs—not an
arbitrary Haskell argument or result.

The retry contract is intentionally narrow:

- a transport retry of the same hosted call/unit returns the already committed
  receipt or continues from its recorded boundary without repeating effects;
- a new hosted call, even with identical Haskell source or label, is new
  intent;
- each effect owner makes its own mutation safely repeatable where meaningful
  (request admission, reply claim, watch registration, worktree merge, stop,
  forget, cleanup);
- arbitrary native shell commands and manual Git sequences are not covered by
  a fictional universal idempotency guarantee.

Returning an already committed workbench response is valid only while the same
resident environment still contains its installed bindings. After machine
loss, the recovery report may prove that an operation committed, but it never
re-executes the unit or claims its arbitrary result binding survived.

Labels remain editable presentation. No request, watch, unfold, merge, or
cleanup deduplicates merely because a model reused a human name.

### Durability and recovery without serializing Haskell

“Durable” must always name a failure domain. Use this matrix in API docs,
status, and tests:

| State | Normal owner | Survives model turn | Survives child process restart | Survives Haskell machine loss | Survives host restart target |
|---|---|---:|---:|---:|---:|
| provider thread identity/history | provider backend | yes | yes | yes | yes, when backend reattach succeeds |
| actor/request/watch/fork lifecycle facts | `tidepool-actor` | yes | yes | operational facts only | yes after actor-ledger phase |
| worktree receipts, Git refs, commits, dirty observations | `tidepool-worktree`/Git | yes | yes | yes | yes |
| accepted declaration source and generation hashes | resident workbench checkpoint | yes | yes | replayable | yes after manifest phase |
| opaque runtime handle identity/type fingerprint | owning runtime record | yes | yes | old handle is stale; history remains queryable | old handle is stale |
| arbitrary Haskell value, closure, lens, projection, result cell | resident JIT heap/root registry | yes | yes while machine lives | **no** | **no** |
| ready domain result stored only as a live Haskell value | resident JIT heap/root registry | yes | yes while machine lives | **lost; mark unavailable** | **lost; mark unavailable** |
| `Watch a`'s arbitrary projection closure | resident JIT heap/root registry | yes | yes while machine lives | **lost; old dependencies are stale** | **lost** |

There is no generic `Checkpointable` constraint, JSON encoding, heap walker,
closure serializer, or automatic “reconstruction” of a domain value. The
resident process and rooted immutable heap are the persistence strategy for
semantic values. Protect them with fault containment and avoid needless
teardown.

Recovery is tiered:

- **R0: diagnostic recovery.** Compile/type/meta errors and caught Haskell
  pattern failures do not replace the machine. Prior declarations, bindings,
  handles, and effects remain exactly as they were.
- **R1: activation quarantine.** A request-local JIT/settlement fault retires
  the poisoned continuation/resource scope, marks the request unavailable, and
  leaves the actor's environment usable if registry/heap invariants still
  hold.
- **R2: external application recovery.** If Codex/the child process dies while
  the Haskell machine remains safe, reattach the provider thread to the same
  actor incarnation and workspace, then deliver typed pending events. All live
  Haskell state remains because the machine never moved.
- **R3: successors after Haskell machine loss.** Terminally fail every exact
  actor incarnation and pending capability whose semantic roots belonged to
  the lost machine/resource scopes. Start successor incarnations at the same
  readable actor paths, replay only accepted declaration source in generation
  order, reopen durable resources through fresh typed handles, and optionally
  reattach provider threads. Never replay effectful input units. Report ready
  live results, projections, closures, old handles, and dependent bindings as
  lost/stale in structured `RecoveryReport`s.
- **R4: host restart and successor construction.** Rebuild historical
  actor/fork/request facts from the versioned actor ledger, then start new
  exact incarnations for applications the supervisor chooses to resume.
  Reattach provider threads and reopen managed worktrees with fresh handles;
  do not resurrect old actor identities or claim their semantic values
  survived. This is a later gate after R0–R2 are boring; it must not block P0
  containment.

A target report is data, not prose:

```haskell
data RecoveryReport = RecoveryReport
  { failedActor :: ActorIdentity
  , successorActor :: ActorIdentity
  , replayedDeclarations :: [DeclarationGeneration]
  , reopenedResources :: [RecoveredResource]
  , lostBindings :: [LostBinding]
  , transitionedResponses :: [LostResponse]
  , unresolvedOperations :: [OperationId]
  }
```

The exact fields may live only in Rust/meta output initially; the distinctions
must remain. Old typed handles are never installed into the successor. A
resource is reopened by its canonical owner and returns a new handle; a
declaration is replayed from source; every other binding is explicitly lost.
“Unknown whether effect happened” is an unresolved operation, not a reason to
replay it.

The persistent manifest contains only replayable or operational material:

- actor/run/incarnation and provider-thread correlations;
- accepted declaration source, imports, generation/content hashes, and the
  last committed unit boundary;
- operation IDs and structured effect receipts;
- durable actor/request/watch/fork/worktree IDs, labels, states, and type
  fingerprints for history and terminal settlement—not for reviving an old
  incarnation; and
- recovery/version metadata through the existing durable-format ladder.

It never contains encoded arbitrary Haskell values. Use the shared durable
JSONL/atomic-write/versioning mechanisms named in the root mechanism index.
`tidepool-runtime` owns declaration checkpoints; `tidepool-actor` owns lifecycle
events; the Shoal composition root persists their projections. Do not create a
second actor registry or event scheduler in the recovery layer.

The R4 actor ledger is an append-only record of owner transitions and cleanup
obligations, not serialized Ractor tasks, mailboxes, continuations, or registry
internals. On restart it reconstructs historical observations and tells the
real actor kernel which successor incarnations the supervisor elected to
start. The kernel still performs every live admission and issues every fresh
handle.

### Observability schema and cache semantics

One typed actor event envelope should feed logs, durable inbox notifications,
`:status`, `:campaign`, `:lineage`, and `:trace`:

```text
ActorEventEnvelope
  sequence / occurred_at
  run / actor incarnation / activation / operation
  optional request / watch / fork group / worktree
  typed ActorEvent payload
```

The payload enum includes lifecycle, request/reply/watch transitions, fork
admission, provider binding/usage, resource lease, worktree observation/merge,
workbench unit, recovery, and cleanup events. Each owning mechanism emits its
transition; the projection layer enriches and renders it. Avoid a generic
string event whose downstream consumers must classify text.

Cache metrics require especially honest semantics. The current
`ActorRuntimeObservation` overwrites one latest cached/uncached pair read from
the provider rollout, so it is an activation sample, not a campaign cumulative
counter. Replace ambiguous scalar fields with a small per-activation time
series or last-sample-plus-history projection containing:

- actor and activation IDs;
- provider thread and provider-parent thread;
- inherited context snapshot/generation when the provider supplies it;
- prompt/prelude version and role-delta fingerprint;
- cached and uncached input tokens for this provider response;
- derived cached share with an explicit zero/unknown case;
- fork-to-first-token latency; and
- a typed cache-boundary reason when Tidepool knows one (`Fresh`,
  `ForkedPrefix`, `ReattachedThread`, `PromptProfileChanged`, `ProviderUnknown`).

Do not claim “shared prefix tokens,” incremental cost, or a cause for a miss if
the provider did not expose enough evidence. A successful cache-friendly fork
is established by provider fork lineage plus the provider's usage sample, not
by identical workspace paths or a suggestive label.

Ordinary status shows readable lineage, current state, deadlines, and latest
cache share. `:trace` exposes full IDs/generations. Logs pair the canonical
workspace alias with actor/worktree identity but redact/suppress physical
cache paths from model-facing text.

### Workspace, build resources, and Git

The process boundary owns one invariant: the same visible path resolves to the
actor's intended checkout and access mode. Extend
`tidepool-node::ProcessMountBoundary` with explicit writable overlay mounts
rather than placing mutable build output inside a managed Git worktree.

For each coding actor incarnation:

- allocate a host/session-owned build-resource lease outside the worktree;
- bind-mount it at one fixed visible path such as
  `/tmp/tidepool-actor-workspace/.shoal/build/cargo`;
- set `CARGO_TARGET_DIR` to that visible path;
- keep the mutable target actor-local for its full lifetime;
- clean it only after the lease owner observes actor retirement; and
- omit the lease entirely for inspection-only actors, whose process policy
  refuses build/test/format/generator commands before launch.

Do not put actor IDs into the model-visible target path or ask the model to
select `/tmp` fallbacks. If immutable dependency sharing is later desired, use
an existing content-addressed compiler/cache owner or a proven tool such as the
configured compiler wrapper; never let siblings share Cargo locks and mutable
target state by accident.

Git identity stays readable:

- actor paths use semantic hierarchical names;
- branches project under `shoal/<campaign>/<groups>/branches/<worker>` with
  deterministic `-1`, `-2`, ... collision suffixes;
- opaque actor/worktree/OID values remain in receipts and deep diagnostics;
- `tryMerge` continues to handle clean, conservative integration and returns a
  discriminating outcome (`AlreadyContained`, fast-forward, merge commit,
  conflict/refusal); and
- the source checkout becomes a typed integration target after the runtime
  verifies its identity and dirty state, so the final safe merge need not fall
  out of the typed custody path.

Do not add a general commit-transplant/cherry-pick DSL in this wave. A complex
merge, rebase, or selective transplant is precisely where Git's familiar CLI
and the model's judgment are superior. The typed operation should refuse with
enough source/target/base evidence to make that fallback safe.

## P0: correctness and fault containment

### Keep child teardown subordinate to the permanent host

The final inside-out cleanup successfully forgot the coordinator's two watches
and four responses. Its next input unit issued three reverse-order `stopAgent`
operations. The unit returned only `host dynamic-tool infrastructure failure`;
three subsequent `:status` calls failed identically. The log's last lifecycle
event was actor 8 retiring as `Completed`, its pane disappeared, and actors 6
and 7 remained, proving that the effectful batch partially executed. The host
pane also disappeared while the root and remaining child TUIs stayed open.
No typed stop outcomes or host exit cause were available.

- [ ] Reproduce stopping one leaf and a three-leaf stop batch under a permanent
  root; prove leaf retirement cannot terminate the host service, root tool
  socket, or sibling tool sockets.
- [ ] Audit linked-task ownership so child application/process completion is a
  supervised event, not a failure propagated into the actor host's serving
  loop.
- [ ] Preserve one typed `StopOutcome` per attempted child even when a later
  operation or the enclosing workbench call fails.
- [ ] Make the successful-prefix boundary visible for effectful batches: actor
  8 retired, while later stop outcomes were unknown.
- [ ] Log host shutdown/failure with the triggering actor, lifecycle event,
  task/link relationship, exception/panic, exit status, and affected tool
  sockets before any pane is removed.
- [ ] Keep `:status` and root supervision available after a child teardown
  failure so cleanup can be resumed authoritatively.

### Repair typed reply settlement across resident generations

Two inspection-only reviewers successfully constructed `ReviewResult` values,
but their first settlement attempts produced three runtime failures:

- a JIT heap-shape constructor-tag mismatch;
- a JIT `SIGSEGV`;
- subsequent `no continuation scont_* parked` errors from `respond`,
  `attemptReply`, and a raw `Replies` effect.

Both replies remained open until their deadlines expired. After cancellation,
fresh requests to the same retained actors settled the same retained values
successfully. This localizes the failure to the activation/reply continuation
boundary rather than the value type or actor context.

- [ ] Reduce both failures to fixtures using an inherited user-defined sum and
  record result, a fork activation, persistent declarations, and `respond`.
- [ ] Make the reply operation's result type fingerprint, compiled generation,
  activation generation, and parked continuation agree before execution.
- [ ] Ensure accepted settlement becomes observable before any terminal control
  transfer; a post-settlement JIT fault must not leave an apparently open reply.
- [ ] If an activation continuation is irrecoverably poisoned, atomically mark
  that request unavailable with a typed runtime failure or remount a safe
  settlement boundary. Do not leave an impossible-to-settle `ReplyOpen` until
  deadline.
- [ ] Prove that a fault in one request cannot corrupt later requests or other
  actors sharing an immutable Haskell snapshot.
- [ ] Add the first-attempt failure, retry, cancellation, and retained-actor
  recovery paths to focused acceptance tests.

### Replace untyped deadline integers

`requestDeadline :: Int -> Either Text RequestDeadline` interprets the integer
as milliseconds. Models naturally read `600` as seconds; this caused all three
discovery replies and both initial review replies to miss settlement.

The model-facing API should make time dimensional:

```haskell
withRequestDeadline (after (minutes 10)) options
withBranchDeadline (after (seconds 90)) branch
```

Use `milliseconds`, `seconds`, `minutes`, and `after` unless an existing public
name makes one impossible. The duration constructor is total over a
non-negative integral type; zero means an immediately due deadline and is
useful in tests. The interpreter performs checked conversion to its monotonic
millisecond clock. Bare signed `Int` is not part of the public deadline API. If
a deprecated compact convenience briefly remains, it means seconds and says so
in its name; millisecond precision always requires `milliseconds`.

- [ ] Introduce an opaque shared `Duration` plus total `milliseconds`,
  `seconds`, and `minutes` constructors and `after :: Duration ->
  RequestDeadline`.
- [ ] Accept `Duration` at request and unfold-branch deadline boundaries;
  convert to runtime milliseconds only in the owning request interpreter.
- [ ] Remove `requestDeadline :: Int -> ...` and every raw `Maybe Int`
  deadline from the model-facing modules; keep the runtime wire conversion
  private and explicitly named in milliseconds.
- [ ] Show the authored duration, absolute wall-clock expiry, and remaining
  monotonic time in `:status`; never label a value merely `deadline=600`.
- [ ] Include the label and current terminal state in deadline notifications.
- [ ] Test sub-second deadlines explicitly without making milliseconds the
  ordinary LLM-facing unit.

## P1: observability as a typed runtime surface

The current host log correlates failures by actor, model turn, and tool call,
but the reply/JIT incident required reconstructing request state from several
panes. Raw heap addresses, unknown tags, and a list of parked continuation names
are insufficient operational evidence.

Define one structured, sequenced event model owned by the existing actor/runtime
mechanisms. It is an observation projection, not another scheduler or registry.

- [ ] Correlate every event with `RunId`, `ActorRef`, `ActivationId`, optional
  `RequestId`/`WatchId`, provider thread, Haskell snapshot/generation,
  fork group, and bound `WorktreeId`.
- [ ] Record reply transitions (`Open`, cancellation requested, accepted,
  ready/unavailable), the effect operation, result type fingerprint, and state
  before/after the interpreter call.
- [ ] Record continuation allocation, park, consume, and invalidation with the
  owning activation/request—not only an `scont_*` name.
- [ ] On heap-shape failure, log expected and observed constructor identity,
  compiled function, source input unit, declaration generation, and type
  fingerprint. Keep raw bytes/addresses as optional deep diagnostics.
- [ ] Expose `:trace request <handle>`, `:trace watch <handle>`, and a compact
  `:lineage` tree rather than requiring pane/log archaeology.
- [ ] Have `:lineage` distinguish supervisor, context parent, provider parent,
  fork group, actor path, Git branch, worktree, role/effect row, and state in one
  tree-shaped view.
- [ ] Emit split/fork metrics: context snapshot identity, cached/uncached tokens
  and ratio, fork-to-first-token latency, and queued/running shards. Show
  shared-prefix tokens or aggregate incremental cost only when provider data
  actually supports them; otherwise render `Unknown`.
- [ ] Pair the canonical workspace alias with actor/worktree/branch identity in
  diagnostics, while suppressing long physical cache paths in ordinary model
  transcripts.
- [ ] Give durable notifications a sequence/watermark, label, and authoritative
  current state so stale delivery is recognizable immediately.
- [ ] Replace the overwrite-only `ActorRuntimeObservation` pair with
  activation-scoped `ProviderUsageSample`s. Label any aggregate as an
  aggregate and preserve `Unknown` when the provider omits evidence.
- [ ] Complete immutable `BranchReceipt` provenance and add
  `observeFork :: Forked a -> Eff ... ForkObservation` for later provider,
  request, cache, and worktree state; do not mutate the admission receipt.
- [ ] Derive one causally consistent `CampaignSnapshot` under
  `AgentInspection` from a `ForkGroupHandle`. Actor/request/fork facts carry
  one actor-event watermark; provider/worktree samples keep their own observed
  times. Include supervisor, context, and provider lineages side by side; do
  not create a campaign scheduler.
- [ ] Make `:status` the concise current-actor/action view, `:campaign` the
  scoped tree, `:lineage` the ancestry view, and `:trace` the exact transition
  history. All four read the same typed snapshots.
- [ ] When a workbench unit binds a newly returned response/watch/fork handle,
  retain the exact binding-to-ID association in resident metadata so a notice
  may say `pollWatch firstWaveWatch`. Never recover that association by parsing
  `Show` output; omit the hint when it is not known.
- [ ] Add a typed activation-kind/event projection to `ActorContext`. Generate
  `sessionInput`/`sessionReply`/`respond` only for request scopes and make
  activation notices derive from the actual mounted context.

## P1: workspace and build-cache reliability

Concurrent coding actors correctly saw independent source worktrees through the
same canonical path. However, the runtime-provided actor-specific
`CARGO_TARGET_DIR` disappeared during dependency metadata writes for two actors.
Both recovered by selecting private `/tmp` target directories, paying cold build
cost and leaving disposable directories that their tool policy would not remove.

- [ ] Give each actor a stable build-cache identity and lease for its lifetime.
- [ ] Never clean or replace an active actor's target directory; coordinate
  cleanup through the resource owner after actor teardown.
- [ ] Mount the target at one stable actor-relative location and set toolchain
  environment consistently, without requiring models to invent fallback paths.
- [ ] Log build-cache allocation, mount, lease owner, cleanup, and unexpected
  disappearance as structured resource events.
- [ ] Keep mutable output/lock custody actor-local in this wave. Preserve any
  sharing already provided by the configured compiler/content-addressed cache,
  but do not invent sibling Cargo-target sharing before measurements demand it.
- [ ] Provide a recoverable typed cleanup operation for actor-owned disposable
  verification resources instead of encouraging `rm -rf` workarounds.
- [ ] Put the physical lease outside the Git worktree and add it as a writable
  overlay in `ProcessMountBoundary`; the fixed visible target path must not
  contain actor IDs.
- [ ] Do not allocate or mount a build target for inspection-only roles. Refuse
  known build/test/format/generator commands in native-tool policy before the
  requested subprocess starts, and state the same contract in its role delta.

## P1: workbench ergonomics

- [ ] Make failed observational commands (`:type`, `:info`, `:browse`,
  `:bindings`, `:status`) report their own diagnostic and continue to later
  units. Preserve stop-on-failure for Haskell evaluation and effectful units.
- [ ] Replace the two-way item status with explicit `Committed`, `Diagnostic`,
  `Rejected`, and `NotRun` dispositions. Attach warnings, installed bindings,
  and completed operation IDs structurally rather than inferring them from
  transcript strings.
- [ ] Expose workbench execution posture as typed metadata—compiling,
  evaluating, awaiting a named effect/continuation, returned, or terminal
  reply transfer—so the client never presents “suspended awaiting an external
  event” as an indistinguishable still-running tool call.
- [ ] Compile interactive units without global `-Werror`. An incomplete
  pattern binding such as `let Right handle = receipt` commits with an
  `IncompletePattern` warning. If a mismatch is later forced, catch the
  Haskell pattern exception as that unit's typed `PatternMatchFailure`; do not
  crash or poison the actor host. CI/library warning policy is unchanged.
- [ ] Put the documented `[fmt|...|]` quasiquoter in the default actor
  environment through the existing turn-import path. Add one live canary.
- [ ] Make every name printed by `:browse` resolvable by `:info`; in this run
  `EffectWitness` violated that invariant.
- [ ] Recognize the common `f Constructor { ... }` parse/type-error shape and
  suggest `f $ Constructor { ... }` or parentheses.
- [ ] Investigate fenced declaration parsing that rejected an otherwise normal
  signature plus binding, forcing a long single-line `let` fallback.
- [ ] Render successful terminal reply/cancellation transfer explicitly instead
  of ambiguous `<no output>`.
- [ ] Filter inherited client-only noise such as stale “conversation
  interrupted,” MCP-login, and usage-reset notices from forked semantic context.
  Preserve actual user/developer/model/tool history.
- [ ] Add `:doc <name>` for short executable examples of the small Shoal
  vocabulary. Keep GHCi source classification; do not add separate `:decl` and
  `:effect` languages unless classifier evidence later proves they are needed.
- [ ] Mark bindings in `:bindings` as declaration-backed, live heap value,
  runtime handle, or lost-after-recovery. This is explanatory metadata, not a
  promise to serialize them.

## P2: status, names, and campaign hygiene

- [ ] Replace `withBranchPrefix :: Text -> ...` with
  `withGitBranchPrefix :: GitBranchPrefix -> ...`; provide a pure projection
  from `ActorPath`/`CampaignPath` and a convenient exact-handle
  `withinForkGroup` query.
  Rename raw receipt fields to reveal whether they are actor paths or Git refs.
- [ ] Replace `createdAfter :: Int -> ...` with a typed timestamp/query value.
- [ ] Collapse terminal responses/watches in ordinary status and expose them
  under an explicit history/all view. Keep existing typed forget operations
  for custody release; do not invent acknowledgement state solely to hide UI
  noise.
- [ ] Make notifications identify watch/request labels, not only numeric IDs.
- [ ] Keep `bound_worktree` distinct from the managed worktree registry in
  status language.
- [ ] Summarize completed-prefix semantics structurally: committed/rejected/not
  run units, installed bindings, and completed effects.
- [ ] Register the verified root/source checkout as a typed integration target
  so a clean final `tryMerge` can remain inside the custody/evidence path.
  Return conflict/refusal evidence and leave complex resolution to ordinary
  Git.

### Derived campaign cleanup

Actors remain retained until explicit control. Cleanup should nevertheless be
easy, boring, and resumable when a campaign is actually done.

`planCleanup group` derives a dependency-ordered plan from the same runtime
snapshot used by `:campaign`; it does not mutate. `executeCleanup plan` invokes
the existing forget/stop/group-cleanup owners and returns a tree of outcomes:

1. forget terminal watches;
2. forget/release terminal responses no longer retained by a watch;
3. stop descendants inside-out;
4. forget stopped actors after their retained dependencies are gone; and
5. retire the fork-group admission record.

Pending work, dirty worktrees, and refused custody are visible plan blockers,
not implicitly cancelled. Cleanup never deletes a worktree, branch, commit, or
user file. Re-executing the same cleanup operation retries only incomplete
steps through runtime operation identity.

Expose the same derived planner/executor through a supervisor/control-plane
command that does not require a healthy resident Haskell workbench. It calls
the same actor/request owners; it is not a second cleanup implementation. This
is necessary precisely because cleanup is often needed after an application or
workbench failure.

## Ownership map

Consult each nested guide before editing. This table prevents an ergonomic fix
from quietly becoming a duplicate mechanism.

| Concern | Owner and primary files | Extension rule |
|---|---|---|
| actor/request/watch/fork identity and lifecycle | `tidepool-actor/src/request.rs`, `lineage.rs`, `interactive_session.rs`, `resident_actor.rs` | add states and transitions to the existing registries/kernel; Ractor remains the scheduler |
| Haskell actor effect dispatch and public facade | `tidepool-actor/src/request_effect.rs`, `resident_workbench.rs`, `haskell/lib/Tidepool/Agent/{Reply,Watch}/`, `haskell/actors/Tidepool/Actors/{Internal/Agent,Unfold,Role,Shoal}.hs` | keep `Member`-polymorphic helpers and opaque handles; update generated bridges only through their owner |
| workbench parsing, units, declarations, and receipts | `tidepool-runtime/src/session/workbench.rs`, `turn.rs`, resident session modules; actor frontend policy in `tidepool-actor/src/resident_actor.rs` | one source classifier; frontend chooses diagnostic continuation policy |
| JIT continuation/root correctness | `tidepool-codegen/src/jit_machine.rs`, `suspension.rs`, `resource_ledger.rs`, `heap_bridge.rs` | preserve registry-rooted continuations and pinned constructor tables; do not patch around corruption in host prose |
| provider thread/context and usage | `tidepool-model` values plus `tidepool/src/actor_host.rs` and `tidepool-actor/src/runtime_observation.rs` | record provider facts; do not infer provider behavior from labels |
| durable delivery | `tidepool-node/src/inbox.rs` | extend `DurableInbox`; never add another wake queue/cursor |
| process/workspace mounts | `tidepool-node/src/process_boundary.rs`; composition in `tidepool/src/actor_host.rs` | add explicit overlays/leases to the one mount boundary |
| toolchain/cache paths | `tidepool-toolchain/src/paths.rs` | allocate host resource roots here; no ad hoc `/tmp` path convention in actors |
| worktree/Git truth | `tidepool-worktree::{registry,journal,git}` and existing handlers | register/query/merge through existing owners; manual Git remains the exceptional model tool |
| durable JSONL and schema migration | `tidepool-repr::{jsonl,version_ladder}` | reuse primitives; actor/runtime modules own event semantics |
| Shoal composition, prompt profiles, and operator commands | `tidepool/src/actor_host.rs`, `actor_host/prompt_catalog.rs`, `tidepool/src/shoal.rs`, `prompts/shoal/` | projections and wiring only; no parallel lifecycle state |

## Linear implementation handoff

Each slice ends in one reviewable commit. The executor may combine trivially
coupled file edits inside a slice, but it must not start the next slice while
the current exit gate is red. If a prerequisite is false, document the exact
evidence and amend this plan before selecting a different boundary.

### Slice 0 — pin reproductions and establish iteration lanes

Goal: turn every live incident into a focused seam before broad refactoring.

1. Add adjacent `.hs` fixtures (loaded with `include_str!`) for:
   - inherited user-defined record/sum reply on first settlement;
   - a retained actor's second typed request;
   - recursive unfold plus watch;
   - incomplete `let Right x = ...` binding and forced mismatch; and
   - `[fmt|...|]` in the actor facade.
2. Add Rust-only deterministic seams for:
   - child retirement task failure;
   - notification publication failure;
   - response settlement immediately between poll and idle;
   - build lease disappearance; and
   - same-operation transport retry.
3. Record current failures without asserting entire prompts/transcripts. Assert
   structured state, IDs, effect counts, and small semantic clauses.
4. If the existing provider-backed recursive fixture makes every API edit take
   minutes, mark it temporarily ignored with the plan slice that re-enables it.
   Keep its fixture compiling where possible. Do not repeatedly rewrite a huge
   escaped Rust string.

Exit gate: every observed bug has either a deterministic failing test or an
explicit provider-only acceptance step, and the ordinary focused lane runs in
one practical edit loop.

Suggested checks:

```text
just test tidepool-actor 'test(<new focused names>)'
just test tidepool-runtime 'test(<new focused names>)'
just test tidepool 'test(<actor-host focused names>)'
```

### Slice 1 — contain child and cleanup failures

Goal: prove a permanent root host outlives every child-scoped failure.

1. Introduce the closed failure-domain classification at the composition-root
   join boundary.
2. Establish the typed actor-event envelope with the lifecycle and cleanup
   variants needed by this slice. Later slices extend this one enum/projection;
   do not emit temporary string-classified events.
3. Change `run_interactive_applications` so retirement, binding discovery,
   owner-notification, and delivery task errors update the affected actor and
   emit a typed event instead of breaking the fleet loop.
4. Refactor `retire_interactive_application` into a best-effort structured
   cleanup transaction that attempts every component and returns per-component
   outcomes.
5. Publish/log the initiating actor, link relation, pane/process status, socket,
   binding, and completed cleanup prefix before destroying presentation state.
6. Keep root tool service and all unaffected sibling services callable.

Exit gate: killing/retiring one leaf and injecting every cleanup-component
failure leaves `:status` callable, siblings running, and a typed
`CleanupDegraded` observation. A three-stop Haskell batch reports which units
committed and which were not run.

Primary tests: actor-host unit/integration tests around the existing deployment
`JoinSet`; no real provider is required.

### Slice 2 — fix first-attempt typed reply settlement

Goal: make typed settlement exactly once and make a local JIT failure terminal
for the request rather than contagious.

1. Run the Slice 0 reply fixture with continuation/table/generation tracing.
2. Identify the first divergence among the reply result fingerprint, compiled
   fragment table, parked continuation table, realm/generation, and live root.
   Fix the owner of that divergence. Do not normalize tags or catch `SIGSEGV`
   in the actor host as a substitute for heap correctness.
3. Validate a parked continuation and materialize its response against the
   constructor table captured for that exact continuation before consuming it.
4. Move reply claim/terminal semantics to the request owner. Remove the path
   that rolls an accepted reply back to `Open` after downstream failure.
5. Add `ResponseSettlementFailed` (with structured correlation, compact
   model-facing rendering) to the unavailable result when private settlement
   cannot finish.
6. Verify all roots are transferred or released once on ready, rejected,
   cancelled, trap, actor stop, and scope retirement.

Exit gate: both a user-defined record and a nested sum settle on the first
attempt across an inherited snapshot; an injected post-claim fault yields one
terminal unavailable response, no second value can win, parked/root counts
balance, and later requests to the retained actor still work.

Primary checks:

```text
just test tidepool-codegen 'test(<continuation/table/root tests>)'
just test tidepool-runtime 'test(<resident-generation reply tests>)'
just test tidepool-actor 'test(<typed settlement tests>)'
```

Run `just fixtures-check` if extractor/serialization output changes.

### Slice 3 — make request admission and hosted effects retry-safe

Goal: eliminate observable half-admission and uncertainty after transport
failure.

1. Add typed internal `OperationId` execution coordinates to workbench effect
   dispatch and unit receipts.
2. Spike the two acceptable implementations described in “Request admission is
   one logical transaction”: one live-blueprint effect versus an
   operation-scoped private reservation guard. Choose the smaller mechanism
   that preserves evaluator/root ownership; record the result in the owning
   module docs.
3. Make `request`/`requestWith` one logical handler transaction. Remove
   `reserveRequest` and `submitRequest` from exported/browseable Haskell. If
   private prepare/commit remains, tie it to the unit operation guard with
   automatic rollback before publication and a terminal receipt afterward.
4. Record each unit's committed operation prefix before returning its tool
   response.
5. On retry of the same hosted tool-call identity, return the existing unit
   receipt without executing its effects again. A new tool-call identity runs
   normally even if source and labels match.
6. Thread operation identity through existing stop, forget, watch, unfold, and
   merge owners as correlation first; add owner-local idempotent lookup where a
   transport retry can otherwise duplicate mutation.

Exit gate: crash/inject failure immediately before and after request commit,
response claim, watch registration, unfold commit, merge commit, and stop
acceptance. Each state is either absent or present exactly once; retry returns
the authoritative receipt and never repeats a committed mutation.

Do not attempt to deduplicate native shell/Git commands or serialize arbitrary
effect results.

### Slice 4 — make the workbench forgiving without becoming ambiguous

Goal: let exploratory Haskell feel like GHCi while preserving exact effect
boundaries.

1. Extend `WorkbenchItemStatus`/receipts with `Diagnostic` and `NotRun`, warning
   diagnostics, installed bindings, and completed operation IDs.
2. Add a typed workbench execution-posture projection for compile/evaluate,
   named-effect suspension, return, and terminal transfer; wire client
   presentation to it rather than elapsed-time guesses.
3. Classify meta commands as observational or mutating. Continue after a failed
   observational query; reject and stop after a failed mutating command,
   Haskell evaluation, or effectful unit.
4. Remove global warning-as-error from the interactive compile profile. Capture
   warnings as diagnostics. Keep library/CI strictness separate.
5. Map the Haskell `patError`/match-failure host path to a typed unit rejection
   under signal/exception protection. Preserve prior units and the machine when
   its invariants remain sound.
6. Fix actor turn imports so `[fmt|...|]` resolves through the same
   `Tidepool.QQ` insertion path tested in runtime turn modules.
7. Make `:browse` output and `:info` resolution use one discovery table; either
   hide private `EffectWitness` or make it inspectable.
8. Add targeted syntax hints, explicit terminal-transfer receipts, and
   `:doc` examples. Do not create a second parser or new cell language.
9. Make root/event workbenches omit request-only bindings and report their
   typed `ActivationKind`; add a regression for the former
   `sessionInput :: ()`/actual-input mismatch.

Exit gate: a call containing failing `:info`, valid `:type`, and `:status`
returns three receipts and runs the latter two; a bad effectful unit stops its
suffix; an incomplete pattern warns without rejection; forcing a mismatch
rejects one unit without host/actor death; `[fmt|...|]` works live.

### Slice 5 — land dimensional time and typed namespace cleanup

Goal: remove the two most expensive model guesses from ordinary Haskell.

1. Add `Duration`/`milliseconds`/`seconds`/`minutes`/`after` to the small public
   prelude/facade and thread the typed value through requests and unfold
   branches.
2. Convert to monotonic milliseconds in exactly one Rust interpreter entry,
   with checked overflow and explicit immediate-deadline behavior.
3. Remove raw deadline `Int`s and update small fixtures mechanically.
4. Expose typed `ActorPath`, `GitBranchPrefix`, and timestamp/query values.
   Rename `withBranchPrefix` to `withGitBranchPrefix` and provide
   `withinForkGroup`/pure path projections so models rarely construct refs
   manually or confuse requested labels with allocated identity.
5. Type `BranchReceipt` requested/allocated path fields and render readable
   paths first, stable IDs second.
6. Normalize runtime authority denials into the shared typed
   `AuthorityFailure` fields embedded by each owning error ADT. Remove any
   model-facing branch that classifies denial text.

Exit gate: the old `requestDeadline 600` cannot compile; examples with
`seconds 600` and `minutes 10` do; status shows unit-bearing authored,
absolute, and remaining deadline values; a campaign worktree query requires no
knowledge of the `shoal/` ref prefix.

Run focused actor facade/extractor tests and `just fixtures-check` if generated
effect definitions move.

### Slice 6 — establish one structured event and observation projection

Goal: make runtime truth explainable without pane archaeology.

1. Extend the typed event envelope established in Slice 1 with all remaining
   payloads in the actor owner. Reuse the existing sequence issuer and include
   operation/activation/request/fork/worktree correlations as optional typed
   fields.
2. Emit transitions from their owners; keep composition-root enrichment (pane,
   provider, mount) separate from lifecycle decisions.
3. Persist through the shared JSONL primitive with a version-ladder entry.
4. Replace overwrite-only runtime usage with activation-scoped provider samples
   and explicit measurement scope/unknown values.
5. Complete `BranchReceipt` and expose `observeFork` from the same projection;
   keep late usage/application state out of the immutable receipt.
6. Build one snapshot projector used by Haskell `AgentInspection`, `:status`,
   `:campaign`, `:lineage`, and `:trace`.
7. Render concise default views and retain exact identifiers/deep paths only in
   expanded trace output.

Exit gate: one recursive two-wave fixture can be understood from a single
campaign tree: actor state, all three lineage relations, branches/worktrees,
requests/watches/deadlines, last provider-cache samples, and cleanup blockers
carry explicit, mutually intelligible watermarks/observation times. No renderer
maintains mutable shadow state.

### Slice 7 — make notification delivery race-free and useful

Goal: make waking reliable while keeping handles authoritative.

1. Commit request/watch state before appending its event.
2. Enrich `WatchChanged`/request events with label, previous/current state,
   occurrence time, event sequence, and current watermark.
3. Batch/coalesce durable inbox events into one next activation when an
   application is already active; preserve the individual event envelopes.
4. Ensure idle transition and pending-inbox scheduling are atomic from the
   application perspective.
5. Render stale delayed events as already superseded, not as fresh imperative
   prose. Do not add a second acknowledgement state merely for presentation.
6. Re-deliver pending events after external application reattachment while the
   actor/Haskell machine remains live.

Exit gate: deterministic tests cover settlement between poll and idle,
settlement during an active response, multiple simultaneous settlements,
notification publication retry, delayed delivery, and process reattachment.
Exactly one later activation is queued where appropriate, and all handles poll
correctly before their notices render.

### Slice 8 — move build output to lifetime-owned overlays

Goal: make the canonical workspace path reliable and make cheap actors truly
cheap.

1. Add a stable per-actor-incarnation build-resource root through the existing
   toolchain path owner.
2. Extend `ProcessMountBoundary` to bind explicit writable overlays inside an
   otherwise read-only/writable checkout view.
3. Mount the actor-local Cargo target at the fixed canonical path and set its
   environment from the mount receipt.
4. Hold a resource lease until actor retirement; cleanup occurs after the
   process/delivery lifecycle settles, never while active.
5. For inspection roles, omit the overlay and deny recognized build/test/
   formatter/generator/install commands in native process policy. Reads,
   `rg`, and non-mutating Git inspection remain cheap and available.
6. Emit allocation/mount/cleanup/degradation events without narrating physical
   paths to the model.

Exit gate: two concurrent coding siblings build without sharing mutable locks
or losing targets; a retained actor reuses its target; an inspection actor is
refused before starting a build; every actor still sees exactly
`/tmp/tidepool-actor-workspace` and the fixed target alias.

Primary checks:

```text
just test tidepool-node 'test(<overlay and access tests>)'
just test tidepool 'test(<actor build lease tests>)'
```

### Slice 9 — campaign views, cleanup, and final integration custody

Goal: let the permanent root manage a large retained swarm without bookkeeping
becoming its task.

1. Add the causally consistent `CampaignSnapshot` projection under
   `AgentInspection`, keyed by an existing `ForkGroupHandle`; do not mint a
   parallel campaign registry. Actor/request/fork facts share an actor-event
   watermark; sampled provider/worktree facts retain their own observation
   times rather than pretending to be globally atomic.
2. Implement concise/expanded status filtering so active and blocked work is
   prominent and terminal history remains available.
3. Implement pure `planCleanup` and idempotent `executeCleanup` over existing
   operations, with a structured receipt tree and a supervisor/control-plane
   entrypoint.
4. Register the root/source checkout as a typed integration target after
   identity/dirty-state validation. Extend `tryMerge` outcomes only where the
   model must make a real next choice.
5. Keep conflict resolution and exotic transplantation in ordinary Git.

Exit gate: after a recursive campaign, the root can inspect one tree, merge a
verified clean head to its checkout, plan cleanup, inject a mid-cleanup failure,
and resume cleanup without losing Git/worktrees or stopping the host. No actor
is torn down merely because its last request replied.

### Slice 10 — recover applications and machines honestly

Goal: preserve continuity where possible and make semantic loss explicit where
it is not.

1. Implement R0 and R1 first: diagnostic recovery and activation quarantine.
   These should be ordinary test paths, not best-effort logging.
2. Implement R2 by reattaching an external application/provider thread to the
   same actor and still-live machine, then draining pending durable events.
3. Add a versioned resident manifest containing accepted declaration source,
   generation hashes, committed-unit/operation receipts, and durable handle
   metadata—never arbitrary values.
4. Implement R3 as failure of every exact incarnation backed by the lost
   machine/resource scopes, followed by successor creation and precise
   `RecoveryReport`s. Replay declarations; reopen durable resources through
   their owners with fresh handles; mark every old response, watch, closure,
   and binding unavailable, stale, or lost.
5. When a provider thread is reattached to a successor, append one
   authoritative recovery developer delta naming the new incarnation,
   replayed declaration generations, reopened resources, and lost binding/
   handle names. `:bindings` and `actorContext` must agree with it.
6. Make cleanup and status callable from the supervisor path throughout.

Exit gate: crash the external application with the machine live, then lose the
Haskell machine in a separate test. Each recovery reports exact preserved and
lost state, never replays effects, never claims a lost function/result was
restored, and can continue with new declarations, requests, and typed Git
evidence.

R3/R4 resume a readable role through a successor actor; they do not adopt the
old exact identity. That honest incarnation boundary is preferable to
constraining all future Haskell values to a persistence codec or making an old
`ActorRef` silently target a different machine.

### Slice 11 — recover operational truth across host restart

Goal: make R4 a clean extension of the event/owner model after R0–R3 are
proven, without serializing actors or adding a shadow scheduler.

1. Persist the versioned append-only actor transition/cleanup-obligation ledger
   through the shared JSONL and atomic-write mechanisms.
2. On startup, validate the run lease and ledger version, reconstruct
   historical observations, and ask the real actor kernel to create fresh
   successor incarnations selected by supervisor policy.
3. Reattach provider threads where supported, reopen registered worktrees and
   build-resource policy through their canonical owners, and replay accepted
   declaration source into new machines.
4. Transition every old pending request/watch/reply capability to a terminal
   host-loss result; never install it in a successor.
5. Resume unfinished cleanup from the supervisor entrypoint using recorded
   operation receipts. Preserve all Git/worktree history.
6. Handle corrupt/truncated/unknown-future ledger data with a typed refusal and
   diagnostic recovery mode; never guess and run effects.

Exit gate: kill the host immediately before and after request admission, watch
publication, merge commit, and each cleanup step in deterministic fixtures.
Restarting yields either no mutation or exactly one committed mutation,
historical status remains queryable, selected roles return as fresh
incarnations, and no Haskell value is claimed to survive.

### Slice 12 — finalize prompt profiles and executable help

Goal: make the discovered good behavior the default without bloating fork
deltas or invalidating cache prefixes needlessly.

1. Deduplicate the stable Shoal prelude and hosted-tool instructions under one
   prompt catalog owner. Version it deliberately.
2. Add the calibrated delegation rule, permanent-root lifecycle, typed
   reply/watch model, final-unit unfold rule, canonical workspace, Git custody,
   and diagnostic-recovery guidance.
3. Make every fork role delta authoritative over inherited role text and render
   its actual effect row/access/compute posture from typed policy.
4. Keep inspection language unequivocal: do not run build-like tools. Work
   needing validation belongs in a new build-capable fork/actor; an actor's
   role and effect row do not mutate because a later prompt asks.
5. Filter transport/client-only noise before provider-prefix capture while
   preserving semantic user/developer/model/tool history exactly. Classify
   ephemeral client notices when they enter the canonical conversation; never
   edit an already-established parent prefix differently for a child.
6. Add `:doc` examples for the multi-wave pattern, dimensional deadlines,
   polling, refinement, lineage, and cleanup. Prefer examples over more
   always-loaded prose.
7. Record prompt-profile and role-delta fingerprints in provider usage events.
8. Derive activation notices from `ActivationKind` and the generated request
   scope. Never claim `sessionInput`, `sessionReply`, or a type that is not
   actually mounted.

Exit gate: root, research, coding, scaffold, and integration prompt tests assert
small semantic clauses plus structured policy—not entire generated strings.
A live child sees the full unfold call, a tiny correct branch delta, no
parent-only result, and no physical-path narration. A prompt-only change has an
explicit profile version and observable cache boundary.

### Slice 13 — remove migration debris and run the acceptance campaign

Goal: finish with one coherent public surface and evidence, not adapters.

1. Delete obsolete raw deadline helpers, reserve/submit facade calls,
   rollback-to-open reply paths, overwrite-only usage fields, physical
   worktree-local target allocation, untyped namespace helpers, duplicate
   prompt clauses, and string-parsed control decisions.
2. Re-enable every temporarily ignored evolving-surface test. Convert large
   Haskell source to adjacent fixtures and brittle transcript assertions to
   structured receipts before doing so.
3. Run formatting for Rust/Haskell, `git diff --check`, focused changed targets,
   `just fixtures-check` where required, then the relevant broad boundary
   check (`just changed` followed by `just verify` at this major integration
   point).
4. Run the provider-backed acceptance campaign below and save its campaign/
   lineage/trace/cache/recovery/cleanup evidence.
5. Hoist stable finished contracts into root `SHOAL.md`, the public Haskell
   module docs, `docs/GLOSSARY.md`, and owning crate guides. Mark or retire this
   plan according to `plans/README.md`; do not leave it as competing standing
   architecture.

Exit gate: all code paths and docs use the new vocabulary, every intended
target compiles, all required checks pass, and the live root experience meets
the first-person review at the end of this document.

## Fault-injection matrix

The deterministic suite should cover these boundaries. “State after” is the
contract; exact error wording is not.

| Injection point | Required state after | Primary home |
|---|---|---|
| failed `:info` before valid `:type`/`:status` | first item diagnostic; later observations execute | runtime workbench + actor frontend |
| incomplete binding warning | binding commits with warning; no `-Werror` rejection | runtime turn/workbench |
| forced failed pattern | one unit rejected; prior effects/bindings retained; host alive | codegen/runtime |
| request before admission commit | no request/roots/activation published | actor request owner |
| request after commit before tool reply | one request; retry returns same receipt | actor/workbench |
| reply before claim | still open; retry may claim | actor request owner |
| reply after claim before cell fill | terminal settlement failure; never reopened | actor + codegen/runtime |
| event after state commit before inbox publish | state pollable; publisher retry appends once | actor + durable inbox |
| watch settles between poll and idle | next activation scheduled; no lost wake | actor application scheduler |
| several watches settle while active | one later activation; all event IDs/handles present | actor + durable inbox |
| stale watch event delivered late | notice labels current/superseded state | projection renderer |
| child pane exits | child state changes; supervisor/root/siblings remain | actor host |
| one retirement component fails | all components attempted; `CleanupDegraded` receipt | actor host |
| active build directory cleanup attempted | lease refuses; directory remains | path/resource owner |
| two coding actors build concurrently | isolated mutable targets, stable aliases | process boundary/host |
| inspection actor invokes build | policy refuses before process work/artifacts | agent tool policy/host |
| provider child cold-forks unexpectedly | typed cache sample says fresh/unknown; trace explains known profile delta | provider observation |
| Haskell machine lost with ready live value | value explicitly lost; response unavailable; no fake decode | runtime recovery |
| Haskell machine lost after declaration | declaration source replayed; effects not replayed | runtime manifest |
| host dies midway through cleanup | ledger identifies completed steps; retry resumes incomplete ones | actor ledger/control plane |
| clean typed merge retried | one resulting head and same receipt | worktree owner |
| merge conflict | source/target unchanged; typed evidence directs ordinary Git fallback | worktree owner |

For process-death tests, inject both immediately before and immediately after
the owning commit point. This is more valuable than broad random killing that
cannot say which invariant was exercised.

## Test and iteration policy

- Prefer pure state-machine tests for transitions, ordering, and derived
  projections. Provider/TMUX tests prove only boundaries that cannot be reduced.
- Put substantial Haskell programs beside their Rust test and load them with
  `include_str!`. Do not maintain multi-page escaped strings in
  `actor_host.rs`.
- Assert enum variants, exact IDs, state transitions, root/continuation counts,
  effect cardinality, and small required prompt clauses. Never golden-test an
  entire generated transcript, status page, or prompt unless its byte identity
  is itself the cache contract.
- During public Haskell API churn, a tedious provider-backed or generated-string
  test may be temporarily ignored. The ignore annotation must name the slice
  that removes it, its fixture should still parse/compile when practical, and
   Slice 13 fails if any such ignore remains.
- Do not update expected output mechanically until the semantic assertion has
  been restated. If every API change forces dozens of string edits, replace the
  test seam before continuing.
- Keep strict library/CI warnings; interactive model code is exploratory and
  reports warnings without treating them as fatal.
- Broad batteries are integration gates, not inner-loop commands. Follow
  `scripts/codex-worktree-guidance.md` if implementation itself is later split
  across worktrees.

## Migration and deletion checklist

Backward compatibility is not a goal for a bad internal boundary. Serialized
ledger/event formats do require an explicit version-ladder migration.

- [ ] Delete the interactive `requestDeadline :: Int -> ...` API and raw public
  deadline milliseconds.
- [ ] Delete exported/browseable `ReserveRequestWith` + `SubmitRequestWith`.
  If evaluator constraints require private prepare/commit requests, keep them
  only behind the operation-scoped admission guard and document that owner.
- [ ] Delete accepted-reply rollback to `Open`.
- [ ] Delete overwrite-only cache observation once activation samples own all
  consumers.
- [ ] Delete worktree-local `.shoal/build/actor-*` target allocation after the
  lifetime-owned overlay is live.
- [ ] Delete or rename raw-`Text` Git-prefix and raw-`Int` time query helpers.
- [ ] Delete duplicate prompt/tool instruction paragraphs after the stable
  prelude/role-delta split.
- [ ] Delete any renderer-side shadow registry introduced during development;
  projections must read owner snapshots/events.
- [ ] Delete temporary ignored-test annotations and obsolete fixture variants.
- [ ] Version and migrate durable event/manifest schema through
  `tidepool_repr::version_ladder`; refuse unknown future versions clearly.

## Acceptance campaign

The hardening wave is complete only after a fresh provider-backed campaign
demonstrates this exact story:

1. The permanent root defines campaign-specific sum/record/function values,
   lenses/helpers, and `[fmt|...|]` prompts in its resident Haskell environment.
   Its root/event activations expose `ActivationKind` but no fake
   `sessionInput` or reply contract.
2. It performs a three-branch applicative unfold: one retained scaffold
   coordinator, one coding specialist, and one inspection-only adversary.
3. The coordinator recursively unfolds at least three leaves from its bound
   HEAD, including coding and inspection roles, then performs its own typed
   fold.
4. Provider-parent, exact Haskell snapshot, prompt-profile, selected-branch,
   effect-row, workspace, and fork-group evidence is visible for every edge.
5. Representative siblings and retained follow-ups report at least 95% cached
   input share when the provider supplies token usage. A deliberately changed
   prompt profile reports the cache boundary rather than being silently mixed
   into the same metric.
6. Every actor sees `/tmp/tidepool-actor-workspace`; branches and worktrees are
   distinct and readable; coding actors retain stable private build targets;
   the inspection actor cannot launch a build and is told why before trying.
7. A user-defined nested review value settles on its first attempt across an
   inherited Haskell snapshot. Worktree evidence is observable before its
   ready notification.
8. A child intentionally ends one response with its reply still pending,
   registers a watch on subordinate work, wakes once, and replies later. No
   lifecycle Haskell operation is involved.
9. Seconds/minutes deadline syntax shows authored, absolute, and remaining
   time. One explicit short deadline expires and produces one labelled terminal
   event without harming later requests.
10. A typed review drives a follow-up request to the original retained coding
    actor; its revision is folded and conservatively merged through typed
    custody to the verified root checkout.
11. While the root is active, several response/watch events settle; one later
    activation delivers the batch, every handle polls correctly, and a delayed
    notification visibly reports its current/superseded state.
12. A failing `:info`, incomplete binding warning, forced failed pattern, and
    injected post-reply-claim failure each stay in their declared failure
    domain. The root can still call `:status` and send a later request.
13. One child process is restarted with its Haskell machine intact. Separately,
    one resident machine is lost: every incarnation rooted there fails, named
    successors are created, declarations replay, old handles are stale, live
    values are reported lost, and no effect is replayed.
14. Cleanup is planned inside-out, interrupted after one committed step,
    resumed from the supervisor path, and completed idempotently. Git history,
    worktrees, and the source checkout remain intact.
15. `:campaign`, `:lineage`, and targeted `:trace` together explain the entire
    run from typed projections. No acceptance fact requires scraping tmux panes
    or guessing from an opaque `<no output>`.

Save the campaign snapshot, lineage view, focused request/watch traces, provider
usage samples, recovery report, cleanup receipt, final Git graph, and exact
checks run. Cache percentage is a live evidence threshold, not a deterministic
golden assertion in the normal test suite.

## First-person permanent-root review

Before calling the surface complete, read it as the model that will inhabit it,
not as the runtime author. This is the experience I would want as the root LLM:

### On waking

I want one compact orientation: I am the permanent root, response termination
ends only this model turn, my canonical workspace is stable, and Haskell is my
resident coordination language. I want `:status` to tell me what is active or
blocked and `:doc unfold` to show the idiom. I do not want an exhaustive runtime
manual, old client notices, or a command called `complete` competing with the
natural turn boundary.

### While understanding the task

I want to define the actual types that sharpen my thinking—plans, verdicts,
evidence, recursive nodes, and integration decisions—and keep them live. I want
normal Haskell composition, lenses, and `[fmt|...|]`; I do not want to squeeze
those ideas into JSON or predeclare a framework schema. Exploratory warnings
should teach me without killing the input batch.

### When splitting work

I want to write every independent branch in one visible applicative expression
and know that each copy receives exactly the understanding I have at that line.
Naming `runtime`, `interface`, or `adversary` should be enough; the runtime
should derive readable branches, isolated worktrees, request labels, and
lineage. I should specify only real semantic differences: task input, role,
worktree seed, a rare deadline, and perhaps a small guidance delta.

I do not want to summarize my own context seven times, pass filesystem paths,
copy actor IDs, choose target directories, manually start one-shot futures, or
grant capabilities one boolean at a time. I do want a branch receipt proving
which snapshot, provider edge, role, row, worktree, and starting head each child
received. Cache evidence can arrive later and must say what it measures.

### While children work

I want persistent typed handles immediately and no pressure to tear them down.
I want to register one compositional watch, end my response naturally, and
trust that no settle/idle race can strand me. Notifications should be concise,
labelled, sequenced, and point back to a typed handle; they should not try to
replace polling or narrate stale state as current.

I do not want raw operation IDs, provider internals, physical paths, all
terminal history, or every low-level event in my normal context. Those belong
one `:trace` away.

### When folding results

I want the child's rich domain value exactly as typed, automatically paired with
execution and worktree evidence. I want completion order erased by the
applicative shape. I want failures represented at the same positions so I can
make a real policy choice. I want to compare reviews, inspect commits with Git,
merge the conservative case with one typed operation, and keep complex Git
work legible rather than hidden behind a weak imitation.

### When a result changes the plan

I want to send a small typed follow-up to the same retained actor that already
understands the branch. I want the next recursive unfold to inherit the
coordinator's newer local understanding, not the root's older approximation. I
do not want the runtime to merge model contexts; I want evidence to update my
own reasoning and then choose the next coalgebra layer myself.

### When I make a mistake

I want a failed `:info` to leave the following `:status` intact, an incomplete
pattern to be a warning, and a forced mismatch to reject one unit rather than
kill my home. If an effect committed before failure, I want a structured receipt
saying so. If transport is uncertain, retrying the same call should tell me the
existing outcome, not duplicate it. I never want to infer whether a child or
merge exists from an empty transcript.

### When the runtime makes a mistake

I want the failure to stop at the request or actor it actually damaged. I want
the root tool and campaign view to remain available. If my Haskell machine is
still alive, restart the external application around it. If the machine is
gone, tell me plainly: that incarnation and these values are gone; here is the
successor, replayed source, durable Git/runtime history, and fresh handles. I
would much rather rebuild one semantic decision than spend every healthy turn
inside a serialization straitjacket.

### When the campaign is quiet or done

I want status to foreground pending, ready-to-act, failed, and cleanup-blocked
work while retaining history off the main path. I want a cleanup plan I can
inspect and run explicitly, with no worktree or Git deletion hidden inside. If
cleanup fails halfway, I want to resume it from the supervisor even if the
Haskell workbench is unavailable. I do not want reply settlement to volunteer
away a context I may need in the next wave.

### Final root-facing rejection test

Reject any implementation that makes the root routinely do one of these:

- restate shared context in branch prompts;
- parse status/prose to recover an ID or decide control flow;
- choose a physical checkout/cache path;
- guess a time unit;
- run builds from an inspection role merely to validate access;
- wrap arbitrary Haskell values in a persistence codec;
- wonder whether an effect ran after a tool error;
- replace a useful retained actor to ask a follow-up;
- inspect several tmux panes to reconstruct lineage or cache reuse; or
- call a Haskell function whose purpose is “my model turn is over.”

If the acceptance campaign avoids all ten while preserving exact typed values,
readable Git, and reliable recursive forks, this is an environment I would be
materially more effective—and happy—to work in for days.

## Out of scope

- replacing Git with a large integration API;
- automatically merging model contexts back together;
- serializing arbitrary Haskell heaps, functions, values, cells, or watch
  projections;
- silently retargeting an exact-incarnation handle to a recovery successor;
- narrating physical workspace paths to forked or resumed models;
- turning effect membership into runtime authority;
- eager actor teardown after one request;
- weakening source-custody checks to make dirty checkout admission convenient;
- a universal label-keyed idempotency layer over native shell commands;
- a second Haskell cell language or JSON workflow AST;
- introducing a second lifecycle, cache, log, or resource registry beside its
  existing owner.
