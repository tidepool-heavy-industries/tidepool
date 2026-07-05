# haskell/ — Tidepool Haskell harness + stdlib

The GHC→Core extractor (`tidepool-extract`) and the eval stdlib
(`lib/Tidepool/*`, auto-imported in the MCP server). See the repo-root
`CLAUDE.md` for the project map and locked decisions.

## Rebuilding the Haskell Toolchain

After changing `haskell/` code (Translate.hs, GhcPipeline.hs, Prelude, etc.).

**How the extract binary is resolved.** `tidepool-extract` is the GHC→Core
extractor. The Rust runtime invokes it via the `TIDEPOOL_EXTRACT` env var if set,
else `tidepool-extract` on `$PATH` (`tidepool-runtime/src/lib.rs`, `cache.rs`).
On `$PATH` that resolves to `~/.nix-profile/bin/tidepool-extract`
— a **nix wrapper** that prepends the with-packages GHC (supplies `lens`) to PATH
and `exec`s the `tidepool-harness` binary **in the nix store**. It does NOT exec
anything under `~/.local/bin` or `~/.cargo/bin`, so a `cp … ~/.local/bin/…` does
nothing. (Those copies, and the `~/.cargo/bin/tidepool-extract` duplicate wrapper,
are stale cruft shadowed by the nix-profile entry, which is earlier on PATH.)

**Local iteration — test against a worktree build (no deploy):**

```bash
cd haskell && cabal build tidepool-extract-bin      # build in dist-newstyle
# Point tests/evals at it; with-packages GHC on PATH supplies lens.
# A new binary fingerprint forces a cache miss.
TIDEPOOL_EXTRACT=$(cabal list-bin tidepool-extract-bin) \
  PATH=/nix/store/<hash>-ghc-native-bignum-9.12.2-with-packages/bin:$PATH \
  cargo test -p tidepool-runtime ...
```

**Deploy to the live MCP server (nix profile):**

`scripts/redeploy.sh` encapsulates this full dance (extract + both Rust servers + cache clear) — prefer it over running these steps by hand.

```bash
git add haskell/...                          # nix flake builds see only TRACKED files
nix profile upgrade tidepool-extract         # rebuild + install the wrapper+harness
# (or: nix profile install .#tidepool-extract for a first install)
rm -rf ~/.cache/tidepool/                     # clear stale cached CBOR
# Then /mcp-reconnect so the server picks up the new extract.
```

## Regenerating Test Fixtures

The Haskell integration tests use pre-compiled CBOR fixtures in
`test/suite_cbor/`. After changing the Haskell serializer or adding test bindings
to `test/Suite.hs`:

```bash
cd haskell && cabal run tidepool-extract-bin -- test/Suite.hs --all-closed \
  --include lib --target-module-only --output-dir test/suite_cbor
# --include lib + --target-module-only: Suite.hs imports Tidepool.QQ
# --output-dir: default derives from module basename → test/Suite_cbor (wrong dir)
```

> `*.cbor` fixtures are gitignored — new ones must be `git add -f`'d or a
> `suite_*!` test won't compile on a fresh checkout.

## Diagnostics — Haskell-extract knobs (separate process)

Env-gated, OFF by default. For the JIT-runtime / effect-machine / cache knobs see
`tidepool-codegen/CLAUDE.md`.

| Knob | What it shows | Reach for it when |
|------|---------------|-------------------|
| `TIDEPOOL_DUMP_CLOSED=<needle>` | Closed Core for bindings whose binder name matches needle | Inspecting what Core the JIT actually receives for a binding |
| `TIDEPOOL_VARID_AUDIT=1` | VarId collision report (distinct binders → same 64-bit id) | SIGILL/case-trap hunts; ruling out id collisions |
| `TIDEPOOL_VARID_AUDIT=<hex>,<hex>` | Resolves specific VarIds to source names + enclosing top-level binder | Naming the function a JIT trace implicates |
| `TIDEPOOL_JOINREC_DEBUG=1` | joinrec-translation forensics (`[313-joinrec]` spew) | Join-point conversion bugs (jumps compiled as calls, wrong continuation) |
| `TIDEPOOL_IFACE_DEBUG=1` | `[fat-iface]` interface-loading trace | Missing unfoldings / "unresolved external" mysteries |

## Eval stdlib (`lib/Tidepool/`)

MCP users get `import Tidepool.Prelude hiding (error)` auto-imported; more modules
via the `imports` field. Inline `data` decls in `helpers` work for eval-local
types; promote to a `.tidepool/lib/` module (project) or `~/.config/tidepool/lib/`
(global) when reused across evals.

> **Shipping the stdlib:** the WHOLE `lib/Tidepool/**` tree is embedded into the
> server binary at build time (`tidepool/build.rs`) and materialized to
> `~/.cache/tidepool/stdlib/<hash>/` at startup — so `cargo install --path
> tidepool` is all that's needed to ship a stdlib change to an installed server
> (no manual prelude copy, no nix step; that's for the *extract* binary). In-repo,
> the server uses `haskell/lib/` directly. The materialization is content-hashed,
> so it can't go stale across binary versions.

**The live API reference is the MCP `eval` tool description** (emitted by the
server) plus the source under `lib/Tidepool/`. Do not re-list the full function
surface here — it drifts. Module map:

- `Prelude` — the auto-imported hub (Text-first; `show :: a -> Text`; polymorphic
  `pack`; lists/Map/Maybe/monadic combinators; JSON construction + lenses).
- `Data/Text` (`T.`) — the canonical text surface: vendored Data.Text bodies
  (predicate fns are JIT-safe here; `pack` is the polymorphic `Pack` class).
  `TextFormat` (`TF.`) — case/format/slugify/pad utilities (NOT the canonical
  surface). `Table` (`Tab.`) — CSV/TSV parse + render.
- `FilePath` — System.FilePath over Text (`FilePath = Text`); the file-IO interface.
- `Data/Time` — `UTCTime` newtype (epoch-millisecond, opaque); `formatISO8601` (ISO-8601, pure civil_from_days); `diffUTCTime`/`addUTCTime` (seconds); `epochMillis` escape hatch. `getCurrentTime :: M UTCTime` lives in the generated `Tidepool.Effects` (via `time_decl()` helpers).
- `Aeson/*` — `Value`, `FromJSON`/`.:`/`withObject`, KeyMap, aeson-lens.
- `QQ/*` — `[fmt|]`/`[j|]`/`[patch|]` quasiquoters.

### Structured LLM / Ask — one `Schema` vocabulary

Two primitives share one schema vocabulary; both return a validated `Value` you
extract with optics:

- `ask schema prompt` — SUSPENDS to the calling agent; the reply is validated
  server-side against `schema` before re-entering (invalid replies do NOT consume
  the continuation). No autonomous token burn — the caller answers.
- `llm schema prompt` — AUTONOMOUS server-side model call (one structured call,
  costs tokens); returns `Either LlmError Value` (#335) — no markdown fences.
  Failure is TYPED and TOTAL: `Left (LlmApi _)` on an API/network failure,
  `Left (LlmRefusal _)` on a declined answer, `Left LlmBudget` when the
  per-eval call budget is exhausted — none of these abort the eval. Natural
  spelling: `Right v <- llm schema prompt` or `>>= liftEither`.
- `Schema` ADT (NOT a JSON Value): `SObj [(Text,Schema)] | SArr Schema | SStr |
  SNum | SBool | SEnum [Text] | SOpt Schema`.
- Extract: `v ^? key "f" . _String` (also `_Int`/`_Double`/`_Bool`/`_Array`). E.g.
  `Right v <- llm (SObj [("c", SEnum ["a","b"])]) p; let cat = v ^? key "c" . _String`;
  `ok <- ask (SObj [("ok", SBool)]) "proceed?" <&> (^? key "ok" . _Bool)`.
- Orchestration: let the LLM DECIDE (`SEnum`/`SBool`) and let deterministic code
  EMIT syntax (regex/AST) — models are unreliable at generating domain syntax.

## Adding new Prelude functions

Dictionary polymorphism runs on the JIT: custom classes, multi-param classes,
and GADT type-indexed dispatch all compile and execute — write the polymorphic
version. A few functions carry monomorphic shadows in the Prelude only for
genuine FFI gaps (`round`, `showDouble`); `Opt_FullLaziness` and `Opt_CprAnal`
are disabled in `GhcPipeline.hs`. The JIT-safe surface is enforced end-to-end by
`tidepool-runtime/tests/jit_surface.rs` — add a `works_*` probe when you add a function.
