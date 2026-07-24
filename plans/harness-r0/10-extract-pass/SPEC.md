# Spec: extract-side `returnControl` pass (type capture at yield sites)

Goal: `v <- returnControl @Verdict "reconcile the verdicts bound as vs"`
suspends the session and the suspension carries `"Verdict"` (pretty-printed,
post-elaboration) so the harness can render the hole and declare
`resume :: Verdict -> …` in the answerer's scope. Extract rejects
polymorphic or function-bearing `T` with an error naming the site.

## ANTI-PATTERNS

- DO NOT rewrite the `object [...]` payload expression in Core — it is
  runtime-computed by the JIT; extract never constructs it. The ONLY Core
  surgery permitted is the head-swap in step 3 (one Var swap + one literal
  App node).
- DO NOT correlate sites by source-occurrence order — branches and loops
  make runtime dispatch order diverge from source order. The site-id must
  travel IN the request value.
- DO NOT build a decl-closure renderer or a forcing-mode classifier — cut
  from R0 (children inherit scope; `DeclLog` holds decl source text).
- DO NOT add a new CLI mode — the sidecar rides the NORMAL extraction
  invocation (`processFile` already writes multiple outputs).
- DO NOT forget: `*.cbor` fixtures are gitignored — `git add -f` new ones.
- DO NOT run full `scripts/battery.sh` (operator policy 2026-07-23:
  targeted tests per leaf; battery is a root-level gate). The GHC-tier
  subset in VERIFY is the required spot check.

## READ FIRST

- `haskell/CLAUDE.md` (rebuild steps, fixture regeneration)
- `haskell/src/Tidepool/Translate.hs`:
  - the `tagToEnum#` interception arm (~1462–1476) — the exact pattern for
    detecting a known Var applied to `[Type ty]` + value args
    (`typeArgs = filter (not . isValueArg) allArgs`)
  - `closeTyCons`/`tyConsOfType` (952–984) — NOT needed for this pass, but
    read to understand the type-walking idiom
- `haskell/src/Tidepool/GhcPipeline.hs`: `capturedUserType` (459–464) and
  `renderType` (~511) — the proven `renderWithContext defaultSDocContext
  (ppr ty)` pretty-print pattern to reuse
- `haskell/app/Main.hs`: `processFile` (~132) and `writeWholeModuleClosed`
  (~302–331) — where `<name>.cbor` + `meta.cbor` are written; the sidecar
  is a third output here
- `tidepool-mcp/src/effect_defs.rs` lines 569–623 — the Ask effect
  definition; helpers are Haskell source strings spliced into the preamble
- `tidepool-repl/src/ask.rs::extract_ask_request` (143–170) — how the
  payload reaches the suspension `meta` (the harness dispatcher mirrors
  this; Rust-side merge target)

## MECHANISM (the chosen design — do not re-derive)

Core is TYPED all the way through `translate` (erasure is a serialization
fact, not a pipeline stage). The pass runs inside `translate`:

1. **Surface verb** (effect_defs.rs `ask_effect_def!` helpers):
   `returnControl :: forall a. Text -> M a` — body sends `AskWith` with a
   placeholder payload and coerces the response. Also a hidden sibling
   `returnControlSited :: forall a. Int -> Text -> M a` whose body embeds
   its Int arg in the payload:
   `returnControlSited sid p = unsafeCoerce <$> send (AskWith p (object ["typedSite" .= sid]))`.
   (`returnControlFork` — same shape, `"fork" .= True` in the payload; the
   harness decides child-spawn policy, the program only requests.)
2. **Detection**: in `translate`, match applications headed by the
   `returnControl`/`returnControlFork` Var with `[Type ty]` in the
   type-args position (mirror the tagToEnum# arm).
3. **Head-swap rewrite** (the one permitted Core synthesis): replace the
   head Var with the `*Sited` sibling and prepend a fresh site-id literal
   arg. One Var substitution + one `NLit` int + one `NApp` node — the
   FlatNode constructors used everywhere in this file.
4. **Checks, then record**: on each detected site —
   - `tyCoVarsOfType ty` non-empty → extract ERROR naming the enclosing
     binder + rendered type ("polymorphic returnControl site").
     (`tyCoVarsOfType` is a new one-line import from `GHC.Core.TyCo.FVs`,
     the module `tyConsOfType` already comes from.)
   - type contains a function arrow anywhere (recursive walk with
     `splitFunTy_maybe`/`splitTyConApp_maybe`, visited-set like
     `closeTyCons`) → extract ERROR ("function-typed answers not supported
     in R0").
   - else append `{site, type}` to the pass state.
5. **Sidecar**: `writeWholeModuleClosed` writes `asks.json` next to
   `meta.cbor`: `[{"site": <u32>, "type": "<rendered>"}]`. Empty list when
   no sites — always write the file (loud absence beats silent missing).
6. **Rust merge**: the harness's ask dispatcher (mirroring
   `extract_ask_request`) reads `typedSite` from the payload, looks up the
   sidecar entry loaded alongside the compiled artifact, and attaches the
   type string to the published hole. (This half lands in segment 30; the
   sidecar format above is the contract — coordinate via the contract file
   in 00-scaffold.)

Both pipeline entry points (`runNormalPipeline`, `runSessionPipeline`)
share `translateModuleClosed` + `Main.hs` write-out, so the pass runs on
both for free — verify, don't duplicate.

## VERIFY

- New fixtures in `test/suite_cbor` regenerated per haskell/CLAUDE.md
  (`git add -f`).
- Positive: monomorphic data-kinded sites (simple ADT, record, nested,
  session-declared type) produce correct sidecar entries; site-ids in
  payload match sidecar keys under branches (`if c then returnControl @A …
  else returnControl @B …`) and inside a `mapM`-loop body.
- Negative: polymorphic site and function-bearing site each fail extract
  with an error naming the site; error text asserted in tests.
- Targeted GHC-tier spot check (NOT full battery — root runs that at
  merge if needed): rebuild extract per haskell/CLAUDE.md, then
  `cargo nextest run --ignore-default-filter -p tidepool-runtime -p tidepool-macro`
  and `cargo nextest run -p tidepool-repr`.

## DONE

`returnControl @T` at any statement or expression position yields a
suspension whose payload carries a site-id resolving to the rendered `T`;
bad sites fail at extract, not at runtime; targeted suites green.
