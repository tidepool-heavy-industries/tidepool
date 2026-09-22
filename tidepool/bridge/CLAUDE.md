# tidepool-bridge — Rust ↔ Haskell value marshalling traits

**Charter.** Belongs: the `FromHaskell`/`ToHaskell` traits and the generic
bidirectional Rust-type ↔ materialized-Haskell-value conversion machinery. Does NOT
belong: the derive macro implementations themselves
(`tidepool-bridge-derive`), any concrete bridged wire-record type
(`tidepool-bridge-effects`).
