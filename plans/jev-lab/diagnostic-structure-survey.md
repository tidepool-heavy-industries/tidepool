# Diagnostic structure survey: where it's built, where it's lost

Scope: read-only trace of how a GHC diagnostic travels from the compiler to
the text a model reads, across three paths (cell compile, inspection/`lookup`
query, and a declaration/workspace-module candidate compile). No fix is
proposed here. Every claim below cites `file:line` in
`/home/inanna/dev/tidepool-jev`. Where I could not pin something down, it is
called out under Open questions rather than guessed.

## 0. The one structured wire format that exists

`haskell/src/Tidepool/DiagJson.hs` is the only place a GHC diagnostic is held
as data before being turned into a display string, and the only serializer
that keeps `(span, severity, message)` as three separate fields:

- `Diag` (`haskell/src/Tidepool/DiagJson.hs:47-53`): `dFile :: Maybe (String,
  Int, Int, Int, Int)` (file, startLine, startCol, endLine, endCol),
  `dSeverity :: DiagSeverity`, `dMessage :: String`.
- `diagsFromSourceError` (`DiagJson.hs:57-58`) walks a caught GHC
  `SourceError`'s `MsgEnvelope`s in order.
- `envelopeToDiag` (`DiagJson.hs:60-66`) is **the hop where GHC's structured
  diagnostic becomes an opaque string**: `dMessage = renderWithContext ctx
  (formatBulleted (diagnosticMessage diagOpts (errMsgDiagnostic env)))`. GHC's
  `errMsgDiagnostic` is a real `GhcMessage` value (the `Diagnostic` typeclass
  exposes a `diagnosticCode`, hints, and reason as separate accessors in this
  GHC version), but nothing here calls `diagnosticCode` or any other
  structured accessor — `grep -rn "diagnosticCode" haskell` returns nothing.
  Only `spanOf` (`DiagJson.hs:100-103`) and `severityOf` (`DiagJson.hs:105-108`)
  extract anything besides the rendered text.
- `renderDiagsJson` / `renderDiag` / `renderSpan` (`DiagJson.hs:118-146`)
  hand-serialize `[Diag]` into a fixed JSON shape:
  `{"version":2,"outcome":...,"diagnostics":[{"span":{...}|null,"severity":...,"message":...}]}`.
  This is consumed on the Rust side and is the **only** hop in the whole
  system where Rust receives span+severity as typed fields rather than
  embedded text.

This JSON report is used by `reportDiags` (`haskell/app/Main.hs:284-310`),
which is the epilogue for every dispatch arm **except** `runInspectionMode`
(see path B). It's also where a non-`SourceError` exception path loses even
the span: `diagFromException` (`DiagJson.hs:113-114`) sets `dFile = Nothing`
and `dMessage = show e`.

GHC error codes (`GHC-76037`, etc.): the code text is never extracted as a
field. It reaches the model, when it reaches the model at all, only because
GHC's own `diagnosticMessage`/`formatBulleted` rendering apparently embeds
`[GHC-NNNNN]` textually inside the message body — confirmed empirically by
the literal fixture string at `tidepool-runtime/src/session/turn.rs:2969`
(`"<cell>:1:1: error: [GHC-76037]\n    Not in scope: ..."`), not traced
further into GHC's own source. `.shoal/Project/Reflex.hs:159` and
`plans/jev/reflex_table.json:279` match on this bracketed code by scanning
rendered text (`Has "GHC-76037"`), not by reading a structured field —
because no structured field exists downstream of `DiagJson.hs`.

## 1. Path A — cell compile rejection

```
GHC SourceError
  → diagsFromSourceError / envelopeToDiag      (Haskell)  DiagJson.hs:57-66   [span+severity kept, message flattened]
  → renderDiagsJson                            (Haskell)  DiagJson.hs:118-146 [JSON wire: span+severity+message]
  → decode_report / ExtractReport              (Rust)     tidepool-extract-report/src/lib.rs:100-121 [span+severity+message, TYPED]
  → decode_extract_result → CompileError::Diagnostics(Vec<ExtractDiag>)
                                                (Rust)     tidepool-toolchain/src/diag.rs:32-71, tidepool-toolchain/src/lib.rs:53-54
                                                            [STILL structured: Vec<ExtractDiagnostic>]
  → render_cell_compile_error → render_diagnostics
                                                (Rust)     tidepool-runtime/src/session/turn.rs:1147-1164
                                                            tidepool-toolchain/src/diag.rs:144-217
                                                            [FLATTENED to one String; severity becomes literal
                                                             text "error:"/"warning:"; span becomes a text
                                                             coordinate, not data]
  → advice_for / with_advice (string pattern matching on the flattened text,
    e.g. message.contains("ZonkAny"))            (Rust)   turn.rs:1186-1251
  → WorkbenchItemReceipt { output: String, warnings: Vec<String>, span: Option<CellSourceSpan> }
                                                (Rust)     tidepool-runtime/src/session/workbench.rs:317-330
  → model (serialized JSON tool response)
```

Call sites building the receipt from the rendered string:
`tidepool-actor/src/resident_workbench.rs:132-141` (per-diagnostic loop,
still walking `diagnostic.span`/`diagnostic.severity` from the *structured*
`ExtractDiag` to decide which item it belongs to, then discarding that
structure into `rendered: String` at line 133-136), and
`tidepool-actor/src/resident_actor.rs:7111`, `:7128`.

Key nuance: `WorkbenchItemReceipt.span` (`workbench.rs:322`,
`CellSourceSpan` defined at `tidepool-runtime/src/session/turn.rs:103-108`)
is the **item's own** author-relative source region (from cell
classification), not the diagnostic's GHC span. It survives to the model
today, but it answers "which cell item" rather than "which exact span the
diagnostic pointed at" or "which of several diagnostics on that item."

**Where structure is lost:** span+severity survive from GHC all the way to
`CompileError::Diagnostics` in Rust (structured the whole way — this is
cheap, it's already there). The loss happens entirely on the Rust side, at
`render_diagnostics` (`tidepool-toolchain/src/diag.rs:144`), which is called
from `render_cell_compile_error` (`turn.rs:1147`). Everything downstream of
that call operates on `String`.

## 2. Path B — inspection / `lookup` query

```
GHC SourceError (caught inside runInspectionMode's own `try`, never escapes
  the process, so `reportDiags`/`renderDiagsJson` never run for this case)
  → renderInspectionDiagnostics                (Haskell)  haskell/app/Main.hs:255-259
       = intercalate "\n" . map render . diagsFromSourceError
       render d = location d ++ dMessage d
       location uses the RAW `dFile` path with no anchor, no relativization,
       and DROPS severity entirely (never shown at all on this path).
       [FLATTENED HERE — earliest flattening point in the whole system,
        and the only one that happens before any wire encoding at all]
  → InspectionRejected String                  (Haskell)  haskell/src/Tidepool/Introspection.hs:88
       constructed at Main.hs:238-246 (inside runInspectionMode, per query)
  → encodeInspectionResults (CBOR, "TPINSP005" batch)
                                                (Haskell)  Introspection.hs:859-860
       encodeListLen 2 <> encodeString "Rejected" <> text diagnostic
       [nothing left to lose — already a bare string]
  → decode_inspection_result, "Rejected" arm    (Rust)     tidepool-runtime/src/session/inspection.rs:555-558
       InspectionResult::Rejected { diagnostic: String }
  → lookup_response, PreparedLookupKind::Rejected arm
                                                (Rust)     tidepool-actor/src/resident_actor.rs:7267-7276
  → LookupResult { outcome: LookupOutcome::Rejected { diagnostic: String } }
                                                (Rust)     tidepool-actor/src/lookup_tool.rs:48, 298-300
  → model
```

This path **never touches** `tidepool-extract-report`'s
`ExtractDiagnostic`/`DiagnosticSpan` types or `renderDiagsJson`'s JSON wire
format at all — it is a structurally separate channel from Path A, built on
the older/private `TPINSP005` CBOR batch. This confirms the task's premise:
"the INSPECTION path flattens inside Haskell before Rust ever sees
structure" — Rust here receives a `String` it never had a chance to keep
structured, because `renderInspectionDiagnostics` (Main.hs:255) discards span
component-wise and severity entirely before any encoding step exists.

This is also the exact mechanism behind the known scratch-path defect: `dFile`
carries GHC's raw compiled-scratch path (e.g.
`/tmp/nix-shell.XXX/.tmpYYY/query-5/Expr.hs`) and `location` (Main.hs:257-259)
prints it verbatim — there is no `RenderOpts.anchor`/`path_ends_with_anchor`
remap on this path the way Path A gets from `tidepool-toolchain/src/diag.rs`
(`render_diagnostics`, `path_ends_with_anchor` at `diag.rs:219-227`). Path A
was built with that remap; Path B was not.

Contrast — prior art for a genuinely structured error already exists one
branch over on the *same* file: `StructuredQueryError`
(`haskell/src/Tidepool/Introspection.hs:195-200`, encoded via
`encodeStructuredError` and mirrored in Rust as `QueryError`,
`tidepool-runtime/src/session/inspection.rs:150-155`) carries `Unknown`,
`Ambiguous`, `UnknownModule`, `Unsupported` as real variants with typed
payloads (`NameQuery`, `Vec<IdentifierRef>`), decoded via `decode_query_error`
(`inspection.rs:666`). That machinery covers "query shape" failures (unknown
identifier, ambiguous name) for the *structured* inspection queries
(`InspectionQuery::StructuredInfo`/`StructuredType`). It does **not** cover
GHC compile-rejection (`InspectionRejected`) — that always stays a bare
`String`, on both sides. So the codebase has already solved "keep structure
and let an unknown case stay representable" for one failure class on this
exact boundary; it just hasn't been applied to the GHC-diagnostic case.

## 3. Path C — declaration / workspace-module candidate compile

This is the compile of a whole candidate generated module (a session's
`.tidepool`-scope declaration, e.g. something a cell's `define_scoped_with_imports_in`
call installs into project/library scope) rather than a single cell wrapper.
It goes through the **same** `reportDiags`/`renderDiagsJson` JSON report as
Path A (it's dispatched via the ordinary extractor one-shot/target compile in
`haskell/app/Main.hs`, not `runInspectionMode`), so it starts identically
structured:

```
GHC SourceError → DiagJson → JSON report → decode_report → CompileError::Diagnostics
  (same as Path A, steps 1-3)
  → SessionError::ValidationFailed(DeclarationValidationFailure)
                                                (Rust)  tidepool-runtime/src/session/mod.rs:207-232
       DeclarationValidationFailure { diagnostics: Vec<ExtractDiag>, anchor, line_offset, source }
       [STILL structured — a private field, not yet rendered]
  → .render(label) / .render_for_input(label, input) → render_with_offset → render_diagnostics
                                                (Rust)  tidepool-runtime/src/session/mod.rs:176-219
                                                        tidepool-toolchain/src/diag.rs:144
                                                        [FLATTENED to String — same function as Path A]
  → Display for DeclarationValidationFailure   (Rust)  mod.rs:222-225 (uses .render, generic label)
    or explicit .render_for_input call at the one call site below
  → ResidentWorkbenchStep::Rejected(String)     (Rust)  tidepool-actor/src/resident_workbench.rs:2652-2655
  → WorkbenchItemReceipt.output: String         (Rust)  same terminal shape as Path A
                                                        (workbench.rs:319-330)
  → model
```

The only call site found for `SessionError::ValidationFailed` is
`tidepool-actor/src/resident_workbench.rs:2652-2655`, which calls
`failure.render_for_input(&format!("<cell item {}>", block.ordinal),
&block.source)` — i.e., it is folded into the identical `<cell item N>`
receipt shape Path A uses. **Paths A and C converge on the same terminal Rust
type** (`WorkbenchItemReceipt`) even though they are two different GHC
compile invocations (a cell wrapper vs. a whole generated module). The
flattening hop is literally the same function, `render_diagnostics`
(`tidepool-toolchain/src/diag.rs:144`), called from two different sites
(`turn.rs:1147` for Path A, `mod.rs:176`/`:183` for Path C).

Two more call sites of `render_diagnostics`/the same structured-then-flattened
shape exist and are worth naming even though they were not asked for by
number: `render_turn_compile_error` for the turn/eval path
(`tidepool-runtime/src/session/turn.rs:1111-1145`, used by
`tidepool/src/actor_host.rs:1985` for the Shoal root driver and by
`tidepool-harness/src/harness.rs:398-469` for authored-harness turns) — same
shape, same loss point, different consumer crates.

## 4. A fourth, more severe case: `lib_isolate.rs` discards diagnostics entirely

Not one of the three requested paths, but relevant to "what survives": when
`tidepool-mcp/src/lib_isolate.rs`'s `probe_import`
(`tidepool-mcp/src/lib_isolate.rs:130-141`) compile-probes a project
`.tidepool/lib` module to find which re-exported module broke the `Library`
facade, it only keeps `Result<(), CompileError>` — `.map(|_| ())` at line 140
— and the caller (`compute_layer`, lines 76-124) branches only on `is_ok()`.
The actual GHC diagnostic for the broken module is never rendered or
surfaced anywhere; the model instead receives a generic `brick_note`
(`lib_isolate.rs:207-214`, `"...a project library module failed to compile
and was EXCLUDED...: {module names}. Fix or remove the module..."`) that
names the broken module but carries none of GHC's own explanation. This is
not a flattening-to-string defect like paths A/B/C — it's a point where even
the rendered text is thrown away, one level earlier than "flattened."

## Field survival table

| Hop | span (file) | span (line/col) | severity | message | GHC code (e.g. `GHC-76037`) |
|---|---|---|---|---|---|
| GHC `MsgEnvelope GhcMessage` (in-process) | yes (`errMsgSpan`) | yes | yes (`errMsgSeverity`) | yes (structured `GhcMessage`) | yes, via `diagnosticCode` (unused) |
| `Diag` (`DiagJson.hs:47-53`) | yes (`dFile` tuple) | yes | yes (`DiagSeverity`) | yes, but pre-rendered to `String` at `envelopeToDiag` (`DiagJson.hs:60-66`) | only if embedded in the rendered text |
| JSON report (`renderDiagsJson`, `DiagJson.hs:118-146`) | yes | yes | yes (`"error"`/`"warning"`) | yes (opaque string) | only if embedded in the string |
| `ExtractDiagnostic` (Rust, `tidepool-extract-report/src/lib.rs:58-65`) | yes, typed `DiagnosticSpan` | yes, typed `u32` fields | yes, typed enum | yes, `String` | only if embedded in the string |
| `CompileError::Diagnostics(Vec<ExtractDiag>)` (Rust, `tidepool-toolchain/src/lib.rs:53-54`) | yes | yes | yes | yes | only if embedded |
| `render_diagnostics` output (Rust, `diag.rs:144`) | folded into text coordinate | folded into text coordinate | folded into literal word in text | yes, as prose | only if embedded, now unlabeled prose |
| `WorkbenchItemReceipt` (Rust, `workbench.rs:319-330`) | no (only the unrelated item-level `CellSourceSpan`) | no | no (only via `status: WorkbenchItemStatus`, item-granularity not diagnostic-granularity) | yes, in `output`/`warnings: Vec<String>` | no |
| Path B: `renderInspectionDiagnostics` output (Haskell, `Main.hs:255-259`) | folded into text, RAW path (no anchor remap) | folded into text | **dropped entirely**, never shown | yes, as prose | only if embedded |
| Path B: `LookupOutcome::Rejected` (Rust, `lookup_tool.rs:298-300`) | no | no | no | yes, `diagnostic: String` | no |

## Ordered change list

Each entry names the one hop, whether it needs an extractor/Haskell change
(expensive: `just fixtures-check`/`just fixtures-update`, see
`haskell/CLAUDE.md`) or is Rust-only (cheap: no wire/schema change to the
extractor binary).

1. **Extractor/Haskell.** `envelopeToDiag`, `haskell/src/Tidepool/DiagJson.hs:60-66`
   — call GHC's `diagnosticCode` (or equivalent structured accessor) on
   `errMsgDiagnostic env` and add a field to `Diag` for it, alongside the
   existing rendered `dMessage`. This is the only hop where the GHC error
   code could be captured as data instead of hoped-for embedded text.
   Requires changing `Diag`'s shape, `renderDiagsJson`'s JSON shape
   (`DiagJson.hs:118-146`), and `tidepool-extract-report`'s `WireReport`
   (`tidepool-extract-report/src/lib.rs:76-81`) in lockstep — a wire version
   bump (`REPORT_VERSION`, `lib.rs:11`), which is the expensive,
   fixtures-touching side of this list.

2. **Extractor/Haskell.** `renderInspectionDiagnostics`,
   `haskell/app/Main.hs:255-259` — stop building a `String` at all; route
   inspection-path rejections through the same `Diag`/JSON-report shape Path
   A already uses (or at minimum reuse `diagsFromSourceError`'s output
   un-rendered), so `InspectionRejected` can carry structure. This also
   requires changing `InspectionRejected String`
   (`haskell/src/Tidepool/Introspection.hs:88`) and its CBOR encoding
   (`Introspection.hs:859-860`) to a structured shape (a list of `Diag`-like
   records), which is the same class of expensive, fixtures-touching change
   as #1 — it changes what the extractor emits on the wire.

3. **Rust-only.** `decode_inspection_result`'s `"Rejected"` arm,
   `tidepool-runtime/src/session/inspection.rs:555-558` — once #2 lands,
   decode into a structured `InspectionResult::Rejected` variant carrying
   `Vec<ExtractDiag>`-shaped data instead of `String`. Cheap once the wire
   carries the data.

4. **Rust-only.** `LookupOutcome::Rejected`, `tidepool-actor/src/lookup_tool.rs:298-300`
   — change `diagnostic: String` to a structured field (e.g. `Vec<ExtractDiag>`
   or a project-local wrapper around it) and update the one construction
   site, `tidepool-actor/src/resident_actor.rs:7274-7276`, plus its two
   `Rejected(...)` construction sites via `PreparedLookupKind::Rejected`
   (`tidepool-actor/src/lookup_tool.rs:130-155`, which build `String`s
   locally for non-GHC rejections like "empty lookup query" — those stay
   strings; only the GHC-diagnostic-sourced case changes).

5. **Rust-only.** `WorkbenchItemReceipt`, `tidepool-runtime/src/session/workbench.rs:319-330`
   — add a structured diagnostics field (e.g. `diagnostics: Vec<ExtractDiag>`
   or a public re-export of it) alongside the existing `output: String`,
   rather than replacing `output` (the constraint given: keep the rendered
   text *alongside* the structure). This is the actual model-facing receipt
   type for Path A and Path C, so this is the single highest-leverage
   Rust-only hop — see summary below.

6. **Rust-only.** `render_cell_compile_error` / `render_turn_compile_error`,
   `tidepool-runtime/src/session/turn.rs:1111-1164` — currently return only
   `String`. Would need to also return (or take a caller-supplied out
   parameter for) the `Vec<ExtractDiag>` they already have in hand before
   calling `render_diagnostics` — no new data, just stop discarding what's
   already a local variable (`diagnostics` at `turn.rs:1148`/`:1128`).

7. **Rust-only.** `DeclarationValidationFailure`, `tidepool-runtime/src/session/mod.rs:166-171`
   — the `diagnostics: Vec<crate::diag::ExtractDiag>` field already exists
   privately; it would need a public accessor so
   `resident_workbench.rs:2652-2655` can propagate it into the receipt
   change from #5 instead of only calling `.render_for_input(...)`.

8. **Rust-only, separate defect, not structure-loss.** `probe_import`,
   `tidepool-mcp/src/lib_isolate.rs:130-141` — currently discards the
   `CompileError` outright (`.map(|_| ())`). Any fix here is "stop
   discarding" rather than "stop flattening"; it doesn't have upstream
   structure to preserve until #1-#4 also land, since today all it gets back
   is a `CompileError` whose `Diagnostics` variant is already the flattened
   consumer of `render_diagnostics` by the time anyone looks at it in this
   file (it currently never even calls `render_diagnostics` — it just drops
   the error).

9. **Not attempted here, flagged as likely infeasible without deeper
   surgery.** GHC's suggested-fix hints (`errMsgDiagnostic`'s `diagnosticHints`
   in GHC's `Diagnostic` class) are not read anywhere in
   `haskell/src/Tidepool/DiagJson.hs` at all — not even as embedded text,
   since `formatBulleted (diagnosticMessage diagOpts ...)` is what produces
   `dMessage`, and whether GHC's own bulleted rendering includes hint text
   inline was not confirmed by reading GHC's source in this survey (see Open
   questions). If hints are not already inline in `dMessage`, capturing them
   as a "suggested edits" field is an **extractor/Haskell** change of the
   same shape as #1 (add a field, bump the wire version, touch fixtures).

## Tests that pin current (string) behaviour and would have to move

All of these assert on the *rendered text*, not on structured fields — they
would need to become assertions against whatever structured type replaces
`String`, or be kept as regression tests of the rendering function itself
(which should keep existing as the "keep the original rendered text
alongside the structure" requirement implies `render_diagnostics` stays, just
stops being the *only* output):

- `tidepool-toolchain/src/diag.rs` — the bulk of the render-format contract:
  `:751-752`, `:788`, `:835-840`, `:880-882`, `:923`, `:966`, `:1008-1009`,
  `:1029-1030`, `:1051`, `:1091`, `:1155`, `:1189`, `:1223-1225`, `:1259`
  (all `assert!(got.contains(...))`/`assert!(got.starts_with(...))` against
  `render_diagnostics`'s `String` output).
- `tidepool-toolchain/src/diag.rs:641-654`, `:718-719` — assert on the
  *decoded* `ExtractReport`/`ExtractDiagnostic` structured fields directly
  (`report.diagnostics[0].severity`, `.span.file`, `.span.start_line`) — these
  ones do NOT need to move; they're already asserting on structure and are
  evidence the structured type is already test-covered up to the point where
  `render_diagnostics` consumes it.
- `tidepool-runtime/src/session/turn.rs` — `render_cell_compile_error`/
  `render_turn_compile_error`/`advice_for` string-content assertions:
  `:3010-3020`, `:3081-3114`, `:3131-3160`, `:3169-3170`, `:3208-3221`,
  `:3238-3252`, `:3265-3313`, `:3332-3379` (`assert!(rendered.contains(...))`,
  `assert!(rendered.ends_with(...))`).
- `tidepool-actor/src/resident_workbench.rs` — roughly 25 assertions
  (`grep -c` count) against `ResidentWorkbenchStep::Rejected`/receipt
  `output` string content; not individually enumerated here but concentrated
  around the `render_cell_compile_error`/`render_turn_compile_error` call
  sites already cited (`resident_workbench.rs:150`, `:2220`, `:2652-2655`,
  `:6383`, `:7267`, `:7303`).
- `tidepool-actor/src/lookup_tool.rs` — roughly 33 assertions on
  `LookupOutcome`/`PreparedLookupKind` content, including the `Rejected(_)`
  match-arm tests at `:467-468`, `:518`, `:529`, `:553`.
- No Haskell-side tests were found pinning `renderInspectionDiagnostics` or
  `DiagJson` output — `find haskell -iname '*.hs' | xargs grep -l
  "renderInspectionDiagnostics\|DiagJson"` returns only
  `haskell/app/Main.hs` (the call site) and `haskell/src/Tidepool/DiagJson.hs`
  itself (the definition); no test file references either name.

## Task 4 — is any structured diagnostic reachable from inside a Haskell cell?

No. `find haskell -iname '*Diagnos*'` finds only `DiagJson.hs` itself (an
extractor-internal module, not part of the Tidepool stdlib surface a cell
imports), and `grep -rln "Diagnostic" haskell/stdlib` returns nothing. A
cell's own failure reaches the model only as the rendered `String` in
`WorkbenchItemReceipt.output` (Path A) — there is no value of any diagnostic
type a running Haskell program (e.g. a harness or `Tidepool.Async` worker)
could construct, inspect, or pass around. Reported plainly as requested: none
exists.

## Open questions

- Whether GHC's `diagnosticMessage`/`formatBulleted` (used at
  `haskell/src/Tidepool/DiagJson.hs:64-65`) already embeds the `[GHC-NNNNN]`
  code and/or hint text inline as part of the rendered `SDoc`, or whether
  the `[GHC-76037]` seen in the `turn.rs:2969` fixture is added by some other
  step not identified here. I could not resolve this without reading GHC's
  own `GHC.Types.Error`/`GHC.Driver.Errors` source for this exact GHC version
  (9.10/9.12 present under `/nix/store`, exact pinned version used by this
  repo's `haskell/` build not confirmed), and no local copy of GHC's source
  tree was found under the Nix store paths checked. This matters for change
  #1/#9: if the code and hints are already inline in `dMessage`, extracting
  them as separate fields is "parse GHC's own rendered text" (fragile) rather
  than "call a separate accessor" (robust) — the class of the Haskell-side
  change differs depending on the answer.
- Whether `rustc`-sourced diagnostics (`E0425` etc., referenced in
  `.shoal/Project/Reflex.hs:133-135` and `plans/jev/reflex_table.json:114`)
  go through any part of this pipeline at all, or are a wholly separate
  system (they read as cargo/rustc output, not GHC/Haskell). Not traced
  further — out of scope for a GHC-diagnostic survey, flagged so it isn't
  mistaken for a fourth path found and then dropped.
- Whether `tidepool-toolchain/src/failclass.rs`'s `classify_compile`/
  `FailureEnvelope` (used as the non-`Diagnostics` fallback in
  `render_cell_compile_error`, `turn.rs:1149`, and referenced at
  `tidepool-toolchain/src/failclass.rs:130`) holds any structure beyond
  `FailureClass`/`Phase`/`message: String` that would be relevant to
  "suggested edits" — read only far enough to confirm it's not itself an
  `ExtractDiag` consumer; not fully audited.
- I did not locate a distinct "compile a whole project/workspace `.hs` file
  and show the model the result directly" tool separate from the paths
  above. The task's third path is best matched here by declaration/session-
  lib candidate validation (`DeclarationValidationFailure`,
  `tidepool-runtime/src/session/mod.rs:166-232`), which compiles a whole
  generated module rather than a cell wrapper and converges on the same
  receipt type as Path A. The `lib_isolate.rs` project-library-facade probe
  (section 4 above) is a second candidate for "workspace/project module
  compile" but it discards diagnostics rather than flattening them, and its
  match against the task's phrasing is less exact. If a different, dedicated
  workspace-compile surface exists that this search missed, it wasn't found
  under `tidepool-mcp/src`, `tidepool-runtime/src`, or `tidepool-worktree/src`
  by name search for `compile`/`check`/`build`-shaped tool entry points.

## Summary: highest-value hop

If exactly one hop had to change first, it is **#5** —
`WorkbenchItemReceipt` at `tidepool-runtime/src/session/workbench.rs:319-330`
gaining a structured `diagnostics: Vec<ExtractDiag>`-shaped field alongside
the existing `output: String`. Reasons:

- It is Rust-only (no extractor/Haskell change, no `just fixtures-check`
  exposure) because `CompileError::Diagnostics(Vec<ExtractDiag>)` already
  carries `span`+`severity`+`message` in full at the point
  `render_cell_compile_error` and `render_turn_compile_error` are called
  (`turn.rs:1147`, `:1111`) — the data exists as a local variable seconds
  before it's thrown away by `render_diagnostics`. Nothing needs to be
  computed that isn't already computed.
- It is the **single terminal type** both Path A (cell) and Path C
  (declaration/workspace-module) already converge on
  (`resident_workbench.rs:2652-2655` feeds the same `Rejected(String)` →
  `WorkbenchItemReceipt.output` as the direct cell-diagnostics loop at
  `resident_workbench.rs:132-141`), so one change covers two of the three
  paths at once.
- It directly satisfies the original ask's two constraints: the rendered
  text (`output`) stays exactly as-is for anything that still wants a
  string, and per-diagnostic `span`/`severity`/`message` become addressable
  without re-parsing `output`. An unknown/future diagnostic shape stays
  representable because `ExtractDiagnostic` already has that shape
  (`span: Option<...>` — `None` is exactly "GHC gave no real span," not a
  parse failure).
- It does not by itself fix Path B (`lookup`/inspection) — that path's fix
  (items #2/#3/#4) is the extractor/Haskell-touching one and is more
  expensive. But #5 is where a model would see the benefit soonest, since
  cell-compile rejections are (per `plans/jev/addendum-A-B-2026-09-17.md`
  and the Reflex table's `GHC-76037` handling) the highest-volume diagnostic
  class already being pattern-matched against rendered text today.
