# Applications: grow the remaining implementation tree

Start with [resume.md](resume.md) and its consolidated applications handoff, then
the applications design README. Existing A1–A4 foundations are starting assets;
use numbered mechanism plans for the remaining joins. The tree below is an initial allocation to refine, not a claim
that every branch is ready or requires a separate actor.

```mermaid
flowchart TD
    L[Sol applications lead: integration and contracts]
    L --> N[Sol native session and admission]
    L --> H[Sol host delivery]
    L --> P[Sol process supervision]
    N --> N1[Instance and generation binding]
    N --> N2[Durable admission and dispatch outcomes]
    N --> N3[Native owner failure cases]
    H --> H1[Inbox identity and reconciliation]
    H --> H2[Dispatcher and request-update consumers]
    P --> P1[Exact process scope and terminal launch]
    P --> P2[Custody and retirement consumers]
    P --> P3[Cancellation and teardown cases]
    L --> C[Later: completion and recovery subtrees]
```

**First shared contract:** select the actual saved A0/native baseline; agree the
live binding identity, input operation shape, readiness/unknown outcomes, fixture
controls and exact file ownership. Reuse current mechanisms. Have native and host
owners establish a minimal real round trip, not a mock-only interface or the
entire admission system before any sibling can work.

Native/session and host/delivery can then develop in parallel against the agreed
contract. Separate native bridge/generation ownership from queue/store transaction
ownership where source permits. The native owner integrates their atomicity and
failure behavior. The host owner separates inbox persistence/reconciliation from
native dispatcher/request consumers after their local types agree. Production
failure tests can be owned alongside implementation; they must drive real owners.

Process/terminal work is independent of completing durable input. Its owner can
split exact process launch/scope from host custody consumers once the scope receipt
and cancellation contract are usable. Keep shared lifecycle-row changes with one
owner; do not treat an entire crate as unavailable to other mechanisms.

**Subsequent joins:** establish the shared accepted-work/process/resource contract
before dependent A5/A6 forks; this is usable owner wiring, not all of custody
implemented serially. Integrate real binding/admission/delivery, then combine
process custody with hosted-completion ownership. Completed-call persistence/fork
release, retirement consumers and adversarial failure cases can form recursive sibling work
with exact native-file reservations. Recovery consumes those checked owners and
engine disposition; its independent status/source-recovery cases need not wait for
unrelated UI work. Matched package acceptance remains the final integration owner.

Astra slots: exact native identity/deduplication/restart uncertainty; completed-call
and cancellation/custody correctness. Commission a concrete design or difficult
repair, then let Sol implement the dependent tree. Do not send every fixture review
through Astra. The lead owns cross-repository integration and complete A0–A8 gates.
