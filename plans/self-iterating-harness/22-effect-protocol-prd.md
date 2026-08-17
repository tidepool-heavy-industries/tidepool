# PRD — The Effect Protocol: one schema, generated everywhere

**Status:** proposed (2026-08-17), operator-approved direction ("do it right,
port one effect at a time")
**Input:** an external adversarial architecture review (Codex, 2026-08-17)
whose central findings this PRD absorbs; the review file itself is ephemeral
(`*.md.tmp`), so everything load-bearing is restated here.
**Relation:** infrastructure under PRD 20/21 — neither depends on it, both
get cheaper as it lands.

## The problem

The effect contract is single-sourced per SLICE, not as a protocol. Today one
verb's truth is spread across up to four hand-maintained registries:

1. `tidepool-mcp/src/effect_defs.rs` — the macro DSL: raw Haskell type
   strings, helper Haskell, error ADTs, docs, handler method names.
2. `tidepool-bridge-effects` — hand-written wire structs (`Wt*`/`Ag*`/`Ev*`)
   whose FIELD ORDER must match the macro's `type_defs` strings, maintained
   by comment ("positional — must match").
3. The extractor's `Translate.hs` — `intrinsicVerbModules`/`sitedVerbs`:
   per-verb arities, sited siblings, answer shapes, type-validation rules.
4. The harness's `classify_hole` — constructor-NAME string lists routing
   suspensions.

Proof this bites: the `RepoEventAwait` outer-row bug (found by review,
2026-08-16) was exactly a verb added to registry 1 and missed in registry 4.
Field-order-by-comment in registry 2 is a human-maintained wire protocol. A
new typed-yield verb is a four-registry change surface.

## The design

A small, data-only **`tidepool-protocol`** crate: plain Rust data structures
describing effects, verbs, records/ADTs, errors, field names and types,
Haskell helper templates, and named domain adapters — plus build-time
generators. NOT a network IDL; no runtime component.

Generated from it:

- `EffectDecl`/MCP tool descriptions and the `Tidepool.Effects` Haskell
  source (replacing raw `type_defs` strings);
- Rust request enums, error ADTs, wire types, and handler dispatch glue
  (absorbing what `effect_glue` does today, then the hand-written
  `Wt*`/`Ag*` mirrors);
- `Tidepool.Records.Bridged` and every Haskell companion declaration that is
  presently a string;
- mechanical `From<domain>`/`TryFrom<wire>` adapter skeletons where a richer
  domain form (PathBuf, Option, newtypes) genuinely differs;
- a Haskell `Generated` metadata module consumed by `Translate.hs` — the
  extractor keeps the generic recognition/head-swap MECHANICS, but no longer
  owns the list of product verbs; per-verb type-shape rules come from a
  CLOSED, enumerated policy set (a new policy is added deliberately in
  Haskell, never serialized through the schema);
- harness hole classification: every verb annotated with a handling class,
  the classifier generated — the `RepoEventAwait` bug class becomes
  unrepresentable. The class VOCABULARY is the code's real nine-way
  distinction, not this PRD's earlier five-name sketch; the phase-1 scaffold
  doc (`22-p1-protocol-scaffold.md`) is authoritative for it. Two
  generation-time rules: a verb without a class fails GENERATION, and an
  unrecognized constructor at runtime fails LOUD — today's silent
  fall-through-to-Ask is exactly how the `RepoEventAwait` omission hid.

Hand-written forever: handler method bodies, Haskell library behavior, the
harness's tree policy, domain types' OS/backend concerns.

Generator requirement (operator's type-level review, 2026-08-17): wire-side
integers and identifiers generate as NEWTYPES (`SiteId`, `FanCount`, …) with
fallible boundary constructors — decode once at the edge, typed everywhere
after. Do not hand-write these ahead of the generator; specify them here so
phase 2+ emits them.

## Hard rules

- **No raw-Haskell escape hatches in the schema.** Arbitrary embedded source
  is how the schema stops being authoritative. Helper templates are
  parameterized, reviewed patterns; anything that cannot be expressed
  becomes a deliberate schema feature or stays hand-written OUTSIDE the
  contract.
- **Byte-compatible migration.** Golden tests prove generated output matches
  the current hand-maintained artifact byte-for-byte (or a reviewed,
  explained diff) BEFORE each effect flips. Haskell surface names, serde
  names, CBOR/Core representation, and the positional union-tag slots (root
  CLAUDE.md locked decision) are all stable across the migration.
- **Effect at a time, smallest first.** No flag day. Prove the whole
  vertical (schema → all generated artifacts → golden match → flip → delete
  hand copy) on one small record/error effect before touching the big rows.
- **One canonical contract type per concept.** At most one richer internal
  type beside it, converted at an explicit generated boundary. The
  `Wt`/`Ag` prefixes retire with the mirrors — two names for one concept in
  two universes is the smell, not the solution.
- **`tidepool-bridge-effects` does not survive as an authored crate.** At
  most it becomes generated output. `tidepool-extract-cmd` and the other
  leaf crates are explicitly preserved (the review's own restraint finding:
  the problem is descriptions and lifecycle owners, not crate count).

## Sequencing (value per risk, from the review's roadmap)

0. **Prerequisite (in flight):** compile-pipeline consolidation — runtime
   becomes the sole extract-artifact pipeline (`CompiledArtifacts`,
   multi-target + typed sidecars; `tidepool-harness/src/compile.rs`
   deleted). Fewer artifact shapes for the generator to serve.
1. **Schema core + first effect.** `tidepool-protocol` + generators; one
   small effect (Time or Console class) end to end with golden tests. Exit:
   the generated slice is byte-compatible and the hand copy is deleted.
2. **Journal, then Worktree/Event.** Journal is small and freshly
   understood; Worktree/Event retire the `Wt*`/`Ev*` mirrors and the
   positional-comment protocol.
3. **Subagent row last** (largest surface: async trio, cycle table, tool
   loop), plus generated harness classification and the `Translate.hs`
   metadata module. Registry count for a new verb: one.
4. **Then** (separate decisions, better-informed post-migration): failure
   envelopes at boundaries; `tidepool-testing` split by dependency
   direction; resident-session host consolidation (deferred until S1/C-lane
   work settles — it is the highest-risk cut and those files are active).

## Acceptance

- Adding a new verb to an existing effect = one schema edit + one handler
  method + one Haskell behavior change where applicable. Nothing else.
- A verb missing a handling-class annotation fails generation, not runtime.
- Grep proves: no `type_defs` raw strings, no hand-maintained wire structs,
  no constructor-name classification lists, no hand-authored `sitedVerbs`
  product rows.
- The deploy handshake, CBOR wire versioning rules (tidepool-repr), and
  every existing authored Haskell program survive unchanged.

## Open questions

1. Generator staging: build.rs per consuming crate vs a committed-output
   generator invoked by a script (leaning committed-output + CI check — GHC
   consumers make build.rs feedback slow, and diffs stay reviewable).
2. Where adapter skeletons live so domain crates keep not depending on top
   crates (leaning: generated into each consuming crate, schema stays leaf).
3. The closed policy-enum vocabulary for extractor type-shape rules — sized
   by surveying today's `sitedVerbs` rows before designing.
4. `Translate.hs`'s `vsMisShapeIsError` is declared, documented, and set on
   `forkMap`/`forkCata`, but NOTHING READS IT (found during phase 1). The
   phase touching `Translate.hs` must resolve it — enforce it or delete it —
   before assuming the generated form preserves behavior.
5. Checks must be TESTS in quick-tier crates (phase-1 finding: an orphaned
   `--check` script nothing invokes, and a generated-files guard living in a
   default-filter-excluded crate, are both non-checks). Placement rule for
   every future generated-files guard.
