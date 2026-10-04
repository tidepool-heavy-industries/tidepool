module Tidepool.PreparedSites
  ( buildYieldSite
  , PreparedSite(..)
  , SiteAuthority
  , resolveSiteAuthority
  , resolveRequestSiteTyCon
  , SiteRejection(..)
  , elaboratePreparedSites
  , lookupPreparedVerb
  , resolvePreparedSiblings
  , resolvePreparedInterfaceSiblings
  , requestReplyIndex
  ) where

import Control.Monad.State.Strict
import Data.Bits ((.&.), xor)
import Data.List (find, nub)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Word (Word64)
import GHC.Core
import GHC.Core.Subst (cloneBndrs, mkEmptySubst, substExpr)
import GHC.Core.FVs (exprFreeVars)
import GHC.Types.Var.Env (mkInScopeSet)
import GHC.Core.Make (mkCoreConApps)
import GHC.Builtin.Types (intDataCon, intTy, mkListTy, mkPromotedListTy, liftedTypeKind)
import GHC.Core.TyCo.Rep (Type(..), Scaled(..))
import GHC.Core.TyCo.Subst (substTyWith)
import GHC.Core.Coercion
  ( Role(Nominal, Representational), mkSymCo, mkUnbranchedAxInstCo )
import GHC.Core.Utils (mkCast)
import GHC.Data.FastString (fsLit)
import GHC.Types.Unique.Supply (UniqSupply, initUs, mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Core.Type
  ( mkTyConApp, splitTyConApp_maybe
  , isLiftedTypeKind, typeKind )
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon (TyCon, isNewTyCon, newTyConCo, tyConArity, tyConDataCons, tyConRoles, tyConTyVars)
import GHC.Core.DataCon
  ( DataCon, dataConOrigResTy, dataConName, dataConWorkId, dataConWrapId_maybe
  , dataConInstOrigArgTys, dataConUnivTyVars, isVanillaDataCon )
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_home_unit, lookupType)
import GHC.Types.TyThing.Ppr (pprTyThingInContext)
import GHC.Types.TyThing (TyThing (..))
import GHC.Iface.Type (ShowForAllFlag (..), ShowHowMuch (..), ShowSub (..))
import GHC.Types.Literal (LitNumType (..), Literal (..))
import GHC.Types.Name (isSystemName, nameModule_maybe, nameOccName, nameUnique)
import GHC.Types.Name.Occurrence (mkTcOcc, mkVarOcc, occNameString)
import GHC.Types.Id (Id, idName, mkSysLocal)
import GHC.Types.Var (varType)
import GHC.Utils.Fingerprint (Fingerprint (..), fingerprintString)
import GHC.Utils.Outputable (SDocContext(sdocSuppressUniques), defaultSDocContext, ppr, renderWithContext)
import GHC.Data.Maybe (MaybeErr(Succeeded, Failed))
import GHC.Types.PkgQual (PkgQual(NoPkgQual))
import GHC.Iface.Env (lookupOrig)
import GHC.Iface.Load (importDecl)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Home (isHomeUnit)
import GHC.Unit.Finder (FindResult(Found), findImportedModule)
import GHC.Unit.Module (mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModIface (mi_module)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Tc.Utils.Monad (initIfaceLoad)
import GHC.Types.Unique (getKey)
import Tidepool.CheckedCell (captureCheckedTypeWitness, captureRequestTypeSignatures)
import Tidepool.SiteClassifier
import Tidepool.EffectSchema
import Tidepool.Identity (binderQualName)
import Tidepool.TypePolicy
  ( TypeGraph, TypeGraphBuilder, TypeNodeId, emptyTypeGraphBuilder
  , finishTypeGraph, internType, modulesOfType
  , nominalHeadsOfType
  )

data SiteAuthority = SiteAuthority
  { requestSiteTyCon :: Maybe TyCon
  , responseResultTyCon :: Maybe TyCon
  , progressStateTyCon :: Maybe TyCon
  , protectedProgressIds :: Set.Set Word64
  , trustedProgressOwners :: Set.Set Word64
  }

-- | Resolve wrapper authority from each type's defining module. The real GHC
-- TyCon crosses into evidence; rendered spelling never carries authority.
resolveSiteAuthority :: HscEnv -> IO SiteAuthority
resolveSiteAuthority env = do
  requestSite <- resolveRequestSiteTyCon env
  responseResult <- exactTyCon "Tidepool.Agent.Reply.Internal" "ResponseResult"
  progressState <- exactTyCon "Tidepool.Agent.Reply.Internal" "ProgressState"
  replies <- exactTyCon "Tidepool.Agent.Reply.Internal" "Replies"
  watches <- exactTyCon "Tidepool.Agent.Watch.Internal" "Watches"
  let rawIds = concatMap protectedConstructors [replies, watches]
      helpers =
        [ (vsSitedModule spec, vsSitedName spec)
        | spec <- sitedVerbs, vsName spec `elem` progressVerbs ]
  sited <- traverse (uncurry exactId) helpers
  sourceOwners <- traverse (exactId "Tidepool.Actor.Source") ["installSource", "attachSource"]
  pure SiteAuthority
    { requestSiteTyCon = requestSite
    , responseResultTyCon = responseResult
    , progressStateTyCon = progressState
    , protectedProgressIds = Set.fromList (map idKey (rawIds ++ [i | Just i <- sited]))
    , trustedProgressOwners = Set.fromList
        (map idKey (rawIds ++ [i | Just i <- sited ++ sourceOwners]))
    }
 where
  -- Resolve the defining module's interface and ask its declaration loader
  -- for the real TyCon. This follows neither re-export spellings nor names
  -- declared by the cell.
  exactTyCon moduleName occurrence = do
    found <- findImportedModule env (mkModuleName moduleName) NoPkgQual
    case found of
      Found _ owner -> do
        name <- initIfaceLoad env (lookupOrig owner (mkTcOcc occurrence))
        loaded <- lookupType env name
        case loaded of
          Just (ATyCon tycon) -> pure (Just tycon)
          Just _ -> pure Nothing
          Nothing -> do
            thing <- initIfaceLoad env (importDecl name)
            pure $ case thing of
              Succeeded (ATyCon tycon) -> Just tycon
              Succeeded _ -> Nothing
              Failed _ -> Nothing
      _ -> pure Nothing

  idKey = getKey . nameUnique . idName
  progressVerbs = ["reportRequestProgress", "pollProgress", "awaitProgressAfter", "awaitAnyProgress", "progressSource"]
  protectedConstructors Nothing = []
  protectedConstructors (Just tycon) =
    [ identifier
    | constructor <- tyConDataCons tycon
    , occNameString (nameOccName (dataConName constructor)) `elem`
        ["PublishProgressWith", "ObserveProgressWith", "ObserveWatchProgressWith"]
    , identifier <- dataConWorkId constructor : maybe [] (:[]) (dataConWrapId_maybe constructor)
    ]
  exactId moduleName occurrence = do
    found <- findImportedModule env (mkModuleName moduleName) NoPkgQual
    case found of
      Found _ owner -> do
        name <- initIfaceLoad env (lookupOrig owner (mkVarOcc occurrence))
        loaded <- lookupType env name
        case loaded of
          Just (AnId identifier) -> pure (Just identifier)
          Just _ -> pure Nothing
          Nothing -> do
            thing <- initIfaceLoad env (importDecl name)
            pure $ case thing of
              Succeeded (AnId identifier) -> Just identifier
              _ -> Nothing
      _ -> pure Nothing

-- Resolve the defining declaration through GHC's module/interface authority.
-- Projection compares this exact TyCon, never its rendered module spelling.
resolveRequestSiteTyCon :: HscEnv -> IO (Maybe TyCon)
resolveRequestSiteTyCon env = do
  found <- findImportedModule env (mkModuleName "Tidepool.Internal.RequestSite") NoPkgQual
  case found of
    Found _ owner -> do
      name <- initIfaceLoad env (lookupOrig owner (mkTcOcc "RequestSite"))
      loaded <- lookupType env name
      case loaded of
        Just (ATyCon tycon) -> pure (Just tycon)
        Just _ -> pure Nothing
        Nothing -> do
          imported <- initIfaceLoad env (importDecl name)
          pure $ case imported of
            Succeeded (ATyCon tycon) -> Just tycon
            _ -> Nothing
    _ -> pure Nothing

data PreparedSite = PreparedSite
  { psOwner :: Id
  , psSite :: YieldSite
  , psDelivery :: SiteDelivery
  , psWireNode :: TypeNodeId
  , psInputNodes :: [TypeNodeId]
  }

data ElaborationState = ElaborationState
  { esUniques :: UniqSupply
  , esCounters :: !(Map T.Text Word64)
  , esSites :: ![YieldSite]
  , esPreparedSites :: ![PreparedSite]
  , esTypeGraph :: !TypeGraphBuilder
  , esRejections :: ![SiteRejection]
  }

-- | A typed site that cannot carry concrete evidence, attached to the top
-- binder containing it. Whether it is an
-- error depends on the projected target: projection raises it only when that
-- binder's body is executed by the program, so an unrelated helper never
-- rejects a program that does not use it.
--
-- Ownership by top binder is sound because elaboration runs on tidied Core:
-- the top-level binders are final, and CorePrep/STG preparation keeps a site
-- application inside the closure of the top binder that contained it.
data SiteRejection = SiteRejection
  { srBinder :: Id
  , srMessage :: String
  }

-- | Rewrite typed surface sites onto their exact generated siblings while
-- Core still carries types. The caller supplies siblings collected from
-- tidied home-module guts in dependency order. A polymorphic site is left
-- unrewritten and recorded as a 'SiteRejection' for its top binder.
--
-- Every occurrence of a prepared verb is classified exactly once: an
-- application head with its arguments (after 'stripNospecSpine'), and a bare
-- reference (an eta-reduced alias, a higher-rank argument, a verb under a
-- cast or tick) with none, which leaves its result type open. No verb reference leaves
-- elaboration without either a rewrite or a recorded rejection; projection
-- then raises only the rejections its executable closure reaches.
-- Surface bodies must delegate to their sited siblings with an unavailable
-- site argument, preserving the sibling's returning behavior. OPAQUE prevents
-- inlining but not demand analysis: a whole-body error would mark callers as
-- bottoming before this pass replaces the call.
elaboratePreparedSites :: HscEnv -> SiteAuthority -> Map String Id -> [CoreBind]
  -> IO ([CoreBind], [YieldSite], [PreparedSite], TypeGraph, [SiteRejection])
elaboratePreparedSites env authority siblings bindings = do
  uniques <- mkSplitUniqSupply 's'
  (bindings', final) <- runStateT (traverse rewriteBind bindings)
        (ElaborationState uniques mempty [] [] emptyTypeGraphBuilder [])
  pure ( bindings'
       , reverse (esSites final)
       , reverse (esPreparedSites final)
       , finishTypeGraph (esTypeGraph final)
       , reverse (esRejections final))
  where
    rewriteBind (NonRec binder rhs) =
      NonRec binder <$> rewriteExpr (binder, binderQualName binder) rhs
    rewriteBind (Rec pairs) = Rec <$> traverse (\(binder, rhs) ->
      (binder,) <$> rewriteExpr (binder, binderQualName binder) rhs) pairs

    rewriteExpr origin expression = case expression of
      Var surface | Just _ <- lookupPreparedVerb surface -> rewriteApplication origin expression
      Var identifier
        | getKey (nameUnique (idName identifier)) `Set.member` protectedProgressIds authority
        , not (getKey (nameUnique (idName (fst origin))) `Set.member` trustedProgressOwners authority) -> do
            modify' (\current -> current
              { esRejections = SiteRejection (fst origin)
                  ("raw or site-aware progress operations require compiler-issued typed helper evidence: " ++ T.unpack (snd origin))
                  : esRejections current })
            pure expression
      Var{} -> pure expression
      Lit{} -> pure expression
      Type{} -> pure expression
      Coercion{} -> pure expression
      Cast body coercion -> (`Cast` coercion) <$> rewriteExpr origin body
      Tick tick body -> Tick tick <$> rewriteExpr origin body
      Lam binder body -> Lam binder <$> rewriteExpr origin body
      Let binding body -> Let <$> rewriteNested binding <*> rewriteExpr origin body
      Case scrutinee binder resultType alternatives ->
        Case <$> rewriteExpr origin scrutinee <*> pure binder <*> pure resultType
          <*> traverse rewriteAlt alternatives
      App{} -> rewriteApplication origin expression
      where
        rewriteNested (NonRec binder rhs) = NonRec binder <$> rewriteExpr origin rhs
        rewriteNested (Rec pairs) = Rec <$> traverse (\(binder, rhs) ->
          (binder,) <$> rewriteExpr origin rhs) pairs
        rewriteAlt (Alt con binders rhs) =
          Alt con binders <$> rewriteExpr origin rhs

    rewriteApplication origin expression = do
      let (headExpr, arguments) = stripNospecSpine (collectArgs expression)
      rewrittenArguments <- traverse (rewriteExpr origin) arguments
      case headExpr of
        Var surface | Just spec <- lookupPreparedVerb surface -> do
          let (topBinder, originName) = origin
          case classifySiteOccurrence siblings spec surface rewrittenArguments of
            Left failure -> do
              modify' (\current -> current
                {esRejections = SiteRejection topBinder
                  (renderSiteFailure (T.unpack originName) spec failure) : esRejections current})
              pure (mkApps headExpr rewrittenArguments)
            Right plan -> do
              case do
                wire <- siteWireType authority spec (spAnswer plan)
                derived <- case vsDerivedInput spec of
                  Nothing -> Right []
                  Just (index, source) -> case drop index (spTypeArgs plan) of
                    inputType : _ -> (:[]) <$> siteWireType authority
                      (spec { vsWireSource = source }) inputType
                    [] -> Left "derived site input names a missing type argument"
                requestTypes <- case (vsWireSource spec, vsDelivery spec) of
                  (ResponseResultEvidence, DeliverExitCellFill) -> do
                    progress <- requestProgressType spec (spInputs plan)
                    Right (Just (spAnswer plan, progress))
                  _ -> Right Nothing
                carrier <- maybe (Left "missing RequestSite type authority") Right (requestSiteTyCon authority)
                -- Injection applies exactly two type arguments and one boxed Int.
                -- Prove that ABI even when the defining module was captured anew.
                let validKinds = case tyConTyVars carrier of
                      [inputsVariable, replyVariable] ->
                        eqType (varType inputsVariable) (mkListTy liftedTypeKind)
                          && eqType (varType replyVariable) liftedTypeKind
                      _ -> False
                if isNewTyCon carrier then Right ()
                  else Left "RequestSite constructor ABI must be a newtype"
                if tyConArity carrier == 2 then Right ()
                  else Left ("RequestSite constructor ABI must have exactly two indices; found "
                    ++ show (tyConArity carrier))
                if tyConRoles carrier == [Nominal, Nominal] then Right ()
                  else Left "RequestSite constructor ABI must have two nominal roles"
                if validKinds then Right ()
                  else Left "RequestSite constructor ABI indices must have kinds [Type] and Type"
                carrierConstructor <- case tyConDataCons carrier of
                  [constructor] | isVanillaDataCon constructor
                    , length (dataConUnivTyVars constructor) == 2 -> Right constructor
                  _ -> Left "RequestSite constructor ABI must have one constructor without hidden evidence"
                let reply = case vsDelivery spec of
                      DeliverExitCellFill -> spAnswer plan
                      _ -> wire
                    inputs = spInputs plan ++ derived
                    carrierType = mkTyConApp carrier [mkPromotedListTy liftedTypeKind inputs, reply]
                    carrierArguments = [mkPromotedListTy liftedTypeKind inputs, reply]
                    instantiatedResult = substTyWith (dataConUnivTyVars carrierConstructor)
                      carrierArguments (dataConOrigResTy carrierConstructor)
                    validField = case dataConInstOrigArgTys carrierConstructor carrierArguments of
                      [Scaled _ field] -> eqType field intTy
                      _ -> False
                if not (eqType instantiatedResult carrierType && validField)
                  then Left "RequestSite constructor ABI must accept exactly one Int field"
                  else if eqType carrierType (spCarrierType plan)
                    then Right (wire, inputs, requestTypes, carrier, reply)
                    else Left "site-aware sibling has another RequestSite input or reply index" of
                Left detail -> do
                  modify' (\current -> current
                    { esRejections = SiteRejection topBinder
                        (vsName spec ++ " site in " ++ T.unpack originName ++ ": " ++ detail)
                        : esRejections current })
                  pure (mkApps headExpr rewrittenArguments)
                Right (wireType, siteInputs, requestTypes, carrierTyCon, carrierReply) -> do
                  missing <- traverse freshEvidence (spMissingEvidence plan)
                  ordinal <- nextOrdinal originName
                  witnesses <- liftIO (traverse (captureCheckedTypeWitness env) siteInputs)
                  signatures <- liftIO (traverse (\(reply, progress) ->
                    captureRequestTypeSignatures env reply progress) requestTypes)
                  let site = (buildYieldSite spec originName ordinal (spAnswer plan) siteInputs)
                        { ysInputTypeWitnesses = witnesses, ysRequestTypeSignatures = signatures }
                      literal = mkCoreConApps intDataCon
                        [Lit (LitNumber LitNumInt (fromIntegral (ysSite site)))]
                      -- Sites are elaborated after simplify/tidy. A newtype
                      -- worker application cannot survive into CorePrep/STG;
                      -- use its genuine representation axiom to wrap the Int.
                      carrierArguments =
                        [mkPromotedListTy liftedTypeKind siteInputs, carrierReply]
                      carrier = mkCast literal (mkSymCo
                        (mkUnbranchedAxInstCo Representational
                          (newTyConCo carrierTyCon) carrierArguments []))
                  current <- get
                  let (wireNode, graph1) = runState (internType wireType) (esTypeGraph current)
                      (inputNodes, graph2) = runState (traverse internType siteInputs) graph1
                      preparedSite = PreparedSite topBinder site (vsDelivery spec)
                        wireNode inputNodes
                  put current
                    { esSites = site : esSites current
                    , esPreparedSites = preparedSite : esPreparedSites current
                    , esTypeGraph = graph2
                    }
                  current' <- get
                  let body = mkLams missing (mkApps (Var (spSibling plan))
                        (map Type (spTypeArgs plan) ++ spEvidence plan ++ map Var missing ++ carrier : spRest plan))
                      initialSubst = mkEmptySubst (mkInScopeSet (exprFreeVars body))
                      ((substitution, typeBinders), remainingUniques) = initUs (esUniques current')
                        (cloneBndrs initialSubst (spMissingTypes plan))
                  put current' {esUniques = remainingUniques}
                  pure (mkLams typeBinders (substExpr substitution body))
        _ -> do
          rewrittenHead <- rewriteExpr origin headExpr
          pure (mkApps rewrittenHead rewrittenArguments)

    freshEvidence (Scaled mult ty) = do
      current <- get
      let (unique, rest) = takeUniqFromSupply (esUniques current)
      put current {esUniques = rest}
      pure (mkSysLocal (fsLit "siteEvidence") unique mult ty)

    nextOrdinal origin = do
      current <- get
      let ordinal = Map.findWithDefault 0 origin (esCounters current)
      put current {esCounters = Map.insert origin (ordinal + 1) (esCounters current)}
      pure ordinal

siteWireType :: SiteAuthority -> VerbSpec -> Type -> Either String Type
siteWireType authority spec answer = case vsWireSource spec of
  SelectedAnswer -> Right answer
  ListAnswer -> Right (mkListTy answer)
  ResponseResultEvidence -> do
    response <- maybe (Left "missing ResponseResult type authority") Right
      (responseResultTyCon authority)
    Right (mkTyConApp response [answer])
  ProgressStateEvidence -> do
    progress <- maybe (Left "missing ProgressState type authority") Right
      (progressStateTyCon authority)
    Right (mkTyConApp progress [answer])

-- Only the known progress verbs expose a progress type. The shared verb table
-- must retain their exact input and answer positions before a signature issues.
requestProgressType :: VerbSpec -> [Type] -> Either String (Maybe Type)
requestProgressType spec inputs = do
  (indices, answer, progress) <- case (vsModule spec, vsName spec) of
    ("Tidepool.Actors.Internal.Agent", "request") -> Right ([1], FirstTypeArgument, False)
    ("Tidepool.Actors.Internal.Agent", "requestWithProgress") -> Right ([2, 0], TypeArgument 1, True)
    ("Tidepool.Actors.Internal.Agent", "requestWithProgressInto") -> Right ([2, 0], TypeArgument 1, True)
    ("Tidepool.Actors.Unfold", "child") -> Right ([2], TypeArgument 0, False)
    ("Tidepool.Actors.Unfold", "childWithProgress") -> Right ([3, 0], TypeArgument 1, True)
    _ -> Left "request site lacks known reply/progress semantics"
  if vsInputTypeArgs spec /= indices || vsAnswerSource spec /= answer
      || vsDerivedInput spec /= Nothing || vsListAnswer spec
    then Left "request site has another input or answer type shape"
    else case (progress, inputs) of
      (False, [_]) -> Right Nothing
      (True, [_, progressType]) -> Right (Just progressType)
      _ -> Left "request site has another live input arity"

-- | Exact generated siblings present in one tidied home module. Merging these
-- maps in dependency order gives later modules the real imported Ids without
-- consulting interface unfoldings or reconstructing names.
resolvePreparedSiblings :: [CoreBind] -> Map String Id
resolvePreparedSiblings = resolvePreparedSiblingIds . concatMap bindersOf

-- | Hydrated original modules supply typed sibling authority even when their
-- executable bodies are recovered from native products instead of prepared
-- again. Read only defining Ids from the current HPT; reexports and interface
-- unfoldings cannot manufacture a sibling.
resolvePreparedInterfaceSiblings :: HscEnv -> Map String Id
resolvePreparedInterfaceSiblings env = resolvePreparedSiblingIds
  [ binder
  | name <- nub (map vsSitedModule sitedVerbs)
  , Just home <- [lookupHpt (hsc_HPT env) (mkModuleName name)]
  , let owner = mi_module (hm_iface home)
  , moduleName owner == mkModuleName name
  , isHomeUnit (hsc_home_unit env) (moduleUnit owner)
  , binder <- typeEnvIds (md_types (hm_details home))
  , nameModule_maybe (idName binder) == Just owner
  ]

resolvePreparedSiblingIds :: [Id] -> Map String Id
resolvePreparedSiblingIds identifiers = Map.fromList
  [ (vsName spec, binder)
  | spec <- sitedVerbs
  , binder : _ <- [filter (isSibling spec) identifiers]
  ]
  where
    isSibling spec binder =
      occNameString (nameOccName (idName binder)) == vsSitedName spec
        && not (isSystemName (idName binder))
        && case nameModule_maybe (idName binder) of
          Just modul -> moduleNameString (moduleName modul) == vsSitedModule spec
          Nothing -> False

lookupPreparedVerb :: Id -> Maybe VerbSpec
lookupPreparedVerb binder = find matches sitedVerbs
  where
    matches spec =
      occNameString (nameOccName (idName binder)) == vsName spec
        && case nameModule_maybe (idName binder) of
          Just modul -> moduleNameString (moduleName modul) == vsModule spec
          Nothing -> False

siteType :: Bool -> Type -> SiteType
siteType listAnswer ty = SiteType rendered
  (modulesOfType ty) (nominalHeadsOfType ty)
  where
    base = renderType ty
    rendered = T.pack (if listAnswer then "[" ++ base ++ "]" else base)

renderType :: Type -> String
renderType = renderWithContext stableContext . ppr
  where
    stableContext = defaultSDocContext { sdocSuppressUniques = True }

-- | The declaration text for a reply type's 'TyCon', for a model constructing
-- that reply value. Stripped of 'GHC.Iface.Type' representation pragmas
-- (@{-# UNPACK #-}@ and the strictness bang it always wraps): those describe
-- runtime layout, not the surface syntax a model would write. Whether this
-- text is worth showing at all (a workspace/session type) versus a library
-- type already named by "reply type X" is decided by the caller, which knows
-- the compiled workspace's own modules; this stays a pure rendering of the
-- 'TyCon' alone.
replyDeclaration :: Type -> Maybe T.Text
replyDeclaration ty = case splitTyConApp_maybe ty of
  Nothing -> Nothing
  Just (constructor, _) -> Just (stripRepresentationPragmas (T.pack (renderWithContext defaultSDocContext
    (pprTyThingInContext (ShowSub ShowIface ShowForAllWhen) (ATyCon constructor)))))

stripRepresentationPragmas :: T.Text -> T.Text
stripRepresentationPragmas =
  T.replace "{-# UNPACK #-} !" "" . T.replace "{-# UNPACK #-}!" "" .
  T.replace "{-# UNPACK #-} " "" . T.replace "{-# UNPACK #-}" ""

buildYieldSite :: VerbSpec -> T.Text -> Word64 -> Type -> [Type] -> YieldSite
buildYieldSite spec origin ordinal answer inputs =
  let answerSite = siteType (vsListAnswer spec) answer
      inputSites = map (siteType False) inputs
      identity = siteIdentity spec origin ordinal answerSite inputSites
  in YieldSite identity origin ordinal answerSite inputSites (replicate (length inputSites) Nothing)
      (replyDeclaration answer) Nothing

siteIdentity :: VerbSpec -> T.Text -> Word64 -> SiteType -> [SiteType] -> Word64
siteIdentity spec origin ordinal answer inputs =
  let Fingerprint high low = fingerprintString
        (T.unpack origin ++ "#" ++ show ordinal ++ "#" ++ vsName spec
          ++ "#" ++ show answer ++ "#" ++ show inputs)
  in (high `xor` low) .&. 0x7fffffffffffffff

-- | The saturated final lifted result index is an intrinsic constructor fact.
-- Unresolved indices remain explicit unconstructible nodes in the type graph.
requestReplyIndex :: DataCon -> Maybe Type
requestReplyIndex constructor = case splitTyConApp_maybe (dataConOrigResTy constructor) of
  Just (family, arguments)
    | length arguments == tyConArity family
    , index : _ <- reverse arguments
    , isLiftedTypeKind (typeKind index) -> Just index
  _ -> Nothing
