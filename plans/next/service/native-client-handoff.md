# Delivered native client consumer contract

> Historical previous-wave reference. The current [implementation handoff](../../../NEXT.md)
> retains interactive Codex TUIs and excludes the controller/observer migration.
> Old dispatch instructions and migration gates below are not current requirements.

Source assessment: retained control designer inspected immutable Codex
600be9df39096121f76745f7d7f73a96bae8c82e in /home/inanna/dev/codex,
against Tidepool2db8f2c6 and root delivery97c6722a. This is attributed source
inspection, not a compiled Tidepool consumer or native test execution. It
supersedes the pending-native section of implementation-contract.md. Native
implementation is delivered; matching dependency build remains root-owned.

## Dependency seam (root-owned)

Use the exact delivered revision of codex-app-server-client,
codex-app-server-protocol, codex-protocol, codex-rollout and, if needed directly,
codex-utils-absolute-path from https://github.com/inanna-malick/codex.git.
The backend also needs existing workspace reqwest for HTTP over UDS. Keep native
wire types inside the Codex adapter; existing codex-codes types are not the same.
Native workspace patches do not propagate transitively: root must reconcile its
crossterm, tokio-tungstenite, tungstenite and SSH-to-HTTPS patch entries explicitly.
Do not infer a matching build from dependency declarations alone.

## One connection, one lifetime

RemoteAppServerClient::connect with RemoteAppServerEndpoint::UnixSocket and
experimental_api=true performs initialize/initialized. Acquire with
request_typed::<ControlAcquireResponse>(ClientRequest::ControlAcquire {
request_id, params: ControlAcquireParams { token } }) on THAT connection.
ControlAcquireResponse.0 contains instance_id, state, shutdown and
reconciliation_required. Require Controlled; retain real instance_id with the
existing exact actor/incarnation deployment. Do not invent a replaceable controller
generation. The fresh private token file is a launcher credential, not endpoint
auth_token; never serialize its contents into logs, prompts or generic traces.

ClientRequest uses request_id, not id. Use public AppServerRequestHandle::Remote
when a named request-handle field is required: the concrete remote handle type is
not reexported. The handle sends requests but cannot resolve/reject server calls.
Keep RemoteAppServerClient in one driver select loop over next_event, completed
HTTP call futures and commands. Resolve callbacks through the client only after
select releases its mutable borrow. Never block native event consumption on a
hosted Haskell call or implement another JSON-RPC pending-response registry.

Disconnected or ControlStatusChanged(Fenced) permanently disables execution for
that service lifetime. No automatic reacquisition, replacement, callback replay or
input resubmission. Diagnostic control/status and control/pending/list cannot prove
effect outcomes. SessionsStopped is not executor quiescence; reviewed process and
custody owners remain authoritative for cleanup.

## Destination readiness and prefix

Read destination /v1/dynamic-tools/registration; validate protocolVersion3 and
preserve exact ordered native DynamicToolSpec declarations. Start/resume/fork
requests raw events and deliberate non-ephemeral persistence. All controlled
threads need readiness, even without tools. Fork uses after_call_id at the actual
closed hosted call, NOT through_call_id; require_client_readiness=true,
expected_dynamic_tools exact, and defer_goal_continuation=true where supplied.
Preserve inherited configuration unless explicitly overridden.

After thread creation, attach destination host via /v1/dynamic-tools/session.
Only actual successful attachment permits ThreadReady; only successful readiness
permits the existing actor-owned assignment/notification command. RPC acceptance,
persisted correlated presentation and incorporation remain distinct. TurnSteer
requires expected_turn_id; start-or-steer never substitutes for ownership checks.

## Hosted-call completion bridge

ServerRequest::DynamicToolCall has distinct JSON-RPC request_id, call_id and
context_call_id. Register context_call_id before dispatch to destination
/v1/dynamic-tools/call; forward protocolVersion3 and exact native params. Resolve
or reject the native request without fabricating successful tool output.

Port only HTTP client forwarding from native private TUI host_dynamic_tools and
completions modules. Existing Tidepool host remains execution/receipt owner.
Reuse public codex_rollout::CompletedCallBoundary on RawResponseItemCompleted.
A callback response write is NOT the closed durable result batch. Only the latter
permits /v1/dynamic-tools/completed and deferred unfold activation. Preserve bounded
pending/ready tracking (256) and idempotent completion-ACK retry, never effect or
input retry. Missing context_call_id cannot manufacture a completed boundary.

## Production ownership and remaining acceptance

Backend controller/bridge belongs in tidepool-agent/src/backend/codex; minimal
provider-neutral commands belong in interactive.rs. Service TL exclusively wires
actor_host.rs. Reuse node.rs launch rendering and the reviewed process owner.
Observer is `codex observe THREAD_UUID --remote unix:///absolute/path/actor.sock`
after controller loads the thread, with no prompt or readiness/config mutation.

Next executable gate: compile a real acquire/thread consumer against root's
matching pin; then exercise no-TUI hosted call plus closed completion before an
actual observer. Full mounted sibling-prefix, intent separation, irreversible
fencing and cleanup acceptance remains open. Source inspection and attributed
native focused tests do not close it. Do not replace this running host.
