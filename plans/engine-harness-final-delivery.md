# Final engine and embedded harness delivery

Approved for implementation 2026-09-30. This refines
`engine-harness-completion-wave.md`; conflicting older sequencing is superseded.
The finish line is verified release readiness. Live trials, pushes and backend
default changes remain separate. Native goal tools stay disabled. Codex is not
built with Buck; the resident compiler remains a runtime service.

## Source checkpoint and ownership

Starting main: `ff2a9c3edde61690a49012fd8d848e2955d2afd7`.
Candidate tips: compiler `595b2f72f`, runtime `dccb98a26`, actor `f5ba3851b`,
facade `667bf6061f`, harness `31ae2e52`. Preserve equivalent cherry-picks and
apply dependency-complete commits rather than importing older branch ancestry.
Compiler/helper and generated Buck WIP is preserved under
`target/completion-evidence/final-delivery/20260930T165141Z`.

The protected hidden display and original declaration-to-binding-to-expression
native gates passed on compiler candidates. Runtime Ephemeral binding tests
passed. Neither M1 nor M2 is accepted; private and keyed actor admission remain
disabled. Existing runtime fresh-process recovery does not establish production
embedded startup/restart. The evidence ledger must distinguish these results.

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
