# Hosted HTTP drain contract

Seed3c96f192 has real axum graceful shutdown via serve_until; production serve
remains never-shutdown until parent explicitly wires control. This layer owns
HTTP admission and connection/request draining, NOT resident effect retirement.

Implement one host-private lifecycle control for the existing service:
Serving -> Quiescing -> Draining (monotone, idempotent). Quiescing fences new
/call and /session admission at one owning handler boundary. A request admitted
before the fence may finish. /completed and read-only registration remain usable
while Quiescing, including on fresh connections, so closing a Haskell call's
native prefix remains possible. Draining fences all new requests and triggers
axum graceful shutdown; awaiting its server task proves HTTP drain only. Caller
must decide when endpoint/native completion permits transition to Draining.
Do not automatically proceed to Draining just because a /call returned.

Use a small typed control handle shared with this service, not a process registry,
request ledger, or duplicate counter of resident work. The admission decision and
phase transition need an explicit linearization contract; no lock across endpoint
await. Keep lifecycle fields private and prevent reopening. Retained server task
must remain awaitable after a bounded timeout; no detach/abort-as-success API.
Old serve can delegate an untriggered control path for consumer compatibility;
serve_until semantics must not silently discard completed callbacks after callers
thought they only fenced new effects. Document exact host integration call order.

Tests must exercise real Unix HTTP requests, idle/keepalive connections, blocked
accepted call, rejected later call, and concurrent completion while quiescing.
Client disconnect and HTTP drain are not proof that submitted resident effects
ceased. Existing endpoint/runtime remains that owner. Test-only endpoint gates
may expose this boundary, not fabricate a resident retirement proof.

Ownership: implementation worker owns host_dynamic_tools.rs lifecycle and existing
inline tests compatibility. Test worker owns adjacent host_dynamic_tools_drain_tests.rs
and fixture files after concrete API scaffold. Lead owns test module wiring and
integration. Parent service exclusively owns actor_host.rs and custody. Fresh
reviewer inspects concrete candidate and drives repair. No native work/manifests.
