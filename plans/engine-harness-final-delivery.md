# Final engine and embedded harness delivery

Approved for implementation 2026-09-30. This refines
`engine-harness-completion-wave.md`; conflicting older sequencing is superseded.
The finish line is verified release readiness. Live trials and backend default
changes remain separate. Pushes are deferred at the user's request; dependency
revisions must be published before their consumers when publication resumes.
Native goal tools stay disabled. Codex is not
built with Buck; the resident compiler remains a runtime service.

The integration and foundation requirements below consolidate the superseded
September 27–29 adoption, runtime, transfer and restart plans. Their branch
queues, host paths and temporary build state are historical. The current
[acceptance evidence](engine-harness-completion-evidence.md) and
[remaining M1 gates](engine-harness-m1-remaining-gates.md) distinguish source,
component execution and joined acceptance; this cleanup closes no gate.

## Retained integration contract

One process per run shares Harness services and the actor kernel. Raw Haskell
notebook cells and AgentSpec tools are the model-facing surface. Haskell authors
programs and orchestration; Rust owns providers, scheduling, processes, storage
and resource authority. Typed kernel requests/replies remain live kernel-owned
values, separate from durable Harness model-input envelopes.

| Boundary | Owner and invariant |
| --- | --- |
| Actor lifecycle | Existing kernel owns identity/incarnation, admission, supervision and retirement. Embedded composition does not install the standalone demo TreeDriver as another supervisor. |
| Conversation and calls | Harness Engine/Store owns model rounds, history, input inclusion and original operations/claims. One owner sequences provider rounds per conversation, while tool executions may remain pending. |
| Tool execution | The admitted resident endpoint binds actor authority; each request pins raw/structured kind, immutable handler, source and tool manifest. Reload affects later admission, never reinterprets an issued call. |
| Source and notebook state | Resident session/source owners retain exact admitted compiler/helper source, declarations, bindings, native instances and publication receipts. A path or generation alone is not a source lease. |
| Commands and cleanup | Existing resource owners retain original handles, output and actual cleanup disposition. Dropping a dispatch waiter or browser connection proves neither cancellation nor descendant cleanup. |
| Browser | The facade composes the Harness authenticated router, authoritative snapshots/cursors and matching immutable assets. Retention gaps require a new snapshot; reconnect never submits commands automatically. |

Internal operation identity includes originating conversation/incarnation,
request and original provider call ID. Preserve public IDs verbatim. Independent
conversations and requests may return equal provider IDs; inherited claims refer
to the same original operation rather than redispatching it in a child. Store,
scheduler, cancellation and recovery must agree. Test collisions and shared
inherited claims through those owners, not a Tidepool-side ID rewrite.

Durable input admission, successful wake and inclusion in an actual provider
request are distinct observations. Embedded input is not also copied into the
Codex inbox. Model final means conversation idle, not typed assignment
completion or actor retirement. Host loss retains historical receipts but does
not reconstruct live heaps, lexical capabilities or external resources.

Backend selection is immutable for an admitted run. Preserve Codex compatibility
and stock Codex independently until the explicit cutover decision; no running
session migration or transcript conversion is implied. Startup validates backend,
workspace, Store schema, assets and bound tool host before readiness. Readiness
does not prove provider authentication. Browser authorization includes explicit
public scheme, origin checks, session expiry and configured-secret rotation;
forwarded identity headers and private-network placement grant no authority.
Authentication failure preserves uncertain calls instead of replaying them.
Compaction preserves runtime state and pending raw/typed claims separately from
its text summary; failed/no-progress compaction retains the last valid history.

### M1 and M2 remain distinct

M1 proves the sequential resident path through the actual embedded Engine,
bound endpoint, Store and authenticated browser: raw Haskell and typed calls,
original output, input inclusion, exact cancellation, retained commands,
reconnect/retry without replay, pinned reload, pending-call compaction/late
output, host loss and resource disposition. A mock hosted operation or
component browser test does not prove resident Haskell or the UI-to-host path.
M1 does not advertise concurrent cells or close M2.

M2 retains one execution identity and its admitted public/source/tool authority
through every park/wake. Each execution owns its private scope, final write set,
continuations, reply/control, effect receipts, after-tool invocation and cleanup.
External waits release machine checkout. Request reservation cleanup and
after-tool timeout cleanup name exact owned operations, never a session-wide
before/after subtraction that can consume a sibling's work. Aggregate posture,
drain and retirement include every admitted execution; structured mailbox turns
remain non-reentrant.

Successful completion publishes a final declaration/binding delta atomically
against the current public view. Unrelated writes survive; same-name winners
follow publication completion order, including reverse compile-generation order.
Old cells/captures retain original Names, SessionVarIds, interfaces, instances
and roots. Do not restore a private starting tip, manufacture typed aliases,
merge rendered names, or replay private source against newer public meanings.
Compiler certification includes original exports, constructors/class methods,
normalized imports, replacements/retractions, instance-only writes and selected
instance plus retained-family consistency. A wrapper typecheck alone is
insufficient. Invalid joins publish nothing; already-performed effects remain
real. Both stale acceptance and stale rejection restage the same frozen intent
without effects. The publication/cancellation ordering and post-rename
confirmation contract below remain authoritative.

A successful capture includes completed private scaffold at its issuing effect
boundary and independent leases on admitted source and lexical/native state.
Function-local values cross as explicit typed inputs. Captures do not snapshot
arbitrary filesystem reads or freeze mutable resources; child admission
revalidates authority and revocation. Reuse a capture for children before the
parent returns; later parent failure, cancellation or token release cannot
invalidate a successfully retained capture. Pending parent calls retain their
original eventual outcomes, never fabricated success or repeated effects.

The full worker-tree gate uses ordinary compiled Haskell orchestration and at
least one Haskell-only result join: a bounded Sol root delegates two useful
components through recursive Luna owners/leaves, receives typed results,
commissions exact-candidate review, repairs, integrates and verifies cleanup.
Retain commits/checks, ancestry, models/effort, call/execution correlation,
latency/memory and resource disposition; interview the root before retirement.
Do not manufacture actors to meet a headcount. This live gate still requires its
separate readiness packet and approval after deterministic M1/M2 gates.

### Publication checkpoint, 2026-09-30

At `b38490f520290f83ff9cbe60bb2eb7cea898502f`, GitHub Tidepool main was
`f84bf313d8f0364fff474dcfd883aeca443b291f`: 619 local commits ahead and zero
upstream-only commits. The rejected push targeted the unrelated local
`.exomonad/workspace` repository because the superproject's origin URL had
changed. Origin now fetches Tidepool over HTTPS and pushes over SSH. The cause
of that configuration change is under investigation; no merge or force push
is required.

Publication is deferred at the user's request. GitHub authentication
also remains unavailable in this session. SSH dry-run
fails host-key verification; HTTPS has no usable credential helper. Publish
the pinned harness revision `9986ca3cf8b1e4be9826cb7420de01e4371922c7` and
verify availability of the Codex gitlink
`d2d1d7c754a72f51087a54c230185831c649913b` before publishing Tidepool. Harness
origin currently names a local bundle, so publication must explicitly target
the harness GitHub repository. These delivery checks do not establish M1,
M2, or full-engine acceptance.

## September 30 source checkpoint and ownership

Starting main: `ff2a9c3edde61690a49012fd8d848e2955d2afd7`.
Candidate tips: compiler `595b2f72f`, runtime `dccb98a26`, actor `f5ba3851b`,
facade `667bf6061f`, harness `31ae2e52`. Preserve equivalent cherry-picks and
apply dependency-complete commits rather than importing older branch ancestry.
Compiler/helper and generated Buck WIP is preserved under
`target/completion-evidence/final-delivery/20260930T165141Z`.

At this checkpoint, the protected hidden display and original
declaration-to-binding-to-expression native gates had passed on compiler
candidates, as had runtime Ephemeral binding tests. Neither M1 nor M2 was
accepted; private and keyed actor admission remained disabled. These historical
states do not describe later activation. Existing runtime fresh-process recovery
does not establish production embedded startup/restart. The evidence ledger must
distinguish these results.

Root owns integration, native focused execution, browser acceptance and packaging.
Up to eight workers own disjoint parcels:

| Owner | Parcel |
| --- | --- |
| Sol 6.1 compiler | Exact interface/native authority, completed-value refinement, immutable artifacts/prefixes, cache acceptance |
| Sol 6.1 runtime | Admitted inventory/native ledger, publication, durable children, scoped view reuse |
| Sol 6.1 actor | Synchronous owned advancement, private activation, cancellation, keyed admission, lifecycle |
| Sol 6.1 harness/facade | Durable conversation binding, Store successor transfer, production recovery/child attachment |
| Sol 6.1 bindings/native | Mutation witnesses, indexes, shared lease chunks, prepared images and reclamation |
| Luna implementation | Frozen-interface task modules, ingress/recovery/concurrency fixtures, evidence tooling |
| Luna Buck | Focused counted execution, generators, declared resources, cache invalidation |
| Fresh Sol 6.1 review | Independent authority/failure-path review and adversarial acceptance |

Reassign completed slots promptly. Freed Sol capacity takes nested after-tool
and child-delivery conversions; Luna takes mechanical consumers and fixtures.
Central owner files have one editor. Leaves submit sibling modules and precise
adapter requirements. Delivered, acknowledged, joined and verified revisions
are distinct evidence.

## Compiler, bindings and native execution

Before checked prefix creation, compare compiler-consumed initial interfaces
with runtime-admitted exact module identities and bytes. Freeze native imports
from the actual scoped binding dependency closure and native export owner.
Thin-interface authority is captured SHA-verified bytes, exact hydration into
the current request, and an opaque retained installed-interface capability with
winning Names/VarIds. Fingerprint zero and editable hints grant no authority.

Finish protected value replacement with certified private value overlays and
completed-Val parsed import refinement. Preserve qualified historical names and
independent type names. Current overlap refusal stays until positive and
adversarial tests pass. Preserve original declarations, constructors, instances,
families and hidden dependencies; never replay source against a newer context.

Make binding identities immutable: identical reinsertion may be idempotent;
incompatible replacement must refuse before mutation. Complete mutation
witnesses in BindingTable itself, including aliases, hiding, promotion,
observations, dependency/native membership and retirement. A global revision
invalidates cached computation, never another execution's scoped authority.

Use indexed immutable bases and deltas to remove repeated full view scans,
prefix-vector copies, historical byte hashing/writes and complete closure lease
acquisition. Each compiler request selects sealed immutable artifacts; a growing
directory is not authority. Refresh request-specific fields even when reusing a
scoped environment. Share immutable programs/group definitions in existing native
owners; keep CAF/live instances installation-owned. Image keys retain all
compilation-relevant identities while excluding machine-local resource handles.

## Actor execution and publication

Preserve OwnedWorkbenchTask, synchronous completion advancement and the full
execution/step fence. Each effect family captures typed inputs, waits off actor,
then applies synchronously. No behavior or KernelContext borrow crosses a wait.
The original cursor, fragment, ordinal, reply/control, admission, source/tools,
private decision, budgets and cleanup claims survive every successor.

Begin private execution before preparation with the original public owner and
PublicationDecision. Retain private authority through cleanup. Convert commands,
requests/watches, child/workspace operations, nested after-tool, builtin/reload,
inspection, terminal settlement, drains and child-exit waits; remove resident
serial fallback. After-tool uses the same private execution and original tool
lease/deadline/ordinal/budgets; timeout recovery requires abort acknowledgement.

Freeze one final intent. Stage and certify outside checkout; stale success or
rejection restages that intent without effects. Cancellation shares the original
PublicationDecision. Before claim it prevents publication; after claim it waits
for the owning outcome. Post-rename uncertainty remains published and permits
confirmation only. Preserve resources when abort/cleanup is unconfirmed.

After single-admission acceptance, replace the pending singleton with a map
keyed by internally issued admission generation. Raw cells/installed tools may
coexist; structured turns remain non-reentrant. Aggregate posture is central.
Reload affects later admission. Drain closes admission and waits for existing
work; shutdown/panic retains every affected cleanup obligation.

## Captures, child readiness and production recovery

Queue readiness is not provider readiness. Register captured/source/lexical
shares and acknowledge queue admission first, so the group can publish.
Capture-backed groups publish during the issuing cell; ordinary unfold retains
its enclosing publication boundary. ReleaseFork installs the actual inherited
scope, then initializes that child's durable public surface. Only confirmed
durability plus conversation attachment permits provider work.

Published-but-unconfirmed children retain Pending placement/resources and their
committed group. Retry confirmation only. Startup-budget exhaustion reports a
retrievable Pending outcome through existing owners; retirement preserves the
manifest and reports cleanup/durability separately. Successful captures and
admitted children survive token release and later parent failure.

Actor journal v3 uses typed conversation bindings, with a journal-owned Embedded
wire identity converted from the actual harness run/path/incarnation. Do not
substitute actor-lineage path syntax. Migrate v2 bound strings as Codex; unbound
applications remain unavailable and v1 stays refused.

Persist intended Embedded identity in ApplicationPrepared before attachment.
Under the retained run lease and latest opaque journal proof, the existing
harness Store performs exact predecessor-to-successor binding CAS. Same run and
path are required; return Installed/AlreadyInstalled, refuse other bindings.
No Store schema bump or second registry is needed for this CAS.

Publish matching ApplicationBound after Store confirmation and before initial
input, Engine activation or Ready. Identical confirmation is idempotent.
Reconcile crash gaps only when latest prepared intent, Store binding, source and
manifest owner agree. Otherwise preserve uncertainty and refuse; never fall
back to older incarnations. Recover retained conversation without repeating its
initial input. Historical input/receipt observation remains available under
original identities; fresh input/control requires the current binding. Claimed
controls, pending rounds and effects never become automatically redispatched
successor work. Lost live heaps/captures are explicitly unavailable.

## Native Buck and resources

Use the existing Buck executor (`--test-arg` for Rust filters). Extend the existing
isolated libtest helper for exact selection, expected count and ignored mode;
keep fresh processes, deadlines and cleanup. Declare runtime resources per
focused group. Separate compilation/linking, execution, fixtures, generation,
browser dependencies/assets and packaging. Keep standard Haskell rules with
explicit SOURCE/boot inputs. No whole-Cargo/Cabal wrapper build migration.

Use the accepted 104 GiB completion slice, separate divergent Cargo/Cabal outputs
and the existing bind-mounted Buck output. Start at most four expensive lanes,
bounded jobs, and admit work within observed memory headroom. Preserve SSH/OS
headroom and the separately capped Nix daemon. Buck stays local-only with
`-c remote.enabled=false`; no shared daemon restart or broad cache deletion.

## Ordered gates and delivery

1. Preserve/record candidate source and WIP, then join reviewed prerequisites.
2. Finish authority contracts and enable private execution under single admission.
3. Run real sequential production/browser/restart gates while converting waits.
4. Enable keyed admission only after single-admission lifecycle/publication gates.
5. Complete M2, scaling, cache, corpus and packaged acceptance on the joined tip.

After M1, engine and M2 each pass their acceptance gates, commission a separate
fresh-context Sol 6.1 adversarial review of that exact milestone revision.
Review the production workflow and owning source for awkward data structures,
duplicated ownership, workaround layers, unnecessary conversions and performance
costs. Give the reviewer the source revision, intended contracts and retained
acceptance evidence without the implementation conversation. Record concrete
failure paths and measurements; return findings to the owning implementation
lane for repair and affected verification before closing the milestone. These
reviews supplement the ongoing candidate reviews; reused reviewer context does
not satisfy this gate.

Required evidence:

- Protected hidden/original native execution, completed-value replacement and
  qualified-name preservation; stale/hash/identity/widening refusals; full
  Ephemeral/Durable declaration and binding publication.
- M1: actual raw Haskell and typed tools, retained output, input inclusion,
  lost ack/retry/reconnect without replay, pinned reload, compaction/late output,
  interrupt/continue, retirement and host loss.
- Production cold startup in a distinct process through the owning startup seam
  with test-only deterministic transport: real run lease, journal/Store transfer,
  hydration, browser reconnect, old observations and no replay.
- Single-admission watch/Sleep/command cancellation, presentation, checkout
  exclusion, after-tool/terminal cleanup, stale steps and both commit/cancel orders.
- M2: A parks/B publishes/A resumes preserving B; completion-order shadowing,
  atomic invalid joins, two parked plus progressing third/control, independent
  cancellation, drain and shutdown.
- Two real capture children reply before parent returns through ordinary Haskell
  orchestration; later failure/revocation/workspace delay/partial launch failure
  and last-owner reclamation. Ordinary deferred children still wait for publication.
- Nonempty persisted child actually executes after restart; missing/corrupt
  artifacts refuse; hidden/retracted/instance/family inventories; pre/post-rename
  faults; uncertain children never reach provider requests.
- Compiler/native cache miss/hit and package/source/boot invalidation; fresh
  mutable compiler state; demand omission, rollback, inherited versus fresh CAFs,
  late dependency demand and final-owner reclamation.
- Actual native settlements at N=1/10/100 with baseline B=0/100: visits/copies,
  hashed/written bytes, leases, stage times and retained memory, including
  park/cancel/reaper. Unchanged guards avoid full scans; settlement avoids repeated
  historical baseline copy/hash/write/lease work. Report residual serialization
  and compiler work separately; source inspection is not a timing measurement.
- Focused native execution, deterministic generators, matched harness/web checks,
  warm cache and controlled Rust/fixture/TS/npm/Haskell input invalidation, complete
  structural corpus/embedded producers, required verify constituents and package
  startup. Existing Codex path is verified independently outside Buck.

Every gate retains source/artifact hashes, exact command, selected/executed
counts, exit status, logs and cleanup. Review/compile-only results never replace
execution. Broad required checks run at the final boundary; subsequent repairs
repeat affected checks. Root continues a useful integration/browser/package
parcel while workers execute. Fix failures at the owner; escalate semantic changes
or external blockers, and log unrelated improvements without an unbounded scope.

Finish with committed selected work, updated evidence/index and stale-path
cleanup, verified Tidepool/harness bundles and reproduction commands. Report M1,
M2, engine/performance and Buck separately. Prepare a concrete bounded live trial
packet for later approval; do not launch, push or change defaults in this wave.

## Retained browser request projection checkpoint

Harness `9986ca3cf8b1e4be9826cb7420de01e4371922c7` extends the accepted
library source `814b1697` with bounded durable model-request projections.
Tidepool refreshes them on lifecycle changes and the existing host heartbeat.
Completion means the model response Items were durably recorded, not that its
tools or the whole execution finished. The latest 128 model completion events
supply chronological metadata only; evictions emit explicit removals. The
partial index is installed for fresh, migrated, and existing version-7 Stores.

Independent source review cleared the projection and index repair. The owning
Rust 1.93 tests executed two cases successfully: bounded completion/reconnect
and existing-database reopen/index query plan. Logs are retained under
`target/completion-evidence/final-delivery/request-projection-reopen-fixed-test.log`.
The companion has a verified complete-history bundle at
`target/completion-evidence/final-delivery/harness-retained-model-requests.bundle`.
Combined Tidepool compilation and the actual browser journey remain pending;
this checkpoint does not establish M1 acceptance.

## Historical foundation evidence

These results belong to the September 29 checkpoints, not the current joined
release. Source-box transfer queues and temporary process state are superseded;
Git tree `d14deb83418d3c831e74b870204344b43955302c` retains the original
transfer/restart documents, exact commands, filters and branch/application
history. The historical destination
join used Harness `c485edb9b697ffc671b22c9ef25a73fc84763d76`, Codex
`2d58f00c6f139d745e0c123d31dfe6d2f04ff997` and workspace
`5248b927e7b432d1747891df5285d6827eace7d6`. Its bundle manifest and bounded
logs were recorded in `/srv/swarm/checkouts/foundation-transfer/README.md` and
`tidepool-evidence/` there. Those local preservation records are not published
dependencies or proof that present source can be rebuilt remotely.

| Historical boundary | Exact result and recorded evidence |
| --- | --- |
| Artifact migration | Seven registered producers regenerated ABI 8/schema 14 artifacts; seven decoded successfully and an old ABI 7 artifact was refused. Destination `target/transfer-evidence/{embedded-fixtures-update-2,embedded-artifact-gate-final,stale-abi-rejection-final}.log`. |
| Native/runtime parcel repair | At `c25a654476b0cd584bb6cf62fddb2aa2245df556`, 638 native library tests and 32 runtime session tests passed, one intentionally skipped in each selection. `joined-native-libs-repaired.log`, `joined-runtime-session-repaired.log`; the original failure remains in `joined-runtime-session.log`. |
| Full structural corpus | At `7881626da56c23732e27ed7af216614ac303a8b1`, unrestricted `scripts/fixtures.sh check` accepted all 12 reported cohorts, metadata and seven embedded artifacts. Suite: 234 successful comparisons; other cohorts: 48. `target/prepared-corpus/latest-success.json` records stage accounting; `fixtures-check-final.log` records the run. Oracle regeneration changed only the fingerprint, retaining payload digest `35ea9188d22b81bb8703f2cac36831935afc114d55acf1176009552d08565a98`. |
| Compiler product | Source-hidden fat/skinny typechecking, product roundtrip and retained-scope cases passed; one skinny interface was 2,336 bytes versus 2,747 fat bytes. `module-product-roundtrip.log`, `retained-scope.log`. This fixture-size result does not establish aggregate savings or durable reuse. |
| Matched host and adapter | Declared local host build and four facade admission/installed-handler/real-Event checks passed after parcel repair. `matched-host-build-repaired.log`, `joined-facade-adapter-repaired.log`; no live session or full M1 composition. |
| Companion | Harness embedded-host selection 5/5 and owner-completion race 1/1; exact Codex source selection 13/13 under Rust 1.95.0, retained in `foundation-transfer/client/exact13.log`. Component evidence is separate from joined M1. |

The compiler declaration-join experiment ran `cabal test
declaration-join-proof --test-show-details=direct` in the declared Nix shell.
Its six join/consumer pairs accepted hidden-name isolation, changed-type
replacement and distinct instance-only imports, and rejected ambiguous class
exports and conflicting type-family instances. The duplicate `instance C Int`
wrapper **compiled**, while later constraint use failed. The recorded
`declaration-join-proof.log` motivates explicit combined-instance validation;
it is not M2 publication acceptance.

Wave22's historical fingerprint input/run and nonreproducibility limits remain
in [the foundation investigation](engine-foundation.md#retained-investigation).
Source-box `/tmp/tidepool-wave22-fullcore` logs were not transferred by the
destination work; historical `G2` source/private `W2` dependencies were missing.
The portable probe has different dependencies, so compare its 0/1,000/10,000
rows internally, never claim an absolute historical speedup. The destination
probe recorded G3 approximately 0.917/0.214/0.221 ms and whole requests
160/183/539 ms in `retained-fingerprint-probe.log`; the first sample included
allocation activity absent in the later samples.

The shared-host memory hypothesis came from 37 wave21 native clients totaling
about 18.1 GiB PSS plus SwapPSS. It is a baseline, not measured replacement
savings. Process sharing does not eliminate compiler/JIT retention or serialize
CPU execution differently by itself; compare equal useful work/concurrency and
live versus retired owners. The original run observations remain in
[the wave22 audit](../docs/reports/wave22-audit/batch.md).

The September 29 runtime checkpoint also retained real Haskell capture tests
(2/2), failed-release/reinstallation retry tests (4/4), and exact idempotent
release-confirmation tests (2/2), under the runtime worktree's
`target/completion-evidence/{capture-haskell-final,release-retry,release-confirm}.log`.
Their producer commits were `a6ab1dbb38f2a55997e5b297fb69eea0c9a0fe96`,
`9a330b2223c5ed1f942c0dfad65f6f7c7b1c5610` and
`9350b19392328db41df8265a853c1870b9db588a`. They do not prove later private
publication or the full worker tree. Terminal/unavailable checkout keeps exact
unconfirmed cleanup custody with the existing owner; contention waits through
the session registry, and teardown reports unresolved custody.
