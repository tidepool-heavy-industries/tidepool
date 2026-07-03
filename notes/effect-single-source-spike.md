# T6 spike — single-definition effects

**Status: design spike + working Console prototype.** Nothing merges until the
design is picked; the prototype exists to prove the mechanism end-to-end with
all existing tests unchanged.

## The drift class this kills

An effect today is **four hand-locked surfaces** that must agree constructor-by
-constructor:

1. the GADT decl string in `tidepool-mcp/src/effect_decls.rs`
   (`"Print :: Text -> Console ()"`),
2. the `#[derive(FromCore)] enum <Eff>Req` in
   `tidepool-handlers/src/handlers/<effect>.rs` (variant per constructor,
   `#[core(name = "...")]` mapping, Rust arg types),
3. the `EffectHandler::handle` match (one arm per variant),
4. the helper-verb docstrings (the `helpers` strings — the eval API's prompt
   surface).

The records half of the contract is already single-sourced (`CoreRecord`
renders the Haskell `data` decl from the Rust struct); the stack ORDER is
already single-sourced (`base_effects!`); the tool-description prose already
derives (`describe.rs`). This spike closes the remaining quadruple.

## Design: definition macro + two projections

The house idiom, applied per effect. Each effect is defined ONCE as an
exported callback macro in `tidepool-mcp/src/effect_defs.rs`:

```rust
#[macro_export]
macro_rules! console_effect_def {
    ($project:path) => {
        $project! {
            effect Console,
            handler ConsoleHandler,
            req ConsoleReq,
            decl_fn console_decl,
            description ["Print text output."],
            type_defs [],
            verbs [
                { ctor Print, method print,
                  args { msg: "Text" as String },
                  ret "()" },
            ],
            helpers [
                { name say, sig "Text -> M ()",
                  doc ["Emit a line of console output. Thin wrapper over the Print effect",
                       "so chains never need `send (Print …)`."],
                  body pointfree Print },
                { raw ["-- | `say` on anything Showable (`say . show`).",
                       "sayShow :: Show a => a -> M ()",
                       "sayShow = say . show"] },
            ],
        }
    };
}
```

Two projections expand it, one per crate:

- **`effect_decl_projection!`** (`tidepool-mcp/src/effect_defs.rs`, expanded in
  `effect_decls.rs`) → `pub fn console_decl() -> EffectDecl`. Constructor
  strings assemble via `concat!` from the structured pieces
  (`"Print" + " :: " + "Text" + " -> " + "Console" + " " + "()"`); helper
  strings assemble via the `helper_text!` sub-macro. Output is **byte-identical**
  to the hand-written builder (locked by a golden test), so the generated
  `Tidepool.Effects` module — and the compiled-artifact cache key — does not
  move.
- **`effect_rust_projection!`** (`tidepool-handlers/src/effect_glue.rs`,
  expanded in `handlers/console.rs`) → the `#[derive(FromCore)] pub enum
  ConsoleReq`, `impl DescribeEffect` (wired to `tidepool_mcp::console_decl`),
  and `impl EffectHandler` whose match arms call **hand-written inherent
  methods** (`self.print(cx, msg)`).

Why this shape and not the alternatives:

- **Dependency direction forces the definition into tidepool-mcp.**
  `tidepool-handlers` depends on `tidepool-mcp`; `tidepool_mcp::standard_decls()`
  / `base_effects!` need the decl builders at their current paths (and
  `tidepool-repl/src` — off-limits — consumes them). A callback macro is the
  only zero-new-crate way one token table reaches both crates. (A leaf
  `tidepool-effect-defs` crate + moving `EffectDecl` down was considered and
  rejected: public-API churn across the repl boundary for no expressive gain.)
- **macro_rules over a proc macro, for now.** The grammar is regular enough to
  match declaratively; the payoffs of a proc macro (arbitrary case conversion,
  nicer diagnostics, inferring Haskell arg types from Rust types the way
  `CoreRecord::hs_type` does) are real but none is load-bearing. The grammar
  is proc-macro-ready: if the token table outgrows macro_rules, the SAME
  definitions can be re-consumed by a function-like proc macro without
  rewriting them — projections are swappable, definitions are not.
- **Explicit over inferred.** Arg rows carry both types (`"Text" as String`)
  rather than inferring one from the other. The dual-typing is the actual
  contract (the bridge is exactly the place 1:1 inference breaks:
  `Value`/`serde_json::Value`, `Vec<(String, i64, String)>`/`[(Text, Int,
  Text)]`), and each duplication is now two tokens apart instead of two crates
  apart. Method names are explicit (`method print`) because macro_rules can't
  case-convert — also fine: greppable.

### What is generated vs hand-written

| Surface | Before | Now |
|---|---|---|
| GADT constructor strings | hand-written string per ctor | **generated** from verb rows |
| Effect description / type_defs / helper docstrings | hand-written in `*_decl()` | **single-sourced** in the definition (helper text generated for thin wrappers, raw pass-through otherwise) |
| `*_decl()` builder fn | hand-written | **generated** |
| `enum <Eff>Req` + `FromCore` | hand-written + derive | **generated** (variant name == Haskell ctor name; the `#[core(name)]` rename layer disappears) |
| `impl DescribeEffect` | hand-written | **generated** |
| `impl EffectHandler` dispatch match | hand-written | **generated**, arms call `self.<method>(cx, args…)` |
| Handler struct (fields, constructors) | hand-written | hand-written (configuration, not contract) |
| **Handler method bodies** | hand-written arms | **hand-written inherent methods** — free to use any `cx.respond*` variant |
| Stack order / union tags | `base_effects!` | unchanged — `base_effects!` stays THE order source; definitions are THE content source |
| Tool descriptions | derived from `EffectDecl` (`describe.rs`) | unchanged — derives from the now-generated decl |

Result shapes the constraint list demands are all expressible:
multi-arg constructors (`args { k: "Text" as String, v: "Value" as serde_json::Value }`),
Either/outcome results (`ret "(Either (Maybe Text) ())"` — ret is a literal
Haskell type), `respond_list`/`respond_stream` verbs (delivery is the
hand-written body's choice; the definition captures the Haskell-visible type),
per-verb doc prose (`doc [...]` lines on helpers), `type_defs` (raw
pass-through, same as today).

### Prototype (this branch)

Console, end-to-end. `git diff` shows the hand-written surfaces deleted:

- `tidepool-mcp/src/effect_decls.rs`: hand-written `console_decl()` body →
  `crate::console_effect_def!(crate::effect_defs::effect_decl_projection);`
- `tidepool-handlers/src/handlers/console.rs`: `ConsoleReq` enum,
  `DescribeEffect` impl, `EffectHandler` impl all deleted → one
  `tidepool_mcp::console_effect_def!(crate::effect_glue::effect_rust_projection);`
  plus the hand-written `ConsoleHandler::print` method.
- New: `tidepool-mcp/src/effect_defs.rs` (definition + decl projection +
  `helper_text!` + byte-identity golden test),
  `tidepool-handlers/src/effect_glue.rs` (Rust projection).

All existing tests pass unchanged (`cargo test -p tidepool-handlers -p
tidepool-mcp --lib`), including the Console FromCore/dispatch roundtrips and
the decl-content assertions in `tidepool-mcp`.

## How #335 typed failure slots in

Verb rows accept an optional, currently-ignored `errors <RustErrorEnum>` slot
— the grammar reserves the seam the #335 migration lands in:

```rust
{ ctor FsRead, method read,
  args { path: "Text" as String },
  ret "Text",
  errors FsError },
```

Designed semantics (per-effect waves, NOT implemented here):

- **Decl projection**: renders the constructor result as
  `Fs (Either FsError Text)` and threads the same `Either` through generated
  thin-helper sigs.
- **Rust projection**: the dispatch arm changes contract — the hand-written
  method returns typed data (`Result<T, FsError>` for scalar verbs) and the
  generated arm responds it as `Right`/`Left`. Handlers become total; the
  respond variants collapse toward one, exactly as #335 specifies.
- **Error ADTs**: a Rust `enum FsError { NotFound(String), NotUtf8(String),
  Sandbox(String), … }` deriving `CoreRecord` + `ToCore` — the existing record
  single-source generates its Haskell `data` decl (via the `inventory`
  registry) so the error-type shape can't drift either. Granularity is a
  per-wave decision (start coarse, refine where dispatch on the case matters).
- **Per-item granularity** (`readGlob :: … -> M [(Text, Either FsError Text)]`)
  stays a verb-level property: the row writes it directly in `ret`, `errors`
  is for the scalar wrapping case.

Because a #335 wave edits ONE definition per effect (rows + helper docs move
together) instead of four surfaces, the migration cost drops to roughly the
decl-string edit alone, and the "decl says Either but the handler still
aborts" desync — the worst failure mode of doing #335 under the status quo —
is impossible by construction.

## Per-effect migration cost

Mechanical recipe per effect: (1) transcribe constructors into verb rows
(Haskell arg types come straight off the decl string; Rust arg types straight
off the Req enum — you're merging two existing lists, not writing new facts);
(2) classify helpers thin/raw (thin = pure send-wrapper, generated; anything
else = raw pass-through, zero rewriting); (3) convert `handle` match arms to
inherent methods; (4) delete the four hand-written surfaces. Byte-identity of
the decl output is checkable per effect with a golden test like Console's.

| Effect | ctors | helpers (thin/raw) | wrinkles | est. |
|---|---|---|---|---|
| Console | 1 | 1/1 | none — done (prototype) | done |
| Time | 1 | 0/1 | nullary ctor (`TimeNow`) → zero-field tuple variant `TimeNow()`; verify FromCore derive on empty unnamed variant (expected fine — fields loop is empty) | ~½h |
| Meta | 7 | 7/0 | all thin, debug-only | ~½h |
| KV | 7 | 3/4 | multi-arg thin (`applied`) | ~1h |
| Git | 4 | 0/4 | bridged records already `CoreRecord`; helpers all raw-doc | ~1h |
| Exec | 5 | 0/6 | tuple args/results; helpers wrap `Proc` (raw) | ~1h |
| Http | 6 | 2/4 | `Value` args | ~1h |
| Ask/Llm | 1/2 | raw-heavy (schema vocab in type_defs) | Ask is interposed, not in `base_effects!` — projection unchanged, only the def site differs | ~1h |
| Lsp | 8 | 8 thin-ish | `type_defs` carry wire types + ToJSON instances (raw pass-through); `respond_list` verbs — body concern only | ~1½h |
| Fs | 12 | 4/18 | biggest helper corpus; mostly raw pass-through | ~2h |

Total: **roughly a day of mechanical work** for full coverage, parallelizable
per effect, each effect independently verifiable (decl byte-identity + existing
roundtrip tests). Recommended order: Time next (proves the nullary-ctor edge),
then Meta/KV, then the rest; do the #335 `errors` implementation as its own
wave AFTER full migration (Fs first, per #335).

## Composition with the existing single-sources

- **`base_effects!`** stays the one ordered list (union tags positional —
  Locked Decision). A definition adds CONTENT at a name; the row
  `(Console, console_decl)` is untouched. Cutting/reordering remains a
  single edit there.
- **`describe.rs`** derives tool descriptions from `EffectDecl` — now fed by
  generated decls, unchanged code. The docstring in the definition IS the
  prompt surface (the API is the prompt).
- **`CoreRecord`** remains the record/error-ADT half; this spike is its
  request-side sibling. Between them, every Haskell decl the server emits is
  generated from Rust source of truth.

## Drift-class impact

The `describe.rs` move killed the tripled-prose class (three hand-maintained
tool-description surfaces). This design applies the same closure to the
remaining quadruple: after migration, a constructor cannot exist in the GADT
without its Req variant, its dispatch arm (a missing `method` is a compile
error at the projection site), and its helper text, because they are the same
tokens. The residual hand-locked surface shrinks to: handler method bodies
(irreducible — they're the semantics), `base_effects!` rows (one per effect,
order-bearing), and the Haskell-side stdlib records (already `CoreRecord`).
What remains UNguarded is Haskell-typestring correctness inside `ret`/`args`
literals (`"Text"` vs `"Int"`) — same exposure as today, caught by the same
JIT roundtrip tests; a proc-macro projection could later infer these from the
Rust types (CoreRecord's `hs_type` mapping) and demote the literals to
overrides.

## Verification (this branch)

- `cargo build --workspace` — green with the prototype in place.
- `cargo test -p tidepool-handlers --lib -- --test-threads=1` — unchanged-green
  (Console FromCore/dispatch roundtrips exercise the generated enum + match).
- `cargo test -p tidepool-mcp --lib -- --test-threads=1` — unchanged-green,
  plus the new `generated_console_decl_matches_handwritten_baseline` golden
  test locking byte-identity.
