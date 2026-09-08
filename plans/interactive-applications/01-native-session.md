# Exact native session binding

Status: planned. Shared decisions are in [README.md](README.md). This work is
slice A1; native input outcome semantics are specified in
[02-delivery.md](02-delivery.md).

## Result

Every automated operation reaches the embedded native session already serving the
actor's full TUI. The backend can identify that owner, observe its state, detect
replacement and reconcile an operation without starting another Codex instance.
The terminal remains the ordinary interactive client.

## Existing owners to extend

| Repository | Source | Change |
|---|---|---|
| Tidepool | `tidepool-agent/src/interactive.rs` | Separate durable conversation references from live session capabilities and typed outcomes |
| Tidepool | `tidepool-agent/src/backend/codex/node.rs` | Launch configuration, binding persistence and capability validation |
| Tidepool | `tidepool-agent/src/backend/codex/active_update.rs` | Use the shared bound session for correlated presentation |
| Tidepool | `tidepool/src/host_dynamic_tools.rs` | Retain actor/tool policy; move Codex wire handling into the backend owner |
| Tidepool | `tidepool/src/actor_host.rs` and modules | Retain and monitor a live connection in the existing deployment row |
| Codex | `codex-rs/tui/src/host_dynamic_tools.rs` and `host_dynamic_tools/input_control.rs` | Extend the repaired owning-TUI relay and registration |
| Codex | `codex-rs/tui/src/app_server_session.rs` | Bind session lifetime, handle replacement and native event subscriptions |
| Codex | `codex-rs/app-server/src/request_processors/` | Reuse native input, queue, turn and observation owners where API support is required |

Do not extend Tidepool's headless stdio `Session` into the interactive connection.
Do not use `app-server connect`, implicit daemon discovery, or a fresh
`thread/resume` as a way to find a running TUI. The relay is HTTP over its private
Unix socket; native app-server Unix transport is a different protocol. Keep both
wire implementations explicit under the backend that owns them.

## Registration and binding

1. The existing deployment row reserves the launch identity before native work
   may exist. It allocates the private socket directory and immutable registration
   data, including expected actor, launch identity and frozen tool definition
   identity. A collision is an error; never unlink an occupied endpoint.
2. The TUI opens its host bridge during native session setup. The host registration
   response supplies the expected input endpoint and protocol requirements. The
   native application creates its own instance identity.
3. After the primary thread exists, the native session reports its conversation,
   application instance, session generation, launch identity and capabilities in
   the existing hosted-session callback. Reattachment updates the actual request
   handle, as in the landed repair, before announcing the new generation.
4. The backend performs a fresh request to that endpoint and validates a nonce
   challenge, the exact identities and the required capabilities. A matching
   persisted callback alone is insufficient. Registration cannot redirect to an
   arbitrary socket supplied by a tool call or message body.
5. The host installs the resulting live capability into the reserved row. The
   actor becomes eligible for automated delivery only after both the hosted tool
   endpoint and this native binding are ready.
6. The host persists a diagnostic binding record. It contains locators and
   observed identities, never a serialized live capability. Reopening it always
   starts disconnected and requires a fresh handshake.

A Shoal launch supplies no automatically submitted positional bootstrap prompt.
Slice A3 publishes the same selected initial-message bytes to the existing inbox
and delivers them once readiness is established. Generic non-Shoal users of
`InteractiveAgentSpec.initial_prompt` keep their existing behavior. For an exact
context fork, readiness follows successful native fork construction and validation
of its completed-call boundary.

All new launches require the complete capability set for the current release.
Absence of a capability fails the affected launch or operation explicitly. A
running older TUI can remain available for manual use; a socket field does not
upgrade it to a new protocol. Initial version allocation is hosted protocol 4 and
binding storage version 6, following the inspected protocol 3 and current binding
version 5. A0 must adjust those numbers if another landed change consumed them.
Unknown future storage versions fail closed through the existing migration owner.

## Live session API

Expose backend-neutral operations at the interactive seam. Keep Codex request IDs,
JSON-RPC types, Unix socket paths and raw events private to `backend/codex/`.

| Operation | Contract |
|---|---|
| Bind / reconnect | Validate the exact launch and current native generation; return a live capability and snapshot |
| Submit input | Submit one immutable operation in an explicit queue or start-or-steer mode |
| Observe input | Query the native admission/presentation record for the exact producer and sequence |
| Withdraw input | Fence one exact input against later dispatch or report its existing presentation/uncertainty |
| Seal producer | Reject further automated input and quarantine its undispatched native queue entries |
| Acknowledge observed outcomes | Allow bounded outcome compaction after host evidence is durable |
| Observe session | Return current native turn, coordination status and usage with a revision |
| Watch session | Subscribe to bounded incremental native observations; report gaps explicitly |
| Interrupt turn | Interrupt only the named native turn in the expected live generation; reject a stale turn |

These are methods on the existing interactive backend/session boundary, not a new
general task service. Do not expose arbitrary native RPC forwarding to Haskell or
accept arbitrary thread IDs from host effect payloads.

Extend the repaired HTTP relay with these routes:

| Route | Payload or response |
|---|---|
| `GET /v1/session` | Challenge, identities, capabilities, current snapshot and revision |
| `POST /v1/input` | Typed immutable input operation; returns typed admission evidence |
| `GET /v1/input/status` | Exact producer/sequence query; returns an outcome or explicit evidence unavailability |
| `POST /v1/input/withdraw` | Exact-key conditional withdrawal, including a fence against delayed submission |
| `POST /v1/input/acknowledge` | Monotonic host-observed terminal prefix for the bound producer |
| `POST /v1/input/seal` | Idempotent producer seal, used for quiescence and recovery fencing |
| `GET /v1/events` | Cursor-based bounded long poll over native observations |
| `POST /v1/turn/interrupt` | Exact generation and turn, with a correlated interruption result |

Paths alone do not select a target. Every request carries the negotiated protocol,
launch, application and generation identity. Payload IDs are bounded, decoded
types, not behavior selected by error strings. Bodies retain explicit limits;
use the stricter of host and native input limits and reject oversize requests
before admission. Keep transport limits and model-context limits separate.

## Generation and concurrency rules

Preserve the existing native thread-store writer lock rather than introducing an
execution registry. `codex-rs/thread-store/src/local/writer_lock.rs` owns the
cross-process claim; `local/live_writer.rs` acquires it for native writers. Verify
that fresh, resumed and forked Shoal paths retain the canonical live-writer guard
and that another interactive process cannot obtain a writable resume of that
thread. History inspection and a new child fork remain supported through their
existing read/fork owners. Lock conflict is an error, not permission to delete a
lock file, discover another daemon or force-load the conversation elsewhere.

The native session lifecycle object owns the handle, generation and admission
gate together. Replacing a handle closes the old admission gate before publishing
the new one. A request either enters the old owner while it is valid or receives
a typed stale-binding rejection before submission. It cannot pass an identity
check and later use an unrelated replacement handle.

Within the same application, reconnecting a host socket does not create another
input producer or change input IDs. A handle replacement advances the session
generation. In-flight requests remain associated with the generation that
accepted them, and their results may still be queried by operation identity.
Replacing the entire TUI creates a new application instance. Old mutation
capabilities cannot be refreshed into that instance automatically.

The primary hosted thread is fixed for the deployment. Native UI navigation to a
different conversation does not retarget hosted tools, input or actor identity.
Returning to the primary conversation can refresh its handle and generation. A
deliberate different conversation requires a new deployment.

Human and host input use the same native admission machinery. Preserve native
interactive input, approvals, queue editing and interruption. Human input does
not acquire a Shoal request lease; an ordinary final answer does not settle a
typed request. The host must tolerate native turn changes caused by the person.
Only the exact-turn interrupt method may interrupt automatically during normal
operation. Scope termination during explicit retirement is separately authorized
by the actor lifecycle owner.

Do not hold an actor registry, host lifecycle or inbox mutex while awaiting a
native operation. One bounded input dispatcher per deployment preserves host
input ordering. Status, input reconciliation, interruption and hosted completion
must remain responsive while an input or model turn is blocked.

## Typed results and observations

Use a closed result vocabulary with structured detail:

| Result | Host interpretation |
|---|---|
| Rejected before admission | Definitive non-submission for this attempt; retain the same logical input ID if retry is allowed |
| Accepted | Native owner has durably accepted this exact operation; no presentation claim |
| Presented | Native owner has correlated the input with its durable model-visible input boundary |
| Rejected or withdrawn before presentation | Terminal negative evidence; no presentation occurred and later dispatch is fenced |
| Unconfirmed | Some work may have happened; only query, seal or stop may resolve the uncertainty |
| Evidence unavailable / expired | Historical result cannot be established; never equivalent to not submitted |
| Stale binding / unsupported | This live capability cannot perform the operation; do not discover a substitute owner |

HTTP status is transport information. The typed response is authoritative only
after identity validation. Connection failure before any request bytes can be
submitted may establish non-submission. Once submission is possible, timeout,
disconnect, cancellation and malformed replies are unconfirmed. Disable hidden
HTTP mutation retries. Use bounded retries for read-only queries.

The native observation stream includes current turn state, input outcomes, usage,
hosted-coordination state and primary-session replacement. Reuse native event
producers and subscriptions; do not drain the TUI's own event receiver or add a
second native history log. Keep a bounded in-memory event window. A lost cursor
requires a fresh snapshot and exact operation queries; it never implies that
missing events did not occur.

Replace ordinary ten-second whole-rollout scans with native usage observations.
Usage remains a cumulative native observation, not a billing ledger. Keep any
offline rollout reader only for a concrete diagnostic/resume consumer, label its
provenance, and bound reads through existing history owners. Remove redundant
live polling once the event consumer is active.

Feed native connection changes into the existing fleet health loop. A responsive
hosted HTTP server does not prove native health; a dead relay does not prove a
dead process. Loss of connection suspends automated input, reports coordination
unavailability and attempts a bounded exact-instance reconnect. Process decisions
use [03-process-supervision.md](03-process-supervision.md).

## Move the wire boundary with a production consumer

Extract Codex registration, request decoding, response encoding and client
transport from `tidepool/src/host_dynamic_tools.rs` into small modules under
`tidepool-agent/src/backend/codex/`. Supply backend-neutral typed callbacks for
session attachment, Haskell invocation and completion. Actor authority, resident
policy, fork gates and hosted retirement remain in the application/actor owners.

The HTTP server's retained lifetime may remain with the host's existing hosted
service handle. The backend owns its protocol adapter; the host owns which
accepted work must finish. Do not create a backend dependency on `tidepool-actor`
merely to move a parser. Do not duplicate the existing generated Tidepool effect
schema in the bridge protocol.

Wire the extracted implementation into actual root, child and resumed launch
paths in the same slice. Delete the old parser and error-to-policy dispatch.
Keep protocol fixtures adjacent to their owners and compile the actual host
consumer, not just the adapter crate.

## Acceptance

- [ ] Fresh and forked full TUIs bind without any default app-server daemon.
- [ ] A retained binding file with a live unrelated socket is rejected.
- [ ] Wrong actor, launch, thread, application or generation is rejected before input admission.
- [ ] A reattached primary handle receives new input; the replaced handle cannot receive later submissions.
- [ ] Switching UI conversations does not redirect hosted calls or host input.
- [ ] Host disconnect/reconnect retains the same application and input producer.
- [ ] A replacement TUI cannot inherit an old live capability.
- [ ] Missing protocol capabilities and oversized requests fail before submission.
- [ ] Concurrent human input and host input use one native execution owner.
- [ ] A second writable resume conflicts with the existing native writer; read/fork operations retain their supported behavior.
- [ ] Event overflow triggers snapshot/query reconciliation without synthetic completion.
- [ ] Native health and hosted endpoint health can disagree visibly.
- [ ] Tests verify actual native routing, not merely that command-line flags or JSON fields exist.

The cross-repository fixture and exact verification workflow are defined in
[06-integration.md](06-integration.md).
