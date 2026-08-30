# tidepool-mcp — MCP eval surface

## Charter

This crate owns the one-shot `eval`/`resume`/`abort` MCP tools, effect
definitions and generated projections, eval preambles, and shared MCP
transport helpers. Concrete handlers live in `tidepool-handlers`; paths and
caches live in `tidepool-toolchain`; resident-session protocol lives in
`tidepool-repl`.

The live eval API reference is the tool description emitted by the server. Do
not duplicate its verb catalog here.

## Effect definitions

Each effect has one definition:

- migrated effects are schemas under `tidepool-protocol/src/effects/`, with
  generated Rust and Haskell projections;
- effects awaiting migration remain in `src/effect_defs.rs`.

Do not hardcode union-tag positions. Derive them from the same declaration list
used for compilation. Do not copy generated preamble or declaration strings
into tests; use the protocol goldens.

To migrate or add an effect:

1. Define its constructors, records, errors, helpers, imports, and extraction
   policy in the schema.
2. Validate schema equivalence before deleting the existing definition.
3. Regenerate projections and ensure unrelated goldens do not change.
4. Switch exports and handlers to the generated definition.
5. Run rustfmt, protocol goldens, relevant handler tests, and a compilation
   test through the real Haskell surface.

New helper signatures are row-polymorphic:

```haskell
Member SomeEffect effs => ... -> Eff effs Result
```

Do not define helpers against the per-compile `M` alias. Effect GADTs and
helpers live in universal Core; a constructor that stores an effectful closure
must existentially package that closure's row.

## Generated effects modules

The generated surface has two layers:

- `Tidepool.Effects.Core`: the universal stable GADTs, records, errors, and
  row-polymorphic helpers;
- `Tidepool.Effects`: per-compile shim that re-exports Core and defines the
  concrete `M` row.

Persistent declarations validate against Core without importing the shim.
`generalize_m_signatures` permits model-authored signatures using `M` to be
inferred row-polymorphically. Explicit concrete `Eff '[...]` rows remain
concrete and may fail when reused under another row.

Records and error ADTs may be inline in `type_defs`; they land in Core and are
nominally stable across turns. Existing `Tidepool.Records.*` modules are valid
but are not required for new definitions.

Imports needed by generated helpers are declaration data. Imports needed by an
authored library helper belong in that library module. A generated effects
module cannot depend on the authored library layer.

## Paths and configuration

All locations resolve through `tidepool_toolchain::paths`:

- cache: compiled artifacts, materialized embedded library, generated effects;
- global config: verb library, secrets, `config.toml`;
- nearest project `.tidepool/`: project library, secrets, KV, patterns;
- CWD: filesystem handler root and initial Exec directory.

Configuration layers as defaults < global < project < environment. Do not add
crate-local path resolution.

## Eval-authoring guidance

Examples in tool descriptions are the style guide callers copy. When the
recommended spelling changes, update every public example in the same change.
Treat friction encountered while following those examples as an API bug, not
as another warning paragraph.

Useful composition patterns:

- gather data before `ask`, then use the validated response to choose expensive
  work;
- batch filesystem operations with `readGlob`/`grepGlob` and return compact
  structured results;
- use record-dot syntax for generated records;
- treat a nonzero command exit as `Right Proc` and inspect `exitCode`; `Left`
  represents launch failure;
- prefer structural and JSON optics over text scraping when structure exists.

## Structural search

Structural search parses Haskell or Rust fragments and matches syntax rather
than raw text. Keep parsing, matching, and result rendering separate. Invalid
patterns and invalid candidate syntax are typed failures, not silent misses.

## Verification

Primary checks:

```bash
cargo nextest run -p tidepool-mcp
cargo test -p tidepool-mcp --test protocol_goldens
```

Set `TIDEPOOL_REGEN_PROTOCOL_GOLDENS=1` only when intentionally changing the
public generated surface. A migration must first pass against existing goldens;
regenerating early destroys the compatibility proof.
