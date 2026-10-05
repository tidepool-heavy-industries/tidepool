# tidepool-mcp — MCP eval surface

## Charter

This crate owns effect declarations and generated projections, Haskell eval
preambles, and source assembly. Concrete handlers live in
`tidepool-handlers`; paths and caches live in `tidepool-toolchain`; resident
sessions live in `tidepool-runtime`. Transport and actor hosting belong to their
production consumers, not this declaration layer.

## Effect definitions

Each effect has one definition:

- migrated effects are schemas under `bridge/protocol/src/effects/`, with
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

Do not define helpers against an implicit concrete row alias. Effect GADTs and
helpers live in universal Core; a constructor that stores an effectful closure
must existentially package that closure's row.

## Generated effects modules

The generated effect vocabulary has three stable modules:

- `Tidepool.Effects.Core`: the universal stable GADTs, records, errors, and
  row-polymorphic helpers, including interpreter-only constructors;
- `Tidepool.Effects.Authored`: a stable facade which hides those private
  constructors while re-exporting the authored vocabulary;
- `Tidepool.Effects`: a stable authored-facing re-export of `Authored`.

No effect module defines an executable row or an implicit `M`. Check and
execution renderers pin each invocation's explicit `Eff` row; importing a
vocabulary name never grants its effect. Persistent declarations compile
signatures verbatim. Reusable helpers use `Member` constraints; authors may
name an explicit row with an ordinary type alias of their own.

`Tidepool.Orchestrate` remains a separate installed-cohort source module. Its
helpers state their own row constraints; actor row selection does not replace
its source.

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

- gather data before `ask`, then use the caller-supplied response to choose expensive
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
just test-lib tidepool-mcp --exact FULL_TEST_NAME --expected-count 1
just test-target tidepool-mcp protocol_goldens
```

Intentional golden replacement is a separate writer step. Native acceptance
rejects generation mutation flags and compares the current public surface
against declared goldens. Protocol/effect modules come from their native
generator artifacts; generated consumers do not refresh source files.
