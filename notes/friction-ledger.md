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

## Spawned (fix thread in flight)

| # | Friction | Thread | Found |
|---|----------|--------|-------|
| 2 | `Hit.path`/`Doc.path` VarId collision — RECLASSIFIED: only reproduces on the stale pre-b4e0f8c deployed binary; current source already disambiguates. Thread finishing as an honest hardening (FldName-namespace disambiguation, single mechanism) + labeled canary, no overclaim | `varid-selector-collision` (TL, finishing) | 2026-07-01 |

## Reverted (re-land with fixes)

- **decl-error-mapping** (item-relative GHC error coordinates): REVERTED 2026-07-01 (revert of 8c4abb1). Two defects: (a) the path rewriter matches ANY `*.hs:L:C` token, mangling GHC-internal panic backtraces (`compiler/GHC/Utils/Panic.hs` → `[wrapper] Panic.hs`); (b) `decl_plane::record_syntax_selectors_localized` regressed — the rewrite interferes with the decl-plane error contract (extract SourceError surfaced as "Uncaught exception"). Re-land requirements: rewriter anchored to the generated module's basename ONLY; the failing decl_plane case as a fixture; full repl suite green including decl_plane.

## Open (needs a thread)

| # | Friction | Notes | Found |
|---|----------|-------|-------|
| 10 | `shLines` stack-overflows on ~4.6k-line command output (`git log` 3-month window) — low ceiling for a line-splitting verb; suspect non-tail split/marshal | held until `contract-truing` merges (Dev.hs boundary overlap) | 2026-07-01 |
| 11 | Runtime overflow hint says "use zipWithIndex/imap instead of [0..]" even when the recursion is inside a lib verb — misdirects the repair | minor; bundle with next repl-errors thread | 2026-07-01 |
| 12 | Runtime `unresolved variable` error carries hex VarId only — could name the symbol via the meta table | partially mooted if #1 makes dangling loud at compile; keep for other unresolved classes (`cycle`) | 2026-07-01 |
| 13 | Reaped continuations lie: resume after TTL says "already spent or never existed" — should say "expired"; suspend response should carry `expires_at` when a TTL is set | mostly mooted by TTL removal (continuation_ttl now None in prod); honest message still right if anyone re-enables it | 2026-07-01 |
| 15 | `writeFile` fails loud on a missing parent directory instead of creating it — agents nearly always want mkdir-p semantics | spawned `fs-writefile-parents` 2026-07-01 | 2026-07-01 |
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
- Multi-line quasiquotes corrupt under the +2 indent — known; route multi-line payloads via the `input` lane (memory: eval-code-indent-and-quoter-names).
