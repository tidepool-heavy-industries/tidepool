# Shared typed-site classifier: implementation plan (read-only investigation, not built)

Open question first: the cached generated `Tidepool/Effects/Core.hs` has no
`*Sited` definitions; locate where `runLLMTurn`/`finalize` siblings come from
and confirm their constraint shape before implementing.

## New module `haskell/src/Tidepool/SiteClassifier.hs` (pure)
- `SitePlan { spSpec, spSibling, spTypeArgs (every forall in order),
  spEvidence (constraint dictionaries), spRest (remaining args, 0..n),
  spLiteralIndex (= foralls + constraints), spAnswer, spInputs }`
- `SiteFailure = MissingTypeArgument Int | MissingEvidence Int |
  OpenSiteType SiteTypePosition Type | CastInSpine | MissingSibling`
- `classifySiteOccurrence :: Map String Id -> VerbSpec -> Id -> [CoreExpr] -> Either SiteFailure SitePlan`
- `stripNospec`, `renderSiteFailure`.
- Literal index: `splitInvisPiTys (idType surface)`; leading `Named` binders
  (nT) must be `Type` args, then invisible constraint binders (nC) must be
  value args. Rewrite `mkApps (Var sibling) (types ++ dicts ++ [I# lit] ++ rest)`,
  well-typed for any remaining arity. Siblings keep the surface foralls and
  constraints and insert `Int` after them (`Agent.hs:273-288`,
  `Actor.hs:303-317`, `Unfold.hs:461-474`); check once per sibling with
  `eqType` in `resolvePreparedSiblings`.
- `stripNospec`: move from `Translate.hs:2395-2400`, match exactly
  `nospec : Type _ : f : rest`, keep `rest` verbatim (today's version filters
  value args and strips casts, ill-typed for prepared Core); strip ticks only,
  a cast is `CastInSpine`.
- Answer/inputs: `vsAnswerSource (TypeArgument i)` and `vsInputTypeArgs`
  index `spTypeArgs` (existing indices stay valid); delete unused
  `AppliedResultType`. Only answer/input types must be closed; an open `effs`
  row still rewrites.
- Remove `vsMisShapeIsError` (`EffectSchema.hs:63`, table rows, `child`),
  `PreparedSites.hs:139-147`, `Translate.hs:1958-1980`, `splitTrailingArgs`
  (:2362), `leadingTypes` (:2771); `vsTypeArgs`/`vsValueArity` removed or
  assertions.

## Consumers
- Prepared `rewriteApplication` (`PreparedSites.hs:98-137`): strip `nospec`
  when the head is a recognized verb, rewrite args, classify; `Right` builds
  the site, `Left` records a `SiteRejection` for every recognized verb
  (including missing sibling, which passes silently today at :101); also
  classify a bare `Var` verb (:80) with `[]`.
- Translate (:1894-1956): `Right` lowers `NVar sibling`, `NApp` dictionaries,
  literal, `filter isValueArg spRest`; `Left` keeps the unseeded poison branch
  (:1924) or errors with `renderSiteFailure` (after reachability pruning).
  `checkSiteType`/`checkSiteInputType` replaced by the plan's types.

## Fixtures (`haskell/test-prepared-stg/Main.hs`), stub `Member` class and toy `Eff`
1. Saturated `request @Bool @Int ref asg`: rewrite; literal equals `ysSite`.
2. Open tail `Member Replies effs => Eff (Other ': effs) …` for `request` and
   `runLLMTurn`: assert `nospec` present first; rewrite; site id not 0.
3. Eta-reduced `askBool = request @Bool @Int` (NOINLINE): rewrite, Core Lint,
   projects.
4. `mapM (fork @Bool)`: exactly one site; projects.
5. Bare `request @Bool @Int @effs` in an effs-polymorphic helper: rewrite;
   `@a` variant: deferred "result type is unresolved", unrelated binding
   projects.
6. Explicit `nospec` with a statically known instance: rewrite on prepared and
   legacy `runPipeline` paths.
7. Existing `child @Bool @Int @Char @String` now rewrites; change its assertion
   to expect projection. `MissingTypeArgument` only as a unit test.

## Risks
- Site ids hash origin and ordinal (`PreparedSites.hs:220-227`); newly
  rewritten sites shift later ordinals, so persisted ids and recovery journals
  go stale.
- Nested-site ordinal order differs (prepared numbers arguments first,
  Translate the outer site first); unify on post-order.
- One occurrence under `mapM`/recursion shares a site id; confirm runtime
  settlement (`receive`/`serve`) accepts it.
- Core Lint on sibling/surface drift (guarded by `eqType`); ticks around the
  head.
- A rejection in a `Rec` group applies to the group's top binder.
- Surface stubs still pass literal `0` (`Agent.hs:278`, `Actor.hs:307`,
  `Unfold.hs:466`); switch to a bottom as `Fork.hs:65` and `Actor.hs:336` do.
