# Operator HTTP workbench

Status: accepted direction; shared forest runtime and optional operator workbench.

## Consumer contract

The client lives in `/home/inanna/dev/shoal-repl`. Its `docs/HTTP-API.md`
defines proposed HTTP/1.1 over a protected Unix socket; `src/wire.rs` defines
canonical copyable JSON types. `docs/HTTP-REVIEW.md` records client validation.
Preserve that wire schema. No routes are implemented by this document.

The operator owns a separate persistent Haskell workbench, attached to an exact
server-issued incarnation. It must be able to address the running Shoal tree.
It must not share a model actor's lexical session or provider-turn identity.

## Runtime ownership

The host owns one resident forest runtime. Tree roots share its actor directory,
request and watch registries, fork-group lineage, machine registry, and deployment
channel. Supervision remains per tree. The operator is an optional debug client
with a separate workbench, not an ancestor of the model coordinator. Disconnecting
it must not affect model work; model-root recovery must not rebuild the forest.

Extend the existing owners rather than adding a forest-shaped copy of each
registry. Today `spawn_resident_root_in_incarnation` constructs the resident
registries, while `spawn_local_actor_in_incarnation` constructs a new routing
directory. Both allocation boundaries must move to an explicit host-owned
runtime; ordinary single-root embedders can create a private runtime through the
same implementation.

Shared visibility does not grant control. Normal model actors retain their
existing subtree restrictions. An operator workbench receives explicit host
inspection/control authority for the forest; its socket session identifies that
already-provisioned authority. This is independent of supervision ancestry.

### Live-value boundary

A shared routing directory alone is insufficient. The current request/call and
mailbox owners reject crossing machine sessions because Haskell values carry
heap/root custody. Independent lexical workbenches can share a resident machine
without sharing lexical bindings, as existing forks demonstrate. Provision the
operator's independent scope in the applicable machine realm when it needs typed
communication with that tree. Do not remove machine-identity checks or serialize
arbitrary live Haskell values to make forest routing appear to work.

The implementation must make explicit whether a forest uses one machine with
multiple root scopes, or contains several machine domains with a separate typed
transfer mechanism. Prefer the former for this initial operator use case; adding
cross-machine live-value transport is outside the HTTP adapter's responsibility.

## Host lifecycle and defaults

The host provisions optional roots with `shoal operator new`, lists their exact
incarnations and attachment details with `shoal operator list`, and explicitly
retires them with `shoal operator stop`. These host-control operations use the
protected Unix transport separately from the client v1 attachment contract.
No workbench is created automatically or persisted across host restart. Multiple
consoles share one workbench's bindings; separate workbenches have isolated scopes.

The forest uses one machine and heap. Root retirement closes only that tree; host
shutdown retires every root. Operator attachment requires no provider context or
fabricated provider turn. Forest inspection/control is an explicit host grant
that descendants do not automatically inherit. Context cloning still requires
an actual provider context.

Wire completion means the call ended. `Committed`, `Completed`, `Replied`, and
`RequestCancelled` map to `completed`; `Rejected` maps to `rejected`. The exact
terminal status and successful prefix remain in the structured receipt.

## Implementation sequence

1. Extract the host-owned forest from existing root construction, including the
   kernel routing directory. Provision independent root scopes and the optional
   operator workbench in that runtime. Cover shared addressing, independent
   bindings, authority, one-tree shutdown with another still usable, and model
   root recovery before adding HTTP.
2. Expose a structured source submission seam from the resident workbench owner.
   Reuse `WorkbenchRequest::from_ghci_input`, receipt production, and actor
   serialization. Do not parse the tool transcript or invent provider contexts.
3. Copy the client's wire module verbatim. Add the two documented routes in the
   Shoal host, with owner-only socket access and published attachment metadata.
   Reject malformed/oversized submissions before admission. Percent-decode the
   opaque session segment once. Never retarget an expired ID.
4. Make admitted work independent of connection lifetime; bound connection input
   and response output. Keep the execution gate held until execution finishes,
   including after disconnect. Preserve receipt and outcome during truncation;
   return an uncertain error if the response cannot fit.
5. Document and test the mapping of WorkbenchRunStatus: Committed/Completed,
   Rejected, Replied, RequestCancelled. Terminal transfers retain their exact
   structured receipt; do not silently declare unsupported states completed.
   Output order means existing receipt order, not streaming stdout/stderr order.
6. Run the client's protocol fixtures against the adapter and live workbench
   checks: retained values; rejected successful prefix; diagnostics followed by
   successful execution; concurrent POSTs; disconnect during execution; authority;
   exact incarnation loss. Publish socket/session instructions and verify one
   real shoal-repl round trip.

No replay, cancellation, execution-status service, provider-driven operator
session, or new contract-distribution mechanism belongs in this slice.

## Implementation status

Implemented, including the additional JSON actor-graph interface requested during
implementation. The host owns one forest across model-root replacements, with
independent operator scopes and explicit forest inspection/control grants. The
provider fleet remains attached across root recovery. Model-root completion or
unavailable recovery leaves operator workbenches attached until host shutdown.

`shoal operator --socket SOCKET new|list|stop` uses the protected host-control
routes. Console v1 inspect/submit preserves the client's wire module verbatim.
`GET /v1/sessions/{session}/actors` provides current graph/state data directly from
the inspection owner, without executing Haskell. See
[operator attachment and graph API](../../docs/SHOAL-OPERATOR-HTTP.md).

The runtime uses the existing machine checkout/settlement owner for both actor
execution and host scope provisioning. Root cleanup closes its resource realm
and retires its lexical scope. Ordinary children do not inherit the operator's
forest grant. Workbench creation has no provider attachment or synthetic provider
identity. Runtime creation now inserts the machine outside `debug_assert!`, so
release builds retain it too.

### Validation

Executed through the repository Nix/toolchain setup:

- `scripts/battery.sh -p tidepool --lib -E 'test(operator::tests::) | test(forest_operator_survives_model_root_recovery) | test(abnormal_root_reuses_only_a_queue_ready_conversation)'`
- `scripts/battery.sh -p tidepool --lib -E 'test(typed_reply_settles_response_and_wakes_registered_watch)'`
- `scripts/battery.sh -p tidepool-actor --test resident_local_actor`
- `scripts/battery.sh -p tidepool-actor --lib -E 'test(independent_roots_share_routing_but_not_supervision)'`
- `cargo test -p tidepool --bin shoal`

The HTTP integration uses a real Unix listener and resident Haskell machine. It
covers retained and isolated bindings, Unicode/multiline source, successful
prefixes, diagnostic-only errors, concurrent submissions, exact path decoding,
request bounds, graph inspection during execution, disconnect after observed
admission, explicit retirement, and stale-incarnation rejection. Receipt mapping
and response bounds are checked separately. The forest recovery test additionally
launches an operator child, exchanges a typed request/reply, checks that the child
has no forest grant, and replaces the model root while preserving operator values.
The existing typed reply/watch integration also passes.

The unchanged client library was compiled in `/tmp/shoal-client-probe`, depending
on `/home/inanna/dev/shoal-repl`, then exercised against the live test adapter via
`SHOAL_OPERATOR_CLIENT_PROBE`. It successfully inspected the exact session,
submitted source, retained a binding across calls, and queried status/lineage.
The client's own `cargo test --test http_protocol` passes all 14 fixtures; those
fixtures use their fake server, while the separate probe tests this adapter.
No graphical TUI rendering or live external model/tmux recovery was exercised.

The matched build command was:

```sh
nix develop .#shoal --command bash -euc '
  unset TIDEPOOL_EXTRACT TIDEPOOL_EXTRACT_WORKER TIDEPOOL_EXTRACT_DAEMON_SOCKET
  source scripts/lib-extract.sh
  resolve_tidepool_extract
  validate_tidepool_extract_endpoint
  cargo build -p tidepool --bin shoal
'
```

It checked the Haskell worker with Cabal and built the Rust extractor and Shoal
from this checkout. The resulting Shoal binary is
`.shoal/build/cargo/debug/shoal` in this environment. The final Haskell-backed
checks used the matched `.shoal/build/cargo/debug/tidepool-extract`.

Formatting, `git diff --check`, and byte comparison of the copied `wire.rs` pass.
Regular Clippy completes for the changed libraries with existing warnings in
`generated/forks.rs`, `lineage.rs`, and `role.rs`. Strict `-D warnings` is blocked
by those unchanged warnings (and `tidepool-node/src/inbox.rs` when linting
dependencies). No extractor translation or CBOR serialization was changed.
