# Form docs: haddock comments → ask-form help text

**Status: PARKED (operator decision, 2026-08-20) — the prompting fix ships
instead: model-initiated asks are taught to precede every `askUser` with a
note that names each field and what a good answer looks like (the note
renders directly above the form on the timeline), and the two
driver-initiated asks (seed gate, between-turns gate) already carry
hardcoded presentation text. Revive this design only if dogfooding shows
notes don't reliably carry field semantics — the machinery below is the
static-baseline insurance, and its one risky lane (extract) shouldn't be
built speculatively.**

## Problem

`askUser @T` derives its form purely from `T`'s `Generic` representation, so
the operator sees field names and nothing else — "Iterations", no statement
of what it governs or what a sane value is. Both dogfood streams hit this:
the human operator ("idk if I understand what it's asking"), and the
zero-context probe ("no indication anywhere of what Iterations controls…
I picked plausible-sounding values with no way to validate them").

The authoring ideal (operator decision, 2026-08-19): **haddock-style
comments on the answer types are the single source of docs**, picked up by
machinery and carried to the form as annotations. Nobody maintains a second
docs artifact; documenting the type documents the form.

## Why not pure `Generic` pickup

GHC's `Generic` metadata (`MetaData`/`MetaCons`/`MetaSel`) does not carry
haddock comments — they are simply not present in the rep type. The two ways
to smuggle docs through Generics both fail our constraints:

- **Type-level annotation** (`field :: Doc "governs the loop budget" Int`,
  `KnownSymbol` read by `GForm`): works today, but taxes every author type —
  wrapped field types leak into `FromJSON`/record-dot/eval ergonomics, which
  violates "the API is the prompt". Docs would also live inside the type
  expression instead of as comments. Rejected.
- **Template Haskell `getDoc`**: reads real haddocks, but injects TH into
  extract-compiled author modules — a new, risky obligation on the one
  compile pipeline everything shares. Rejected.

## Design: extract-time docs table, present-time join

The machinery that already parses author modules with the real GHC is
`tidepool-extract`. Haddock docs are available to it: compiling with
`-haddock` makes GHC retain doc comments (GHC ≥9.2 stores them in the
interface as `mi_docs :: Maybe Docs`, with per-decl and per-binder maps —
and 9.12 is our toolchain). So:

```
HarnessTypes.hs          tidepool-extract            driver               web
  -- | One steering        --form-docs mode:           enrich FormShape     render help
  --   reply…               parse author modules  →    with docs at    →    text under
  data OperatorSteering     with -haddock, emit        present_form         labels
    = …                     formdocs.json (names →     (a name join,
                            doc text)                  Rust-side)
```

1. **`tidepool-extract --form-docs <module>...`** — a new invocation mode:
   parse the given author modules with `-haddock`, walk declarations, emit
   one JSON table:

   ```json
   { "OperatorSteering": {
       "doc": "A steering reply the operator sends mid-run.",
       "constructors": {
         "OperatorSteering": {
           "doc": null,
           "fields": { "steeringReply": "Free-text guidance…" } } } } }
   ```

   Keyed by NAME (type/constructor/field), exactly the keys `FormShape`
   already carries (`type_key`/`constructor`/`FieldKey`) — the join needs no
   new identity. Scoped per module; a cross-module name collision is
   last-wins with a loud extract warning (author-module sets are small).

   This runs ONCE per harness boot, not per turn — it never touches the
   turn-compile path or the compile memo. The one `tidepool-extract`
   invocation builder (`tidepool-extract-cmd`) grows the typed arg.

2. **Driver-side enrichment** (`tidepool-harness`): at boot the driver runs
   the docs extraction over `HarnessSource::answerer_imports` (the same
   structurally-derived module set the answer contract uses) and holds a
   `FormDocs` table. `present_askuser_form` (and the outer-loop variant)
   enriches the decoded `FormShape` — filling `doc` fields by name lookup —
   before the observer event and the gate call, so the web page, the
   form-api, and `FormPresented` in the transcript all see the same
   annotated shape. Extraction failure is non-fatal (warn, present bare
   forms): docs are presentation, never a gate on operation.

3. **Wire change, Rust side only**: `FieldShape` and `VariantShape` gain
   `doc: Option<String>`; `Product`/`Sum` gain `type_doc: Option<String>`.
   All `#[serde(default)]` + skip-if-none, so the HASKELL encoder
   (`Tidepool.Form.Wire`) is untouched — the Haskell side never knows docs
   exist, and every existing wire fixture (`generic_form_wire.rs`) stays
   byte-identical. Enrichment happens after decode, before presentation.

4. **Rendering** (`tidepool-web`): `type_doc` as a one-line subtitle under
   the form's title; field `doc` as micro help text under the humanized
   label; variant `doc` beside its radio option. All maud-escaped text
   nodes (model-authored? no — author-authored, but same discipline). The
   form-api `GET` serializes the enriched shape, so agent operators get the
   docs too — closing the probe's "no way to validate semantics" finding.

5. **Author docs** (`harness-dogfooding`): write real haddocks on
   `SeedQuestion`, `OperatorSteering`, `LayerApproval`, `ContinueSignal`'s
   web-side presentation strings, and the demo bin's sample types — the
   immediate payoff and the live acceptance.

## What deliberately does NOT change

- The Haskell form wire and `Tidepool.Form` derivation — zero author-facing
  API change; a type with no haddocks renders exactly as today.
- The turn-compile path, the compile memo, `asks.json`.
- The frozen `OperatorGate` contract (the enriched shape is still a
  `FormShape`).

## Spike first (the one real unknown)

Whether `-haddock` doc retention behaves under extract's exact GHC session
config (fat interfaces, no TH). Two candidate mechanisms, try in order:
(a) read `mi_docs` off the compiled interface; (b) take docs straight off
the parsed AST (`HsDocString`s are present in the parse tree under
`Opt_Haddock` — no compilation needed at all, and `--form-docs` only needs
parsing). (b) is likely sufficient and cheaper — a parse-only mode that
never typechecks. Timebox the spike; its output decides which arm the
extract lane implements.

## Lanes

1. **extract** (Haskell): the spike, then `--form-docs` + JSON emission +
   unit fixtures (documented module → expected table). GHC-heavy tests.
2. **harness** (Rust): `FormDocs` load at boot + enrichment in
   `present_askuser_form` + the additive `FormShape` fields + a fixture
   acceptance (documented ask type → `FormPresented` carries docs).
3. **web** (Rust, small): render the three doc slots + form-api passthrough
   + render tests. Sonnet-sized.
4. **dogfood** (Haskell, small): haddocks on the companion's form types;
   verify live.

1 → 2 → (3 ∥ 4). Lane 1 is the only one with real uncertainty.

## Verification

- Extract unit: documented fixture module → exact JSON table.
- Harness: `scripts/battery.sh -p tidepool-harness -E 'test(<docs acceptance>)'`
  — an `askUser @DocumentedType` fixture whose `FormPresented` event and
  gate-visible shape carry the field doc.
- Web: render unit tests + form-api integration (docs in `GET` output).
- Live: the companion's steering ask shows help text under its field.
