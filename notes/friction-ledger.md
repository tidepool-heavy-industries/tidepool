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
| 10 | `shLines` overflow on large output | FIXED 2026-07-02 (c0197cd): deep_force was already iterative — the real culprit was the EXTERNAL Data.Text `lines` body (non-guarded under the JIT when unfused; the in-expression case survived only via fusion). Vendored `lines`/`words` as guarded corecursion, Data.Text-exact semantics, registry-pinned; 20k-line shLines binds fine | 2026-07-01 |
| 11 | Overflow hint misdirected ("use zipWithIndex instead of [0..]" — a dead scar's advice) | FIXED 2026-07-02: hint now names the real classes (non-tail recursion, or long-list strict-force at bind) with the honest workaround | 2026-07-01 |
| 12 | Runtime `unresolved variable` error carried hex only | FIXED 2026-07-02 (c0197cd): varId→name map rides meta.cbor (`var_names`, optional key, backward-compatible), registered process-globally, rendered in breadcrumb + RuntimeError — errors now say `VarId(0xfe…) = GHC.Internal.List.cycle`-style. ClosedModule record replaced the 5-tuple en route | 2026-07-01 |
| 13 | Reaped continuations lie: resume after TTL says "already spent or never existed" — should say "expired"; suspend response should carry `expires_at` when a TTL is set | mostly mooted by TTL removal (continuation_ttl now None in prod); honest message still right if anyone re-enables it | 2026-07-01 |
| 15 | `writeFile` fails loud on a missing parent directory instead of creating it — agents nearly always want mkdir-p semantics | spawned `fs-writefile-parents` 2026-07-01 | 2026-07-01 |
| 20 | `grepGlob pat "dir"` (bare directory) → silent `[]` — the #8 fix landed in the `grepIn` wrapper, not the underlying verb; same class: `readGlob "dir/**"` matches directories and dies `Is a directory` | Design settled with Inanna (2026-07-01): rg semantics — root arg may be file/dir/glob, dir recurses, NONEXISTENT root = loud error, zero hits = clean `[]`; `Entry {path, kind :: File\|Dir\|Link, size}` record for `glob`; `readGlob` files-only by construction; ONE shared root-resolution fn at the handler so the contract can't drift per-verb | 2026-07-01 |
| 21 | Decl responses under-paint: `{decl:"heatOf"}` omits the inferred type the server had at compile time — cost a `:t` round-trip. Notebook-frame criterion: render every mutation fully, once, at mutation time. Also: an item whose decl leads with imports reports `{decl:"import"}` (head-name picker takes the first token) | audit the response shapes against render-once (with `stale:` field + `:program` repaint, per the coupled-context design conversation) | 2026-07-01 |
| 23 | Decl plane's import surface ≠ stmt plane's: `readGlob` (Tidepool.Orchestrate) and friends are bare in stmt items but need explicit `import Tidepool.Effects` + `import Tidepool.Orchestrate` in decls — docs claim the planes share the base import set. Cost two failed round-trips ×2 tasks | either give decl ModuleEnv the same two imports (check collision story) or fix the docs + add a not-in-scope hint naming the module (the stmt plane KNOWS where the name lives) | 2026-07-01 |
| 24 | Bricking a lib module (appended `explainGhc` using `T.` to Dev.hs, which lacked the import) kills ALL evals incl. the `writeFile` that could fix it — host-side edit is the only unbrick. Known class (memory), now hit live | consider: lib-module writes via a `writeChecked`-style verb that compile-probes the module before landing it; or at minimum the brick error should name the broken lib module first | 2026-07-01 |

| 26 | Kata-sweep finds (2026-07-02 night): vendored FilePath lacks `normalise`; decl-plane compile errors re-print WARNINGS from earlier generations' decls (noise leak, e.g. a `P.head -Wx-partial` from gen-25 glued to every later failure); README.md links a nonexistent LICENSE (needs Inanna: add license or drop link) | normalise: add to Tidepool.FilePath next stdlib pass; warning leak: filter warnings to the CURRENT decl's span in validate_candidate; LICENSE: decision | 2026-07-02 |

## Whole-block / GHCi-environment (2026-07-02 — "The session IS a GHCi environment")

The session now behaves like a GHCi environment, within a block AND across
tool-call turns. Principle: **a pure binding is a declaration**; only effects
are values.

- **M1 decl-batch**: consecutive decl items elaborate as ONE generation
  (`SessionLib::define_batch`) → a sig+binding pair or a mutual-recursion SCC
  split across items typecheck together. Reused the multi-source-per-turn
  render; `run_block` segments consecutive decl/auto runs and optimistically
  batches (falls back to per-item on failure).
- **M2 pure-bind-as-decl**: `let x = e` / `x <- pure e` / `x <- return e` route
  into the decl plane as `x = e`, so GHC generalizes them — `xs <- pure []`
  stays polymorphic (instantiates `[Int]` here, `[Char]` there); `n <- pure 5`
  then `n + 1.5` = 6.5. Falls back to materialize when the RHS references an
  effectful value. Effectful binds materialize once, unchanged.
- **M3 NoMonomorphismRestriction** on the DECL module only (`decl_pragmas()`;
  NOT the eval expr module — there NMR+ExtendedDefaultRules mis-defaults
  `__user = pure …`) so nullary constrained binds (`n = 5`) generalize.
- **Routing fix**: bare expressions take the reference path (Eff-first/pure
  fallback) whenever there's ANY session state — value bindings OR decls.

Follow-ups: type-probe cost (pure-bind-as-decl = define + a separate
`probe_pure_type` compile for `{bound,type}`; fold into `validate_candidate`
to drop the extra extract call — perf wart, not a kludge); cross-plane
shadowing (pure-decl `x` then effectful-value `x`) is the known edge.
Observables UNIFIED (chose "unify" over split): `:bindings`, `:program`,
stale-tracking, and `:i` all surface pure binds (tier `DeclBacked`, module
`Session.Lib.G<g>`) alongside materialized value binds — one coherent
environment view. STATUS: SHIPPED (repl suite 139/0, fmt+clippy clean).

## Whole-block follow-ons (2026-07-02 reload-gate live probes → fixes, commit 4da461a)

Live acceptance after reconnect surfaced three warts, all FIXED principled (not
routed around):

- **define-then-call idiom broke** (`["sq :: T", "sq x = …", "sq 7"]`): the
  batcher segmented on the coarse lexical `Auto` tag, so the trailing bare call
  got swept into the decl batch (`sq 7` is no valid top-level decl → batch
  fails → per-item fallback fails the lone signature). ROOT: `--emit-stmt-binders`
  parsed statement-context ONLY and reported a decl as `"expr"` on parse-failure,
  leaking the decl/expr ambiguity into Rust. FIX: `Binders.classifyTurn` now
  parses BOTH decl + stmt contexts and combines by precedence (bind markers →
  bind; `SigD` → decl [resolves two-faced `sq :: T`]; name-binding `ValD` →
  decl; valid bare expr → expr BEFORE the other-decl catch-all, rejecting the
  binder-less splice `parseDeclaration` over-accepts for `sq 7`/`filter p xs`).
  `TurnClassification.is_decl` drives `run_block` segmentation (`is_decl_shaped`)
  — trailing expr ends the run, no lexical `=`-scan, no compile-shrink-retry.
  GHC's parser is the single authority (same invariant as the bind-vs-expr split).
- **noisy pure-fallback error**: a bare-expr compile failure dumped the Eff-first
  wrap's `Couldn't match … Eff …` scaffold noise (internal `do _r <- __user;
  paginateResult …`) instead of the real cause. FIX: surface the pure-wrap
  (`result = <expr>`) error; drop the `(also failed as a pure value)` suffix
  (both leaked the dual-wrap routing detail — an impl detail users shouldn't see).
- **`succ`/`pred`/`toEnum` unexported**: `map succ "abc"` failed "Variable not in
  scope: succ" (only `P.succ` was reachable). FIX: re-export from `Tidepool.Prelude`
  (JIT-safe — lazy poison closure defuses the `succ maxBound` error branch;
  verified `map succ "abc"` → "bcd" live).

Deploy note: repl uses the dev cabal extract (shim), rebuilt + live after
reconnect; the nix-profile extract (oneshot `tidepool` server) is now behind on
Binders.hs but the oneshot path never classifies, so no functional gap — a
`nix profile upgrade tidepool-extract` is hygiene-only, deferred.

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

## Night-shift wave (2026-07-02, autonomous)

Shipped (all installed, need ONE /mcp reconnect to go live):
- #20 Fs rg-semantics: bare dir recurses, missing root is LOUD, `glob` files-only. Handler test.
- normalise → Tidepool.FilePath (+Prelude); #26 partial.
- Foreign-gen WARNING filter: a gen-K `-Wx-partial` no longer rides every later item's errors (block-level, shared splitter). errmap 14/14.
- Diagnostic dedupe now keys by CONTAINMENT (subset show-se copies collapse even with extra Suggested-fix/gutter lines).
- Repl effects-dir SELF-HEAL per turn (rm -rf ~/.cache/tidepool mid-session no longer bricks until restart; oneshot parity).
- `:program` — replayable-notebook repaint (decls + binds-with-defining-text; round-trips live). The compaction-seam primitive.
- `stale:` field on redefine — names live binds holding the OLD value (notebook display truthfulness). BindingEntry gains defining_expr.

Re-confirmed live (worth the queued fixes):
- #24 (bricking a lib module) hit again: a failed first write left `import Repo` in Library.hs pointing at a broken Repo.hs; the preamble's Library auto-import then failed to compile EVERY eval, including the writeFile that would fix it. Unbrick was host-side (bash). The `writeChecked`-before-landing fix (compile-probe a lib module before splicing its Library re-export) would prevent this whole class — promote from ledger note to a real thread.
- #23 cousin: a `.tidepool/lib` MODULE (not just the decl plane) can't see `readGlob` (it lives in Tidepool.Orchestrate, imported by the eval preamble, NOT re-exported to lib modules). Worked around by using the Fs primitives glob/readFile directly. Fix: either re-export Orchestrate helpers to lib modules or document the split.
- Session bind shadows a lib verb of the same name (correct Haskell scoping, but a footgun when you reuse a name across a session): `buildOrder <- ...` then a lib `buildOrder` verb resolves to the stale tuple. Expected; worth a one-line doc note.
- Test-infra race (NOT a product bug): `rm -rf ~/.cache/tidepool` + parallel `cargo test` processes race on the shared content-addressed effects dir → transient effects_smoke failures (pass in isolation). The self-heal fix hardens the PRODUCT against this exact wipe; the TEST harness should stop sharing / stop wiping mid-run.

Verified live (kata hunt, pre-reconnect binary):
- Grow-your-own-verb loop WITH a self-test gate: wrote `.tidepool/lib/Repo.hs` (crateGraph/topoOrder/buildOrder + embedded `buildOrderT` acceptance check) live; the verb runs on the 17-crate workspace while its own self-test returns True. The "evolving tools over a GHCi session" thesis, made concrete + verified.
- Continuation machinery under REPEATED resume: a 5-suspension git-bisect knot, heap-persistent lo/hi bounds + accumulating step history across every resume — the capability no other tool has. Zero issues.
- Map/Set/deep-recursion (Kahn topological build-order over the real crate graph): solid. The "cycle" it reports is dev-dependencies (Cargo allows those) — a true workspace property, not a bug.

## Open (needs a thread) — night-shift adds

| # | Friction | Notes | Found |
|---|----------|-------|-------|
| 27 | No PURE JSON parser in eval: `parseJson`/`tryParseJson` are both effectful (Rust-side serde). JSONL analytics (parse-per-line in a pure fold) can't be pure — you `mapM parseJson` (an effect call per line, slow at scale) or hand-parse. A pure `decodeJson :: Text -> Maybe Value` would unlock JSONL-as-pure-fold. The aeson-on-jit spike concluded "hand-roll a pure JSON parser (proven on JIT)"; either it was never landed or never exported. | land/export a pure JSON decoder in Tidepool.Aeson; check the spike's hand-rolled parser first | 2026-07-02 |
| 24 | Bricking a lib module bricks ALL evals (Library auto-import) — now DIAGNOSED (lib-brick hint reframes lib-only errors) but not PREVENTED | the real fix is compile-probe-before-land (`writeChecked` for lib modules); diagnosis shipped 2026-07-02 | 2026-06-10 |
| 23c | A `.tidepool/lib` MODULE can't see `readGlob` etc. (Orchestrate-only, not re-exported to lib modules) | re-export Orchestrate helpers to lib modules, or document; use glob/readFile directly meanwhile | 2026-07-02 |
| 31 | Record-dot field naming INCONSISTENT across effect records: `Hit` uses BARE fields (`h.text`/`h.path`/`h.line`) but `Match` (from `sgFind`) uses `match`-PREFIXED (`m.matchText`/`m.matchFile`/`m.matchLine`/`matchVarsList`/`matchReplacement`). Guessing `m.text` (the Hit convention) → `No instance HasField "text" Match` — cost a `:i Match` round-trip. `Commit`/`StatusEntry`/`FileDelta` use bare too, so Match is the outlier. | unify Match to bare fields (`text`/`file`/`line`/`vars`/`replacement`) — DuplicateRecordFields now lets `text` coexist across Hit+Match. BUT the decl string (`tidepool-mcp/src/effect_decls.rs:185`) ↔ SG bridge struct `#[core(name)]` must change together (friction #25 drift class, no compile-time tie, e2e-only) — do with the bridge in view + a live sgFind probe. | 2026-07-02 |
| 30 | SHOW correctness: a negative `Double` in constructor-argument position is NOT parenthesized — `show (Just (-2.5))` → `Just -2.5` (GHC: `Just (-2.5)`). Negative `Int` is CORRECT (`Just (-1)`). Breaks `read . show` round-trips; affects any structure with negative Double fields (records/tuples/Maybe/lists). ROOT: `emitShowDoubleSpecBody` (Translate.hs) replaces `$fShowDouble_$sshowSignedFloat` with `\showPos p d rest -> ShowDoubleAddr d ++ rest`, DROPPING the real `showSignedFloat`'s `showParen (p > 6)` when `x < 0`. FIX: emit the conditional — `if d < 0 && p > 6 then '(' : ShowDoubleAddr d ++ ')' : rest else ShowDoubleAddr d ++ rest` (needs unbox of boxed `d`/`p`, DoubleLt + IntGt primops, char-cons for parens, NCase; SIGILL-risky, test hard). ShowDoubleAddr already emits the sign, so only the parens are missing. | 2026-07-02 |
| 28 | `HasField`-constrained record-dot helper pure bind (`let f h = h.path`, `let f h = T.toUpper h.path`) defines fine on the WARM live server (DeclBacked) but FAILS `define` COLD in the test harness (same 4da461a binary + same dev extract) → falls to the materialize path, whose `pure f` can't monomorphize the unresolved `HasField` constraint → `No instance for HasField`. Ruled out: my NMR-probe change (fails identically stashed), session state (fresh live session binds fine). Suspect: `define`/closed-Core extraction of a constrained (dictionary-free) binding depends on PROCESS warmth (prior compiles seeding the DataConTable / effect-machine bootstrap). A fully-open `HasField "path" r a` (both r,a free) fails even warm. Record-dot is a CORE idiom → worth root-causing. | repro: cold test, define a plain fn first (warm up) THEN the constrained helper — if it then binds, it's a process-warmth/bootstrap ordering bug in `SessionLib::define`'s Core extraction. Related to the empty-type-display (now fixed via NMR probe, commit pending). | 2026-07-02 |
| 29 (fixed) | Record-dot helper pure binds showed EMPTY type (`{type:""}`) — the type probe ran under the eval preamble's MR and couldn't monomorphize the `HasField` constraint → probe failed → blank. Also `n <- pure 5` showed a defaulted `Int` not the true polymorphic type. | FIXED (commit pending): `probe_pure_type` now compiles in an NMR context (`to_nmr_pragmas`), mirroring the decl module the bind lives in → generalized types show (GHCi parity: `n :: Num a => a`). Guard: `pure_numeric_bind_type_generalizes_in_display`. | 2026-07-02 |

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
