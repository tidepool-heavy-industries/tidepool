# Friction Ledger

Every ergonomic wart hit during real tidepool/tidepool-repl use lands here — spawned, queued, or explicitly wontfix. Silent workarounds are protocol violations (see memory: never-route-around-friction). Newest first within each status.

## Fixed (merged to ghci-session)

| # | Friction | Fixed by | Merged |
|---|----------|----------|--------|
| 1 | `$sunion` dangling NVar (`fromListWith Set.union` collision path → runtime hex trap) | `dangling-sunion` — syntactic `exprFreeVarKeys` (no stale IdInfo), varId-keyed dedup/reachability, LOUD dangling error naming symbols; guards `repro_dangling_spec` (3/3 green vs merged extract) | 2026-07-01 |
| 3 | Contract drift: docs taught `run :: M (Int, Text, Text)`; live is `M Proc`; record-dot idiom taught nowhere | `contract-truing` — CLAUDE.md "Eval Records API" section + resources.rs/effect_decls.rs/repl server.rs strings | 2026-07-01 |
| 4 | `:i Proc` blind to stdlib types | `repl-introspect/repl-info` — `:i` resolves stdlib/library sources (fields+types verbatim, `source: stdlib/session`, shadowing works, miss carries hint); live-session tests | 2026-07-01 |
| 5 | Truncation stubs not fetchable | `repl-introspect/repl-stubs` — truncation moved Rust-side (`truncate.rs`), subtrees stashed on session, `:stub <n> [page]` retrieves in full, hint self-teaches | 2026-07-01 |
| 6 | Lost-session error unexplaining | `repl-introspect/repl-lost-session` — error states server restart, process-scoped sessions, reopen+redeclare guidance | 2026-07-01 |
| 13a | Parked-ask 30 min reap (silent knot death) | root direct (e01c07e) — `continuation_ttl: None` in prod, `wedged_ttl` split keeps dead-turn sweep | 2026-07-01 |
| 14 | `session_run` debug-noisy response shape (double-encoding, counters, decl internals, duplicated value) | `slim-run-output` — slim default shape, `verbose: true` escape hatch, docs + simplified tests | 2026-07-01 |
| 16 | `Dev.sh`/`shLines` swallowed exit codes — a failed command looked like an empty success | `verb-library-polish` — nonzero exit now errors loud (code + stderr head); `shProc` escape hatch; plus 100% verb haddock coverage + PATTERNS.md truing | 2026-07-01 |
| 15 | `writeFile` failed on missing parent dirs | `fs-writefile-parents` — mkdir-p at the handler (sandbox-safe: canonicalize deepest existing ancestor, `starts_with` root check on full path), both surfaces | 2026-07-01 |
| 7 | `Schemes.histogram` was RLE (consecutive-only) with a pre-sorted docstring example | `contract-truing` — histogram/rle split, unsorted examples | 2026-07-01 |
| 8 | `Dev.grepIn` silent-empty (glob was non-recursive) | `contract-truing` — appends `/**` for recursive search | 2026-07-01 |
| 9 | `vocab` fossils (`Probe.t1..t11`, `MechDemo`) in every session's prompt | `contract-truing` — atticed, Library re-exports removed | 2026-07-01 |

> Deploy note: instruction strings are compiled into the server binaries — changes
> land for live sessions only after `cargo install --path tidepool` + repl binary
> rebuild + MCP reconnect. Deferred until the remaining repl threads merge (one
> restart instead of three; restart kills live sessions).

| 18 | Compile errors doubled on the wire: extract printed GHC's log-action diagnostics AND `show se` (the fix was already described in a comment at Main.hs:242 but never landed) | root direct — `Compilation failed.` terse marker only (haskell/app/Main.hs, both sites); verified live | 2026-07-01 |
| 19 | Error coordinates lie: `Expr.hs:35:8` / `G2.hs:29:17` for item line 2 — every error taxed a mental remap; tmp paths leaked | root direct — `tidepool_runtime::session::errmap::remap_generated_coords`, anchored to the generated module's path suffix ONLY (per decl-error-mapping revert spec; panic-backtrace + embedded-suffix fixtures). Bind/expr/multi-bind/ref/`:t` planes via marker offsets (`__user =`/`result = do`) → `<item>:l:c`; decl plane via `RenderedModule.body_line` (+`hoisted_lines` skip-guard) → `<decl>:l:c`; gutter renumbering width-padded so carets stay aligned. 130/130 repl suite green | 2026-07-01 |

| 25 | ALL node-returning LSP ops dead on BOTH servers since fdc82ce (2026-06-30 phase-2 records): the decl renamed the constructor `Node`→`LspNode` but the handler bridge kept `#[core(name = "Node")]` → every `lspWhere`/`lspCallers`/… failed `Unknown DataCon name: Node (arity 6)` at stream conversion. Invisible because LSP e2e tests skip without a daemon | root direct — attr → `LspNode`; found by differential probe (repl vs oneshot failed identically → not a parity gap). GUARD GAP remains open: decl `type_defs` constructor names ↔ bridge `core(name)` attrs have no compile-time tie and no always-on e2e — the docs-derived-from-types idea now has a third drift class (decl↔bridge) | 2026-07-01 |
| 22 | `llm` effect dead on the repl server: `.tidepool/secrets/OPENAI_API_KEY` exists on disk but only the ONESHOT server loaded secrets into env — the repl binary never got the wiring (parity gap) | root direct — loader hoisted to `tidepool_runtime::paths::load_secrets()` (returns a report; binaries log), called from BOTH mains; oneshot's local copy deleted | 2026-07-01 |

## Spawned (fix thread in flight)

| # | Friction | Thread | Found |
|---|----------|--------|-------|
| 2 | `Hit.path`/`Doc.path` VarId collision — RECLASSIFIED: only reproduces on the stale pre-b4e0f8c deployed binary; current source already disambiguates. Thread finishing as an honest hardening (FldName-namespace disambiguation, single mechanism) + labeled canary, no overclaim | `varid-selector-collision` (TL, finishing) | 2026-07-01 |

## Reverted (re-land with fixes)

- **decl-error-mapping** (item-relative GHC error coordinates): REVERTED 2026-07-01 (revert of 8c4abb1). Two defects: (a) the path rewriter matches ANY `*.hs:L:C` token, mangling GHC-internal panic backtraces (`compiler/GHC/Utils/Panic.hs` → `[wrapper] Panic.hs`); (b) `decl_plane::record_syntax_selectors_localized` regressed — the rewrite interferes with the decl-plane error contract (extract SourceError surfaced as "Uncaught exception"). Re-land requirements: rewriter anchored to the generated module's basename ONLY; the failing decl_plane case as a fixture; full repl suite green including decl_plane.

## Open (needs a thread)

| # | Friction | Notes | Found |
|---|----------|-------|-------|
| 10 | `shLines` stack-overflows on large output — ROOT-CAUSED 2026-07-02: not the verb; Tier-0 `deep_force` at BIND time walks the result spine with host recursion → ~15k-element ceiling (20k fails at bind; identical processing INSIDE one expression returns 20000 fine). Fix: iterative deep_force (explicit worklist), plans/stack-safety.md allocation | mechanism scoped; hint text fixed meanwhile (#11) | 2026-07-01 |
| 11 | Overflow hint misdirected ("use zipWithIndex instead of [0..]" — a dead scar's advice) | FIXED 2026-07-02: hint now names the real classes (non-tail recursion, or long-list strict-force at bind) with the honest workaround | 2026-07-01 |
| 12 | Runtime `unresolved variable` error carries hex VarId only — could name the symbol via the meta table | partially mooted if #1 makes dangling loud at compile; keep for other unresolved classes (`cycle`) | 2026-07-01 |
| 13 | Reaped continuations lie: resume after TTL says "already spent or never existed" — should say "expired"; suspend response should carry `expires_at` when a TTL is set | mostly mooted by TTL removal (continuation_ttl now None in prod); honest message still right if anyone re-enables it | 2026-07-01 |
| 15 | `writeFile` fails loud on a missing parent directory instead of creating it — agents nearly always want mkdir-p semantics | spawned `fs-writefile-parents` 2026-07-01 | 2026-07-01 |
| 20 | `grepGlob pat "dir"` (bare directory) → silent `[]` — the #8 fix landed in the `grepIn` wrapper, not the underlying verb; same class: `readGlob "dir/**"` matches directories and dies `Is a directory` | Design settled with Inanna (2026-07-01): rg semantics — root arg may be file/dir/glob, dir recurses, NONEXISTENT root = loud error, zero hits = clean `[]`; `Entry {path, kind :: File\|Dir\|Link, size}` record for `glob`; `readGlob` files-only by construction; ONE shared root-resolution fn at the handler so the contract can't drift per-verb | 2026-07-01 |
| 21 | Decl responses under-paint: `{decl:"heatOf"}` omits the inferred type the server had at compile time — cost a `:t` round-trip. Notebook-frame criterion: render every mutation fully, once, at mutation time. Also: an item whose decl leads with imports reports `{decl:"import"}` (head-name picker takes the first token) | audit the response shapes against render-once (with `stale:` field + `:program` repaint, per the coupled-context design conversation) | 2026-07-01 |
| 23 | Decl plane's import surface ≠ stmt plane's: `readGlob` (Tidepool.Orchestrate) and friends are bare in stmt items but need explicit `import Tidepool.Effects` + `import Tidepool.Orchestrate` in decls — docs claim the planes share the base import set. Cost two failed round-trips ×2 tasks | either give decl ModuleEnv the same two imports (check collision story) or fix the docs + add a not-in-scope hint naming the module (the stmt plane KNOWS where the name lives) | 2026-07-01 |
| 24 | Bricking a lib module (appended `explainGhc` using `T.` to Dev.hs, which lacked the import) kills ALL evals incl. the `writeFile` that could fix it — host-side edit is the only unbrick. Known class (memory), now hit live | consider: lib-module writes via a `writeChecked`-style verb that compile-probes the module before landing it; or at minimum the brick error should name the broken lib module first | 2026-07-01 |

## Scar-tissue audit (2026-07-01 — live probes on the deployed server)

Inanna's theory ("accumulated gotcha memory is stale scar tissue around since-fixed bugs; what's real is a bug list") — verdict: **mostly correct**. Every behavioral flinch in the memory corpus probed live:

| Scar | Verdict | Action |
|------|---------|--------|
| `read` unsupported → parseInt crutch | **STALE-ish**: JIT works (registry-pinned, golden un-ignored); Prelude just never re-exported it | Fixed #26: `read`/`Read` exported from Tidepool.Prelude |
| `zip [1..]`/`[0..]` → zipWithIndex crutch | **STALE** (live: works) | memory rewritten |
| infinite `filter`/transforms diverge | **STALE** (live: `take 4 (filter even [1..])` works) | memory rewritten |
| `T.takeWhile` section predicates corrupt | **STALE for usage** (direct + session-decl wrapper live-green); Prelude-shadow retirement still guarded | memory rewritten |
| Integer→Double / big literals broken | **STALE** (±2^100 and 1e308 live-correct; the review-branch fixes landed via integration) | memory rewritten |
| bind names shadowing Prelude (`tail`) error | **STALE** (live: works) | drop from repl CLAUDE.md friction list when next touched |
| `pub fn $NAME($$$ARGS)` silently unreliable | **HEALED INTO UX**: handler now rejects signature-shaped patterns with a teaching error | memory note |
| `cycle` unresolved | **WAS REAL — FIXED 2026-07-02** (ebc3be0): not Resolve — the LetRec emit dropped self-captures of floated corecursive value knots; now knot-tied via promised captures + pending_capture_updates. Registry re-pinned `works_cycle_value_knot` | Known-Limit deleted; the diagnosis chain (differential probe → hex instability → DUMP_CLOSED → VARID_AUDIT) took ~40 min |
| multi-line QQ +2 indent corrupts payload | **WAS REAL — FIXED 2026-07-02** (73bf2c6): wrappers now embed user text BYTE-VERBATIM inside explicit-layout brackets (no indent transform exists to corrupt anything); error columns became exact as a side effect | principled fix per Inanna's push against the lexer-lite scanner |
| `[fmt\|]`/`[j\|]` not in repl scope at all | **FIXED 2026-07-02** (73bf2c6): per-turn token-gated Tidepool.QQ import in the repl stmt plane, same gating as oneshot | |
| assoc-list over Data.Map in lib code (closed-Core bloat) | unprobed (compile-size claim, needs measurement) | keep, low priority |

## Measured (2026-07-01, interface metrology trial)

- **~6.0s floor per session item** (`tb <- getCurrentTime` alone = 6.1s; decl+timestamp pair = 11.8s). Each item shells the extract (bind = classify + compile + inner-type probe); the per-(session,generation) cache salt means in-session items can never cache-hit. Inanna's call: acceptable while leverage is high (one census item replaces ~6-8 read/grep round-trips + inference); optimize when leverage is proven. Direction when we do: resident GHC service in the extract, and/or drop the salt for pure re-references.
- Remap coverage gap (by design, v1): an item whose decl contains hoisted imports/pragmas skips coordinate remap (`hoisted_lines` guard) — raw `G<n>.hs` path shows. Exact mapping through hoist positions is possible if it bites often. Combined with #23 (imports often needed in decls), it bites more than expected — bump priority if #23 resolves toward explicit imports.
| 14 | `session_run` return shape is debug-noisy: `generation`/`valGeneration` counters, `index`, decl internals (`module: Tidepool.Session.Lib.G<n>`, `defined:true`), and `items[].result` is DOUBLE-ENCODED (a JSON string containing JSON); last item's result duplicates top-level `value` | spawn after repl-stubs merges (same rendering code). Slim shape: per-item just `{bound,type}` / `{decl}` / `{error}` inline (no string-encoding); top-level `{value, type}`; internals behind `--debug` or a `verbose` param | 2026-07-01 |

## Filled at the verb layer (stdlib promotion candidates)

| # | Gap | Interim fill | Found |
|---|-----|--------------|-------|
| 17 | Time mirror round-trip asymmetry: `formatISO8601` shipped without `parseISO8601`, but `Commit.date` is ISO-8601 text (git `%cI`) — any recency math needs the parse direction | `.tidepool/lib/Churn.hs` — `parseISO8601` + `daysFromCivil` (Hinnant inverse) + `hotspots n k` verb; verified round-trip incl. pre-1970. Promote parse+inverse into `Tidepool.Data.Time` with golden tests next stdlib pass | 2026-07-01 |

## Notes from live use (not defects)

- Two-plane inference asymmetry (diagnosed live 2026-07-01, Inanna prompted the correction): `let` binds MATERIALIZE heap values via a synthetic `pure x`, so they must be monomorphic — a record-dot lambda (`let heat = \c -> c.date…`) fails with an opaque `HasField r0` error regardless of MR. Bare DECLS generalize normally: `heatOf now c = … c.date …` (no signature) compiles polymorphic and resolves at the use site, GHCi-style — and can reference session binds (`now`) across the plane boundary. Idiom: polymorphic helpers → decl plane; `let` → concrete values only. **Fix worth a thread**: bind-plane error hint — unsolved constraint arising at the synthetic `pure` should say "session binds are materialized values (monomorphic); drop the `let` to make this a declaration, which generalizes — or annotate."
- **WIN**: `.tidepool/lib` live-reloads mid-session — a running session wrote `Churn.hs` + spliced the `Library.hs` re-export via `writeFile`/`T.replace`, then called `hotspots` in the next item. No reset, no reconnect. The grow-your-own-vocabulary loop is real.

## Design questions (need Inanna)

- ~~**Continuation TTL vs the knot thesis**~~ RESOLVED 2026-07-01 (Inanna: "cut the 30m TTL"): parked asks never expire in prod (`continuation_ttl: None`); wedged sweep kept at 30 min via new `wedged_ttl` (commit e01c07e). Knots may park indefinitely.

## Queued (affordance ideas awaiting the bench)

- Docs-derived-from-types: generate tool descriptions/vocab/contract docs from signatures at build time — kills the #3 drift class systemically.
- `:promote <decl>` (session decl → lib module with dependency cone) + usage-count fitness; the sedimentation ladder.
- Provenance-tracked bindings + `:refresh` — re-derive downstream bindings after redefinition (the reactive session).
- `:uses <decl>` — typed blast radius of a redefinition.
- `:conts` — list parked continuations with their ask prompts (a lattice of knots needs visibility).
- Executable done-criteria for exo spawns (`acceptance :: M Bool` gating merge).
- Session save/load as workshop templates (declarations + provenance, not heap).

## Wontfix / accepted for now

- Heap durability across server restarts — out of scope by design; sessions are process-scoped, declarations are the replayable skeleton (#6 makes the error say so).
