# Phase B — implementation contract

Companion to [`one-spawn-turn-protocol.md`](one-spawn-turn-protocol.md), which
stays the design authority. This file is the **shared contract** every Phase B
workstream implements against: the extract CLI after the swap, the Rust API
after the swap, and the four decisions that were left open by the protocol and
are settled here.

Read the protocol doc first. Everything below is additive to it, except where a
section says "supersedes".

---

## Decision 1 — the classify lane becomes a BATCH lane, not a per-turn lane

The protocol's spawn-count table already says this
(`repl block of N items | N classify + per-item compiles | 1 batch classify +
per-item compiles`); this records what it means concretely, because a reader
who only sees `--emit-stmt-binders is deleted` will expect no parse-only lane
to survive at all.

**The single-turn classify spawn is gone.** A turn that compiles gets its
verdict from the same `--turn` process that compiles it. `--emit-stmt-binders`,
the flag whose whole job was that per-turn spawn, is deleted.

**A block-level classify spawn replaces it**, because `tidepool-repl`'s
`run_block` cannot avoid needing verdicts before it runs anything:

- it segments the block into maximal runs of consecutive DECL-shaped items and
  elaborates each run as ONE generation (`define_scoped` over N texts), so a
  sig+binding pair or a mutual-recursion SCC split across items typechecks
  together. Discovering decl-ness lazily, item by item, would put each decl in
  its own generation and break that.
- a statement item may only be compiled AFTER the decls preceding it in the
  block are defined (it can reference them), so items cannot be eagerly
  compiled in a single forward pass to discover their kinds.

So `run_block` classifies its whole block in ONE spawn, then runs each item
with the verdict already in hand. Per turn that is one compile spawn; per block
it is one extra parse-only spawn regardless of N. Today the same block costs
one classify spawn per `Auto` item **plus** a second classify of the same text
inside `run_eval` — an `Auto` bind/expr item is classified TWICE per turn right
now. Both go away.

**Honest statement of the result**, to be repeated in the submit note rather
than rounded off: a harness turn goes 2 spawns → 1. A repl block of N items
goes (up to 2N + N compiles) → (1 + N compiles). A repl block of ONE item goes
3 spawns → 2. "One spawn per turn" is true of the compile lane everywhere and
of the harness end to end; the repl retains one parse-only spawn per BLOCK.

### Consequence for the stage vocabulary

`classify_extract` is retired as decided (arbitrated; see the protocol's
"The decision"). Its only emitter is `tidepool-harness/src/harness.rs`'s
`run_block`, and that call site is deleted — the harness genuinely stops making
a classify spawn. Retiring the constant is therefore not a bookkeeping choice,
it is the truth about the harness timeline.

`CLASSIFY_STAGE_PREFIX` / `classify_stage_name` **stay**. They name "the inside
of the parse-only classify lane", which still exists as the block lane, and the
repl forwards that spawn's phases under them. Only `STAGE_CLASSIFY_EXTRACT` and
its `RUST_STAGES` entry are retired.

Worth recording as a win rather than a compromise: the block lane runs no
typecheck, so its `ghc_session` phase still isolates pure GHC-API boot cost.
The measurement that reframed this workstream (classify at 33–75ms against a
~6.8s compile) remains reproducible after the swap instead of becoming
un-rerunnable.

---

## Decision 2 — `tidepool-repl` keeps its own wrapper builders

`run_turn` becomes the one entry point for `tidepool-harness` and for the
decl-items harvest. It does **not** absorb `tidepool-repl`'s five eval
sub-paths, and that is deliberate.

The reason is mechanical, not a scheduling dodge. `TurnTemplate` has two
placement modes, `{{TURN}}` (verbatim) and `{{TURN_STMT}}` (as a `do`-block
statement). Two of the repl's wrappers fit `{{TURN_STMT}}` exactly —
`wrap_bind_source` and `wrap_multi_bind_source`, already pinned byte-identical
by `turn_template_byte_identity_tests`. The other four
(`wrap_bare_it_monadic`, `wrap_bare_it_pure`, `wrap_eff_reference_source`,
`wrap_probe_source`, plus `wrap_pure_ref_source`) hoist the turn text through
`push_verbatim_binding`, which emits

```
<name> = let {
 __b =
<text>
 } in __b
```

and normalizes a missing trailing newline. A `{{TURN}}` splice reproduces the
*position* but not the newline normalization, so byte-identity would fail for a
turn text that already ends in a newline. Closing that needs a third placement
mode on both sides of the wire plus its own byte-identity cases — new surface
the protocol does not ask for, on the exact file
(`writeWholeModuleClosed`'s callers) a successor TL owns.

Nothing about Phase B's goal needs it: once the verdict arrives from the block
classify, the repl's wrappers never classify. `run_eval` stops calling
`classify_turn` because it is *handed* a verdict, which is what "migrate the
callers off the old path" requires.

**What the repl does change:** it threads verdicts instead of computing them,
and it drops `split_discard_bind` (Decision 3).

---

## Decision 3 — `split_discard_bind` deletion, and what replaces it

`split_discard_bind` exists because the repl compiles a discarding bind
(`_ <- e`, `(_, _) <- e`) as a bare *expression*, where `_ <- e` is a parse
error, so the pattern has to be stripped first. The protocol deletes it: placed
inside a `do` block, `_ <- e` is an ordinary statement and nothing needs
stripping.

So the repl grows one wrapper, `wrap_bind_discard_source`, shaped exactly like
`wrap_bind_source` but yielding `()` and splicing no binders:

```
__result :: Eff <stack> _
__result = do {
<{{TURN_STMT}} placement of the whole statement>
 ; pure ()
 }
```

and the zero-binder arm of `run_eval` compiles THAT (through
`compile_session_turn` with `bind = None`) instead of re-routing a stripped RHS
into `run_session_reference` / `run_plain_eval`.

`split_discard_bind` and its unit test are deleted. A behavioural test replaces
the unit test — a discarding bind still runs its effect and yields no binding —
so the repl suite's test count does not silently drop.

---

## Decision 4 — the decl-items harvest moves onto `--turn`

`define_batch_with_vals` joins its N declaration texts with a blank line and
calls `extract_binders` on the combined module. `--emit-binders` is deleted, so
that call becomes a `run_turn` call with a `decl` template and
`--turn-verdict decl`, returning `TurnResult::Decl { items, .. }`.

This is a 1:1 replacement at the same spawn count, and it fixes two things the
protocol flags:

- the decl template carries `wrap_decls`' 17-extension pragma block, authored
  in exactly one place (the Rust caller) instead of copied into the extract;
- `extractBinders`' "named `SessionDecls`, else `head summaries`" fallback is
  no longer reachable — the turn path uses `extractBindersNamed`, an exact
  match on the module name the caller's own template declares. `extractBinders`
  loses its last caller and is deleted with `--emit-binders`.

---

## The extract CLI after Phase B

### Removed

```
--emit-stmt-binders <out.json>      (with Tidepool.Binders.emitStmtBinders,
                                     renderStmtBindersJson)
--emit-binders <out.json>           (with Tidepool.Binders.emitBinders,
                                     extractBinders, renderBindersJson)
```

`extractStmtBinders` SURVIVES (both remaining lanes classify with it) and
`extractBindersNamed` survives (the `--turn` decl path). `renderItem` survives —
`renderTurnOutJson` uses it.

### Added — the block classify lane

```
tidepool-extract-bin <item1.txt> [<item2.txt> …] --classify --classify-out <out.json>
```

- Every POSITIONAL file is one item, classified in order. One GHC session boot
  for the whole batch (`extractStmtBinders` currently calls `runGhc` per call —
  the batch lane must boot ONCE and parse N times, or it is N spawns wearing a
  trench coat).
- Output, a single JSON object written to `--classify-out`:
  ```json
  {"verdicts":[{"kind":"bind","binders":["x"]},
               {"kind":"decl","binders":["sq"]},
               {"kind":"expr","binders":[]}]}
  ```
  `verdicts` has exactly one entry per positional file, in argv order. `kind`
  and `binders` are verbatim the `--emit-stmt-binders` contract, so
  `classifyTurn`'s documented precedence carries over unchanged.
- Failure convention unchanged: one JSON diagnostics report on stdout, non-zero
  exit. `classifyTurn` rule 6 means an unparseable item is a `"expr"` verdict,
  not a failure — the lane only fails on I/O or a genuine crash.
- Dispatched BEFORE `--turn` in `main`'s guard chain, mirroring how
  `--emit-stmt-binders` was dispatched before it.
- Every hand-rolled JSON string goes through `Tidepool.Binders.jsonString` (the
  FULL escaper). Do not introduce a second, minimal one.

### Phases the block classify lane emits

`startup`, `ghc_session` (the ONE boot), `classify` (the N parses, forced), and
`total` wrapping the whole lane. It must NOT emit `typecheck` — that name was
always a misnomer here and the protocol's resolution is that it leaves with the
lane, not that it gets carried forward.

### `--turn` after Phase B

Input surface unchanged (see the protocol's "Inputs"), with one addition to how
callers use it: a `decl` template is now supplied by every caller, including
the decl-batch caller.

Phases: `runTurnMode`'s whole body is wrapped in `timePhase timing "total"`
(it emits no `total` today — `processFile` has one, `processSessionFile` and
`runTurnMode` do not, which is how a repl-shaped turn currently produces a
`total`-less phase table). The classify substep is wrapped in
`timePhase timing "classify"`. `startup`/`ghc_session`/`typecheck`/`core` arrive
from `runPipelineSession`; `translate`/`cbor_encode`/`write` from
`writeWholeModuleClosed`.

`classify` is emitted ONLY when the mode actually classifies. With
`--turn-verdict` supplied no classify runs, and an absent row is the honest
report — the branch's existing principle (absent data over phantom rows).

### `extractStmtBinders` stops self-timing

Signature becomes `extractStmtBinders :: String -> IO StmtBinders` (the leading
`Bool` goes) and its three `emitPhase` calls (`startup`, `ghc_session`,
`typecheck`) are removed. It is a SUBSTEP now, in both surviving lanes; a
phase's owner has to be whatever knows it is a whole lane, and it no longer
does. Both callers time it themselves:

- `runTurnMode` → the single `classify` phase;
- the block classify lane → `startup`/`ghc_session` around its own boot, and
  `classify` around the N parses.

Known and accepted, recorded so nobody reads too much into "classify inside the
already-booted session": `extractStmtBinders` calls `runGhc`, so `--turn` boots
two GHC sessions per process. The parse-only boot loads no packages (which is
why the whole classify process measured tens of ms), so this is a tidiness and
attribution issue, not a performance one. Reusing the compile's session for the
classify parse is the clean end state and belongs to the successor extract TL.

### The `TurnOut` CBOR wire (unchanged by Phase B — this is the reader's spec)

`--turn-out` holds a bare CBOR value (NO `TPLR` header — that header belongs to
the tree format only). Tagged 2-element list, `[tag, payload]`:

```
["Decl", [ [Text],            -- binders
           [ExportItem] ] ]

["Bind", [ [Text],            -- binders
           Int,               -- variant
           [BoundBinder],
           [Ask],
           Text ] ]           -- wrappedSource

["Expr", [ Int,               -- variant
           [Ask],
           Text ] ]           -- wrappedSource

ExportItem  = ["EValue", Text] | ["EType", Text, [Text]] | ["EClass", Text, [Text]]
BoundBinder = [Text, Word64, Text, Text, Text]   -- name, varId, module, tier, typeDisplay
Ask         = [Word64, Text]                     -- site, rendered answer type
```

`varId` is a real CBOR `Word64` here (the decimal-string convention is a JSON
concern only). `tier` is `"Tier0Data"` / `"Tier1Closure"`.

---

## The Rust API after Phase B

### `tidepool-runtime/src/session/turn.rs`

```rust
pub enum TemplateSelector { Decl, Bind, BindDiscard, Expr }   // Decl is NEW

pub fn run_turn(req: TurnRequest<'_>) -> Result<TurnResult, CompileError>;

/// One parse-only extract spawn classifying N items in order.
pub fn classify_block(items: &[&str]) -> Result<Vec<TurnClassification>, CompileError>;
```

- `classify_turn` is DELETED. `classify_block` replaces it; a caller needing one
  verdict passes a one-item slice.
- `TemplateSelector::for_verdict(TurnKind::Decl, _)` now returns
  `Some(TemplateSelector::Decl)` — `Decl` selects a template like every other
  verdict, because the extract's decl path requires `--turn-template decl=<file>`.
- `run_turn`'s body is ONE `--turn` spawn: write the turn text and each template
  to files under a `TempDir`, pass `--turn-template <kind>=<file>` per template,
  `--turn-out`, `--output-dir`, `--include`, and (for a compiling verdict)
  `--session-root` / `--inject-val` / `--bind-gen`; decode the `TurnOut` CBOR;
  for `Bind`/`Expr` also read `result.cbor` / `meta.cbor` off the output dir.
  Template kind strings on the wire are `decl` / `bind` / `binddiscard` / `expr`.
- `asks` come from the `TurnOut` variant, not from re-reading `asks.json`.
- Timing: forward the spawn's phases under the `"extract"` prefix, and record
  the `extract_spawn` stage around the `cmd.output()` call, exactly as
  `compile_session_turn` does today. `classify_block` forwards under
  `"classify"`.
- Error mapping is unchanged from the paths being replaced: a parsed
  diagnostics report on a non-zero exit is `CompileError::Diagnostics`; an
  unparseable one is `MalformedDiagnostics` (→ VersionSkew); a spawn failure is
  `Io`.

### `tidepool-runtime/src/session/binders.rs`

Deleted. `extract_binders` was its only public entry, and the items now arrive
as `TurnResult::Decl`'s payload. `wrap_decls`' pragma block MOVES to the decl
template `define_batch_with_vals` supplies — do not delete the pragma set, and
do not retype it from memory: move the exact string.

### `tidepool-runtime/src/session/mod.rs`

`define_batch_with_vals` sources its `items` from `run_turn` with a `decl`
template over the joined text, `verdict: Some(TurnClassification { kind: Decl,
binders: vec![] })`. The joined text and the downstream use of `items` are
unchanged.

---

## Non-negotiables carried from the protocol

- **Rust never classifies.** No lexical bind/expr scanners. Every verdict comes
  from GHC, whether via `classify_block` or from inside `--turn`.
- **One emission path.** The turn mode reaches translation through the shared
  `writeWholeModuleClosed`. Do not add a second route to `translateModuleClosed`.
  Do not touch `Translate.hs`'s recognizer/qualification tables.
- **One-format wire.** The `--emit-*` removal makes the new Rust side
  incompatible with an older deployed extract. That is correct and deliberate:
  a stale extract fails loud and `scripts/redeploy.sh` ships both sides. Any
  merge of this work carries a redeploy requirement.
- **The dialect criterion at the compile boundary.** A valid canonical
  declaration that compiles today must still compile through the new path. The
  19-case corpus in `turn.rs` (including the `LambdaCase` / `QuasiQuotes` /
  `MultiWayIf` extension tripwires) is the floor; extend it for any new surface
  the swap exposes.
- **JSON escaping.** Every hand-rolled JSON string in the extract goes through
  the full escaper. Verify sidecars with a strict parser (`python3 -c
  'json.load(...)'`, never `strict=False`) — a raw newline inside a string
  renders as a line break, so invalid JSON looks correct in a terminal.
