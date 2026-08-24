# tidepool-protocol — the effect protocol, as data

**Charter.** Belongs: one schema describing effects, verbs, records, errors,
and types, plus the generators that project it into consuming crates — a
zero-dependency `std`-only leaf with no runtime component. Does NOT belong:
any effect not yet migrated onto the schema (still hand-written in
`tidepool-mcp/src/effect_defs.rs`), the generated Rust/Haskell artifacts
themselves (each consumer's own `src/generated/`).
