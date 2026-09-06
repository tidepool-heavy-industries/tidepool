# Native control worker — exclusive controller, harmless observers

## Bounded assignment

Implement/review the native Codex portion of the accepted Shoal architecture:
one mounted app-server per actor, one persistent Shoal controller, optional observer
TUI. Parent service TL owns Tidepool integration; return native commit and protocol
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
- `app-server/src/thread_processor/readiness.rs`: readiness needs role checks too.
- `tui/src/host_dynamic_tools.rs` and `tui/src/app/app_server_events.rs`: existing
  host bridge/tool forwarding; service TL moves/reuses controller-side forwarding.
- `tui/src/app_server_session/cli_fork.rs`, CLI Unix remote resume/listen paths:
  existing transport/history infrastructure, not yet a harmless observer contract.

## Contract to settle with parent before wiring

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
