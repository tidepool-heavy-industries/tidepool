# Service TL — execution/control ownership

## Assignment and fixed decisions

Deliver one actor-owned native app-server service plus one persistent Shoal
controller; a separately attachable TUI observes it. This is the user-selected B
architecture. It is not a global daemon, shared multi-actor service, or patch to
embedded TUI steering. Operate in the existing per-actor managed mount; preserve
cwd, permission, environment and native-goals-disabled policy. Default forks are
exact full-prefix. This document is self-contained; no old actor handles are live.

Own `tidepool-agent/src/backend/codex/`, actor request/notification and lifecycle
mechanisms in `tidepool-actor`, relevant `tidepool-node` process boundary, and
`tidepool/src/{shoal.rs,actor_host.rs}` host integration. Root owns manifests and
final native dependency pin integration. Read nearest AGENTS and production callers.
Do not create a second launcher, mailbox, pending-request registry or scheduler.

## Diagnosis to preserve, not repeatedly rediscover

Pinned native source inspected: `/home/inanna/dev/codex`, revision
`c8460ffd7c859da2a1467f4384020cf9a19bcc69`. Pinned executable was
`/nix/store/jan5kb6zi1af7gb4ajpl0wban9b72nzk-codex-rs-0.0.0-dev+c8460ff/bin/codex`.
PATH Codex differed; record actual executable, not just version text.
Tidepool inspected baseline `e5a1842dd4d3302d6eb8c0519a678937599d9c7e`.

`active_update.rs` calls `Session::connect_proxy` in `process.rs`: raw JSONL
initialize through native `app-server proxy`. Pinned proxy copies bytes to Unix
socket; server calls WebSocket `accept_async`. Meanwhile `--destination-local`
selects embedded server, not the global daemon the proxy assumes. Eleven retained
update warnings support failed presentation; these do NOT imply request/respond,
launch or watches were all broken. Missing XDG environment was not established.

Native already supports `app-server --listen unix://ABSOLUTE_PATH`, remote Unix
TUI resume, after-call fork boundaries and readiness/declaration validation.
Native dynamic-tool HTTP-over-UDS bridge lives in TUI today: move/reuse forwarding
under Shoal controller, leave actual tool execution/receipts in actor host.

## Semantic contract (commit actual types before worker implementation)

Use distinct typed intents, not a string-tagged generic “message”:

| Intent | Owner/invariant |
|---|---|
| New assignment | existing typed `request`, queued; unique response obligation |
| Response | settles exact active request, never “latest arbitrary input” |
| Amendment | requester owns exact active response; safe-boundary delivery, settlement fencing |
| Notification | one-way text, no response obligation, no replacement of assignment input; may wake idle actor |
| Dependency wake | existing watch continuation; does not fabricate an assignment |
| Cancel/retire | exact actor/incarnation/request ownership; completed effects survive |

Common transport is not a merged semantic stack. Specify idle notification
activation with no `respond` binding and active notification presentation without
replacing current typed bindings. Keep notification data out of assignment matching.
Choose exact small Haskell surface with root before publishing it; do not assume a
notification API already exists. Cancellation remains its own state transition.

Retain service incarnation, thread, controller generation, request and call identity
at owning entries. Distinguish before-send `NotSubmitted`, accepted submission,
correlated presentation, and post-send `Unconfirmed`. Names here describe semantics,
not an already shipped enum. Persisted matching input proves presentation, not model
understanding; ask worker for intended change/incorporation evidence separately.
No blind retries after ambiguous send, no fallback amendment -> new assignment.

## Launch sequence and ownership

1. Install legitimate worktree custody **before first bootstrap worktree use**.
   Delegate [custody.md](custody.md); keep actor_host integration exclusively here.
2. Existing supervisor launches pinned service inside actor mount, private explicit
   socket bound to existing incarnation identity. No second endpoint registry.
3. Persistent WebSocket-over-UDS controller initializes and routes JSON-RPC responses,
   server requests and notifications separately. Delegate native ownership contract
   to [native-control.md](native-control.md), then consume the reviewed protocol.
4. Fork on destination child service using source thread + `afterCallId` only after
   the enclosing Haskell call's actual result is durably closed. Use full prefix,
   deferred continuation, `requireClientReadiness`, expected dynamic tools. Preserve
   inherited settings except explicit override. Verify source history is accessible
   across real mounts; do not reconstruct summaries if it isn't.
5. Establish controller and destination host/session tool bridge, then release
   readiness and submit assignment once. TUI never does these steps.
6. Attach observer to explicit service/thread without initial prompt. Controller
   disconnect fences continuation at safe boundary and reconciles pending work;
   observer disconnect has no execution effect. Retirement reaps service/executors.

Retained host receipts support effect replay without reexecution. Correlation is
not deduplication. Existing `turn/start` uses atomic start-or-steer; `turn/steer`
supports expectedTurnId. Select the right operation only after verifying assignment
fencing for both idle and active states. Readiness and client custody are different
from “process exists” or “TCP/Unix connection opened”.

## Recursive waves and deliverable

First commit local interface/ownership scaffold compatible with root contract.
Fork native-control and custody obligations immediately. In parallel inspect and
prepare controller/bridge consumer against explicit unsupported holes. Avoid two
writers to actor_host; worker returns a scoped candidate, TL integrates it.
Native source requires a correctly seeded native checkout: do not seed a Tidepool
worktree and write the user's `/home/inanna/dev/codex` checkout incidentally. Inspect
available repository/worktree authority; have root establish allocation if needed.

Next wave: backend client/bridge implementation plus focused protocol fixtures;
fresh native/custody reviewers trace failure paths and request repairs directly.
After incorporating reviewed native contract, run mounted integration and retain
an implementer for failures found by root's fresh acceptance branch.

Deliver exact native and Tidepool commits, pin/build requirements, changed API,
focused checks, supported reconnect semantics, known limits and cleanup evidence.
Delete obsolete proxy/embedded assumptions when replacement covers consumers;
do not leave duplicate production control paths. If controller loss cannot safely
resume, expose a truthful terminal/recovery state rather than fake recovery.

Acceptance includes no-TUI hosted tools, observer noninterference, two siblings'
closed prefix, stale controllers, no effect duplication, amendment/notification
separation, first-bootstrap custody and exact retirement. Use the full matrix in
`../acceptance.md` for integration; mock success is not the final gate.
