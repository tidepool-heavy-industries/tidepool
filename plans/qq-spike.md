# QQ Spike (Phase 0): TH splices through the extract pipeline — VERDICT: GO

Date: 2026-06-11. Branch: `root.qq-suite`. Env: GHC 9.12.2, x86_64-linux,
nix dev shell, extract binary built via `cabal build tidepool-extract-bin`,
pipeline at `-O2` with `backend = noBackend`.

> **Update (same day):** basic splice operation needed zero pipeline
> changes, but extraction QUALITY in QQ graphs was silently degraded by
> the driver's TH code-provisioning — root-caused and fixed in
> GhcPipeline.hs. See "The deoptimization bug" below.

## The question

`haskell/src/Tidepool/GhcPipeline.hs:53` sets `backend = noBackend`. TH
splices need an evaluator. Does GHC 9.12's API auto-provision bytecode for
splice-needing modules under `noBackend`, and at what latency cost?

## Answer: splices run AS-IS; extraction quality needed one fix (below).

GHC 9.12's driver (`enableCodeGenForTH` in `GHC.Driver.Make`) detects that a
home module enables `QuasiQuotes`/`TemplateHaskell` and automatically
compiles the splice-needed subgraph of home modules to **bytecode**, even
under `noBackend`. The spoofed `genericPlatform`, SSE/AVX unsets, and
`Opt_FullLaziness`/`Opt_CprAnal` unsets are unaffected — none of the
candidate fallback recipes (`-fprefer-byte-code`, `bytecodeBackend`, prebuilt
package) were needed.

Spike artifacts in `scratch/qq-spike/`:
- `UpperQQ.hs` — minimal quoter (`[upper|hello|]` → `"HELLO"`), combinator
  style (`litE . stringL`), imports base + template-haskell only.
- `UpperTH.hs` + `SpikeHelper.hs` — quoter #2 using **`TemplateHaskellQuotes`
  `[| |]` quotes** whose expansion references a *home module* function
  (`SpikeHelper.shout`) and a package module (`Data.Text.pack`).
- `M1Plain/M2Pragma/M3Import/M4Splice/M5THQuote.hs` — the latency matrix.

Both spike quoters compile, splice, translate, and serialize: `spike_upper`
expands to a plain string literal (18-node CBOR, `unpackCString#` cons-cell
chain — byte-shape identical to hand-written code); `M5THQuote.result`
closes over `shout` + Text internals (3247 nodes) with hygienic `NameG`
references resolving across the splice boundary. **Quoters can therefore be
written hygienically with `[| |]` quotes — no `mkName` fragility — including
references to home modules like `Tidepool.Aeson.Value`.**

## Latency measurements

`tidepool-extract-bin <mod>.hs --include scratch/qq-spike --target result`,
5 runs each (3 for M5), steady-state. Note: each extract run is a fresh GHC
session (no interface files written under `noBackend`), so there is no
cross-run incremental state — "warm" here means OS page cache only. The
very first run after boot pays ~0.5s extra cache warming.

| Case | Pragma | Import quoter | Splice | Time | Δ vs M1 |
|------|--------|---------------|--------|------|---------|
| M1 baseline | – | – | – | ~235ms | – |
| M2 | QuasiQuotes | – | – | ~245ms | **+10ms (noise)** |
| M3 | QuasiQuotes | yes | – | ~620ms | **+385ms** |
| M4 | QuasiQuotes | yes | yes | ~1900ms | +1665ms |
| M5 (TH-quotes quoter, home-module ref) | QuasiQuotes | yes | yes | ~2070ms | +1835ms |

Reading:
- **The pragma is free** (M2 ≈ M1). `QuasiQuotes` can sit unconditionally in
  the eval pragma line.
- **The import alone is NOT free** (+385ms): `runPipeline` compiles every
  home module in the graph through parse/typecheck/desugar/core2core, and a
  quoter module drags template-haskell interface loading with it. An
  unconditional `import Tidepool.QQ` in `build_preamble` would tax every
  no-splice eval ~385ms — violating the "zero regression" gate.
- **A splice costs ~1.3–1.5s on top of the import** (bytecode codegen for the
  quoter subgraph + splice execution). Acceptable: it's paid only by evals
  that actually use QQ, in exchange for compressing N tool calls into one.

## Decisions

1. **Recipe: home-module route.** `Tidepool.QQ` lives in
   `haskell/lib/Tidepool/` like Tidepool.Prelude, found via `importPaths`.
   No cabal package in the package db, no flake.nix toolchain change, no
   GhcPipeline.hs change. (The package route would shave the M3/M4 deltas
   but costs nix/cabal wiring complexity on every toolchain; revisit only if
   QQ latency becomes a measured pain.)
2. **Quoter style: `TemplateHaskellQuotes` + `[| |]` quotes** (hygienic
   `NameG` references, proven by M5). Generated code references
   `Tidepool.Aeson.Value` / `Data.Text` names directly; the *splice site*
   needs no imports in scope. Antiquoted user expressions use `mkName`
   (they SHOULD resolve in the eval's scope — that's the point).
3. **Phase 2 wiring: conditional injection.** `QuasiQuotes` (+
   `ViewPatterns` for the j-pattern side) go in the static pragma string
   constants (free, M2). The `Tidepool.QQ` import is injected into the
   eval's import set **only when the code contains `[fmt|` or `[j|`** —
   detection is exact because GHC's QQ syntax is literally that token (no
   space allowed after `[`). No-splice evals see byte-identical module
   source → zero regression by construction. NOTE: the conditional lives in
   the eval assembly path (`eval()` → `all_imports`), a hair outside the
   "string constants in build_preamble" boundary — escalated to root with
   these measurements before implementation.
4. **Suite regen: `--target-module-only`.** `--all-closed` sweeps binders
   from ALL home modules in the graph; with `import Tidepool.QQ` in
   Suite.hs, quoter internals (TH-library Core, 76KB+ per binding in the
   spike) would land in `suite_cbor/` and flow into the JIT differential —
   bloat + crash risk. An additive Main.hs flag restricts fixture emission
   to the target module's binders. Regen command becomes:
   `cabal run tidepool-extract-bin -- test/Suite.hs --all-closed --include lib --target-module-only`.

## The deoptimization bug (found post-verdict, FIXED)

**Symptom:** with the QuasiQuotes pragma + a quoter home-module import,
even a bare `litd = -2.5 :: Double` in the TARGET module extracted as
`negate @Double $fNumDouble (D# 2.5##)` instead of folded `D# -2.5##` —
dictionary-method Core that chases Integer machinery and dies with
"Unsupported primop: clz#". Affected `--all-closed` (Double/round Suite
fixtures silently SKIPPED) and eval-mode alike, and silently degraded
even "working" splice modules into the JIT's eager-dictionary-error-branch
crash class. Repro matrix M1–M8 in `scratch/qq-spike/` (committed past
the scratch gitignore as evidence).

**Mechanism (two layers, both empirically confirmed via flag probes):**

1. `enableCodeGenForTH` downgrades the splice-needed home modules'
   `ms_hspp_opts` to **-O0 + `Opt_IgnoreInterfacePragmas`** (and picks a
   real backend — NCG on this host — for code provisioning). The
   extraction loop in `runPipeline` re-uses each summary's
   `ms_hspp_opts` → the quoter module extracts unoptimized.
2. **EPS poisoning (the load-bearing layer):** the downgraded modules
   compile FIRST during `load`, so every external interface they demand
   (GHC.Num, GHC.Float, …) is cached in the session-global External
   Package State **without unfoldings** (`Opt_IgnoreInterfacePragmas`
   governs iface loading, and the EPS is load-once per session). The
   later -O2 compile of the target module — and the extraction loop, no
   matter what its dflags say — can then never fire class-op rules:
   `$fNumDouble` has no unfolding anywhere in the session.

**Fix (GhcPipeline.hs, both halves needed):**

- `canonicalizeDFlags` — the backend/opt/gopt pinning factored out of
  session setup and re-applied to each `ms_hspp_opts` (+ the loop's
  `HscEnv` via `hscUpdateFlags`) before extraction. Platform spoofing
  stays session-setup-only: re-pinning bare `genericPlatform` later
  strips the platform constants populated at session init ("Platform
  constants not available!" panic).
- **Conditional EPS flush** after `load`: if any summary shows the
  downgrade (`gopt Opt_IgnoreInterfacePragmas`), reset the EPS IORef to
  `initExternalPackageState` so extraction re-reads interfaces with
  pragmas honored. Conditional ⇒ non-TH runs keep their warm, healthy
  cache — the no-QQ path is byte-identical, zero latency impact.

**Verification (all green):** PlainD control folds; SpikeUse folds
(`litd = D# -2.5##`, `addd = D# 3.75##`) with the splice intact
(`spike_upper = "HELLO"#`); M5 TH-quotes hygiene intact; M6 eval-mode
emits cleanly; full Suite regen (`--all-closed --include lib
--target-module-only`): **186 fixtures, zero skips**, all
`arith_double_*`/`round_*`/`lit_double_*` present; A/B of fixture
name-sets with vs without the QQ import: identical modulo the
pre-existing `_u<uniquekey>` churn (below).

## Out-of-scope issues documented for root (TODOs)

- **aarch64 hosts + QQ:** the driver provisioned splice code via the
  NATIVE code generator on this host. Under the spoofed
  `genericPlatform` (x86_64) that works only because host == x86_64.
  On ARM hosts, QQ evals may need `-fprefer-byte-code` or equivalent.
  Untested; flag if/when an ARM host shows up. Pre-existing property of
  the TH path, not introduced by this branch.
- **suite_cbor staleness (pre-existing, NOT QQ-related):** the
  checked-in `haskell/test/suite_cbor/` predates the #313 externalize
  fix; any full regen renames internal-float fixtures
  (`showIntNeg_1.cbor` → `showIntNeg_u<key>.cbor`) and the `_u<key>`
  suffixes churn on every regen variation. `tidepool-eval`'s
  `haskell_suite.rs` does `include_bytes!` on exact names, so any full
  regen breaks `cargo test -p tidepool-eval`. This branch dodges by
  regenerating ADDITIVELY (copy only new `qq_*` fixtures + merged
  meta). Someone with tidepool-eval write access should own a true
  reconcile (likely: make the include_bytes! names churn-proof, then
  regen wholesale).

## Locked-decision clarification (for reviewers)

"No JSON parsing in Haskell" (PR #144) bans **runtime** parsing capability
in evals — `encode`/`decode` stay gone. The `[j|...|]` quoter contains a
compile-time JSON parser that runs **only during GHC compilation** (inside
the splice evaluator) and emits Value-construction/destructuring Core. No
runtime parsing capability is reintroduced; an eval still cannot parse a
Text it computed at runtime. This does not violate the locked decision.

## Known dialect tradeoffs (documented, accepted)

- With `QuasiQuotes` enabled, `[x|x<-xs]` (list comprehension with no space
  before `|`) parses as a quasi-quote and fails. Mitigated by conditional
  pragma injection if root prefers; otherwise a dialect note ("space before
  `|` in comprehensions"). LLM-written code essentially always has spaces.
- QQ-using evals pay ~+1.7s compile. Cached identically to all evals
  (CBOR cache keyed on source), so repeat calls are unaffected.
