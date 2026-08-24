# tidepool-bridge — Rust ↔ Core value marshalling traits

**Charter.** Belongs: the `FromCore`/`ToCore` traits and the generic
bidirectional Rust-type ↔ Tidepool-Core-value conversion machinery. Does NOT
belong: the derive macro implementations themselves
(`tidepool-bridge-derive`), any concrete bridged wire-record type
(`tidepool-bridge-effects`).
