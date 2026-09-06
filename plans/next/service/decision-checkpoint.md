# Service checkpoint: client reuse and notification surface

This is an explicit design escalation, not service acceptance. Custody work and
its independent review continue in a retained child. No service binary changed.

## Native client dependency boundary

Recommendation: reuse `codex-app-server-client::RemoteAppServerClient` at the
externally delivered revision, pinned alongside the CLI, provided root accepts
its dependency closure. At native c8460ffd7c859da2a1467f4384020cf9a19bcc69,
`codex-rs/app-server-client/Cargo.toml` unconditionally depends on app-server,
core, exec-server, config and protocol; it has no remote-only feature. This is
not a small WebSocket dependency. Root owns manifest/pin decisions. If that
closure is unacceptable, decide explicitly between an external remote-only
packaging change and extending Tidepool's existing transport owner; do not
silently add a second router.

`Session::request` in Tidepool `backend/codex/process.rs` explicitly discards
interleaved server requests/notifications. Plugging in WebSocket transport there
would lose hosted work. The headless metadata seam must not become the persistent
interactive controller by assumption.

The bridge must forward registration, session attachment, tool calls AND completed
call boundaries. Native `tui/src/host_dynamic_tools/completions.rs` tracks raw
completed response items with `CompletedCallBoundary` and calls
`/v1/dynamic-tools/completed` only after durable closure. Tidepool's
`src/host_dynamic_tools.rs` validates bound thread and calls `complete_boxed`.
A successful hosted HTTP call or native resolve-response write is insufficient to
release an enclosing unfold. Keep exact source prefix and actual-result closure.

## Proposed model-facing notification contract (root approval required)

Propose `notify :: Member Notifications effects => AgentRef -> Text -> Eff effects
(Either NotificationError NotificationReceipt)` with small opaque receipt and
`pollNotification` for delivery certainty. Notifications is a distinct extensible
effect, not a Reply request that manufactures an unused response obligation.
The effect/interpreter still uses exact actor/incarnation authorization and the
existing durable inbox, never a second queue/registry. The receipt identifies the
existing inbox event, not an independent log. Errors distinguish denied/unavailable
admission; observation distinguishes accepted, presented, and unconfirmed transport.
Detailed provider failures stay in Rust evidence, not a model-facing mirror.

Active notification must preserve `sessionInput`, `sessionReply`, and `respond`.
Idle notification without an active request has none of those bindings; an idle
provider whose typed request remains pending must retain that request's bindings.
These are two different idle states. Use existing actor event activation and
standing-request state, not provider-busy as the test. No automatic notification
acknowledgment masquerades as understanding or typed reply. Amendment remains exact
requester-owned `updateRequest` with its existing settlement fence.

Before implementation, root should choose whether this distinct effect/small
receipt surface is appropriate, or whether initial admission-only `notify` with
artifact-based presentation evidence is preferable. Recommendation: retained receipt
because the wave explicitly requires correlation and reconnect uncertainty, but
keep it backed by the one inbox owner. No proposed notification API is shipped yet.

## Evidence and limits

Client design child read immutable native source with `git show` and Tidepool seed
72ac093b8ea3e7872ec78b370614bbbfdeebcfe1. Service TL directly rechecked
Session::request, native client manifest, and native completion consumer. No builds
or behavioral checks were run for these source-only decisions. External ownership
protocol remains in flight. Mounted service/TUI/fork/reconnect acceptance is blocked.
