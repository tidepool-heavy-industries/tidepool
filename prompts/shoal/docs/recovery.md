A replaced Haskell machine creates successor actor incarnations. Old handles,
closures, materialized values, replies, and watch projections are never
silently revived.

```text
:recovery
:bindings
```

`:recovery` reports source sessions, declaration generations and hashes that
were replayed through GHC, and exact losses. `:bindings` shows what actually
exists in the successor. Operational receipts may prove an earlier effect
committed, but Tidepool never replays an effectful input unit after loss.

This is resident workbench recovery, not a promise to reconstruct a campaign
after a host restart. Preserve important decisions and Git evidence in the
repository. If a handle was lost, use the receipt and runtime inspection to
establish what happened before deciding whether to issue new work.
Provider failure does not settle an actor request or imply process death.
Inspect `rosterProviderHealth`, `rosterProviderTurn`, and
`rosterProviderObservationStale` before deciding how to recover. A child failure
publishes a deduplicated durable notice to its exact supervisor. Root failures
are operator-visible without injecting another request into the failing root.
Do not blindly steer a provider-rejected conversation: more messages do not
repair incompatible retained history. Use Shoal communication and explicit
retirement; native collaboration is disabled on hosted launches.

`:status!` shows the verified executable and version, requested model/effort,
and separately the provider-confirmed settings. Actor shells receive that
executable as `TIDEPOOL_INTERACTIVE_CODEX_BIN`; invoke the quoted variable when
using backend CLI commands. PATH may resolve a different rollout protocol.
