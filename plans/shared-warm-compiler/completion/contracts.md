# Locked decisions for the completion wave

Read [plan.md](plan.md) first. These decisions incorporate the user's review and
the independent source audits at `6f7ae09b3b`. A worker may simplify an
implementation while preserving these contracts; changing a contract requires
the campaign owner's review before implementation.

## One supported path

The compiler world service is the sole resident cell compiler. Delete the finite
actor admission/preparation route and its fixture-only alternatives in the same
integrated tranche as the replacement. Warm reuse and cold reconstruction are
states of this service, not alternative engines. Missing residency restores the
same authenticated logical world; it never selects legacy preparation or replays
cells/effects. Build-action portable fixtures and deliberately untrusted wire
fixtures are distinct consumers, not permission to preserve resident backdoors.

Reuse existing GHC cell planning, certified products, checked items, compiler
transactions/END, evaluator, materializer and publication owner. Do not create a
second certifier, graph registry, selected-generation log, generic query framework,
test framework or teardown supervisor.

## Facts and ownership

- Rust owns primary published bindings/source selection, runtime values, exact
  native execution provenance, grants, operation settlement and atomic publication.
- Haskell owns derived compiler environments, dependency checking and compatible
  resident realizations. Logical identity is independent of worker/process identity.
- Custody, canonical interface compatibility, selected implementation, issued
  dependency edges and executable demand are separate facts. Equal payloads or
  interface bytes do not make different issuing selections interchangeable.
- Fresh source/execution admission is required even on a warm hit. Keep issuing
  source A distinct from receiving target B, both captured from their real owners.
  A caller's current transaction cannot substitute for the original A.
- No machine checkout or mutable actor-state borrow spans compiler/shutdown waits.
  Exact output and cleanup ownership survive a disappearing waiter.

## Representation boundaries

Apply [the code-quality decisions](quality/plan.md) at the existing owners.
Admitted operations retain their parsed plan; completed products derive their
views without separately supplied owner facts. Use one typed item/closure entry
instead of rejoining parallel tables. Keep recorded facts separate from acquired
live roles, and descriptive compiler labels separate from runtime permission.
The compiler's logical identity comes from its existing semantic roots, never
work counters or transport layout. Preserve the native issuance order: physically
bound current output enters the availability issuer, which produces issued current
availability for execution consumers; pending output cannot enter that issuer.

Prefer private complete payloads, small enums and consuming transitions over
independent flags, optional required fields and public trusted-value builders.
Delete replaced constructors and raw escape accessors. Immutable facts remain
shareable; exact acquisition identity and temporal authority remain dynamic.
Normalize immutable input once without demanding future GHC stage outputs early
or serializing a live compiler environment. Carry compound failures through their
actual consumer and drain every state-owned resource on terminal transition.

## Prepared output, item settlement and final publication

Extend the existing checked output owner; do not pair free-standing world IDs with
arbitrary output receipts. It retains its exact base, operation, ordinal, source
selections, compiler origin and successful output. Consume the matching settled
compiler transaction before sealing executable items. Unconfirmed END retains
owners and refuses execution/publication; it is never an excuse to retry cold.

The world client's existing completion path combines exactly two owned inputs:
the checked item's successor description and that item's actual runtime settlement
delta. It derives no compiler roles, native choices or edges from successful
evaluation, names or broad inventory. This combination assembles already issued
facts into the logical product; it does not issue new compiler authority.

Use item-specific deltas: declarations/imports change their checked lexical/source
facts; bindings and expression observations carry actual accepted binder identities
and issued rows; removals/shadowing include the expected prior fact. Preserve the
whole-cell typecheck-before-effects contract. Items advance private prefixes only.
A failed/cancelled cell publishes no new cell names; prior separately committed
cells remain visible and completed external effects remain recorded.

Final public scope remapping and removals come from the SAME prepared binding
promotion, after removals. Prepare immutable backing before claim, recheck the
semantic base, and commit world association plus bindings in the existing atomic
manifest callback. Metadata-only restaging may reuse semantic facts; a changed
semantic base refuses without replay. Retain predecessor and candidate custody
until commit and cleanup settle; uncertain publication remains recoverable.

No mandatory extra GHC round trip is required merely to record runtime identities.
The next existing ensure/acquisition request carries the authenticated logical
product/predecessor relationship and needed changed blocks. Haskell realizes it
against that exact predecessor or reconstructs the same product cold. Inherited
dependency reuse validates exact edges and producer/profile/source/interpreter
compatibility; matching source hashes do not select a predecessor. Share immutable
facts/backing while keeping mutable attempt state independent.

## Native selection and published originals

Extend existing native support/verified-binding owners with explicit demand
provenance. A root consumes its carried exact choice; a historical dependency
consumes an authenticated parent edge and carrier; a current product consumes
authenticated completed output. Only the last can issue a new product binding.
Absence of a choice never means "pick the sole available candidate". A foreign
root may legitimately retain a historical child through its exact issued edge.

Availability-only candidates remain custody-only unless the real completion
explicitly authenticates their accepted cached product and dependency evidence.
An unrelated successful output cannot promote all contextual candidates.

First publication closes the compiler-issued lexical/type/instance/native
obligations, including empty imports, family/orphan requirements and exact native
edges, then captures its birth selection. This is required closure, not the entire
ambient inventory. Reopen validates the immutable birth; current role projection
is a separate operation. Retain birth through its existing owner while selecting
only current roles/demand. Unrelated withdrawal succeeds; missing required roles
refuse. Never edit birth or use an API that restores all birth roots as a narrower
projection. Keep competing historical custody without making it executable.

## Source lifetime, reload and cleanup

The [source authority lifetime contract](contracts/SOURCE-AUTHORITY-LIFETIME.md)
governs the original-source/lexical-resource split. Installed provenance retains
canonical source tokens; actual generations, captures and operations retain
lexical leases. Automatic resource release and explicit source cutoff are
different owner transitions.

Preserve affine Fresh/Bound/Consumed preview states. Capture original A and target
B separately. Canonical identity plus Open-gate admission must enclose the bind
claim; recheck both before consumption. A refused run does not reset a bound token
to Fresh. Generic ordinary installs require the same runtime-issued source-bound
preparation; explicit bootstrap recipes use their existing production issuer.

Reload validates frozen candidate B first. Existing RunState owns one affine
retirement attempt that atomically fences new A-generation admissions and captures
actual active generation-dependent non-LLM work, including pending startup and
transfer. Its required immutable custody is retained separately; inert historical
values/code/source and settled receipts are not blockers merely because they remain
rooted. Default mode reports blockers without stopping them. Force drains only
that captured set through existing owners. Inspect the physical closure of each
proposed stop and refuse collateral shutdown of independent children before any
stop. Independent LLM worlds and the exact compiled self-reload continuation/receipt
remain live; new old-generation work refuses.

The private publisher sends an affine **unreserved** prepared product to
RunState. The live receiver reserves it only after selecting its existing
reservation record and securing owner-loss retention capacity. A failed send
or dropped queued preparation releases B without fencing A. Once reserved,
the actual attempt, candidate and observer enter that record before an await or
stop. Reserving outside RunState permits an unowned publication attempt to be
created after RunState has already published its terminal owner-loss result.

The attempt owns candidate custody, including B's authenticated logical compiler
world and restoration roots from the same frozen candidate preparation, and
stop/publication observations even if its caller disappears. Publish and recover
that world with B's source, tools, prompts and notebook reset through the existing
generation owner; never reconstruct it from a later draft C. Reopen A only when
the attempt still owns its fence, A remains
selected, publication is known uncommitted and initiated stops/cleanup have settled
without uncertainty. Known partial stops remain in the receipt; restart nothing.
Unknown stop or commit keeps the owner fenced for explicit recovery. Persist the
necessary outcomes through existing operation/recovery owners, not another log.

Known cleanup refusal, including a confirmed busy-reader refusal before release,
returns the SAME opaque custody with a typed cause. Successful release has a
receipt; lost/panicking physical settlement remains retained uncertainty. Preserve
the primary causal classification and independent secondary cleanup/END/storage
obligations; the combined outcome governs retryability and recovery. Physical
release cannot require fresh source execution permission.

## Query observations and compatibility

`currentScopeView` is an entry snapshot. Compiler-query answers carry that precise
view and compare it again at continuation settlement; pinned mismatches refuse.
For `scopeBindings`, perform final view comparison and guest resumption in one
existing machine checkout. Snapshot-only immediate results are labelled as such;
do not promise a universal reply-time fence. Allocation counters alone do not
change semantic provenance. Availability/row fit is advisory compiler evidence,
not runtime resource authority. `typeSearch` bounds emitted answers, not all work.

Internal API compatibility is not a goal. Change the existing wire/durable version
when its meaning changes and explicitly refuse unsupported older formats. Do not
retain legacy execution to load them, silently reinterpret identity, or add a
migration framework. Preserve saved source/memory and deliberate compatible
formats; record the exact refusal and fresh-run path.

## Tests and evidence

Port behavioral contracts through production world acquisition, output issuance,
publication and recovery. Delete helpers that fabricate trusted proofs or implement
their own merge/restore semantics. Raw malformed-data fixtures remain valid local
decoder/authenticator inputs; they must not become positive execution authority.
Use the existing proptest and hosted campaign infrastructure, independent models
and shrinking. Keep payload, issuing selection, current roots, custody and demand
as independent model facts. Verify exact identities, real refusal causes,
observable work and release—not incidental Arc counts, matching rendered text,
unchecked booleans or an empty set of observations.

Every declared pass identifies the exact code, executed body and terminal outcome.
Compiler linkage, query dispatch, public API execution, frozen acceptance and
measured speedup are distinct evidence. No source-only helper, constant telemetry
or successful legacy run satisfies an acceptance requirement on the new path.
