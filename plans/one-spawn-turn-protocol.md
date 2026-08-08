# One extract spawn per turn — protocol

## Problem

A session-eval turn costs 2–3 `tidepool-extract` process spawns:

| # | Invocation | Purpose |
|---|-----------|---------|
| 1 | `extract <raw turn> --emit-stmt-binders <out>` | GHC parse-only decl/bind/expr verdict + binder names |
| 2 | `extract <Wrapped.hs> --session-root … --inject-val …` | Core CBOR + meta + `asks.json` |
| 3 | `extract <SessionDecls.hs> --emit-binders <out>` (decl turns) | export items for the selective re-export |

The classification in spawn 1 exists only so the *Rust* side can pick which
wrapper module to build for spawn 2. GHC already has the parse in hand at
spawn 1 and a booted session at spawn 2; the round trip through Rust is the
only reason they are two processes.

**This is not a latency problem.** An earlier draft of this document claimed
every spawn pays a flat ~6–8s GHC boot floor, making turn latency a function of
spawn count. Measurement says otherwise: the compile spawn is ~6.8s (~70% of a
turn) and JIT codegen ~2.7s (~28%), while the parse-only classify is **33–75ms**.
Spawns 1 and 3 are parse-only — they build no module graph, load no package DB,
and typecheck nothing — so collapsing them buys tens of milliseconds, not
seconds. The premise was wrong because the boot floor belongs to the *compile*
spawn, which this work does not remove and could not.

So the justification is conceptual, and it is worth stating plainly rather than
dressing a cleanup as an optimization:

- one verdict source instead of two parses that can be handed different inputs
  (the bug class recorded below, which was live, not hypothetical);
- one emission path that cannot drift from a second one;
- both `--emit-*` flags and their parallel channels deleted;
- the verdict's four real shapes handled by selection rather than by a text scan
  standing in for a parse.

One piece of this design *is* latency-relevant, and it is not the classify
collapse: the repl compiles an expression turn as effectful and, on a type error,
recompiles it as pure — a second **compile** spawn, ~6.8s, on every turn that
hits it. Folding that retry into one booted session (see *Variant retry*) is the
only place real time is available here. It is unproven, conditional on GHC
tolerating two pipeline runs per process, and explicitly not what justifies this
work.

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
- a decl batch → module parse only; there is no per-statement verdict to compute
  and the caller already knows the kind.

Get this wrong and multi-declaration batches lose export items, so the selective
re-export silently stops shadowing — a failure that shows up as a stale binding
several turns later, not as an error.

Under rule 1 below, the decl batch arrives as a block of items rather than as
joined text, and the extract assembles the module it parses. The constraint is
unchanged; what changes is that the two parses can no longer be handed different
inputs.

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

Read that table as spawn *count*, not as time saved. The spawns it removes are
the parse-only ones at 33–75ms each; the ~6.8s compile spawn is still there in
every row.

## Acceptance

Because the removed spawns are cheap, the bench is a **regression guard, not a
win condition**:

- the contract corpus green end-to-end through the one-spawn path — byte-identical
  verdicts, binder names, and wrapped modules against the path it replaces;
- the repl suite and the harness acceptance batch green with the two-spawn shim
  gone;
- before/after turn latency showing **no regression**. A delta of tens of
  milliseconds either way is the expected and correct result. Do not spend effort
  chasing latency here; if a turn gets meaningfully faster, suspect the
  measurement before believing it.

## One input shape; where harvested data may come from

The wrong-parse bug class above exists because two parses produce data that
outlives them. Two rules were proposed to make it unrepresentable rather than
merely documented. Verdicts differ, so they are recorded separately.

### Rule 1 — one input shape, a block of items. ADOPTED.

The entry point takes a **block of items**, each classified by the trial-parse
ladder (`import` | `decl` | `stmt`) that `classifyTurn` already mirrors. The decl
batch stops being a separate entry point with its own joined-text convention: it
becomes a block whose items all classify as decls. One grammar, one entry, N
items. Mirroring canonical GHCi is the project's api-is-the-prompt rule paying
out rather than costing.

This is what actually kills the bug class. The two parses were not dangerous
because they were two — they were dangerous because they saw **different
inputs**: a single turn for the ladder, joined text for the module parse. Remove
the different-inputs condition and a verdict computed for item *i* can no longer
be applied to a text that is not item *i*.

Note what this does NOT buy: a block containing several statements still needs
one compile per statement, because statement *i+1* may depend on effects that
statement *i* performed and on bindings it introduced, and a statement may
suspend mid-block. The block collapses *classification* to one call and compiles
the leading decl group as one artifact; it does not collapse N statements into
one compile.

### Rule 2 — harvest only from the compiled artifact. NO-GO.

The proposal: the verdict parse yields exactly one thing, the wrapper choice, and
no data that outlives it; binder names, export items, and types all come from the
one wrapped module GHC typechecks and compiles. Three independent obstacles, any
one of which is disqualifying:

1. **The bind case is circular.** The wrapper is
   `__result = do { x <- e ; pure (x) }` — the binder name is an *input* to
   wrapper construction. A name needed in order to produce the artifact cannot be
   discovered from that artifact; harvesting it post-hoc could only confirm what
   the wrapper already asserted. GHCi's `execStmt` returning bound names is not a
   counterexample: it runs the statement in the interactive context and lets GHC
   name the bindings, whereas this pipeline wraps into a module and extracts Core
   for the JIT. Adopting the `execStmt` shape means giving up module-wrapping,
   which is the architecture.
2. **The decl case is circular too, less obviously.** `render.rs` builds the
   module's export list from *this* turn's items, and the items additionally drive
   the import `hiding` computation and the ambiguous-occurrence avoidance. Items
   are therefore required to render the very module whose compilation would supply
   them. Breaking that needs the export list restructured to not depend on the
   current turn's items — a change to the decl plane's shadowing mechanism, far
   outside this work and in a file the repl leans on heavily.
3. **The hook is not reachable.** `PipelineResult` carries `prBinds` (Core),
   `prTyCons`, `prHscEnv`, `prCapturedType`, `prResultType`, `prWarnings`. The
   typechecked module is a local inside `runPipeline`/`runPipelineSession`,
   consumed and dropped. Exposing it means editing `GhcPipeline.hs`. Harvesting
   from Core instead is worse: `do { x <- e ; pure x }` desugars to
   `>>= e (\x -> pure x)` and the name survives only as a lambda binder whose
   shape depends on the monad and on which passes ran.

### What is adopted from rule 2's intent

- Each parse produces exactly one kind of thing: the ladder produces the verdict
  and nothing else; the module parse produces export items and nothing else.
  Under rule 1 both see the same block, so there is no second input to mismatch.
- Everything about a *compiled* turn that can come from the typechecked module
  already does: `boundBinders`' types, `varId`s, and tiers are computed from
  `prResultType` / `sessionBinderName` / `isClosureType`, not from the parse. Only
  the name string is echoed from the verdict.
- That echo is **corroborated, and fails loud**: `emitBindArtifacts` errors when
  `result`'s type was not captured, and when N binder names do not match an
  N-tuple bound type. A name the parse invented but the module does not bind
  cannot reach the caller silently. This is the invariant to preserve — it is the
  reachable form of "unrepresentable" without `GhcPipeline` surgery.

Revisiting rule 2 in full is a standalone piece of work whose prerequisite is
exposing the typechecked module from the pipeline. It is not blocked by anything
here.

## What the two-placeholder contract does not cover

Building the interface surfaced three cases the `{{TURN}}` / `{{BINDERS}}` pair
cannot express as-is. Resolutions, none of which move a classification decision
into Rust:

**The input payload lane — no change needed.** Several wrappers splice a runtime
JSON blob as an `input :: Aeson.Value` binding (`begin_user_module`'s
`input: Option<&serde_json::Value>`). This is not a third placeholder: the
payload does not vary with the verdict, so the caller bakes it into
`TurnTemplate.source` when it builds the template, exactly as it bakes in the
pragma block, imports, helpers, and effect stack. Anything knowable before the
verdict belongs in the template source, not in a placeholder.

**The two placement modes must not be substituted independently.** Whichever
placement a template uses, the turn text has to be spliced exactly once and
last, because the turn text is arbitrary user Haskell that may itself contain
literal placeholder syntax. Substituting `{{TURN_STMT}}` and then `{{TURN}}` as
two chained replacements re-scans the text the first one inserted, so a turn
containing `{{TURN}}` gets mangled; doing it in the other order has the mirror
bug. Pick the placement mode first, then perform exactly one turn splice — the
implementation reads as an `if`/`else` on which placeholder the template
contains, and that shape is load-bearing rather than incidental. A template
carrying both placeholders is an authoring error; it surfaces as an
unsubstituted placeholder reaching GHC, which is loud.

**Placement is the template's business, so the template declares it.** A bind
turn whose text begins with `let ` is rewritten to the layout-safe explicit-brace
form `let { … }` before being spliced into a `do` block (`push_braced_stmt`) —
without it, a `let` at column 1 breaks the block's layout. That is a *transform*
of the turn text, so a byte-exact verbatim splice cannot reproduce it. The fix is
a second placement placeholder rather than a template language: `{{TURN}}` places
the text verbatim, `{{TURN_STMT}}` places it as a `do`-block statement, applying
exactly the normalization `push_braced_stmt` performs today. Which one a template
uses is a property of where its splice point sits, decided when the template is
written; byte-identity against the current wrapper stays testable either way.

**The verdict space has four shapes, not three.** A bind that binds no name
(`_ <- e`, `(_, _) <- e`) is a different wrapper choice from a bind that binds
one: the discarding form runs the right-hand side for its effects and keeps no
value. Today the repl detects the empty binder list, strips the pattern with
`split_discard_bind`, and re-routes the right-hand side as an expression. Passing
an empty name list through to a bind compile does not work — the extract rejects
it (`session-bind requires at least one --bind-name`) — so the discarding bind
needs its own template kind, selected by the extract from its own verdict.

Doing it that way is also more correct than the status quo, incidentally: the
extract splices the right-hand side from the parsed statement, whereas
`split_discard_bind` finds the first `<-` in the raw text. The lexical split
happens to hold for the shapes seen so far, but it is a text scan standing in for
a parse.

## The extract lags the Rust side — three gaps to close before any caller is wired

The `--turn` mode exists and works for the shapes it covers, but three things the
Rust side already does are not yet in it. All three must land before the shim is
swapped, because each one silently degrades rather than failing at the boundary.

**1. The decl path narrows the language surface.** `--turn`'s decl branch hands
the RAW turn text to the whole-module parse. The path it replaces does not: Rust's
`wrap_decls` wraps decl text in `module SessionDecls where` behind a 17-extension
pragma block (GADTs, LambdaCase, RecordWildCards, QuasiQuotes, TypeApplications,
…). Reproduced against the built extract:

```
f = \case { 0 -> 1 ; _ -> 2 }

--turn --turn-verdict decl   →  error [GHC-51179] Illegal \case   (turn fails)
--emit-binders, wrap_decls   →  {"items":[{"kind":"value","name":"f"}]}
```

So any declaration needing one of those extensions parses today and stops parsing
through the turn mode. `QuasiQuotes` makes this concrete rather than theoretical:
a `[fmt|…|]` turn already caused a live classification regression once.

Fix: `--turn-template decl=<file>`, the caller supplying the decl *parse* wrapper
the same way it supplies bind and expr wrappers. That keeps the pragma set
authored in exactly one place instead of copied into the extract where it would
drift, and it reuses the splice machinery already there. It also repairs
something currently accidental: `extractBinders` selects the module summary named
`SessionDecls` and falls back to `head summaries` otherwise, so the raw-text path
works by luck rather than by the name match the function was written around.

**2. `{{TURN_STMT}}` is not implemented in the extract.** It recognizes `{{TURN}}`
only, so a bind template using statement placement would leave the placeholder
literal in the module it compiles. The Rust side depends on this placement for the
`let`-at-column-1 case.

**3. The discarding-bind template kind is missing.** The extract's template kinds
are `bind` and `expr`; the Rust selector has `Bind`, `BindDiscard`, and `Expr`.
A discarding bind currently has no kind to select.

Gaps 2 and 3 postdate the extract work's spec and were flagged by its author
rather than quietly skipped. Gap 1 was found by probing the built binary.

One smaller thing, worth fixing while in there: with `--turn-verdict decl` the
`Decl` variant echoes the supplied (empty) binder list while `declItems` carries
the real names, so `binders` is unreliable on the batch path. Derive `Decl`'s
binders from the harvested items' head names when the verdict supplies none.

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

Verifying this crate: `cargo nextest run -p tidepool-runtime` runs **nothing** —
`.config/nextest.toml`'s `default-filter` classifies `tidepool-runtime` as
GHC-extract-heavy and skips all 48 of its binaries, so the command exits "no
tests to run" and reads as a pass to anything not checking the count. The turn
lane's pure tests need
`cargo nextest run --ignore-default-filter -p tidepool-runtime --lib -E 'test(turn::tests)'`.

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
