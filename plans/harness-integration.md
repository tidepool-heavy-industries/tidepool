# Embedded harness integration

Accepted implementation contract, 2026-09-28. This supersedes conflicting
assumptions in the historical adoption plan, which remains read-only. The
standalone library remains independent of Tidepool. Its companion contract is
`exomonad-harness/docs/embedded-host-prd.md`.

Implementation starts from Tidepool `b2de366b2b52e9120ed07f200bae62501b5681e6`
and harness `d36ef10acbc3ad09aed5cc2f9c2f339052aa8f2a`. Changes are isolated under
`/home/inanna/dev/harness-integration/`; these are not deployed revisions.
The [runtime handoff](harness-integration-runtime.md) identifies the declaration
join, binding lifetime and scheduler changes required before concurrency can be
enabled.

## Current work boundary — 2026-09-28

This batch is documentation only. Compiler, binding publication, resident
scheduling and checkpoint implementation are deferred pending the engine
investigation. The verified prerequisite commits below remain isolated on
`integration/embedded-harness`; importing this plan does not integrate them.
Harness production work remains with its separate owner. No default backend,
pin, prompt, deployed binary or running session changes in this batch.

## Outcome

One Exomonad process per run hosts the actor kernel and shared harness services.
A browser operates a Sol Medium root, recursive Luna workers, and Haskell-only
workflow actors. Raw notebook cells and AgentSpec-authored Haskell tools are the
model interface. Model/effort/tool/source changes are runtime configuration;
typed Rust embedding interfaces connect the library to the runtime.

The release gate is a complete worker tree, not a single-root demonstration.
Keep stock Codex as an independent fallback. Existing deployed runs stay on
their frozen binaries. Server provisioning is independent.

## Owners and transitions

| Concern | Sole owner | Required transition evidence |
| --- | --- | --- |
| Actor incarnation, supervision, authority, retirement | Tidepool actor kernel | Exact handle, admitted/retired outcome |
| Typed live Haskell requests and replies | Kernel request/mailbox owners | Admission, presentation, reply, incorporation remain distinct |
| Model input envelopes and request inclusion | Harness Store | Persist before wake; exact delivery correlation, not RPC acceptance |
| Provider requests, conversation history, calls and output claims | Harness Engine/Store/job owner | Original call/request identity and one settlement |
| Execution, declarations, bindings, source versions | Resident runtime and source owner | Captured environment, execution identity, publication sequence |
| Checkouts, commands, output retention and cleanup | Existing worktree and command resource owners | Original handles and actual resource disposition |
| Browser protocol and assets | Harness server, composed by facade | Authoritative snapshot and reconnect cursor |

The kernel authorizes and routes model messages; Store persists them before
waking the conversation. Embedded input does not also enter the Codex durable
inbox. Model completion makes a conversation idle; it is neither typed assignment
completion nor actor retirement. The standalone demo may use its own driver;
embedded mode must not install that driver as a competing supervisor.

Each provider is bound to an exact authorized actor. Model arguments do not
select another actor's authority. The harness exposes the supplied tools rather
than appending mandatory native spawn/message/checkpoint tools.

## Concurrent notebook contract

1. Admission captures the published declaration environment, bindings, source
   version and tool surface. Each cell receives a private lexical scope.
2. Each execution owns its continuation, cancellation, reply and checkpoint
   state. Use existing execution journal and Ractor messages, not another
   scheduler or result registry.
3. Execution is sequential within a cell. Effect suspension releases machine
   checkout and returns to actor scheduling; other cells and control input can
   proceed. No external wait retains exclusive machine access.
4. Successful completion atomically publishes only declarations/bindings newly
   defined by that cell. Completion-order publication determines shadowing for
   later admissions. Existing cells keep their captured meanings.
5. A completed old cell cannot restore its entire starting environment. Exports
   include compiler-owned declaration identities, not merely rendered names.
   Preserve ownership of roots/code referenced by older captures.
6. Publication has one runtime-owned commit point ordered against cancellation.
   Cancellation before commit publishes no notebook definitions; after commit it
   cannot undo them. Invalid declaration joins fail atomically, even if each
   private environment was valid. Independently retained captures survive a
   failed join. Completed effects,
   commands and receipts remain observable; failure is not rollback. Explicit
   effects such as successful source reload keep their own commit semantics.
7. Dependent steps belong in one cell or a later admission after completion.
   There is no implicit future or unresolved-variable dependency graph.
8. Installed Haskell tools receive the same execution isolation. Transport
   retries resolve the original execution; they never rerun source for output.

Current source still serializes calls and preserves binding prefixes on failure.
Do not advertise the new contract until its implementation and real resident
tests pass. Removing `dispatch_gate` alone is unsafe: actor-wide workbench,
fork-publication and continuation fields must change first.

## Explicit checkpoints

Inherited launch/unfold requires an opaque runtime-issued checkpoint with a
readable label. Fresh-context launch remains available separately. Capture the
conversation prefix and pending calls, Haskell/helper source and environment,
and the issuing cell's completed private scaffold at the effect boundary.
Function-local values are explicit typed inputs, not invented notebook exports.

Checkpoint capture is immediately usable: a child can start while the parent
cell remains pending, and later parent failure does not invalidate the capture.
Never synthesize a successful parent result. Harness claims retain original call
identities and route actual later outputs without rerunning work.

Project checkout is a separate launch input. An implementer can use the starting
revision and a reviewer the candidate revision with the same requirement
checkpoint. Captured helper sources do not silently change with that checkout.

A Haskell-only workflow actor can hold a checkpoint and supervise model workers.
Context origin, creator and supervisor remain distinct. Revalidate and narrow
authority at admission; context possession grants no extra privilege. Mutable
resources remain ordinary live handles, not a frozen copy of the world.
Release leases when consumers release them. Durable metadata does not restore
heap values after process loss; refuse stale incarnations explicitly.

## Workflow and sustained use

Use existing record actors, typed requests, dynamic sources and review helpers
for one bounded implementation/check/review/repair workflow. Its worker receives
a checkpoint and checkout, returns an exact candidate, and remains available for
repair. The workflow runs declared checks, gathers change evidence, commissions
independent review, then returns reviewed work or an escalation. Supervisors
own cleanup. Deterministic facts stay in Haskell; Jev receives sufficient bounded
evidence only for declared semantic choices. No new WorkPlan language and no
second Jev scoring pass over the independent review.

Compaction initially requests plain handoff text, then seeds the next context
with standing instructions, summary and bounded recent user messages. Reuse
the known local Codex approach. Trigger at configurable approximately 50% input
capacity with protection against no-progress compaction. Calls, claims, input
and Haskell state live independently of that text. Structured builders and
template-driven handoffs are deferred.

Successful reload changes future admissions; existing cells/checkpoints retain
their source versions. Failed reload preserves the valid surface. Browser
reconnect reads authoritative state without resubmitting commands. Host loss
retains history/evidence, reports unavailable live state, and never blindly
replays effects. Existing browser authentication/origin checks remain required;
loopback plus Tailscale is the initial network boundary. Credentials are explicitly
configured; expiry is surfaced rather than retried indefinitely.

Pin tool kind and handler/source version to the model request that exposed
them. A call from an already issued request must not execute an unrelated newer
handler after reload. Publish the changed tool surface at the next request
boundary; notebook cells capture the published bindings at admission under
that request's source view. Use the harness's request-scoped tool information,
not a global current-schema lookup.

## Implementation sequence and gates

### Current dependency boundary

The canonical harness `Provider` exposes JSON function calls, and its cell
provider wraps source in a JSON object. The retained wave22 final manifest at
`7b3124b41afc60f735dde028d9cc6021fb2ed66f` says the custom-cell provider,
Engine, Store and browser candidates are not joined at the root. This is a
concrete missing integration input, not evidence that raw cells are accepted.

The adapter must consume an exact reviewed harness revision with:

- Raw text versus structured tool input preserved through dispatch and replay;
  no JSON-cell compatibility wrapper or hidden conversion to a function call.
- Call/request/conversation identity available to the bound resident endpoint.
- Cancellation that can retain owner acknowledgment and uncertain cleanup;
  aborting the provider future alone is not a resident cancellation receipt.
- Pending-call checkpoints with real later settlement and children that can
  start before the parent's enclosing output exists.
- An externally supplied lifecycle implementation and visible tool manifest.

Some hooks already exist (`Provider::all_tools` is overridable); extend these
owners rather than replacing them. The canonical Engine's
`await_here_invocation_output` waits for the parent output, so that path cannot
implement the accepted immediate-checkpoint contract unchanged. Record these
requirements in the companion PRD and let the harness owner return its accepted
hash. Do not create a pretend adapter against guessed future signatures or pin
an unverified WIP branch simply to make Cargo resolve.

Independent runtime prerequisites can proceed before this dependency closes.
The adapter, main-binary wiring and live browser acceptance remain explicitly
open until it does.

The companion PRD is now committed in the canonical harness checkout:
`d64285f` establishes the embedded profile and reconciles existing requirements;
`a9ff9ea` aligns compaction reuse and the tree acceptance. These are documentation
commits, not an accepted raw-call library revision.

### Ordered work

- [x] D0: companion harness PRD, reconciled roadmap/PRD/NEXT, and typed boundary
  obligations. Canonical harness docs are at `6326ef6`; no harness production
  code changed.
- [x] T0a: per-execution hosted-call tracking and exact cancellation after
  transport loss (`1059890d1`). Kernel rejection paths settle the control even
  when no transport waiter remains. Calls are still serialized.
- [x] T0b: transitive retained binding dependencies survive a second detached
  capture/import; repeated alias imports do not double-count a dependency lease
  (`41583b5eb`). No public API or publication semantics changed.
- [ ] C0: accepted harness custom-cell baseline and shared interfaces.
  Converge wave22 WIP through its existing owner. Candidate tests are not joined
  acceptance. Do not pin an unreviewed branch as a finished library.
- [ ] T1: per-execution workbench controls; private compile/binding scope and
  atomic export publication. Keep serialization until all execution-local state
  and suspension/resumption paths are ready.
- [ ] T2: immediate explicit checkpoints, independent checkout selection,
  workflow-owned launch, and scope/root lifetime evidence.
- [ ] C1: harness external lifecycle, raw/typed async calls, cancellation
  acknowledgment and pending-call checkpoint contracts, implemented by its owner.
- [ ] T3: typed adapter and facade composition against exact C1 revision;
  resident tools, host commands, inputs, lifecycle, shared services and web assets.
- [ ] C2/T4: text compaction, browser operation, reload and interrupted-host
  reporting across actual production consumers.
- [ ] T5: compiled Haskell workflow exemplar and prompt migration, full tree
  acceptance, matched packaging and opt-in rollout.

Scaffold contracts before parallel consumers. Tidepool implementation uses
isolated worktrees; one expensive compiler slot. Review exact candidates and
failure/cleanup paths. Never restart shared daemons. Preserve other owners'
dirty work, WIP and running sessions. Track source/hash/check counts separately
from expected outcomes.

### Required acceptance evidence

- Two cells suspend independently while a third cell and control input progress.
- Different-name exports compose; same-name exports follow completion order.
  Old cells retain old meanings without overwriting unrelated new definitions.
- Failed/cancelled cells export nothing but preserve completed-effect evidence;
  exact-call cancellation cannot cancel a sibling or claim cleanup prematurely.
- A checkpoint starts children before parent completion, survives parent failure,
  and is usable by a Haskell-only supervisor at different project revisions.
- Typed reply, model final, input acknowledgment and retirement remain distinct.
- Compaction handles pending raw and typed calls; reload preserves running views;
  reconnect does not duplicate work; host loss does not replay side effects.
- A browser Sol Medium root manages two independent component workflows with
  Luna delegation, exact-candidate review, one repair, integration and verified
  cleanup. A root-only probe does not close this gate.
- Record source and workspace pins, model/effort, call counts, concurrency,
  latency, retained memory and resource outcomes; distinguish replay and live
  provider evidence. Do not infer constant-cost actors from process sharing.

Use focused checks and barrier-driven tests. Build every changed target and
direct consumer with repository recipes. Cross-repository tests use their own
pinned toolchains. No new live deployment or successor wave is implied by a
passing unit test. Remove custom Codex build/runtime dependencies only after
the connected path passes and remaining consumers are audited.

## Deferred

Server-wide multi-run hosting, heap restoration, structured compaction builders,
browser terminals/server administration, host executable hot reload, and a new
machine-placement policy are not requirements of this integration.

## Verified prerequisite handoff — 2026-09-28

Tidepool branch: `integration/embedded-harness`, isolated at
`/home/inanna/dev/harness-integration/tidepool`, based on
`b2de366b2b52e9120ed07f200bae62501b5681e6`. These commits have not been
integrated into Tidepool main or deployed:

- `1059890d1`: per-execution tracking, retained cancellation after transport
  loss, and early rejection settlement (original candidate `956aa79ec`).
- `41583b5eb`: transitive binding retention and balanced dependency leases
  (original candidate `20a031feb`).
- `166006306`: inspect settlement without cloning the complete reply.

Verification:

- Combined actor branch: `just test-lib exomonad-actor` selecting
  `hosted_calls_retain_each_execution_and_cancel_only_the_running_match`,
  `lost_transport_waiter_keeps_running_cell_cancellable_until_settlement`,
  `a_dispatched_hosted_cell_is_visible_on_its_actor_until_it_ends`, and
  `workbench_rejections_settle_control_without_a_transport_waiter`, each with
  its exact module-qualified `test(=NAME)` filter. **4 executed, 4 passed**;
  352 skipped. Retained log in the implementation worktree:
  `target/tidepool-test-runs/20260929T002136Z-216102-battery/nextest.log`;
  `reproduce.sh` beside it retains the exact invocation.
- Binding candidate: Nix-shell `cargo test -p tidepool-codegen --lib NAME`
  for `second_detached_capture_keeps_shadowed_first_source_binding_alive`,
  `importing_detached_source_retains_its_inherited_hidden_dependency`, and
  `importing_alias_does_not_double_lease_existing_dependency`.
  **Each selected and passed one test.** These were executed on the original
  candidate, then cherry-picked unchanged; they were not rerun on the combined
  branch. Each regression failed before its corresponding repair.
- Combined branch: `bash scripts/dev-shell.sh cargo check -p tidepool --lib`
  passed. This is consumer compilation, not facade execution.
- Rust formatting and `git diff --check` passed. No full verification battery,
  live provider test, concurrent-cell acceptance, or browser launch ran.

Checks reused `CARGO_TARGET_DIR=/home/inanna/dev/tidepool/target`. The build
used the already initialized, matching canonical Codex submodule temporarily;
that worktree-only symlink was removed afterward. No shared daemon restarted.

Harness documentation is already on canonical master through `6326ef6`, with
no production changes or push. C0/C1 remain external implementation dependencies
owned by the harness workstream. T1/T2 remain substantial Tidepool work that
can proceed independently; the external dependency does not imply those are
implemented or blocked. The runtime handoff documents their source owners and
publication/scheduling design. Shipped prompts retain current semantics until
those runtime contracts actually pass.

## Host/library implementation handoff

### Evidence and source status

This contract pass inspected Tidepool canonical
`b2de366b2b52e9120ed07f200bae62501b5681e6`, harness canonical
`6326ef680dc5824d60f321f34d2c6c79aaf7eea4`, and the isolated Tidepool
prerequisite/docs branch at `656ea2e3606978588ab2de418ddfacb1949465e1`.
Harness paths below are relative to the standalone repository. Source inspection
is not execution evidence. Wave22's `docs/wave22-final-manifest.md` retains
unjoined component candidates and unexecuted combined gates; its component
successes do not close C0. Revalidate interfaces against the eventual accepted
harness hash before writing the adapter.

| Boundary | Existing source and consumer | Embedded delta / owner |
| --- | --- | --- |
| Tool dispatch | `exomonad/actor/src/resident_tools.rs`, `ResidentToolEndpoint`; `exomonad/tool/src/lib.rs`, `ToolArguments::{Raw,Structured}` and `ToolInvocationContext` | Adapter binds an endpoint to an exact authorized actor and maps harness request/call identity without a JSON source wrapper. Actor owns execution; harness owns call claims. |
| Provider dispatch | Harness `crates/harness/src/provider.rs`, `Provider`, `CallContext`; Engine consumes context | Current `call` is JSON-only, progress is unbounded, request identity optional. Harness owner supplies accepted raw/structured input, bounded progress and required provenance for embedded calls. `all_tools` already supports overriding the default native verbs. |
| Input and acknowledgment | `exomonad/agent/src/interactive.rs`, bound submission/query/withdrawal; `exomonad/node/src/inbox.rs`, durable native delivery | Keep Codex delivery intact. Embedded path persists model envelopes only through harness Store and reports durable admission separately from request inclusion. Kernel typed request states remain authoritative. |
| Lifecycle | `bridge/facade/src/actor_host/hosted_retirement.rs`, retained seal/drain/resource observations; actor kernel identity | Adapt real lifecycle evidence to in-process conversations; do not synthesize native pane/socket exit. Harness attaches history to host-admitted identities without using demo TreeDriver as a second supervisor. |
| Commands | `bridge/facade/src/actor_host/commands.rs`, both NativeCommandBackend and HostCommandBackend; `exomonad/node/src/host_command.rs` | Reuse host command backend for embedded actors and existing job/resource owners. Preserve mount confinement, cgroups, original handles, output offsets and cleanup uncertainty. PTY is currently refused. |
| Checkouts and publication | `bridge/facade/src/actor_host/workspace.rs` and `workspace_publication.rs` | Keep managed checkout/source owners. Native publication transport stays Codex-specific; embedded acknowledgment must refer to the actual installed source/request view. |
| Browser | Harness `crates/harness/src/server.rs` and `server/ws_protocol.rs`; demo drives server today | Facade composes library Router/assets/control with real host. Browser commands resolve through the authorized host; snapshots combine kernel actors (including Haskell-only actors) and Store conversations. |
| Credentials | Harness `crates/harness/src/transport/auth.rs`, CodexFileAuth | Initial explicit read-only bridge. No implicit credential copy, refresh loop or inference in offline demo; independent coordinated login/refresh is later work. |
| Compaction | Harness `crates/harness/src/compaction.rs`, Compactor/Server, and Engine threshold; pinned local `vendor/codex/codex-rs/core/src/compact.rs` | Existing server endpoint/typed-turn strategies do not prove agreed plain-text summarization. Harness owner extends existing strategy/transport owner with a text-summary operation; preserve pending raw and structured calls. |
| Packaging | `flake.nix`, Cargo dependencies, facade launch composition | Keep Codex distribution and protocol checks. Future embedded package pins accepted harness source and matching assets with runtime/worker/workspace identity. Historical excluded `exomonad/harness` and `exomonad/web` are not the implementation. |

### End-to-end transitions

1. **Input.** Browser authentication authorizes control of the run. Kernel
   validates the target incarnation and producer operation. Store durably admits
   that exact model envelope before wake; Engine records its inclusion in an
   exact request. Admission alone cannot advance a typed request to presented.
   A lost acknowledgment queries the original identity; it does not create a
   replacement delivery. Rejected and uncertain outcomes remain distinct.
2. **Call.** The issuing model request records the actual tool manifest and
   handler/source view. Adapter validates raw/structured kind and maps original
   conversation/request/call identity into the bound endpoint. Runtime admission
   assigns/retains execution identity. Progress is bounded observation, not a
   final result. The harness publishes one retained result for that original
   call, including execution classification and recovery reference. An exact
   retry observes the original execution rather than evaluating source again.
3. **Cancellation.** Operator or actor requests cancellation for an exact
   execution. Its owner acknowledges stopped, already completed, unsupported or
   unconfirmed work. A dropped waiter is not cancellation. Terminal arbitration
   preserves the winning outcome; late success cannot overwrite acknowledged
   cancellation. Cleanup evidence stays separate from model-call settlement.
4. **Child attachment.** Kernel admission binds authority, creator, supervisor,
   explicit checkpoint and separate checkout. Harness registers that admitted
   conversation and pending-call claims; attachment failure unwinds provisional
   ownership and reports failure to the admission owner. Model final makes the
   conversation idle; typed reply and actor retirement require their own events.
   Immediate usable checkpoints remain a deferred runtime requirement.
5. **Reload/reconnect.** The source owner atomically accepts or rejects reload.
   Existing issued requests retain their handler/source meaning; subsequent
   requests publish the new tool manifest. Browser reconnect obtains a snapshot
   and retained events, with a full snapshot after a retention gap. Reconnect
   never resubmits commands. No global latest-tool lookup may reinterpret an old
   call. Binding publication semantics remain with the deferred runtime work.
6. **Retirement/loss.** Seal new admissions, settle or expose pending calls,
   release actor-owned resources, and retain unconfirmed cleanup. Host shutdown
   drains shared services after actor retirement attempts and Store persistence;
   a deadline reports remaining uncertainty. After crash, Store history survives
   but live actor/checkpoint capabilities may not. Reopen marks them unavailable
   and offers deliberate new-run recovery, never automatic side-effect replay.

### Codex compatibility and future deletion boundary

| Classification | Current consumers | Requirement |
| --- | --- | --- |
| Retained for Codex | `InteractiveAgentBackend`, native input controller, process launch, tmux, hosted-tool socket, native commands | Keep current protocol, pins and regression checks. Embedded integration must not emulate a native process merely to satisfy this interface. |
| Shared | Actor kernel, typed mailbox, ResidentToolEndpoint, source owner, managed worktrees, command jobs/resources, prompt catalog | Reuse current ownership. Do not delete watches or Haskell-only actors because the new model transport is asynchronous. |
| Adapted | Facade conversation launch/input, hosted retirement, workspace publication; rollout readers under `exomonad/agent/src/backend/codex/` | Embedded history/usage observations come from Store, commands from the host owner. Preserve exact identities and unavailable evidence; do not add a parallel journal. |
| Removable after acceptance | Custom Codex process/relay integration, private protocol dependency and build wiring, obsolete historical sources | Separate later consumer audit and operator cutover; neither this contract nor one successful probe authorizes removal. Stock Codex remains independently installed. |

The initial embedded command contract supports closed or piped stdin, paging and
owner-driven cancellation. Terminal requests are explicitly unsupported; do not
silently coerce a PTY request to pipes. PTY support can later extend the existing
host-command owner. Retained output is bounded per stream (currently 4 MiB), with
original byte offsets and retention gaps. A command's exit is not proof that all
its descendants were reaped; resource-owner cleanup evidence is required.

### Operator and packaging defaults (settled)

- One shared harness instance per run. Select the backend explicitly at run
  creation; Codex remains the development default. Record backend in run metadata
  and refuse incompatible resume. Never automatically switch an admitted run's
  backend or replay its uncertain operations through Codex.
- Reuse the existing private run root issued by the toolchain path owner. Put
  embedded Store data beneath `run_root/harness/`; keep run logs and source/
  resource metadata under their existing owners. Browser assets are immutable
  build artifacts. Store schema migrations stay owned/versioned by the harness;
  incompatible reopen fails before actor admission. No automatic Codex transcript
  migration and no heap reconstruction.
- Loopback service behind Tailscale HTTPS, with existing browser-session login,
  HTTP/WebSocket authorization and origin checks. Configure the public scheme
  explicitly; do not trust arbitrary forwarded identity headers. Existing
  eight-hour browser session default is retained. Authentication contract tests
  must cover expiry and configured-secret rotation; Tailscale is not a substitute
  for those checks.
- Use explicitly configured read-only Codex credentials for the initial release.
  Authentication failure preserves pending evidence and requests reauthentication;
  do not retry indefinitely. Operator refreshes through Codex. Independent auth
  is a later milestone, and the bridge remains a declared dependency until then.
- Plain-text compaction uses existing request/Store owners, standing instructions,
  model-authored summary and bounded recent user messages. Default threshold is
  approximately 50% of configured model capacity. Preserve pending-call claims
  and retained Haskell state separately. Failed/no-progress compaction keeps the
  previous valid history and prevents repeated compaction of unchanged input.
- Startup validates selected backend, configuration, schema, assets and workspace
  before admitting actors. Embedded readiness means Store opened, tool host bound
  and browser service available; it does not claim a live provider request passed.
  Shutdown retains dirty checkouts, commits and uncertain resource evidence.
- Pin accepted harness library and matching assets together through existing build
  ownership. Record runtime, extractor/worker, workspace and harness revisions.
  Preserve the current Codex build closure until an explicit later removal batch.

### Implementation parcels after the hold

These are integration parcel IDs. Companion roadmap H0 is C0; its resident H1
and lifecycle H2 feed T3/R, browser H3 feeds T4, transport H4 contributes to C2,
sustained H5 spans C2/T4, and packaging H6 feeds T5. C1 names the harness library
seams, not the roadmap's real-resident H1.


| Parcel / owner | Exclusive responsibility | Prerequisite and deliverable |
| --- | --- | --- |
| C0 / harness owner | Converge raw custom-cell Engine/Store/provider/browser candidates | Exact accepted library hash, public interface inventory, component and combined evidence; no guessed adapter API. |
| C1 / harness owner | Input inclusion, request-scoped manifests, bounded progress, cancellation acknowledgment, external lifecycle and opaque checkpoint attachment | C0; public-library deterministic host stub exercises ownership without importing Tidepool. |
| R / engine-runtime owner | Private environments/publication, execution-local scheduling and checkpoints | Engine investigation resolved and explicit resumption; current runtime handoff governs. No work in this docs batch. |
| T3 / Tidepool integration owner | Direct endpoint adapter, host command and lifecycle composition | C0/C1 and R for real cells/checkpoints; preserve Codex branch and use existing resource owners. No fake process backend. |
| C2 / harness owner | Plain-text compaction and credential-expiry behavior | Accepted call/claim interfaces; replay tests retain raw/structured pending calls. |
| T4 / Tidepool facade owner | Backend selection, shared Engine/Store/server composition, assets, run metadata/readiness/shutdown | T3 plus harness server/C2; no separately drifting UI or browser supervisor. |
| T5 / integration owner | Matched packaging, Codex regression and full tree acceptance | Reviewed T3/T4 and runtime gates. Only then propose the opt-in embedded wave; default cutover remains an operator decision. |

One owner controls shared signatures and integration order. Every candidate reports
base/full hash, exclusive commits, prerequisites, owned paths, exact checks and
counts, compiled-only targets, unverified live behavior and migration implications.
Do not assign consumers until the prerequisite contract is available. Harness
owner receives requirements through its PRD/NEXT, not a production patch here.

### Acceptance matrix (planned; not executed in this batch)

| Gate | Production path and scenario | Evidence / owner |
| --- | --- | --- |
| A — library | Engine/Store/provider with deterministic external host: raw Unicode and structured calls, kind mismatch, original-ID retry, bounded progress, envelope admission vs inclusion, outliving dropped waiter, cancel/completion race | Exact harness hash and counted offline tests; extend `tests/adapter_readiness.rs`. Mock resident work is labeled mock. Harness owner. |
| B — resident | Real endpoint: source persistence, execution classifications, retained output, sibling cancellation, two suspended cells plus progressing third/control input, publication and pending-parent checkpoint gates above | Exact matched hashes and counted real resident tests; requires R. Integration/runtime owners. |
| C — browser | Real Router/Store/host: messages, tool detail, tree including Haskell-only actor, interruption, reconnect/retention gap, expired browser session/credential, failed reload and host crash | Offline browser journey plus separately labeled live-provider evidence; no fake evaluator presented as Haskell acceptance. Facade/harness owners. |
| D — tree | Sol Medium root, parallel Luna component workflows with recursive delegation, retained typed results, exact-candidate independent review, repair, integration, cleanup | Source/workspace pins, run/Store/log IDs, review OIDs, product checks, root interview and actual release receipts. Integration owner. |
| E — Codex | Existing launch/input controller/tool dispatch/command/fork/workspace publication/retirement consumers | Focused existing protocol replay and runtime tests, including lost publication reply without retry; explicit live smoke separately if authorized. Codex integration owner. |
| F — sustained/package | Compaction with pending raw/structured calls, configured backend resume, schema refusal, missing assets, orderly stop/uncertain cleanup | Matched artifact manifest and retained failure/recovery evidence; no uncontrolled repeat or cross-backend replay. Harness/facade owners. |

Count matched and executed tests, not only exit status. Keep replay, mock, real
resident and credentialed provider evidence separate. Run the broad release gate
only at the actual integration boundary. Documentation completion is not an
accepted embedded release and authorizes no launch, deployment or cutover.
