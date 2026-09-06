# External Codex handoff — exclusive controller, harmless observers

## Execution boundary

**This is not a Shoal fork assignment.** User requires any Codex edits to happen
outside Shoal, in a separate session in the Codex repository. First assess whether
existing native capabilities can satisfy the contract without edits. Only hand this
to the external session if a concrete missing capability makes changes necessary.
Shoal may inspect and specify the dependency, then integrate the delivered revision;
it must not implement native changes even through an allocated Codex worktree.

## Pre-wave verdict — native change required for attached observer TUI

Rechecked the pinned checkout on 2026-09-06 at the exact revision below. **Yes,
the agreed direct observer-TUI architecture requires Codex changes.** This is a
source-verified missing ownership mechanism, not another transport guess:

- `app-server/src/lib.rs:1137` receives a response with a known connection ID but
  calls `process_response(response)` without passing that identity.
- `app-server/src/message_processor.rs:838` forwards only response ID/result.
- `app-server/src/outgoing_message.rs:383,457` removes the pending callback by
  request ID alone; an attached peer is not checked against responder ownership.
- `app-server/src/thread_state.rs:328` has no observer/controller capability.
- `app-server/src/request_processors/thread_processor/readiness.rs:6` acknowledges
  readiness without caller identity. TUI dynamic-tool forwarding is active code,
  not an observer-only connection mode.

Mandatory native deliverable: enforce single controller/mutator/responder custody
(including response/error paths and readiness); provide side-effect-free observer
TUI attachment; define disconnect/reconnect fencing and pending-request disposition.
The external implementer chooses the smallest compatible protocol shape, backed by
real two-client tests. It need not invent Shoal assignment or notification schemas.

Already available / not reasons to change Codex: explicit Unix service transport,
full-prefix fork boundaries, readiness machinery, ordinary turn submission/steering.
Shoal implements its typed RPC versus one-way notification distinction, mounted
service lifecycle, persistent client and hosted-tool bridge in Tidepool. Do not
expand this native request into a generic actor scheduler or messaging framework.
Seamless controller recovery is not to be claimed prematurely: explicit fail-closed
recovery is acceptable if pending effects remain honest and cannot execute twice.

A no-native-change *reduced* milestone is possible: controller-only headless service,
no directly attached native TUI. It can exercise transport/host tools but does not
satisfy the agreed observer acceptance gate. A filtering TUI proxy or custom UI could
avoid native edits only by adding a new policy/presentation layer; that is not the
recommended ownership boundary and is not an approved silent substitution.

**Human-managed handoff:** give this file to the Codex-repository LLM outside Shoal
before the next service integration wave. Ask it to return the reviewed commit,
protocol/CLI contract, binary/build instructions, tests and limitations. The human
brings those artifacts back to Shoal. No cross-repo actor handle is presumed. While
it runs, independent run-map, usage and custody work may proceed; attached-TUI
integration stays gated on that delivery. Further native gaps must be escalated to
the human, not implemented from Shoal.

## Bounded external assignment

Implement/review the native Codex portion of the accepted Shoal architecture:
one mounted app-server per actor, one persistent Shoal controller, optional observer
TUI. Shoal service TL owns Tidepool integration; return native commit and protocol
contract. No global daemon migration, new mailbox, dashboard or multi-actor service.
Read native contributor guidance. Use an explicitly allocated native worktree;
never modify another checkout just because its path is visible.

Inspected native revision: `c8460ffd7c859da2a1467f4384020cf9a19bcc69` in
`/home/inanna/dev/codex`. Facts below are source inspection, not executed canaries.
Existing app-server supports explicit Unix sockets with WebSocket upgrade; native
client uses `client_async("ws://localhost/", UnixStream)`. Raw JSONL proxy is not
that protocol. Existing full-prefix after-call fork and client-readiness machinery
must be reused rather than replaced.

## Production seams

Under `codex-rs/`:
- `app-server-protocol/src/protocol/v2/thread.rs`: fork boundary/readiness options.
- `app-server/src/request_processors/thread_fork_boundary.rs`: boundary validation.
- `app-server/src/outgoing_message.rs`: thread-scoped fanout and pending callbacks.
  Requests currently fan to attached clients; callback removal keys request id,
  not controller identity. Hiding TUI input does not enforce ownership.
- `app-server/src/thread_state.rs`: connection capabilities (inspected shape only
  had request_attestation); extend the owning connection/subscription mechanism.
- `app-server/src/request_processors/thread_processor/readiness.rs`: readiness needs role checks too.
- `tui/src/host_dynamic_tools.rs` and `tui/src/app/app_server_events.rs`: existing
  host bridge/tool forwarding; service TL moves/reuses controller-side forwarding.
- `tui/src/app_server_session/cli_fork.rs`, CLI Unix remote resume/listen paths:
  existing transport/history infrastructure, not yet a harmless observer contract.

## Contract to settle with Shoal coordinator before wiring

Add explicit controller/observer attachment with server-enforced mutator/responder
custody. Observer receives history/notifications, never tool or approval requests;
reject ready/start/steer/interrupt/configure mutations from it. One controller
per execution thread; reconnect generation fences stale responses. Keep native
pending requests under their current owner and define reconciliation/reoffer on
controller replacement, without duplicate effect execution. Specify negotiation
and errors; old serialized/external clients require an explicit compatibility choice.
The initial Shoal TUI is observer-first. Human mutations must route through Shoal's
authoritative assignment/amendment layer, not independently start turns.

Unix observer disconnect must not stop service/thread. Existing Unix loop appears
to support this, but controller loss and pending tool recovery are not established.
Controller loss must pause/fence at safe boundary or expose explicit recovery, never
silently continue tool work through a stray observer. Attach/resume must not issue
an initial prompt, alter model config or run implicit fork readiness.

## Waves / acceptance

Scaffold protocol + one real consumer, then split disjoint server enforcement and
TUI observer consumption if useful. Fresh reviewer checks actual request routing,
permissions, cleanup and migrations, requests repairs from retained implementer.
Tests: two connected clients cannot race to answer; stale generation rejected;
observer mutations denied; readiness guarded; disconnect with pending call;
reconnect correlation; no extra turns/config changes on attach/detach. Existing
fork boundaries reject incomplete calls/schema mismatch before inference.

Run offline real Unix WebSocket initialize/attach checks with exact built binary;
coordinate bounded provider tests with service TL, not an uncontrolled separate
battery. Deliver tested native revision, binary identity, protocol examples,
consumer compile checks and unverified paths. No claim that source-supported
reconnect behavior has been executed unless it has.
