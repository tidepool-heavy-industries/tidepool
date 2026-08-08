# One extract spawn per turn — protocol

## Problem

A session-eval turn costs 2–3 `tidepool-extract` process spawns:

| # | Invocation | Purpose |
|---|-----------|---------|
| 1 | `extract <raw turn> --emit-stmt-binders <out>` | GHC parse-only decl/bind/expr verdict + binder names |
| 2 | `extract <Wrapped.hs> --session-root … --inject-val …` | Core CBOR + meta + `asks.json` |
| 3 | `extract <SessionDecls.hs> --emit-binders <out>` (decl turns) | export items for the selective re-export |

Each spawn pays a flat GHC-session boot floor (~6–8s) independent of block
size, so turn latency is dominated by spawn count, not by the work.

The classification in spawn 1 exists only so the *Rust* side can pick which
wrapper module to build for spawn 2. GHC already has the parse in hand at
spawn 1 and a booted session at spawn 2; the round trip through Rust is the
only reason they are two processes.

## Decision

One mode, `--turn`, whose positional file is the **raw turn text** (not a
wrapped module). The extract classifies it, picks its own wrapper, compiles,
and writes one rich result sidecar. The Rust side supplies the wrapper
*templates* and consumes the result; it never parses Haskell and never
branches on a verdict it derived itself.

### Inputs

```
tidepool-extract-bin <turn.txt> --turn \
  --turn-template <kind>=<file> [--turn-template <kind>=<file> …] \
  --turn-out <turn.cbor> [--json-output <turn.json>] \
  --output-dir <dir> [--include <dir> …] \
  [--session-root <dir>] [--inject-val <mod> …] [--bind-gen <g>] \
  [--turn-verdict <kind>[:<name>,<name>…]]
```

- `--turn-template <kind>=<file>` — `kind` is `bind` or `expr`. Repeating a
  kind appends an ordered variant list (see *Variant retry*). The file holds a
  complete Haskell module with two substitution points and nothing else:
  - `{{TURN}}` — the raw turn text, spliced verbatim.
  - `{{BINDERS}}` — the harvested binder names, comma-joined (`a, b`).
  Byte-exactness is the contract: given the same verdict, the module the
  extract compiles is byte-identical to the module Rust wraps today.
- `--turn-verdict` — a verdict the caller already holds from a batch classify.
  Skips the re-parse; still GHC-sourced (the batch classify is the same
  `classifyTurn`). Absent, the extract parses.
- `--session-bind` is **gone from the turn mode's input surface**: the extract
  infers the bind path from its own verdict. `--bind-gen` stays an input —
  generation numbering is the caller's session state, not something GHC knows.

### Output — the rich result, a tagged variant

The result is a **sum type over the verdict**, not a record with fields that are
meaningful for some kinds and dead for others. Which variant came back is what
the caller branches on; a variant carries exactly the payload its kind has.

```
TurnOut
  = Decl { binders :: [Text], declItems :: [ExportItem] }
  | Bind { binders :: [Text], variant :: Int, boundBinders :: [BoundBinder]
         , asks :: [(Word32, Text)], wrappedSource :: Text }
  | Expr { variant :: Int, asks :: [(Word32, Text)], wrappedSource :: Text }
```

- `binders` — verbatim the `--emit-stmt-binders` contract, so the existing
  verdict semantics (`Tidepool.Binders.classifyTurn`: bind-marker first, then
  signature, then name-declaring `ValD`, then bare expression, then remaining
  decls, else expression) carry over unchanged.
- `declItems` — the `--emit-binders` payload, harvested from the same parse. A
  decl turn does **not** compile in this mode: the decl plane's module render is
  the caller's, a different pipeline.
- `variant` — which template of the chosen kind's list compiled.
- `boundBinders` — the `--emit-bound-binders` payload, inlined. `varId` stays a
  decimal string wherever it is rendered as JSON (an f64 would lose the u64).
- `wrappedSource` — what was actually compiled. Diagnostic, and the anchor for
  the byte-identity contract test.

**CBOR is the machine path.** The variant is written as a CBOR sidecar beside
the unchanged `result.cbor` / `meta.cbor` / `asks.json`. The tree wire format is
untouched (no `TPLR` version bump): classification and binders ride *beside* the
tree bytes, never inside them.

`--json-output` renders the same variant as JSON for scripting and debugging.
It is a *rendering*, not a second format: variant names and payload shape stay
identical between the two, so a script reading the JSON is reading the same
structure the runtime reads. Nothing consumes the JSON in the product path.

Failures keep the existing convention: exactly one JSON diagnostics report on
stdout, non-zero exit. A caller distinguishes a real GHC rejection
(`ExtractFailed` / `Diagnostics`) from an unparseable report
(`MalformedDiagnostics` → version skew) exactly as it does today.

### `--emit-stmt-binders` and `--emit-binders` are deleted, not deprecated

When this mode lands, both flags are removed from the extract CLI and every use
of them across the tree goes with them (`session/turn.rs`, `session/binders.rs`,
`session/mod.rs`'s decl-batch path, the repl path that shares them, any script or
test referencing them). `--emit-stmt-binders`' parse-only mode **is** the second
spawn being deleted; keeping it would preserve exactly the seam this work
removes. Afterwards binders and exports have one source: the rich result
variant. No parallel channel, no deprecation shim.

Consequence, deliberate: the new Rust side is incompatible with an older
deployed extract binary. That is correct under the one-format wire policy — a
stale extract fails loud, and `scripts/redeploy.sh` ships both sides together.
Any merge of this work carries a redeploy requirement.

`--emit-binders` (declaration export items) goes the same way, for the same
reason: its payload is what the `Decl` variant carries, and binders and exports
having one source is the whole principle. Keeping one legacy flag while deleting
its sibling would preserve exactly the seam-shape being removed.

That removal carries a constraint worth stating, because missing it breaks the
decl plane quietly. `--emit-binders`' caller is `define_batch_with_vals`, which
joins N declaration texts with a blank line and passes the **combined
multi-declaration module**. Its Haskell side (`Tidepool.Binders.extractBinders`)
is a whole-module parse — `guessTarget` / `parseModule` over every declaration.
`classifyTurn`, which serves the verdict, parses ONE statement-or-declaration
and cannot cover a batch. So the `Decl` variant's `declItems` must be harvested
by the module parse, not the statement parse. The two parses already live side by
side in `Binders.hs`; the turn mode needs both, picked by what it is looking at:

- a single turn whose verdict is `decl` → statement parse for the verdict,
  module parse for the items;
- a decl batch (`--turn-verdict decl` over combined declaration text, the
  `define_batch_with_vals` path) → module parse only; there is no per-statement
  verdict to compute and the caller already knows the kind.

Get this wrong and multi-declaration batches lose export items, so the selective
re-export silently stops shadowing — a failure that shows up as a stale binding
several turns later, not as an error.

### Variant retry

The repl compiles an expression turn as effectful, and on a type error
recompiles it as pure — a second spawn today. As an ordered variant list, both
attempts happen inside one booted session: try in order, first that
typechecks wins, `variant` reports which. Only the last variant's diagnostics
are reported when all fail.

Open risk: whether `runPipelineSession` can run twice in one process. If it
cannot, retry stays a second spawn (still one fewer than today) and the
variant list degrades to length 1. Spike this before relying on it.

## Spawn count after

| Turn shape | Before | After |
|-----------|--------|-------|
| expr | 2 (3 with the pure retry) | 1 |
| bind | 2 | 1 |
| decl | 2 | 1 + the decl-plane module compile |
| repl block of N items | N classify + per-item compiles | 1 batch classify + per-item compiles |

The block runner segments items by verdict before running any of them, so it
needs verdicts for the whole list up front. One batch classify covers that,
and each item's `--turn` run then carries `--turn-verdict` so nothing
re-parses.

## Rust side

`tidepool-runtime/src/session/turn.rs` grows the request/result pair the
callers consume:

```rust
pub struct TurnTemplate { pub kind: TurnKind, pub source: String }
pub struct TurnRequest<'a> { /* raw text, templates, session ctx, gen, verdict */ }
pub enum TurnResult {
    Decl { binders: Vec<String>, items: Vec<ExportItem> },
    Bind { binders: Vec<String>, bound: Vec<BoundBinder>, variant: usize,
           compiled: CompiledTurn, wrapped_source: String },
    Expr { variant: usize, compiled: CompiledTurn, wrapped_source: String },
}
pub fn run_turn(req: TurnRequest<'_>) -> Result<TurnResult, CompileError>;
```

`TurnResult` mirrors the extract's variant: an enum, so a caller cannot read a
field that its verdict does not have. `CompiledTurn` groups what a compiled turn
yields (`expr`, `table`, `warnings`, `asks`) — the `Decl` variant compiles
nothing and carries none of it.

`run_turn` is the only entry point callers see. Its body is replaced, not
wrapped: while the extract mode is being built it performs the two spawns
(`classify_turn`, then Rust-side template selection, then
`compile_session_turn`), and when the mode lands that body becomes the single
`--turn` call. `classify_turn` and its `--emit-stmt-binders` spawn are then
deleted outright, along with the bind-turn binder spawn. There is no
configuration switch between the two and no surviving two-spawn path — the
interim body is scaffolding with a deletion date, not a fallback seam.

Non-negotiable invariants:

- Rust never classifies. No lexical heuristics, no "looks like a bind"
  scanners. GHC's parse is the only verdict source, whether it arrives from a
  batch classify or from inside the turn run.
- Template selection is a lookup keyed by GHC's verdict, never a guess.
- Byte-identity: for every corpus turn, the module the new interface compiles
  equals the module the old path wrapped.

## Rejected alternative

*Move the wrapper templates into Haskell.* The repl authors eight-plus
wrappers (single bind, multi-bind tuple, pure bind, effectful expr, pure expr,
paginated, discard-pattern) parameterised by effect stack, import block,
helper block, and the live session-lib module — all Rust-side session state.
Reproducing that context in the extract makes the extract own the repl's
surface. Keeping template *authoring* in Rust and moving only template
*selection + splice* into the extract puts the branch where the parse already
is and leaves the surface where its state lives.
