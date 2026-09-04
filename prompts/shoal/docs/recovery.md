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
