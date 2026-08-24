# tidepool-protocol

The Tidepool effect protocol, as data — not a network IDL, and no runtime
component: nothing here is linked into a server.

**Why this crate exists.** The effect contract used to be single-sourced per
*slice* rather than as a protocol: one verb's truth was spread across up to
four hand-maintained registries — the macro DSL, the wire structs whose
field order was maintained by comment, the extractor's per-verb tables, and
the harness's constructor-name classification lists. A verb added to one and
missed in another didn't fail; it *misrouted*. This crate is where a verb's
truth lives instead: one `schema::Effect` value per effect, projected by
`gen` into every artifact those registries used to hold by hand.

**The two rules that keep it honest** (see `src/lib.rs`'s module doc for the
full statement): it's a leaf — zero dependencies, `std` only, never on
another tidepool crate, so the low crates it describes can consume its
output without a dependency inversion. And there are no raw escape hatches —
no "arbitrary Haskell source" slot, no "arbitrary Rust body" slot. Anything
that can't be expressed in the schema either becomes a deliberate schema
feature or stays hand-written *outside* the contract; it is never smuggled
in as a string.

## What's here

- `schema.rs` / `types.rs` / `hs.rs` — the schema vocabulary: `Effect`,
  verbs, records, errors, field names, and a closed Haskell-type language
  (`HsType`) instead of arbitrary type strings.
- `effects/` — one file per described effect. `effects::all()` is the
  **migrated** set (generated files actually replace the hand-written
  copy); `effects::all_described()` (tests only) also covers effects
  described but not yet flipped over.
- `gen/` — the generators: schema → macro DSL strings, wire mirrors,
  extractor verb tables, harness classification lists.
- `bin/tidepool-protocol-gen.rs` — the `tidepool-protocol-gen` binary that
  runs the generators and writes output.

## Migration status

Effects move here one at a time, each proven byte-compatible against its
hand-written predecessor before that predecessor is deleted. As of this
writing (`git log -1 -- tidepool-protocol/src/effects/`), the migrated set
is `Exec`, `Journal`, `Worktree`, `RepoEvent` — check `effects::all()` for
the current, authoritative list. Everything else still lives in
`tidepool-mcp/src/effect_defs.rs` (see that crate's `CONTRIBUTING.md`
guidance on the two paths).

## How to change it

1. Migrating an existing hand-written effect, or adding a new one: start
   from an already-migrated file in `effects/` as a template, write the new
   `schema::Effect` value, and run the golden byte-compatibility check
   before deleting (or, for a new effect, before wiring) the hand-written
   copy.
2. Extending what the schema can express (a new `HelperBody` shape, a new
   `HsType` variant): these are deliberate, reviewed additions — see the
   "no raw escape hatches" rule above before reaching for a shortcut.
