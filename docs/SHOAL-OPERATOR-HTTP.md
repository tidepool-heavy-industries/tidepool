# Shoal operator attachment and actor graph

A running Shoal host prints its protected Unix socket path at startup. The host
owns one resident forest and heap. Workbenches are optional, independently scoped
roots; no provider process or provider-turn identity is created for them.

Provision and attach:

```sh
shoal operator --socket /path/to/run/operator/operator.sock new
shoal operator --socket /path/to/run/operator/operator.sock list
shoal-repl --socket /path/to/run/operator/operator.sock --session operator-42-1
shoal operator --socket /path/to/run/operator/operator.sock stop operator-42-1
```

`new` prints JSON containing `session` and `socket`. Use the returned ID verbatim.
Multiple consoles attached to that ID share bindings. Another `new` creates an
isolated scope. Console disconnects do not retire workbenches. Explicit stop
retires the workbench and its descendants. Model-root replacement preserves
operator bindings. Model-root completion or unavailable recovery leaves operators
attached; host shutdown retires the entire forest. IDs are not restored
across host restart.

The socket directory is mode 0700 and the socket is mode 0600. Access is local to
the host owner. Operator roots receive forest inspection/control authority;
ordinary actors retain their subtree boundaries, including children launched by
an operator. Context cloning still requires an actual provider context.

## JSON graph interface for TUIs

`GET /v1/sessions/{session}/actors` returns a JSON snapshot without executing
Haskell. For example:

```sh
curl --unix-socket /path/to/run/operator/operator.sock \
  http://localhost/v1/sessions/operator-42-1/actors
```

The response has `protocol_version: 1`, the exact `session`, and an `actors`
array sorted by actor identity. Every node has:

| Field | Meaning |
| --- | --- |
| `actor` | `{ "id": number, "incarnation": number }`; use both as the graph key |
| `label` | Human-readable actor label |
| `supervisor_parent` | Exact parent identity, or null for an independent root |
| `context_parent` | Exact context-cloning parent identity, or null |
| `terminal` | Null while live, otherwise `{ "kind": "completed"/"failed"/"cancelled", "summary": string }` |
| `workbench` | Tagged current workbench state described below |
| `provider_thread` | Actual provider thread, or null for a workbench without a provider |
| `provider_turn` | Null or `{ "thread", "turn", "revision", "state" }` |
| `provider_observation_stale` | Whether the provider observation is stale |
| `bound_worktree` | Bound worktree identifier, or null |
| `active_requests`, `queued_requests` | Request identities from the request owner |

`workbench.kind` is `idle`, `running_unit`, `awaiting_effect`, `terminal_transfer`,
or `failed`. Running/waiting states include `input_unit_index` (zero based) and
`total`; waiting also includes `effect`. Terminal transfer includes `transfer`
(`reply` or `cancellation_acknowledgement`). Provider `state.kind` is `active`,
`succeeded`, `failed`, or `interrupted`; failure retains its typed `failure`.

Use `supervisor_parent` for tree edges and `context_parent` for optional secondary
edges. Keep terminal nodes until a subsequent snapshot omits them. The topology
is captured under the existing actor-record lock; individual runtime observations
are sampled from their owners. This is a current-state polling API, not an event
stream or an atomic snapshot of all ongoing effects. It remains available while
actors execute Haskell. It uses the same visibility rule as typed inspection.

## Console API v1

The client's `wire.rs` is copied verbatim to `tidepool/src/operator/wire.rs`.

- `GET /v1/sessions/{session}` returns `SessionInfo`.
- `POST /v1/sessions/{session}/submit` accepts `{ "source": "raw UTF-8 source" }`
  and returns ordered output/diagnostic blocks, outcome, and structured receipt.

Encode the entire opaque session ID as one URL segment; it is decoded once.
Unknown or retired IDs return JSON 404 errors and are never replaced silently.
The graph route is an additive interface and does not change the console schema.

Source is classified by the existing workbench parser. Ractor serializes admitted
submissions. Admitted work survives loss of the HTTP observer. Successful prefixes
and diagnostics remain in receipt order; rejection does not imply rollback.
`Committed`, `Completed`, `Replied`, and `RequestCancelled` map to wire `completed`;
`Rejected` maps to `rejected`. The exact status remains in the receipt. Completion
means the call ended, including a terminal transfer that skipped trailing units.

Requests are bounded to 1 MiB of encoded JSON, responses to 8 MiB. Oversized requests
are rejected before admission. Display truncation is explicit and preserves the
outcome and receipt. If that required result cannot fit, the service returns JSON
500, leaving execution unknown to the caller. No replay, retries, streaming,
cancellation endpoint, or execution-status lookup is provided.

## Host control

Host control is separate from the console contract:

- `POST /host/operators` provisions a workbench.
- `GET /host/operators` lists live attachment IDs and socket paths.
- `POST /host/operators/{session}/stop` explicitly retires an attachment.

The graph endpoint requires an existing workbench; it grants no authority beyond
that workbench's host-provisioned grant.
