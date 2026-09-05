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
