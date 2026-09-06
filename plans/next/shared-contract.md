# First-wave integration contract

Seed parent: acb3bc11a0d3c5053f22e63a95b112b2208f8f4a. External Codex
implementation is in flight in the human-managed session. No delivered revision
is accepted yet. The native pin remains c8460ffd7c859da2a1467f4384020cf9a19bcc69.

The existing Rust `ActorRef { id, incarnation }` is the authoritative exact actor
identity; do not introduce another issuer. Existing `RequestUpdateId` and
`RequestUpdateDelivery::begin` own amendment correlation and settlement custody.
`UpdatePresentationError::{NotSubmitted, Unconfirmed}` already distinguishes
pre-send failure from uncertain submission. Preserve these semantics while
replacing transport. RPC acceptance is not correlated presentation, and neither
is incorporation. Notifications must not enter request matching or acquire a
reply obligation. No amendment-to-assignment fallback or ambiguous-send retry.

Deliberate scaffold holes: service incarnation/thread/controller-generation
binding and notification entry points require the service lead's concrete
consumer and native contract assessment. Service lead must commit those runtime
contracts before its implementation forks, proposing the small Haskell
notification surface to root before publication. Do not invent an unused public
runtime mirror here or claim these behaviors exist on the current host.

`WaveContract.hs` defines the task-local typed delivery used by root and leads.
Its `executedChecks` projection is a reporting consumer, not an acceptance gate.
Every check retains exact revision, binaries, command, outcome, expectation,
evidence basis and artifact path. An empty selection proves no behavior.

Ownership: service lead owns control/lifecycle and exclusively integrates
`actor_host.rs`, including custody contributions. Run-map owns its derived reader
and isolated tests, requesting instrumentation rather than editing service
owners. Usage owns evidence helpers/examples/guidance, not runtime registries or
response states. Root owns manifests, native pin and cross-lead integration.
Each lead assigns one owner per test file before delegating. Fresh independent
review follows concrete candidates; root acceptance follows integration. Small
workers remain gated on mounted service acceptance.

The running host must not be replaced. `scripts/shoal-init.sh` builds a matched
extractor/worker and Shoal before launching; restart remains user-owned. Source
revision alone does not identify an already running executable.
