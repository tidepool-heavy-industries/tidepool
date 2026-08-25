# haskell/ — Tidepool Haskell harness + stdlib

**Charter.** Belongs: the GHC→Core extractor (`tidepool-extract`) and the
eval stdlib (`lib/Tidepool/*`, auto-imported in the MCP server). Does NOT
belong: Rust-side toolchain resolution/deploy handshake
(`tidepool-runtime::toolchain`), CBOR reading (`tidepool-repr`).

The GHC→Core extractor (`tidepool-extract`) and the eval stdlib
(`lib/Tidepool/*`, auto-imported in the MCP server). See the repo-root
`CLAUDE.md` for the project map and locked decisions.

## Rebuilding the Haskell Toolchain

After changing `haskell/` code (Translate.hs, GhcPipeline.hs, Prelude, etc.).

**Package layout.** `cabal build` (no target) builds only `tidepool-extract-bin`,
the production executable. Its `Main.hs` depends on the internal library
`tidepool-extract-internal` (`hs-source-dirs: src`), which holds the extractor
implementation (`Tidepool.Binders`, `.GhcPipeline`, `.Session`, `.Translate`,
`.Resolve`, `.FatIface`, `.CborEncode`, `.DiagJson`, `.Timing`, `.Json`) — compiled ONCE
and shared by the production binary and the four `src`-dependent test-suites
below, instead of once per component. Four non-production components exist as
`test-suite` stanzas, so `cabal build` skips them by default:

| Component | Purpose | Run |
|---|---|---|
| `spike-extract` | scratch pipeline spike | `cabal test spike-extract` |
| `session-c-test` | session-binder acceptance pin | `cabal test session-c-test` |
| `varid-mechanism-test` | `stableVarId`/`fieldParentDisamb` contract pin | `cabal test varid-mechanism-test` |
| `extract-fidelity-test` | erasure symmetry, recognizer qualification, unboxed-tuple arity — through the real pipeline | `cabal test extract-fidelity-test` |

`cabal build --enable-tests` builds all four without running them. Each
`.cabal` stanza carries its own `Run:` comment; this table just indexes them.

## Toolchain resolution — extract, stdlib, and the deploy handshake

**One locator, `tidepool-runtime/src/toolchain.rs`,** owns both precedence
tables below plus the startup handshake. Its module docstring is the source of
truth — read it before touching either table or the handshake mechanism; what
follows here mirrors it.

**Precedence: the extract binary.**

| # | Source | Notes |
|---|--------|-------|
| 1 | `$TIDEPOOL_EXTRACT` | Explicit override, and STRICT: set-but-unreadable is a hard error, never a silent fall-through to `$PATH` — falling through would run a different binary than the caller believes it is running. |
| 2 | `tidepool-extract` on `$PATH` | Normally `~/.nix-profile/bin/tidepool-extract`, a **nix wrapper** that prepends the with-packages GHC (supplies `lens`) to PATH and `exec`s the `tidepool-extract-bin` binary **in the nix store**. |

Deploying a new extract means updating that nix profile entry (see below) —
copying a binary under `~/.local/bin` or `~/.cargo/bin` does nothing, as the
nix-profile entry is earlier on PATH.

**Precedence: the Haskell stdlib source root.** The stdlib root is the GHC
include dir under which `Tidepool/Prelude.hs` lives; a candidate without that
file does not count as a hit.

| # | Source | Rationale |
|---|--------|-----------|
| 1 | `$TIDEPOOL_PRELUDE_DIR` | Operator override. **Set-but-not-a-stdlib-root is a hard error**, never a silent fall-through — a typo'd override that quietly served a different stdlib is exactly the failure this locator exists to kill. |
| 2 | `./haskell/lib`, then `./lib` (walked upward from CWD) | In-repo development: the working tree you are editing wins over anything installed. |
| 3 | Sibling of the extract's `dist-newstyle` | Walks `$TIDEPOOL_EXTRACT` up to a `dist-newstyle` component and takes its sibling `lib/` — pairs a worktree-built extract with that worktree's stdlib. |
| 4 | The stdlib embedded in the server binary | Installed mode: materialized to a content-addressed cache dir at startup. Immutable and guaranteed to match the binary. |
| 5 | The source tree the binary was built from | Last resort (`CARGO_MANIFEST_DIR`-derived) — keeps a repo-installed `tidepool-repl` working when launched outside the repo. |
| — | otherwise | error, listing every path tried. |

### Deploy handshake

Extract, both servers, and the stdlib must move together
(`scripts/redeploy.sh`). This is checked at startup, not left to script
discipline:

- `scripts/redeploy.sh` finishes (after clearing `~/.cache/tidepool/`) by
  running `tidepool --write-toolchain-stamp`, which fingerprints the CONTENT
  of the extract binary and the stdlib tree it just deployed and writes a
  stamp (default: `<cache_dir>/toolchain-stamp.json`, override with
  `$TIDEPOOL_TOOLCHAIN_STAMP`).
- Each server calls the handshake once at startup: it fingerprints the
  extract + stdlib it just resolved and compares them to the stamp. A
  mismatch means one side moved without the other, and fails loud, naming
  `scripts/redeploy.sh` as the fix.
- Severity is `$TIDEPOOL_TOOLCHAIN_HANDSHAKE`: `error` (default — a skew
  aborts startup), `warn` (log and continue), or `off` (skip the check
  entirely). **`warn` is the escape hatch** for deliberately running a
  mismatched pair — e.g. the Local iteration workflow below, pointing
  `TIDEPOOL_EXTRACT` at a worktree build while running an otherwise-installed
  server. No stamp on disk (nothing ever deployed via `scripts/redeploy.sh` on
  this machine, or the cache was hand-cleared) is informational only, not a
  skew.

**The cross-worktree Haskell cache is nix, not `dist-newstyle`.** Patched GHC
and every dependency derivation (`base`, `lens`, `cborg`, …) are
content-addressed in the nix store and shared through Cachix — that's what
makes a from-scratch `cabal build` fast on a box with many active worktrees:
the compiler and every boot/Hackage package it needs are already built and
shared, only this package's own modules compile. `dist-newstyle` is mutable
per-worktree build state (object files, the local plan) and must NEVER be
shared between active worktrees — two `cabal` processes writing the same
`dist-newstyle` race each other's build lock and cache files. Each worktree
gets its own `dist-newstyle` (gitignored) for free by virtue of being a
separate checkout; don't "optimize" this into a shared directory.

**Local iteration — test against a worktree build (no deploy):**

```bash
cd haskell && cabal build tidepool-extract-bin      # build in dist-newstyle
# Point tests/evals at it; with-packages GHC on PATH supplies lens.
# A new binary fingerprint forces a cache miss.
TIDEPOOL_EXTRACT=$(cabal list-bin tidepool-extract-bin) \
  PATH=/nix/store/<hash>-ghc-native-bignum-9.12.2-with-packages/bin:$PATH \
  cargo test -p tidepool-runtime ...
```

Running a worktree-built extract against an otherwise-installed server (rather
than a `cargo test` process) will trip the deploy handshake — the extract you
just pointed at was not deployed via `scripts/redeploy.sh`, so it won't match
the stamp. Set `TIDEPOOL_TOOLCHAIN_HANDSHAKE=warn` in that case to log the skew
and continue instead of aborting startup.

**Deploy to the live MCP server (nix profile):**

`scripts/redeploy.sh` encapsulates this full dance (extract + both Rust servers + cache clear + deploy stamp) — prefer it over running these steps by hand.

```bash
git add haskell/...                          # nix flake builds see only TRACKED files
nix profile upgrade tidepool-extract         # rebuild + install the wrapper+harness
# (or: nix profile install .#tidepool-extract for a first install)
rm -rf ~/.cache/tidepool/                     # clear stale cached CBOR
tidepool --write-toolchain-stamp              # record what was just deployed (see Deploy handshake above)
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

> Do NOT prune `*_t<n>.cbor` lifted-local fixtures here the way
> `regen-corpus.sh` does for `test/corpus_cbor` (older extracts named these
> `*_u<n>.cbor` — a raw-Unique suffix; `Tidepool.GhcPipeline.externalizeInternalTops`
> now mints a stable per-module ordinal instead, see its doc comment). This
> suite replays the test suite's own bindings, where lifted locals compare
> cleanly and carry ~75 of the differential's compared fixtures; dropping them
> takes `compared` under `haskell_suite_differential`'s `COMPARED_FLOOR`. The
> corpus harness drops them because it replays real modules, where a lifted
> local runs outside the call site that gives it meaning.

> `*.cbor` fixtures are gitignored — new ones must be `git add -f`'d or a
> `suite_*!` test won't compile on a fresh checkout.

## Diagnostics — Haskell-extract knobs (separate process)

Env-gated, OFF by default. For the JIT-runtime / effect-machine / cache knobs see
`tidepool-codegen/CLAUDE.md`.

| Knob | What it shows | Reach for it when |
|------|---------------|-------------------|
| `TIDEPOOL_DUMP_CLOSED=<needle>` | Closed Core for bindings whose binder name matches needle | Inspecting what Core the JIT actually receives for a binding |
| `TIDEPOOL_VARID_AUDIT=1` | VarId collision report (distinct binders → same 64-bit id) | SIGILL/case-trap hunts; ruling out id collisions |
| `TIDEPOOL_VARID_AUDIT=<hex>,<hex>` | Resolves specific VarIds to source names: binding sites render with their enclosing top-level binder and unique, reference-only ids (externals, DataCon workers, dangling refs) as the plain qualified name | Naming the function a JIT trace implicates |
| `TIDEPOOL_DANGLING_DEBUG=1` | `[DANGLING NVAR]` line per id the emitted program references but nothing binds — **including** the legit `Tidepool.Session.Val.*` repl-session class the hard failure subtracts | An `unresolved_var_trap` at runtime, or auditing which danglings a repl turn legitimately carries |
| `TIDEPOOL_JOINREC_DEBUG=1` | joinrec-translation forensics (`[313-joinrec]` spew) | Join-point conversion bugs (jumps compiled as calls, wrong continuation) |
| `TIDEPOOL_IFACE_DEBUG=1` | `[fat-iface]` interface-loading trace | Missing unfoldings / "unresolved external" mysteries |
| `TIDEPOOL_TEST_DROP_DC=<module-qualified-name>` | D1 mutation-test fault injection: `recordDC` silently skips recording exactly the one constructor whose qualified name matches | Proving the D1 hard-fail metadata-subset defense (`Main.assertMetaCoversEmitted`) actually fires — never set outside `extract-fidelity-test` |
| `TIDEPOOL_TEST_FORCE_VALIDATION_ONLY=<module-name>` | E6 mis-tiering fault injection: the `OptimizeCoreReachable` tier (`normalVariant`) forcibly denies `core2core` (the -O2/exposed-unfoldings pass) to the one named home module, regardless of what the real Core-reachability closure (`GhcPipeline.reachableModuleClosure`) found | Proving E6's tier detects a module the target actually needs being wrongly denied optimized Core — never set outside a deliberate detection-power test |

## Resident compile daemon (Phase 0, `--daemon`)

`tidepool-extract-bin --daemon --socket <path> [--rotate-after N]
[--rss-ceiling-mb M] [--watch-stamp <path>]` runs the SAME binary as a
long-lived process serving compile requests over a UNIX domain socket
instead of exiting after one compile — plans/compile-daemon-design.md is the
design (read its Decisions section first; §7 records every point where the
implementation deviated from the doc's literal wording, with the reasoning).
Opt-in only: unset `$TIDEPOOL_EXTRACT_DAEMON_SOCKET` (every existing caller)
is byte-identical to today, both in behavior and — per this lane's own
integration test — in the exact CBOR/diagnostics bytes produced.

**Module boundary.** `Tidepool.DaemonServer` owns ALL transport (socket
bind/accept, the length-prefixed frame codec, stdout/stderr capture,
request-count rotation, the RSS-ceiling backstop, the toolchain-stamp watch,
every clean-exit path) and knows nothing about GHC. `Tidepool.GhcPipeline`
gains `withResidentPipeline` — boot one `runGhc` session once, hand back a
`runPipelineSession`-shaped IO closure that reuses the warm `ModIfaceCache` +
per-module `GutsMemo` across calls — and knows nothing about sockets.
`app/Main.hs`'s `--daemon` handling is exactly two things: parse the daemon
flags, and build a `DaemonServer.RequestHandler` closure that wraps its own
existing argv dispatch (`runOneInvocation`/`dispatch` — the SAME functions
the CLI entry point calls, just parameterized over which compiler closure to
use) so a daemon-served request runs the identical code a spawned process
would, with `exitWith` turned into a returned `ExitCode`.

**The env/cwd contract.** The daemon's own process environment (
`$TIDEPOOL_GHC_LIBDIR`, `TIDEPOOL_DUMP_CLOSED`/`TIDEPOOL_VARID_AUDIT`/etc.
from the Diagnostics table above, `$TIDEPOOL_BUILD_PRODUCTS_DIR`) is fixed
once at daemon boot — a per-request `--build-products-dir` flag over the
wire is a no-op under the daemon (the resident session's `DynFlags` are
already set; see `withResidentPipeline`'s haddock). `argv` on the wire is
the CLIENT's own argv, exactly what it would have spawned with — including
its OWN `--include`, which the resident API applies PER CYCLE (patched onto
the live session's `importPaths` via `hscUpdateFlags`, not
`setSessionDynFlags`, which is too expensive to pay every request). The
request also carries the client's `cwd`; the daemon's single worker
`setCurrentDirectory`s to it before each cycle (safe — one worker,
serialized), so relative-path semantics match a spawned process exactly.

**Isolation.** The shared `GutsMemo` is populated by whatever a request's
own import closure touches (warmed lazily, not swept up front) but every
cycle's OWN target/session module names — and any `Tidepool.Session.*` name
— are stripped from the shared memo immediately after that cycle
(`GhcPipeline.sanitizeMemo`), so a later request compiling the same
`ModuleName` (`__result`, `Tidepool.Session.Val.G1`, ...) never observes a
stale entry. See design §2.2/§2.3 and §7's deviation note for why this is a
post-hoc filter rather than the doc's literal `mMemoRef = Nothing` per
request — that literal reading would also have disabled reads of the
already-warmed stdlib entries, defeating the daemon's purpose.

**New dependency.** `tidepool-extract-internal` (the `src/` library) now
depends on the `network` Hackage package (resolved the same way `QuickCheck`
already is — pinned index-state, not the with-packages nix GHC closure) for
`DaemonServer`'s UNIX-domain-socket transport. `tidepool-extract-cmd`
(the Rust client) stays a std-only, zero-dependency leaf — see its own
CLAUDE.md.

**Testing.** `Tidepool.DaemonServer`'s frame codec is pinned in
`extract-fidelity-test`'s `Fidelity.DaemonCodec` group (pure, no extract
compile). The transport-equivalence, isolation, relative-cwd, and rotation
checks live on the Rust side —
`tidepool-extract-cmd/tests/daemon_integration.rs` — since that is where the
client (and the real `ExtractCmd::run()` gating) lives; see that crate's
CLAUDE.md.

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
- `Aeson/*` — `Value`, `FromJSON`/`.:`/`withObject`, KeyMap, aeson-lens, and
  `Schema` (`JsonSchema` — the JSON Schema OF the generic `ToJSON`/`FromJSON`
  encoding, derived from the same `Generic` metadata and sharing their
  `GAllFieldsNamed` compile-time rejection; this is what agent tool
  `input_schema` and subagent `outputSchema` publish).
- `QQ/*` — `[fmt|]`/`[j|]`/`[patch|]`/`[uri|]` quasiquoters, all defined
  entirely in this stdlib (not shipped with GHC). A new stdlib module needs no
  build-time registration to be eval-importable: `lib/` is a pure runtime
  asset tree (the extract binary resolves `haskell/lib` as a GHC include path
  at runtime, and `tidepool-extract-bin`'s import closure reaches only
  `tidepool-extract-internal`, never `lib/`), so nothing there is
  host-compiled or needs listing. Gated by
  `works_stdlib_quoter_survives_extract` and
  `stdlib_quoter_bad_input_fails_loudly_at_compile_time`
  (`tidepool-runtime/tests/jit_surface.rs`, built on `[uri|]`): a stdlib
  quoter survives the pipeline splice-and-all, and a malformed quote fails at
  COMPILE time rather than misparsing silently.
- `Form` — the operator-input surface: `askUser :: DerivedForm a => M a`
  (`askUser @T` derives the form from `T`'s own `Generic` representation) plus
  `choose`/`chooseMany` for alternatives that exist only as runtime VALUES.
  Sub-modules: `Form.Shape` (the `FormShape` algebra),
  `Form.GForm` (the generic interpreter — shape out, typed value back),
  `Form.Check` (compile-time `TypeError` diagnostics), `Form.Wire` (the JSON
  transport, matching `tidepool-harness`'s `selfharness::operator` module
  docs byte for byte). Reachable ONLY when `AskUser` is in the compiling row
  (it builds on `askUserRaw`), and auto-imported whenever it is.

- `Agent/*` — surfaces (provisional): `Contract` (mode-
  interpreted endpoint records compiled to declarations and dispatch),
  `Spawn` (typed `spawnAgent`/`spawnAgentWithTools`; compiles
  only in rows containing Subagent + Worktree, same row-gating as Form).
  **The parent serves the child's tool calls, and the loop that does it is
  Haskell.** `spawnAgentWithTools rounds tools spec` runs `compileTools`
  ONCE, declares the result through `agentBeginRaw`, and then answers each
  parked call with the authored record's OWN handler — in the parent's `M`,
  performing ordinary parent effects — before `agentResumeRaw` drives the
  turn on. The parent is never suspended while a handler runs; what is
  parked is the child's request, on the far side of the seam (the loop lives
  here because a `Tool`'s `handler :: input -> m output` is parent Haskell
  that Rust cannot run).
  Three things are answered as REFUSALS rather than dispatched, so the child
  always finishes its turn: a name absent from `declarations` (`dispatch`'s
  own fallthrough `error`s, which would abort the eval with the turn still
  parked), a call past the `ToolRounds` cap (resident POLICY — distinct from
  the runtime's `MAX_TOOL_ROUNDS` catastrophe backstop), and — before any
  process is spawned — a tools record that does not compile
  (`SpawnDriveFailed StageAllocating`). `spawnAgent` keeps its signature and
  IS this loop at zero tools (`NoTools`), so there is no second
  implementation to drift. Gates:
  `tidepool-handlers/tests/subagent_tool_loop.rs` (the whole loop on the real
  extract/JIT against `MockBackend`) and `subagent_one_cycle.rs` (the
  no-tools vertical).
  **There is no model codec.** A model reads and writes ordinary JSON through
  the vendored `ToJSON`/`FromJSON` generic defaults (TaggedObject wire for
  payload sums, bare constructor-name strings for enums, `Maybe` fields
  absent-or-null; `tidepool-runtime/tests/generic_recursive_sums.rs`), so a
  result type needs only `deriving (Generic, FromJSON, JsonSchema)`. Decode
  errors are the plain vendored ones (`key "caveats" not present`);
  snake_case normalization and JSONPath-carrying errors are deliberately
  absent and are not coming back.

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
`tidepool-runtime/tests/jit_surface.rs` — add a `check` line to the relevant
stdlib family bundle when you add a function (e.g. a new `Data.Text` shadow
goes in `works_text_family`), or a standalone `works_*`/`fails_loudly`
probe when your addition falls into one of that file's documented exclusion
classes (compile-fail assertions, real-effect dispatch, a distinct compiler
mechanism rather than a stdlib function, or render-fidelity across the outer
JSON boundary — see the module doc at the top of `jit_surface.rs`).

## Known Limits / Gotchas

**Read this list as COMPLETE, not as samples of a larger danger.** The
frontend is real GHC and extraction is post-typecheck, so there is no
mechanism for a language construct GHC accepts to fail here on language
grounds — operator/if/composed expressions in quasiquote holes, custom
typeclasses, GADT dispatch, multi-param classes with fundeps, foldM,
newtype-deriving arithmetic, record-dot sections, and their relatives all
compile and run (probed 12/12, 2026-08-25). Caution is legitimate only at
the named seams below and in the bridge/schema seams documented at their
own homes (WHNF-only `Value` at the bridge; sums at a spawn/schema root).
Write the idiomatic version first; if something new fails, it is a
substrate bug to report or a limit to add HERE with its mechanism — never
a reason for diffuse conservatism.

**A failing generated module reports its own diagnostic, not a downstream
cascade — recovery order is topological.** `GhcPipeline.hs`'s diagnostic-
recovery pass (`normalVariant`'s `cpSummaries`) redoes each module's own
`parseModule`/`typecheckModule`, independently of the earlier `load'` call, in
DEPENDENCY order (`topSortModuleGraph` + `flattenSCCs` — the same idiom
`sessionVariant` already used one seam down). A module with a genuine failure
of its own — e.g. a generated `Tidepool.Effects` SHIM whose `type M` row names
an unresolved type — is always reached before anything that imports it, so its
real diagnostic fires first and stops the loop there; a downstream importer's
own typecheck (which would otherwise choke on the failing import with GHC's
generic "attempting to use module X which is not loaded") is never reached.
Since stable-effects-core (`tidepool-mcp/CLAUDE.md`'s section of that name)
split the generated surface in two, the SHIM shape above is specifically about
the tiny per-agent-session half (the only one that still declares `type M`) — the
stable `Tidepool.Effects.Core` half has no row to fail on. The harness also
still sidesteps this for pinned `Finalize` rows by probe-compiling the
generated SHIM module STANDALONE first (`EngineConfig::turn_target`, memoized
per module content), independent of this fix. Gated by
`Fidelity.TopoRecovery` (`extract-fidelity-test`): a minimal two-module
mask reproduction plus the shim-shaped unresolved-name case.

**Manual repro of GHC-heavy tests needs the with-packages GHC on PATH.** A
bare `nix develop` shell reproduction with the wrong GHC on PATH fails with
missing `lens`/`freer-simple` package errors that look like the bug under
investigation. Check `which ghc` resolves to the with-packages derivation
before trusting any manual failure.

**`Map.` (`Data.Map.Strict`) breaks knot-tied self-referential folds.**
`Data.Map.Strict.mapWithKey` forces each value into WHNF *during construction*,
which blackholes (infinite loop / "thunk forces itself") a lazy fixed point
like `memo = Map.mapWithKey (\_ deps -> 1 + sum [Map.findWithDefault 0 d memo |
d <- deps]) g` — even on fully acyclic input. This is not a cycle-detection
issue; it happens regardless of whether `g` is a DAG. Fix: `import qualified
Data.Map.Lazy as ML` and use `ML.mapWithKey` for that specific call — lazy Map
preserves the thunk chain so the self-reference can resolve once, on demand.
`Map.` stays strict by default for the usual reason (predictable space
behavior on ordinary lookups/inserts); reach for `Data.Map.Lazy` specifically
for knot-tying, not as a general substitute.

