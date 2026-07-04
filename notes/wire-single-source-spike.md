# Wire single-source spike (T5)

Recommend the mechanism for single-sourcing the CBOR wire format between
`haskell/src/Tidepool/CborEncode.hs` (the production encoder) and
`tidepool-repr/src/serial/{write,read}.rs` (the Rust codec).

**Recommendation: (a) — cross-language golden roundtrip contract suite, with
the corpus as the single source of truth.** Proof:
`tidepool-repr/tests/golden_wire_contract.rs`, 4 tests green — including
**byte-identical** re-encode of all 78 committed tree fixtures.

---

## 1. The drift class, with evidence from this spike

The two sides are hand-mirrored; the failure mode is *undetected divergence*
that surfaces later as a downstream symptom (most recently: `5976fa18`, where
tidepool-testing's hand-mirrored heap reader rendered pointer-carrying lits as
empty placeholders and produced false differential divergences).

The spike's read of both codecs found live drift in every hand-mirrored
surface it touched:

1. **Doc vs code**: `tidepool-repr/CLAUDE.md` says the version is "currently
   1.0"; both `serial/mod.rs` and `CborEncode.hs` are at 1.1.
2. **Writer vs writer**: the two encoders disagree *today*. Haskell always
   emits 7-element meta entries and optionally `captured_type`/`var_names`
   warnings keys; Rust `write_metadata` emits 5/6/7-element entries
   conditionally and can only emit `has_io: false` (it doesn't accept a
   `MetaWarnings` at all). Only reader tolerance (5..=7, unknown-key skip)
   hides this.
3. **Corpus vs encoder**: all 80 committed fixtures are stale relative to the
   current encoder — headerless (pre-TPLR) with 6-element meta entries. They
   pass only via the legacy pass-through in `strip_header`.

None of these is currently caught by any test. That is the class to kill.

A structural fact that shapes the options: **in production, CBOR flows one
way** — Haskell writes, Rust reads. `write_cbor`/`write_metadata` have no
production callers; they exist for the differential/property harness. So
"the format" is operationally *whatever CborEncode.hs emits and read.rs
accepts*, and the Rust writer is a third hand-mirrored implementation, not
the source.

## 2. Catalog of encoded shapes

Everything on the wire, from reading both sides end to end. All frames are
CBOR definite-length arrays led by a text tag; both `cborg` and `ciborium`
emit canonical minimal-length integers (verified byte-exactly by the proof).

**Header** (both payload kinds): 8 bytes, `TPLR` + major/minor as big-endian
u16 (currently 1.1). *Optional on read*: no magic → legacy pass-through.
Major mismatch or newer minor → loud `UnsupportedVersion`.

**Expression payload**: `[nodes_array, root_idx]`; root must be the last node.

| Frame | Shape |
|---|---|
| Var | `["Var", varid:u64]` |
| Lit | `["Lit", <literal>]` |
| App | `["App", fun:idx, arg:idx]` |
| Lam | `["Lam", binder:u64, body:idx]` |
| LetNonRec | `["LetNonRec", binder:u64, rhs:idx, body:idx]` |
| LetRec | `["LetRec", [[binder:u64, rhs:idx]…], body:idx]` |
| Case | `["Case", scrut:idx, binder:u64, [<alt>…]]` |
| Con | `["Con", dcid:u64, [field:idx…]]` |
| Join | `["Join", label:u64, [param:u64…], rhs:idx, body:idx]` |
| Jump | `["Jump", label:u64, [arg:idx…]]` |
| PrimOp | `["PrimOp", name:text, [arg:idx…]]` |

Literals (`[tag, value]`): `LitInt`:i64, `LitWord`:u64, `LitChar`:u32
codepoint, `LitString`:bytes, `LitByteArray`:bytes, `LitFloat`:u64 bits,
`LitDouble`:u64 bits. Alt: `[<altcon>, [binder:u64…], body:idx]`; AltCon:
`["DataAlt", dcid]` / `["LitAlt", <literal>]` / `["Default"]`.

**Metadata payload**: `[entries_array, warnings_map]` (legacy: bare entries
array, detected structurally). Entry: `[dcid:u64, name:text, tag:uint,
arity:uint, [bang:text…]]` + optional 6th `qualName:text` (`""` decodes to
None — placeholder when labels follow without a qn) + optional 7th
`[fieldLabel:text…]`. Reader accepts 5/6/7. Warnings map: `has_io:bool`
required; `captured_type:text` and `var_names:[[u64,text]…]` optional;
unknown keys skipped.

**Shared vocabularies encoded as strings** (drift surfaces beyond structure):
frame tags, literal tags, altcon tags, bang names
(`SrcBang`/`SrcUnpack`/`NoSrcBang`), warnings keys, and the **primop name
set** (`PrimOpKind::serial_name` ↔ the names Translate.hs emits — ~86/230
covered, growing; an unknown name is a loud read error, but a *renamed* one
on either side is exactly hand-mirroring drift).

## 3. Options

Criteria per the task: drift-catching power, migration cost, TPLR
version-header story, maintenance burden, extract-build impact.

### (a) Cross-language golden roundtrip contract suite — RECOMMENDED

Haskell encodes a canonical corpus; the golden bytes are committed; CI on the
Haskell side asserts `encode(corpus) == golden` (encoder can't move alone),
and on the Rust side asserts `decode(golden)` succeeds, matches expected
structure, and — for the expression payload — `re-encode == golden`
byte-for-byte (reader *and* the harness writer can't move alone).

- **Drift-catching**: byte-level, both directions, and — uniquely among the
  three — it *pins the tolerance paths*, because old-format goldens stay in
  the corpus forever (append-only). Legacy headerless, 5/6/7 entries,
  optional warnings keys each get a fixture. Coverage staleness (new shape
  never added to corpus) is closed structurally: the census test matches
  exhaustively on `CoreFrame`/`Literal`/`AltCon`, so adding a variant breaks
  the test's *compilation* until the corpus decision is made in the same
  commit. The primop vocabulary gets the same treatment: a corpus fixture per
  emitted primop name.
- **Migration cost**: near zero. The proof below is most of the Rust side; the
  Haskell side reuses the existing fixture pipeline (`tidepool-extract-bin
  --output-dir`, per haskell/CLAUDE.md) plus one golden-compare test in the
  existing suite.
- **Version-header story**: the goldens pin the exact header bytes. A minor
  bump = add new-version goldens, keep the old ones (they now test
  minor-forward acceptance). A major bump = old goldens move to an
  expect-rejection test. The 1.1-vs-doc drift found above would have been
  impossible to ship: the golden regeneration diff *is* the review artifact.
- **Maintenance**: add a fixture when adding a shape — forced by the census,
  not by discipline.
- **Extract-build impact**: none. Golden regeneration uses the existing
  extract binary out-of-band; CI only byte-compares committed files.
- **Honest limit**: it does not dedup code — three hand-written codecs remain.
  It converts silent drift into a red CI job, which is the actual failure
  mode that shipped.

### (b) Schema DSL generating encoder + decoder on both sides

- **Drift-catching**: perfect *within the generated region* — but the format's
  hard parts are exactly what a generator can't own: the asymmetric reader
  tolerance (legacy headerless, 5/6/7 entries, `""`-placeholder, unknown-key
  skip) is version-*history*, not schema; and the primop name set lives in
  `define_primops!`/Translate.hs, outside any wire schema. Those seams stay
  hand-written and still need (a) to be safe. Generated-file staleness is
  itself a new drift channel unless CI diffs regenerated output — which is a
  golden contract check again, applied to source text instead of wire bytes.
- **Migration cost**: highest by far — design a DSL with
  optionality/version-gate semantics, write two generators, keep three
  artifacts (schema + 2 codegen paths) for a format with *two* payload types
  that changes a few times a year.
- **Version-header story**: must be modeled in the DSL (per-field
  since-version annotations) — most of the DSL's complexity budget goes here.
- **Extract-build impact**: worst. Generated Haskell must be committed (nix
  builds see only tracked files) or generated in-build (TH is a no-go given
  the in-process extract GHC constraints), plus a staleness check.
- Verdict: the machinery-to-surface ratio is wrong at this format's size.
  Revisit only if the wire grows many payload types.

### (c) Rust-as-source: generate CborEncode.hs from the write.rs shapes

- **The precedent doesn't transfer.** The `CoreRecord` derive renders small
  Haskell *record decl strings* at macro-expansion time and injects them at
  *runtime* into generated eval modules (inventory registry →
  `haskell_decl()`). It never touches the extract build. Generating
  `CborEncode.hs` is a different regime: committed generated source in
  `haskell/src/`, a CI staleness diff, and a build-order inversion (a Rust
  crate emitting a source file the Haskell toolchain consumes).
- **Which source?** `write.rs` is not a schema — it's hand-written `ciborium`
  code. To generate Haskell from it you'd first restructure it around a
  declarative shape table… at which point you've built (b) with Rust as the
  DSL host, inheriting (b)'s costs minus one generator.
- **The source and the mirror disagree today.** Rust's writer emits 5/6/7
  meta entries and drops warnings; Haskell's always emits 7 and carries
  `captured_type`/`var_names`. Generating Haskell from the Rust shapes would
  silently crown the *test-only* writer — the one with no production callers
  and the known warnings gap — as canonical, changing shipped bytes as a side
  effect of a refactor. Backwards: production truth flows Haskell → Rust.
- **Decoder still drifts**: read.rs's tolerance logic resists generation for
  the same reasons as in (b), so (c) covers only the encoder half.

## 4. Recommendation

**(a).** The corpus-as-contract *is* the single source of truth — not of
code, but of the thing that actually matters at this boundary: the bytes.
Both sides are machine-checked against one committed artifact; neither can
move alone; the tolerance history stays regression-tested because old goldens
never leave. (b) and (c) both still require (a) for their hand-written seams,
so (a) is the load-bearing mechanism, not a hedge — and it's the only option
whose failure mode list doesn't include "generator normalized away a
tolerance path we ship against."

The proof also settled the one empirical risk: canonical integer encoding.
cborg and ciborium agree byte-for-byte on all 78 real fixtures, so the strong
(byte-equality) form of the contract is available for the expression payload,
not just semantic compare.

**Follow-on work if adopted** (small, ordered):
1. Generate a *current-format* corpus (headered, 7-element meta, all literal
   kinds, warnings keys populated, one fixture per emitted primop name) via
   the existing fixture pipeline; commit alongside — not instead of — the
   legacy fixtures. Census gaps today: `LitByteArray`, `LitFloat`,
   `LitDouble` (frames and altcons are already fully covered).
2. Add the Haskell-side golden-compare test to the existing suite.
3. Fix the findings this spike surfaced: `write_metadata` should accept
   `MetaWarnings` and emit the always-7 shape (writer/writer parity → meta
   byte-equality too), and correct the 1.0 version claim in
   `tidepool-repr/CLAUDE.md`.

## 5. Proof artifact

`tidepool-repr/tests/golden_wire_contract.rs` — runs with
`cargo test -p tidepool-repr --test golden_wire_contract`. Four tests, all
green:

| Test | Result |
|---|---|
| `tree_fixtures_roundtrip_byte_identically` | 78/78 fixtures: decode → Rust re-encode → **byte-identical** payload → re-decode equal |
| `meta_fixtures_roundtrip_semantically` | 2/2 meta fixtures semantic-equal (byte parity blocked by the known writer gap, §1.2) |
| `header_and_legacy_paths_agree` | current-header and legacy decode of the same payload agree |
| `corpus_shape_census` | exhaustive-match census: all 11 frames + 3 altcons covered; literal gaps reported; adding a variant breaks compilation until the corpus is extended |

This spike is doc + proof only; the test rides the spike branch as the
skeleton for follow-on 2 above, not as a production merge.
