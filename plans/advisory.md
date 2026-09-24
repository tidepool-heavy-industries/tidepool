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
