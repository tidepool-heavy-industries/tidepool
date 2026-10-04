# Finalized compiler products and explicit output

## Delivery scope

Qualify the joined finalized compiler products, explicit display, authenticated
replies and atomic cell publication through a finite set of owner checks,
deterministic embedded scenarios and a fresh matched worker-tree smoke.
This plan specifies acceptance; source review and executable linking do not
establish that any gate ran or passed.

## Ownership contract

- Keep exact GHC identities typed until wire encoding. Carry validated immutable
  evidence across compiler phases instead of reconstructing authority from
  names, paths, or rendered types.
- Use sum types for genuinely exclusive admission modes. Separate a product's
  semantic identity from the request context's obligations; preserve and check
  each context's obligations without changing the immutable product.
- A finalized module owns its canonical skinny interface and matching tidy
  Core, produced by the same frontend execution. Keep compact checked facts;
  do not retain a mutable HscEnv or TcGblEnv in the product.
- Ordinary checks may use provisional interfaces. Durable witnesses require
  finalized owners, finalized in dependency order before their importers.
  Never retarget provisional certificates to a different final interface.
- One canonical interface node owns each exact module identity within a selected
  sealed closure. The inventory may retain independent immutable versions
  globally by content ID; admission and merge resolve requirements only within
  the selected closure. Each native implementation is a separate immutable node
  with an edge to its exact carried canonical interface ID. Interface nodes
  never acquire edges to later native implementations.
- A type-only capability cannot gain executable authority when native code is
  added. Native dependency edges select exact implementation versions.
- A certificate binds producer/profile, interface and Core companion hashes,
  actual finalized dependency seals, package evidence and complete home units.
  Missing source-free recovery evidence refuses demand; it never permits
  recompiling source or replaying TH to reconstruct an old product.
- Canonical certificate v3 also authenticates each source original's import
  shapes: qualifier, module, boot flag and selected home-unit classification.
  Current source selection must agree with those sealed shapes and the exact
  admitted owners. This is evidence on the canonical interface, not a third
  dependency graph alongside canonical interfaces and native implementations.
- Core companions use bounded captured-file descriptors. The ordinary exact
  type-interface reader continues to reject defining Core.
- Request helper selection is request-local and explicit: none, or actor reply
  helpers. Native signatures carry authority; printed types carry presentation.
- GHC make consumes request-owned interface views. Executable demand attaches
  the separately authenticated Core; type-only demand stages the skinny bytes.
  Source identity and dependency admission remain unchanged. Preserve GHC's
  loaded linkable when restoring the canonical skinny interface, and retain
  views through deferred checking. Durable artifact paths never become make
  scratch paths.
- The NoLink compiler request boundary retires prior home execution symbols
  through GHC's loader. Immutable interface/Core/bytecode caches remain separate
  from loaded symbols, so reuse cannot execute a previous source version.
- Retained compiler execution closes over canonical dependency seals, including
  authenticated interface/Core owners without native products. `HomeProducts`
  supplies checked Core attachment for durable and transaction-local owners;
  GHC's `loadIfaceByteCode` consumes the paired interface and hydrated types.
  Missing executable Core refuses demand. Current authored imports, lexical
  visibility, instances, families and fresh provider compilation retain their
  separate admission checks; loading old code cannot publish hidden names.
- Execution schema 15 and ABI 9 carry explicit constructor reply evidence.
  `Static` identifies an ordinary reply type; only `AtSite` interprets the
  constructor's first field as an erased `RequestSite`. Its input and reply
  parameters have nominal roles. Exact constructor/site evidence authorizes
  reply delivery; rendered types and constructor spelling do not.
- An admitted cell executes against a private declaration and binding prefix.
  Only successful completion of the whole cell can atomically publish that
  prefix. Failed, rejected and cancelled cells retain explicit nonpublication
  receipts; previously public names remain intact. Effects already performed
  and independently owned captures or children retain truthful settlement and
  cleanup evidence. Publication racing cancellation must preserve the actual
  winning outcome, including visible bindings with unconfirmed durability.

## Qualification owners

The coordinator owns the joined revision, GHC frontend/finalization and the
single compiler-backed build/test lane. Owning crates qualify canonical
inventory and source selection, native request/reply routing, atomic publication,
explicit display, progress type safety and host composition. Repair stale
callers at their owners instead of restoring removed compatibility paths.
Source work may proceed in parallel; do not start another expensive compiler
lane or multiply resident compiler heaps to fill worker slots.

## Required acceptance

1. Build the joined extractor and affected Rust consumers with the pinned
   repository tools. Run focused checks at each owner: finalized interface/Core
   reuse and cold source-less loading, canonical v3 import-shape agreement and
   drift refusal, complete home-unit classification, source/package/producer
   drift, same-name exact owners, missing or changed Core/dependency seals, and
   a TH counter proving retained loading does not replay source. Include a
   retained native root whose compiler dependencies have Core but no native
   products, planned-cell/activation certificates, native-owner shadow survival,
   retained command output, cancellation recovery and last-capture reclamation.
2. Run focused reply and publication checks for schema 15/ABI 9: `Static` versus
   `AtSite`, malformed carriers and unknown sites, nominal input/reply mismatch,
   and exact owner selection. Exercise atomic whole-cell failure/rejection,
   cancellation and publication ordering. Progress mismatch must refuse before
   revision changes, wakes or cross-session import; retain watch/source cleanup
   and a later correct publication. Execute explicit-display pagination,
   settlement and Store-backed startup checks: expansion cannot compile or
   replay effects, and cancellation cannot rewrite a delivered reply.
3. Execute the four deterministic M2 scenarios below, plus complete-cell
   preflight and checkpoint release. Use the native no-Codex selection in
   [embedded-gates.md](../bridge/facade/tests/embedded-gates.md), with six
   executed tests and retained exact operation/publication/cleanup evidence.
   Only `ResponsesTransport` is mocked in the four provider scenarios; Engine,
   Scheduler, Store, dispatcher, actor, compiler and native execution are real.

   | M2 scenario | Required behavior |
   |---|---|
   | Immediate captured replies | Two children reply while the exact parent call remains pending. |
   | Failed creator and reuse | No failed-cell prefix becomes public; earlier names survive. `ActorOwned` children and a transferred checkpoint retain private values, and a third child directly evaluates `privateCapturedHelper capturedValue` in a fresh typed result. |
   | Nominal A/B publication | A parks, a real Engine B tool publishes on the same root while A remains pending, and A resumes; a final tool reads both results and B's current shadow. |
   | Interrupt and durable-input recovery | A real browser/Engine interrupt of the active root round retains `NotPublished Cancelled`, confirms `InvocationOwned` child cleanup and leaves the root live; durable input wakes the driver and a subsequent Haskell tool succeeds. |

   The immediate-reply scenario uses `InvocationOwned` children; the nominal
   join uses `ActorOwned` children. Preflight rejects a late type error before
   effects or publication and retains rejection on exact-operation retry without
   compiler work.
   Checkpoint release is idempotent, refuses new admission after release and
   preserves an already admitted child's context; typed cleanup confirms the
   known owners. The historical descendant test is gated out of this native
   profile; a zero-match selection cannot substitute for live recursion.
4. Run the structural prepared corpus and validate all seven registered embedded
   artifacts in
   [embedded-fixtures.json](../bridge/haskell/test-prepared-stg/embedded-fixtures.json).
   Regenerate artifacts through their declared producer when required by the
   schema/ABI migration; never edit version bytes. Retain the corpus and artifact
   checks separately from linking and test discovery.
5. Build fresh matched pinned frontend, worker, harness and browser outputs.
   Freeze their paths, source revision and deployment manifest. Execute the M1
   browser gate with its declared browser closure and an explicit nonzero count,
   covering real Haskell output, reload/retry, active interrupt, continuation and
   root retirement. Follow the bounded commands and environment requirements in
   [embedded-gates.md](../bridge/facade/tests/embedded-gates.md).
6. Start a fresh matched Tailscale-accessible custom TUI run with a Sol 6.1 root
   and recursive Luna workers. Demonstrate root -> child -> grandchild creation,
   typed requests/results through the tree and confirmed typed cleanup. Retain
   actual descendant identities, model selections, replies and cleanup outcomes,
   exercise the deployed TUI and provide its URL. Deterministic M2 covers creator
   failure and private capture reuse; live parent-failure choreography is not an
   additional delivery gate. Retain the previous host until the fresh smoke
   succeeds.

For each gate record the joined source OID, exact command, executed count, exit
status and evidence location. Keep source review, compilation, execution and
deployment evidence distinct. Stop at the first failing action and repair its
owner before repeating the affected check; do not broaden to every target.

## Measurements and deferred work

Measure cold finalization and warm A,A,B,A separately with frontend counts,
GHC profiling, allocations, GC, retained memory, artifact size and elapsed phase
boundaries. Keep compiler work, provider latency, scheduler delay and cleanup
separate. Observe cache preservation using normalized provider requests and
actual usage, with bounded private traces. These measurements inform later
optimization; no timing, allocation or cache threshold is a release gate.

Distinct-machine and restart qualification are deferred. They require their own
environment, retained artifacts and acceptance evidence and do not extend this
finite delivery gate.

Strict internal wire migration is intentional. Preserve other workers' edits,
live hosts and evidence. Do not build Codex or clear shared caches.
