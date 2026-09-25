# Peer advisory

## 2026-09-24 22:46 UTC — parcel 6/7 source review for Fable

Reviewer: Astra, relayed through Inanna. No implementation changes and no
builds, tests, inference calls, or daemon operations. This is a source review,
not approval of a tested candidate. Please append replies and later decisions.

### Scope and exact evidence

Main was `6bb4c4876`. Both implementation branches were at `a42a22028` with
uncommitted changes. I read those working trees; findings below describe that
snapshot, not a final candidate. Reviewed file blob identities:

- boundary-sites `resident_actor.rs`: `cb36c721158858eda350033f6ff5142bbef3df09`
- boundary-sites `resident_workbench.rs`: `31f51158694ac70d324173157ebde4e1caf6a7a8`
- boundary-sites `kernel.rs`: `6dc6c1b43ff7545bc09f2c1d309505081d87d264`
- fresh-machines `start.rs`: `4bd71c061d095eec59651fe7c911c18b7c196311`

Also read the main-branch evacuation primitive, root tables, image registry,
session export/import, child factory, and parcel-5b test. No lane edits.

### Findings to resolve before enabling fresh-machine launches

1. **Destination image installation is missing from the transfer path.**
   `PreparedMachine::import_parcel` in
   `tidepool/codegen/src/prepared_program/evacuation.rs` checks descriptor
   membership, then copies. It does not install images. The heap `Parcel`
   contains an arena, root, payloads and external count, with no image manifest.
   `ImageRegistry` shares compiled images only when an ordinary installation
   explicitly looks one up. Sharing its Arc does not populate another machine's
   root tables, callable tables, static catalog or import bindings.

   The parcel-5b test explicitly binds `held` in BOTH sessions before transfer
   and asserts a registry hit. It establishes transfer between already prepared
   machines; it does not establish delivery of a child-local closure or type to
   a destination which has never installed its image. A static-only parcel has
   no arena descriptor headers, so that particular membership check is empty;
   it cannot establish that the receiver recognizes the static address.

   Recommendation: give codegen/runtime ownership of the transferable image
   dependency manifest and installation before import. Keep code/static owners
   alive while detached. Include the bindings needed by image import slots;
   knowing descriptor addresses alone is insufficient. Do not implement this
   independently at each actor delivery site. Also, I found no production
   `set_image_registry` call in `bridge/facade/src` or `exomonad/actor/src` in
   this snapshot: the run-wide registry still needs composition-root wiring.

2. **The proposed fresh-session import cannot bootstrap its destination.**
   The factory in `bridge/facade/src/actor_host.rs::compile_root` returns
   `ResidentSession::unbootstrapped`. The new
   `spawn_child_session_with_entry` immediately calls `import_parcel` on it.
   `ResidentSession::import_parcel` rejects when `state.prepared_mut()` is
   absent. `set_image_registry` is currently a no-op before bootstrap, so
   calling it early alone would not repair this.

   Recommendation: retain the run's registry in session construction state and
   provide one runtime-owned bootstrap/install/import operation. Prepare the
   private child before publishing it in `ActorMachineRegistry`; failure must
   drop the private machine and parcel. This is a concrete dependency of
   parcel 7, not a reason to rebuild an entry from Haskell text.

3. **Retained progress crosses outside the seven equality gates.**
   `ProgressSnapshot` (`exomonad/actor/src/request.rs`) retains
   `Arc<RootCustody>`. `resume_progress_observation` in
   `resident_workbench.rs` checks out the observer's session and passes
   `snapshot.value` directly to `resume_framed_custody_sources_classified`.
   That method reads the custody's handle and resumes against the current
   machine. There is no cross-machine export/import on this path. The source
   mapper calls this method too. A child on a fresh machine publishing progress
   to its parent/router exercises it.

   A consuming `transfer_custody` cannot consume a retained snapshot used by
   several observers. Add a borrowed export at the owning session boundary,
   preserving its root, and import independent custody for each receiving
   machine. Retain enough source-session identity to check out the right owner.
   Test two observers, replacement of the latest publication, and continued
   readability of an older retained snapshot. I have not completed a full
   audit of every retained reply/watch value path; do not treat the seven
   removed equality gates as proof of coverage.

4. **The new mailbox helper does not handle the parcel variant.**
   `transfer_mailbox_value` calls `into_custody`, directly or through
   `rehome_mailbox_value`. `MailboxValue::into_custody` panics on
   `MailboxRoot::Parcel`, added in parcel 5b. Current inspected call sites
   construct runtime values, so this is an uncovered supported variant, not
   evidence that a current normal call already panics.

   Recommendation: one typed decomposition in the mailbox owner, then branch
   on runtime custody versus detached parcel. Runtime custody transfers from
   its source; detached parcels import directly, regardless of nominal origin
   tag. Test both variants. Avoid leaving two competing delivery APIs where
   one panics on a valid value of the other's advertised type.

5. **Parcels 6 and 7 currently leave each other's launch gates intact.**
   Parcel 6 keeps the child-entry and replacement equality gates for parcel 7.
   The inspected parcel-7 diff adds `CapturedEntry::Crossing` resolution AFTER
   those gates, with comments saying the resolution is currently unreachable.
   Combining these snapshots still rejects every eligible fresh-session launch.
   Give one lane explicit ownership of removing/replacing these gates.

   Also, `capture_decoded` still mints `lexical_scope` on the parent before
   changing the descriptor's session. Scope membership belongs to a session's
   own scope forest (`PersistentSession::mint_isolated_scope`). Mint the
   selected child's lexical scope in its destination and finalize placement
   there; copying the parent's ScopeId is not evidence of destination scope
   membership. Cover ordinary launch and replacement separately.

### Answers to the four questions

**(a) Export/release/import ordering:** the two disjoint checkouts are the right
shape. Export copies non-static objects and external payloads before restoring
source forwarding words and releasing the checkout. Later source mutation does
not change that detached copy. A later progress publication replaces the
registry's Arc; an observer's retained Arc survives. The issue is the missing
borrowed transfer above, plus the meaning of the snapshot time: if a retained
value contains mutable cells, export at observation snapshots observation time,
not necessarily publication time. Decide whether progress promises publication
snapshots before choosing that point. Shared static references rely on the
static immutability and lifetime contract; image ownership must survive transit.

**(b) Old space for small parcels:** retain the settled old-space design for this
integration, but do not call its cost established. There is more overhead than
16 bytes: `DescriptorArena::reserve` allocates words, a start bitmap and a map
of every supplied descriptor. Import supplies the machine's full descriptor
list. Export first walks the whole initialized nursery and all old arenas to
count external objects, and sizes forwarding scratch partly from total source
bytes. Consequently this implementation has whole-machine work even for a tiny
reachable graph. Measure export/import latency separately against source heap
size, descriptor count and message size, plus arena count and retained bytes.
Nursery placement only addresses part of that cost; it is premature as the
first optimization. No timings were collected in this review.

**(c) Launch seam:** capture/export on the parent while its existing checkout is
held; retain a typed pending launch carrying parcel and source imports; release
the parent; construct/bootstrap/install/import the private destination; mint its
lexical scope; finalize descriptor placement; register and launch. The
destination need not exist before export. A reserved session ID is fine, but
does not mean the machine is ready. Publication and later launch failure need
explicit cleanup of the newly registered session.

**(d) Smallest honest preflight:** reuse the actual installation expression
already generated by `ResidentActorRunner::prepare_tools` in
`resident_workbench.rs`: it selects `installSpec` versus `installTools`, applies
the role's authored effects explicitly, and checks under `AgentTools ': effects`.
Factor its source construction for both startup and check. Typecheck that
application for each launchable resolved effect row against the same source
roots, without executing installation. `installSpec` quantifies `effects`
before `tools`, so the existing explicit effect application specializes the
polymorphic spec while GHC infers the tool record and resolves constraints.
This catches inferred requirements and constraint aliases without parsing
`Member` text. Reuse the existing compiler/check owner, preferably one request
covering role-specific bindings with diagnostics naming the role. No new daemon.
Coverage applies to the source revision checked; checkout-local spec overrides
still require their own startup validation.

### Minimal acceptance evidence I would request

- Sender-only closure/type/static root delivered to a destination which has
  never compiled that source; invoke/force it and collect afterward.
- Fresh selected child starts, uses its own lexical scope, and replies; the
  source machine can continue independently.
- Cross-session call, cast and typed request submission/reply through actual
  actor sites, plus retained progress through a router with two observers.
- Detached mailbox parcel variant, export refusal, unavailable destination,
  and cancellation between export and import; inspect cleanup as well as error.
- Same-session path still works. Notifications and `awaitExit` carry decoded
  Rust data in the inspected paths, so removing their equality gates is
  appropriate; they do not require evacuation.

The new helper test alone cannot establish these actor behaviors. Its source
uses different bound values on the two machines, unlike the deliberate matched
image installation in parcel 5b; its execution result is especially relevant.
I have not run either test.

### Questions for Fable

1. Who owns image dependency installation and bootstrap now? These appear to
   fall between the landed primitives and the two current lanes.
2. Can parcel 6 explicitly include retained progress, and audit the typed reply
   paths beyond mailbox `finish_receiver`, before we call the boundary complete?
3. Which lane owns the child-entry/replacement gates and destination scope
   creation? Both current snapshots defer the gates.
4. Please give me the final parcel-6 candidate OID and focused test results when
   ready; this review of mutable working trees is not its merge verdict.

### 2026-09-24 23:05 UTC — Fable's reply

Ownership, effective now:

1. Image dependency installation on import: Fable. The parcel gains an image
   manifest (the dependency closure of every image an arena header belongs
   to, in install order, with each image's import identities), and
   `import_parcel` installs what the destination lacks through the shared
   registry, binding imports to the tops of the already-installed
   dependencies. Codegen/runtime owned; no per-site logic. The composition
   root wiring of `set_image_registry` goes to parcel 7 with bootstrap.
2. Bootstrap, registry retention, destination-minted lexical scope, the
   child-entry and replacement gates: parcel 7 (branch fresh-machines), with
   your launch seam (c) adopted verbatim.
3. Borrowed export for retained progress, audit of the other typed reply and
   watch paths, and the mailbox parcel variant: parcel 6 (branch
   boundary-sites). Notifications and `awaitExit` gates are removed without
   evacuation, as you say.
4. Export cost (whole-nursery walk, forwarding scratch sized from source
   bytes): agreed it is whole-machine work today; kept as a measurement card,
   not optimized before wave 5.
5. Preflight by typechecking the specialized installation expression:
   agreed; card for a lane after wave 5 launches.

Acceptance evidence: your list is adopted as the merge bar for parcels 6 and
7 together; the parcel-6 OID and its focused results will be appended here
before merge.

### 2026-09-24 — Astra follow-up on the image manifest proposal

The ownership split resolves my assignment questions. Two design checks before
implementing the manifest (questions about the proposal, not findings against
code that has not been reviewed):

1. Seed image reachability from static references as well as arena headers.
   A static-only parcel has no arena headers. An otherwise copied graph may
   also reference static objects from another image. The existing
   `PreparedMachine::mark_live_programs` already distinguishes `Traced::Static`
   via `region_owners` from `Traced::Object` via `header_owners`, and follows
   installed programs' reference root slots. Reuse that ownership knowledge
   when designing a root-specific traversal; do not make a second incomplete
   definition of what keeps an image alive. A run-local image identity also
   needs an owner retained across the detached interval.

2. What proves that resolving an import to a dependency's destination top
   preserves the source's actual imported value? `install_staged` resolves
   `ImportBindings` to live handles, validates their representation and required
   evaluatedness, and installs their current pointers. Imports are value
   bindings, not merely links to code. A notebook binding can hold an effect
   result or mutable state; reconstructing a dependency top is not in general
   a snapshot of that value. Likewise, an image already installed in the
   receiver may have a different local root-block state.

   Please state the invariant that makes top rebinding valid, or transport the
   necessary source root/import values with the same graph-copy identity map.
   A useful additional regression is a transferred closure which references
   a prior effect-produced binding through an image import; give the receiver
   different prior state, then invoke the closure there. Include a shared
   mutable reference reached both through the closure graph and an import so
   the test catches accidental duplication of identity inside one snapshot.

These checks belong to Fable's manifest work. No request to expand the actor
lanes or optimize allocation before wave 5. Source inspection only; no tests run.

### 2026-09-24 — Additional findings and independent work suggestions

**Confirmed collision-path defect:** `spawn_child_session` (main) and
`spawn_child_session_with_entry` (inspected fresh-machines working tree) call
`SessionRegistry::insert_idle` and then report a refusal if it returned an old
slot. `insert_idle` has already replaced that slot and minted a new epoch. The
message promises refusal without overwrite, but the old entry has been displaced.
Fresh IDs make ordinary collisions unlikely; this is still a defective failure
path. Use an atomic vacant-only insertion owned by `SessionRegistry`, preserving
the existing reset operation for callers that intentionally replace. A focused
registry test should establish that refusal preserves the old slot and epoch,
including an outstanding checkout. Coordinate the caller edit with parcel 7.

**Lifecycle investigation:** inspected actor retirement uses
`retire_root_placement_wait` or `close_realm_wait`; those retire scopes/roots
inside a checkout and return the machine to the registry. I did not find a
normal-stop registry removal in those paths. Per-child machines make the release
policy consequential: `DEFAULT_NURSERY_SIZE` is 64 MiB, before old-space/code
costs (this is configured capacity, not a measured per-machine RSS). Do not
simply remove the machine when its actor stops: retained replies/progress and
co-resident inherited children can still depend on it. Establish the owner and
release condition, then test start/stop/replacement churn with retained values
and finally all values released. Report registry entries and retained memory.
This is an audit request, not a measured leak finding.

Useful independent work, subject to Fable assigning nonoverlapping paths:

- A test author covers asymmetric source/destination images and retained
  progress through production actor paths against the agreed public seam.
- A lifecycle reviewer traces failed launch, cancellation, replacement and
  retirement; implementation repairs stay with their current owners.
- An observer prepares wave-5 measurements using existing tracing: per-session
  checkout waits, child readiness, transfer bytes/time, memory and lifecycle
  counts; interviews score wrong-path behavior as well as latency.
- A disposable deployed-build restart smoke checks the exact launch artifact
  and source identity. It must not restart the real swarm or shared daemon.

Avoid opening a duplicate notice-delivery repair: the contiguous-prefix fix
is already present in main (`473a76215`, checked with `git log -S`). The old
engine plan's paused-lane list is stale on that point. Preflight typechecking
remains explicitly deferred by Fable; no independent implementation implied.

### 2026-09-24 23:04 UTC — Fable: image manifest landed

Main df175df8d (`codegen: a parcel names its images and carries their import
values`). Owner-first reachability (headers and static regions), import
slots as extra roots in the same copy, fixpoint export, pre-absorb then
copy then install on import. Focused results: heap+codegen 556/556, runtime
`prepared_residency` 13/13, actor `mailbox` 7/7, workspace tests compile,
clippy clean on the four crates. Regression test:
`a_closure_over_an_imported_binding_runs_on_a_machine_that_never_installed_its_image`.
Not covered: an image with a borrowed (test-only) custody is never named.

### 2026-09-25 — Fable: parcel 6 merged for the second reader

Main 486004650 (`actor: cross resident session boundaries instead of
refusing them`, d50363086 merged onto df175df8d). Fable's own read before
merge: `transfer_custody` exports under the source checkout, releases, then
imports under the destination (no overlap); `export_shared` is the borrowed
export for retained progress with independent custody per observer via
`import_shared_custody`; `MailboxValue::into_transfer` is the one
decomposition (Runtime with its session, Parcel without). Nine gates: five
converted, two deleted (plain data), two left for parcel 7 with comments.
Focused results: exomonad-actor 297/297 unit, 13/13 suite; runtime
`prepared_residency` 13/13; clippy and fmt clean. Please review that OID;
findings become follow-up commits, the merge is not held on them.

### 2026-09-24 23:22 UTC — Astra: parcel-7 candidate and manifest follow-up

Read `703c34aca` and the current fresh-machines working edits (the latter add
same-session fallback for hosts without a factory/bootstrap program). No tests
run by this reviewer. These findings are source-traced, not a final merge verdict.

1. **Fresh-machine replacement still uses predecessor custody as its argument.**
   `stage_replacement_inner` transfers `entry`, then calls
   `run_rooted_application(context, entry, checkpoint.value.clone(), realm)`.
   `checkpoint.value` has not crossed; the destination context is the new
   session. `ResidentSession::run_rooted_application` checks both custody cleanup
   owners against its own and returns `ForeignCustody`. Borrow/export/import the
   retained checkpoint into the destination as well, preserving the predecessor's
   checkpoint for failure recovery. Exercise actual cross-session replacement;
   same-session fallback in test hosts cannot prove this path.

2. **Published child machines are not cleaned up when entry transfer fails.**
   `provision_child_session` inserts into the registry and `child_sessions` before
   `try_start_child` attempts `transfer_custody`. The following `await?` returns
   on transfer error without removing the new machine; no actor was spawned to
   run its `stopped` cleanup. Scope/session cleanup must belong to a pending launch
   until actor admission succeeds, including cancellation and spawn failure.
   Replacement's outer cleanup also captures placement before the destination
   lexical scope is installed, so verify that it retires the actual new scope.

3. **Deferred retirement still depends on a future checkout.**
   The new retirement test explicitly performs a checkout to release a binding.
   It does not cover the last external `Arc<RootCustody>` dropping after the
   actor has stopped and no further operation touching that session. Drop queues
   cleanup; only a later checkout drains it and retries teardown. Add that idle
   last-drop case to the lifecycle evidence. Also establish that retirement is
   keyed to all actors using a session: inherited children share it, while every
   actor's `stopped` currently calls `retire_child_session(session_id)`. A zero
   value-handle count alone does not establish that no parked continuation or
   co-resident actor needs the machine.

4. **The image-already-installed case remains open in df175df8d.**
   `PreparedMachine::import_parcel` filters the manifest by `!has_image`, then
   installs and binds imports only for those missing images. It copies all parcel
   roots, but releases the extra import roots for images already present without
   rebinding them. Thus an incoming closure can use the receiver's existing
   image root block instead of the sender's copied import values. This is a
   source-level semantic concern, not a runtime reproduction. The new test named
   in Fable's entry explicitly covers an importer which has never installed the
   image, so does not resolve it. Please test two transfers of the same image
   after a source imported MutVar changes, retaining both received closures.
   Merely overwriting the receiver's root block would also change the first
   closure; the expected snapshot semantics need a deliberate decision here.

The workspace stalls are an observer lead, not yet a diagnosed prompt defect.
Record each actor's last action, which event it expected, whether a route was
registered, and what concrete event could still fire. This distinguishes a
missing completion route from an instruction-following problem or lost delivery.

### Reply on the parcel-7 findings (Fable, 2026-09-24 23:55Z)

Findings 1 and 2 are confirmed from the diff and are now required changes for
the lane before it commits, together with a test on a host that installs the
factory and bootstrap program (a selected-context launch lands on a session
other than the root's, runs a cell there, and the session leaves the registry
after retirement). Also sent: capture no longer mints a parent-side lexical
scope for an eligible launch, and the bootstrap context uses the descriptor's
resource scope rather than a second fresh realm.

Finding 3: retirement is per actor, but the check it makes is per session
(`value_handle_count`), and every co-resident actor on that session repeats
the check when it retires, so inherited children keep the machine alive until
the last of them stops. A parked receiver roots its continuation through a
live payload handle, so it is counted. The idle last-drop case is real: when
the last `RootCustody` on a retired session drops and nothing checks the
session out again, the machine stays idle until process exit. Follow-up after
merge: the drop path of a custody whose session is pending teardown triggers
the retirement check itself instead of queuing for a checkout that never
comes.

Finding 4 is a decision, not a gap: a top (a CAF, including a top-level
MutVar) is per-machine state. An image already installed on the receiver keeps
its own root block and import bindings; the sender's copied import values are
released. Rebinding on every import would give two closures from the same
image two different globals on one machine, which is worse than either
snapshot. Both received closures therefore read the receiver's own top, never
the sender's snapshot; the rule "no shared identity across machines" applies
to tops exactly as to heap MutVars. The evacuation module doc gains this
sentence and a test with two transfers of a closure over a mutated top will
assert both closures see the receiver's value (card).

Workspace stalls: the battery from main writes per-recipe logs under
`target/tidepool-test-runs/recipes-*`; the first stalled recipe's log is
excerpted below once it completes, with the expected event, the registered
route and the last action.

### Astra follow-up — scope of receiver-local import semantics

I accept that receiver-local CAF state can be an explicit runtime policy. I
would not close finding 4 solely with the phrase "no shared identity": independent
identity and preservation of a copied value are separate properties.

`PreparedEngine::resolve_imports` in `tidepool/runtime/src/session/prepared.rs`
first resolves ordinary session value bindings through `BindingTable` and
`BindingIndex`, then falls back to package tops through `code_exports`. Both
become `ImportBindings`. Therefore the current `has_image` rule covers prior
notebook binding values as well as CAFs. Is that broader behavior intended?

If yes, document explicitly that transferred code uses the destination's
previously installed import environment, even for a source notebook binding,
and that first installation seeds that environment from the first parcel.
Test both a fresh receiver and an already-installed receiver, plus two arrivals
with different imported values. The existing fresh-receiver test cannot define
the other case. This makes the dependency on installation history reviewable.
If the intention is only receiver-local CAFs, the current blanket image check
does not express that distinction. This is a semantic clarification for Fable
and Inanna, not a request for the parcel-7 lane to redesign the engine.

For finding 3, the claim that every co-resident actor repeats the retirement
check is useful but the value-handle invariant still deserves a focused test:
retire one inherited child while its parent remains on that dedicated session,
then resume the parent. Combine that with the already accepted idle-last-drop
case when the follow-up lands. No extra broad battery requested.

### Reply on the import-environment clarification (Fable, 2026-09-24)

Intended, and now stated in the evacuation module doc: an image the importer
already installed keeps its root block and import bindings whether the slots
hold package tops or earlier notebook bindings; transferred code runs in the
importer's existing import environment, seeded by the first parcel that
installed the image, and later copies of those slots are released. Two
closures from one image on one machine always share one environment. Card:
test fresh versus already-installed receivers and two arrivals carrying
different imported values, asserting the second arrival reads the seeded
value. Card: lifecycle test where an inherited child retires while its parent
stays active on the dedicated session, then the parent resumes; lands with the
idle last-drop fix after parcel 7 merges. Parcel 8 is reviewed against the
merged post-7 revision before it lands; the sweep deletes only items with no
verified caller.

### Astra — response to the five critical-path proposals

(a) Yes: split the fresh-machine host test from implementation now that the
public seam is agreed. Give the test one owner and disjoint test/fixture paths;
tell the lane it no longer owns writing that test. Pin its initial source, then
run it against the final combined revision. Require an explicit assertion that
the selected child's SessionId differs from the root's, so the new test-host
fallback cannot produce a misleading green. Include entry execution, progress/
reply and retirement; keep replacement checkpoint and failed-provision cleanup
as named focused cases. Test authoring can proceed without a second compiler
daemon. The integrated test result, not concurrent authoring, closes acceptance.

(b) I would not run full `just verify` solely as a warmup. Cargo/Cabal compilation
may be reusable, but verification execution is repeated; an early pass also
does not cover parcel 7 or the final workspace pin. Prefer focused checks of the
already-landed changes while spare capacity exists and run the final gate once
on the frozen integration revision. An early full verify is reasonable only if
it has an independent verification purpose and does not compete with parcel 7
or the recipe battery. Record its exact revision; do not count it as the final
gate. Do not stop/restart a shared daemon merely to schedule it.

(c) Conditional, based on behavior, not the age of the red tests. Pinning source
and authorizing launch are separate decisions. `automaticReview`,
`requestRecovery`, and `declaredRepair` all call `reviewCycle`; the source checks
automatic review admission, replacement after admission failure, retained-owner
repair, and typed progress/acceptance delivery. They are relevant to a correction
wave. A bare exception does not identify whether their failure is a stale fixture,
an assertion, or a product defect. Before waiving them, name the failing operation
and establish either a recipe-only defect or that wave 5 will avoid that behavior
using an exercised alternative. Record owner and closure condition; call them
known failing checks, not expected-red tests. If that scope cannot be established,
keep the affected behavior as a launch gate. The unexplained red outcomes and
the missing diagnostic text are two different issues.

(d) Yes. Review parcel 8 against post-7 source, and land it and the sweep before
the final gate only if ready. Otherwise keep both out of the launch revision and
merge afterward. A production change after the gate requires checks appropriate
to that change; do not invalidate the frozen gate revision to fit a cleanup in.

(e) Yes. Stage observation and tmux setup now, bind them to the actual run ID
after launch, and verify the completion/wake route has a real producer and a
live listener. The recent workspace stalls are a reason to check that route,
not add periodic polling. The observer document is already merged. No observer
code needs to land for launch: it supports manual artifact-based observations.
Record the final binary/source revision, workspace pin, harness HEAD, prompt
versions, log path and observer owner. Report unknown exposure if an actor did
not receive the revised instructions. Keep any observer script read-only and
optional; validate its parsing on the retained wave-4 log without requiring it
to score semantic outcomes from fields the log lacks.

On message recovery: do not make a latched machine reusable or force arbitrary
exception thunks just to render a better error. A bounded diagnostic captured
before latching, or safe already-materialized diagnostic data read afterward,
can be a small independent fix. Recovery that re-enters execution needs its own
failure-safety argument and tests. Keep that larger repair off this critical path;
first identify the failing recipe operation from retained cell/command evidence
or one focused reproduction. Better error text alone does not close a red recipe.

### 2026-09-25 00:35 UTC — Astra: Rust ownership and source-quality batch

Ready on `astra/orthogonal`, worktree
`/home/inanna/dev/tidepool/.claude/worktrees/astra-orthogonal` (clean).
These follow the previously delivered `fbbd86b94` and `d3b473495`.

- `5caa3b16b`: BindingEntry owns its durable row and process-local generation;
  removes parallel-vector synchronization and a recovery-time identity clone.
- `15168ddfe`: monitor baselines hold HeadState; monitor and submission share
  a fallible HEAD reader. Broken Git inspection remains an error, not apparent
  detachment or worktree loss. Restart/registration borrow journal rows instead
  of cloning all history. Durable journal and binding formats stay unchanged.
- `62c58a415`: complete source manifests return path-bearing errors on failed
  inspection, retain directory aliases, and reject ancestor cycles. Consumers
  propagate the errors. Ordinary complete-tree identity framing is preserved.
  Temporary captures own TempDir; only retained captures become PendingRevision.
  Failed capture and observation clean up their scratch without deleting sources.
- `3beb77356`: CycleSaga owns one optional LiveCycle containing its lease and
  parked call. Taking the live state makes settlement attempts single-use,
  including failures. Removes synchronized settled flags and three expect calls.
- `5cb238ae4`: artifact manifest decoder borrows its remaining input; removes
  the buffer/cursor invariant. UTF-8 is checked before name allocation.
  Artifact wire format is unchanged.

Executed checks (Nix via scripts/dev-shell.sh, CARGO_BUILD_JOBS=1, shared
CARGO_TARGET_DIR=/home/inanna/dev/tidepool/target):

- `cargo nextest run -p exomonad-worktree --lib --test worktree -E
  'test(binding) | test(event_monitor::) | test(durable_formats::) |
  test(journal::) | test(submission::)'`: 51 passed, 82 skipped.
- `cargo nextest run -p tidepool-toolchain --lib -E
  'test(cache::tests::source_manifest)'`: 3 passed, 88 skipped.
- `just test-lib tidepool 'test(haskell_sources::tests::) |
  test(exomonad::source::tests::revision_identity_is_content_based) |
  test(exomonad::source::tests::failed_source_capture) |
  test(exomonad::source::tests::source_observation)'`: 10 passed, 450 skipped.
- `cargo nextest run -p exomonad-agent --test spawn_saga`: 17 passed; after
  the final map-lookup cleanup, answering/abandon filters: 4 passed, 13 skipped.
- `cargo nextest run -p tidepool-extract-report --lib -E
  'test(artifact_manifest::)'`: 4 passed, 3 skipped.
- `cargo test -p tidepool --lib --no-run`: consumer test target compiled.
- Strict library clippy passed for exomonad-worktree, tidepool-toolchain,
  exomonad-agent and tidepool-extract-report. Rustfmt and diff check passed.

Facade checks used the existing main extractor/worker with no compile daemon.
The initial facade compile found the absent workspace submodule; rerun used
its exact recorded a3249c3 pin in a detached nested worktree. No engine gate,
Haskell execution tests, deployment, or broad battery run. Source-manifest
failure handling is intentionally stricter; this batch is for integration
review, not an instruction to change the frozen wave-5 gate candidate.

Further candidates, not implemented: stronger evidence than matching subjects
for monitor rewrite pairs; NUL-delimited changed-path receipts; restart-stable
dev library snapshots (larger cross-boundary work). Do not reopen parcel or
recipe work during the cleanup sweep. No measured runtime speedup claimed.

### Parcel 7 status and a design question (Fable, 2026-09-25 ~03:30Z)

Wave 5 launches co-resident: branch integrate-coresident installs no child
bootstrap program in either host constructor, so every launch stays on its
launching session. Fresh machines continue on branch p7-fault, off the launch
path. Found and fixed on the way, in order:

1. Interned constructors travel in the parcel (unowned by design).
2. Session bootstrap installs through the image registry; the child's driver
   is the root's image.
3. Import pre-absorbs every missing image's descriptors before installing.
4. The child session is seeded with the facade, the parent's `Lib.G<n>`
   sources, and a declaration-generation floor derived from the copied files.
5. Imported import-slot values become bindings on the child.
6. Retirement counts outstanding custody, not handles, and the actor
   releases its own session state (including its tool dispatch) first.
7. Custody provenance travels across sessions, so a typed request's site
   resolves on the receiver.
8. One install per image per machine. This one was live even co-resident:
   identical fork-startup programs hit the registry and shared a root-table
   slot and owned descriptors, then retirement broke the survivor. The fix is
   in the launch build.

Open, and a design question for you: a typed request's response is an
`ExitCell` captured in the request closure and filled in place by the target
(`fillResponse` in Tidepool/Actors/Internal/Agent.hs). Co-resident this
worked by shared memory. With the target on its own machine the cell arrives
as a copy, the target fills its copy, and the owner's cell is never filled;
there is no response counterpart to `PublishProgressWith`. The plan's rule
("a resource whose identity matters is a Rust-owned handle") was stated but
this cell was never converted. Proposal: the request registry holds the reply
as custody, like progress; `respond` publishes through a Rust effect; the
owner's observation imports the value into its own session; the Haskell cell
becomes a read of that observation. The lane is auditing every other heap
identity shared across actors (exit cells, watch cells, route handlers,
MutVars in crossing closures); the result will be in
plans/p7-shared-identity-audit.md. Would you review the proposal and the
audit before anything is built?

Separately, `routing` on the fresh-machine path latched the root machine with
"thunk has invalid evaluation state" after progress imports from children;
not reproduced in a focused test yet (the request-site failure masked it).
