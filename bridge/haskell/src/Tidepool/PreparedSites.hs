module Tidepool.PreparedSites
  ( buildYieldSite
  , PreparedSite(..)
  , PreparedSiteEnvironment, resolvePreparedSiteEnvironment
  , preparedSiteRequestAuthority
  , PreparedSiteDependencies, preparedSiteDependenciesMatch, preparedSiteDependenciesEquivalent
  , elaboratePreparedSitesWithDependencies
  , resolveRequestSiteTyCon
  , SiteRejection(..)
  , lookupPreparedVerb
  , resolvePreparedSiblings
  , resolvePreparedInterfaceSiblings
  , IntrinsicCensus, censusPreparedIntrinsics, intrinsicFree, intrinsicNames
  , requestReplyIndex
  ) where

import Control.Exception (throwIO)
import Control.Monad (forM_, unless, when)
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
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
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
import GHC.Core.TyCon (TyCon, isNewTyCon, newTyConCo, tyConArity, tyConDataCons, tyConRoles, tyConTyVars, tyConName, isDataTyCon)
import GHC.Core.DataCon
  ( DataCon, dataConOrigResTy, dataConName, dataConWorkId, dataConWrapId_maybe
  , dataConInstOrigArgTys, dataConUnivTyVars, isVanillaDataCon )
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_HUG, hsc_home_unit, hscEPS, lookupType)
import GHC.Types.TyThing.Ppr (pprTyThingInContext)
import GHC.Types.TyThing (TyThing (..))
import GHC.Iface.Type (ShowForAllFlag (..), ShowHowMuch (..), ShowSub (..))
import GHC.Types.Literal (LitNumType (..), Literal (..))
import GHC.Types.Name (Name, isSystemName, nameModule_maybe, nameOccName, nameUnique)
import GHC.Types.Name.Occurrence (OccName, mkTcOcc, mkVarOcc, occNameString)
import GHC.Types.Id (Id, idName, mkSysLocal)
import GHC.Types.Var (varType)
import GHC.Utils.Fingerprint (Fingerprint (..), fingerprintString)
import GHC.Utils.Outputable (SDocContext(sdocSuppressUniques), defaultSDocContext, ppr, renderWithContext)
import GHC.Data.Maybe (MaybeErr(Succeeded, Failed))
import GHC.Types.PkgQual (PkgQual(NoPkgQual))
import GHC.Iface.Env (lookupOrig)
import GHC.Iface.Load (importDecl)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt, lookupHpt)
import GHC.Unit.Home (isHomeUnit, mkHomeModule)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module (Module, ModuleName, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModIface (mi_module, mi_iface_hash, mi_final_exts, mi_decls)
import GHC.Unit.External (ExternalPackageState(eps_PIT))
import GHC.Unit.Env (lookupHugByModule)
import GHC.Unit.Module.Env (moduleEnvToList, lookupModuleEnv)
import GHC.Iface.Syntax (IfaceDecl(..), ifaceDeclImplicitBndrs)
import GHC.Types.TypeEnv (typeEnvIds, lookupTypeEnv)
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

-- | The preparation owner inspects every typed Core occurrence. This census
-- permits the stable cache only when neither elaboration nor reply carriers
-- depend on the admitted interface environment. Constructors are private.
data IntrinsicCensus = IntrinsicCensus Bool [Name] [Id]

intrinsicFree :: IntrinsicCensus -> Bool
intrinsicFree (IntrinsicCensus dependent _ _) = not dependent

intrinsicNames :: IntrinsicCensus -> [Name]
intrinsicNames (IntrinsicCensus _ names _) = names

censusPreparedIntrinsics :: [TyCon] -> [CoreBind] -> IntrinsicCensus
censusPreparedIntrinsics tycons bindings = IntrinsicCensus dependent names surfaces
  where
    identifiers = concatMap bindingIds bindings
      ++ [dataConWorkId constructor | tycon <- tycons, isDataTyCon tycon
          , constructor <- tyConDataCons tycon]
    types = concatMap bindingTypes bindings ++ map varType identifiers
    surfaces = filter (maybe False (const True) . lookupPreparedVerb) identifiers
    names = nub (map idName surfaces)
    dependent = not (null surfaces) || any protected identifiers
      || any hasCarrier types
    hasCarrier = any carrier . nonDetEltsUniqSet . tyConsOfType
    carrier tycon = defining (tyConName tycon) "Tidepool.Internal.RequestSite" "RequestSite"
    protected identifier = mayBeProtectedName (idName identifier)
      || any (uncurry (defining (idName identifier)))
      [("Tidepool.Actor.Source", "installSource"),
       ("Tidepool.Actor.Source", "attachSource")]
    defining name owner occurrence =
      occNameString (nameOccName name) == occurrence
      && maybe False ((== owner) . moduleNameString . moduleName) (nameModule_maybe name)
    bindingIds binding = concatMap (\(binder, body) -> binder : exprIds body) (pairs binding)
    bindingTypes binding = concatMap (exprTypes . snd) (pairs binding)
    pairs (NonRec binder rhs) = [(binder, rhs)]
    pairs (Rec entries) = entries
    exprIds expression = case expression of
      Var identifier | isId identifier -> [identifier]
      App function argument -> exprIds function ++ exprIds argument
      Lam binder body -> [binder | isId binder] ++ exprIds body
      Let binding body -> bindingIds binding ++ exprIds body
      Case scrutinee binder _ alternatives -> binder : exprIds scrutinee
        ++ concat [filter isId binders ++ exprIds body | Alt _ binders body <- alternatives]
      Cast body _ -> exprIds body
      Tick _ body -> exprIds body
      _ -> []
    exprTypes expression = case expression of
      Type ty -> [ty]
      App function argument -> exprTypes function ++ exprTypes argument
      Lam _ body -> exprTypes body
      Let binding body -> bindingTypes binding ++ exprTypes body
      Case scrutinee _ result alternatives -> result : exprTypes scrutinee
        ++ concat [exprTypes body | Alt _ _ body <- alternatives]
      Cast body _ -> exprTypes body
      Tick _ body -> exprTypes body
      _ -> []

data SiteAuthority = SiteAuthority
  { requestSiteTyCon :: Maybe TyCon
  , responseResultTyCon :: Maybe TyCon
  , progressStateTyCon :: Maybe TyCon
  , protectedProgressIds :: Set.Set Word64
  , trustedProgressOwners :: Set.Set Word64
  }

-- These keys describe the resolver's closed authority vocabulary. A Name in
-- a dependency query is the original GHC identity, not a printed spelling.
data ProgressAuthority
  = ReportProgress | PollProgress | AwaitProgressAfter | AwaitAnyProgress | ProgressSource
  deriving (Eq, Ord)

data AuthorityDeclaration
  = RequestCarrier | ResponseWrapper | ProgressWrapper | ReplyConstructors | WatchConstructors
  | ProgressHelper ProgressAuthority | InstallSource | AttachSource
  deriving (Eq, Ord)

data DeclarationIdentity
  = NominalIdentity TyCon
  | ValueIdentity Id

data DependencyFact
  = MissingModule ModuleName
  | MissingDeclaration Module Fingerprint Name
  | KnownDeclaration Module Fingerprint DeclarationIdentity
  | MembershipFact Bool [DependencyFact]
  | SignatureOwnerFact Module
  | NoSelectedSibling
  | UnverifiableDependency

sameDependencyFact :: DependencyFact -> DependencyFact -> Bool
sameDependencyFact (MissingModule a) (MissingModule b) = a == b
sameDependencyFact (MissingDeclaration a version name) (MissingDeclaration b version' name') =
    a == b && version == version' && name == name'
sameDependencyFact (KnownDeclaration a version identity) (KnownDeclaration b version' identity') =
    a == b && version == version' && sameIdentity identity identity'
    where
      sameIdentity (NominalIdentity x) (NominalIdentity y) = tyConName x == tyConName y
      sameIdentity (ValueIdentity x) (ValueIdentity y) = idName x == idName y
        && eqType (varType x) (varType y)
      sameIdentity _ _ = False
sameDependencyFact (MembershipFact value facts) (MembershipFact value' facts') =
    value == value' && length facts == length facts'
      && and (zipWith sameDependencyFact facts facts')
sameDependencyFact (SignatureOwnerFact a) (SignatureOwnerFact b) = a == b
sameDependencyFact NoSelectedSibling NoSelectedSibling = True
  -- A failed declaration load is not evidence that authority is absent.
sameDependencyFact UnverifiableDependency _ = False
sameDependencyFact _ UnverifiableDependency = False
sameDependencyFact _ _ = False

data DependencyQuery
  = NominalQuery AuthorityDeclaration
  | ProtectedQuery Name
  | TrustedQuery Name
  | SiblingQuery Name
  | SignatureOwnerQuery
  deriving (Eq, Ord)

-- | One coordinator-owned resolved interface context. Its lookup tables are
-- acquired once after interface publication; only queries actually consumed
-- by elaboration are retained in a module's dependency witness.
data PreparedSiteEnvironment = PreparedSiteEnvironment
  { environmentAuthority :: SiteAuthority
  , environmentDeclarations :: Map AuthorityDeclaration (Maybe TyThing, DependencyFact)
  , environmentRecoveredSiblings :: Map String (Maybe Id, DependencyFact)
  , environmentInterfaceSiblings :: Map String Id
  , environmentVersions :: Map Module Fingerprint
  , environmentSignatureContext :: HscEnv
  }

-- | An immutable query recipe and its exact observations. Keeping original
-- owned siblings preserves the existing owned/imported/HPT/recovered order.
data PreparedSiteDependencies = PreparedSiteDependencies
  (Map String Id) (Map DependencyQuery DependencyFact)

preparedSiteRequestAuthority :: PreparedSiteEnvironment -> Maybe TyCon
preparedSiteRequestAuthority = requestSiteTyCon . environmentAuthority

preparedSiteDependenciesMatch :: PreparedSiteEnvironment -> Map String Id
  -> PreparedSiteDependencies -> Bool
preparedSiteDependenciesMatch environment imported (PreparedSiteDependencies owned facts) =
  Map.foldrWithKey (\query previous rest -> rest
    && sameDependencyFact previous (observeDependency environment owned imported query)) True facts

-- | Deduplicate alternatives by the consumed observations, not by all owned
-- siblings or the ambient interface roster. Unverifiable facts never match.
preparedSiteDependenciesEquivalent :: PreparedSiteDependencies -> PreparedSiteDependencies -> Bool
preparedSiteDependenciesEquivalent (PreparedSiteDependencies _ facts)
    (PreparedSiteDependencies _ facts') =
  Map.keysSet facts == Map.keysSet facts'
    && and (Map.elems (Map.intersectionWith sameDependencyFact facts facts'))

resolvePreparedSiteEnvironment :: HscEnv -> IO PreparedSiteEnvironment
resolvePreparedSiteEnvironment env = do
  declarations <- Map.fromList <$> traverse resolveAuthority authorityDeclarations
  recovered <- Map.fromList <$> traverse resolveSibling sitedVerbs
  external <- hscEPS env
  let versionsOf interfaces = Map.fromList
        [(mi_module interface, mi_iface_hash (mi_final_exts interface)) | interface <- interfaces]
      versions = Map.union (versionsOf (map hm_iface (eltsHpt (hsc_HPT env))))
        (Map.union declarationVersions (versionsOf (map snd (moduleEnvToList (eps_PIT external)))))
      declarationVersions = Map.fromList
        [(owner, version) | fact <- map snd (Map.elems declarations) ++ map snd (Map.elems recovered)
          , (owner, version) <- case fact of
              KnownDeclaration owner version _ -> [(owner, version)]
              MissingDeclaration owner version _ -> [(owner, version)]
              _ -> []]
      tycon role = case fst (declarations Map.! role) of
        Just (ATyCon original) -> Just original
        _ -> Nothing
      identifier role = case fst (declarations Map.! role) of
        Just (AnId original) -> [original]
        _ -> []
      constructors original =
        [ identifier'
        | tycon' <- maybe [] (:[]) original
        , constructor <- tyConDataCons tycon'
        , occNameString (nameOccName (dataConName constructor)) `elem` rawProgressOccurrences
        , identifier' <- dataConWorkId constructor : maybe [] (:[]) (dataConWrapId_maybe constructor)
        ]
      raw = constructors (tycon ReplyConstructors) ++ constructors (tycon WatchConstructors)
      sited = concatMap (identifier . ProgressHelper) progressAuthorities
      sources = identifier InstallSource ++ identifier AttachSource
      key = getKey . nameUnique . idName
      authority = SiteAuthority (tycon RequestCarrier) (tycon ResponseWrapper) (tycon ProgressWrapper)
        (Set.fromList (map key (raw ++ sited))) (Set.fromList (map key (raw ++ sited ++ sources)))
  pure (PreparedSiteEnvironment authority declarations recovered
    (resolvePreparedInterfaceSiblings env) versions env)
  where
    resolveAuthority role = do
      let (owner, occurrence, nominal) = authorityDeclaration role
      result <- resolveDeclaration env owner occurrence nominal
      pure (role, result)
    resolveSibling spec = do
      (thing, fact) <- resolveDeclaration env (vsSitedModule spec) (mkVarOcc (vsSitedName spec)) False
      let sibling = case thing of
            Just (AnId original) -> Just original
            _ -> Nothing
      pure (vsName spec, (sibling, fact))

authorityDeclarations :: [AuthorityDeclaration]
authorityDeclarations = [RequestCarrier, ResponseWrapper, ProgressWrapper, ReplyConstructors,
  WatchConstructors] ++ map ProgressHelper progressAuthorities ++ [InstallSource, AttachSource]

progressAuthorities :: [ProgressAuthority]
progressAuthorities = [ReportProgress, PollProgress, AwaitProgressAfter, AwaitAnyProgress, ProgressSource]

progressVerb :: ProgressAuthority -> String
progressVerb role = case role of
  ReportProgress -> "reportRequestProgress"
  PollProgress -> "pollProgress"
  AwaitProgressAfter -> "awaitProgressAfter"
  AwaitAnyProgress -> "awaitAnyProgress"
  ProgressSource -> "progressSource"

rawProgressOccurrences :: [String]
rawProgressOccurrences = ["PublishProgressWith", "ObserveProgressWith", "ObserveWatchProgressWith"]

authorityDeclaration :: AuthorityDeclaration -> (String, OccName, Bool)
authorityDeclaration role = case role of
  RequestCarrier -> nominal "Tidepool.Internal.RequestSite" "RequestSite"
  ResponseWrapper -> nominal "Tidepool.Agent.Reply.Internal" "ResponseResult"
  ProgressWrapper -> nominal "Tidepool.Agent.Reply.Internal" "ProgressState"
  ReplyConstructors -> nominal "Tidepool.Agent.Reply.Internal" "Replies"
  WatchConstructors -> nominal "Tidepool.Agent.Watch.Internal" "Watches"
  InstallSource -> value "Tidepool.Actor.Source" "installSource"
  AttachSource -> value "Tidepool.Actor.Source" "attachSource"
  ProgressHelper progress -> case find ((== progressVerb progress) . vsName) sitedVerbs of
    Just spec -> value (vsSitedModule spec) (vsSitedName spec)
    Nothing -> error "progress authority is absent from the site registry"
  where
    nominal owner occurrence = (owner, mkTcOcc occurrence, True)
    value owner occurrence = (owner, mkVarOcc occurrence, False)

-- This is the closed domain of the resolver's protected-membership query.
-- Both the intrinsic census and elaboration use it, including constructor
-- wrappers and references to already-sited helpers with no surface verb.
mayBeProtectedName :: Name -> Bool
mayBeProtectedName name = case nameModule_maybe name of
  Nothing -> False
  Just owner ->
    let spelling = moduleNameString (moduleName owner)
        occurrence = occNameString (nameOccName name)
        raw = rawProgressOccurrences ++ map ("$W" ++) rawProgressOccurrences
    in (spelling `elem` ["Tidepool.Agent.Reply.Internal", "Tidepool.Agent.Watch.Internal"]
          && occurrence `elem` raw)
       || any (\progress -> let (provider, sibling, _) = authorityDeclaration (ProgressHelper progress)
              in spelling == provider && occurrence == occNameString sibling) progressAuthorities

resolveDeclaration :: HscEnv -> String -> OccName -> Bool
  -> IO (Maybe TyThing, DependencyFact)
resolveDeclaration env spelling occurrence nominal = do
  found <- findImportedModule env (mkModuleName spelling) NoPkgQual
  case found of
    Found _ owner -> do
      name <- initIfaceLoad env (lookupOrig owner occurrence)
      let home = lookupHugByModule owner (hsc_HUG env)
          declared interface = any (\(_, declaration) -> occurrence `elem`
            (nameOccName (ifName declaration) : ifaceDeclImplicitBndrs declaration)) (mi_decls interface)
          absent = case home of
            Just entry | not (declared (hm_iface entry)) -> True
            _ -> False
      existing <- case home of
        Just entry -> pure (lookupTypeEnv (md_types (hm_details entry)) name)
        Nothing -> lookupType env name
      loaded <- case existing of
        _ | absent -> pure Nothing
        Just thing -> pure (Just thing)
        -- importDecl reads ambient EPS, not the current home declaration. A
        -- partial home type environment cannot authorize a stale typed object.
        Nothing | Just _ <- home -> pure Nothing
        Nothing -> do
          result <- initIfaceLoad env (importDecl name)
          pure $ case result of
            Succeeded thing -> Just thing
            Failed _ -> Nothing
      external <- hscEPS env
      let selected = case home of
            Just entry -> Just (hm_iface entry)
            Nothing -> lookupModuleEnv (eps_PIT external) owner
          version = mi_iface_hash . mi_final_exts <$> selected
          missing = case version of
            Just fingerprint | absent -> MissingDeclaration owner fingerprint name
            _ -> UnverifiableDependency
          result thing identity = case version of
            Just fingerprint -> (Just thing, KnownDeclaration owner fingerprint identity)
            Nothing -> (Just thing, UnverifiableDependency)
      pure $ case loaded of
        Just thing@(ATyCon tycon) | nominal, tyConName tycon == name ->
          result thing (NominalIdentity tycon)
        Just thing@(AnId identifier) | not nominal, idName identifier == name ->
          result thing (ValueIdentity identifier)
        _ -> (Nothing, missing)
    NotFound { fr_pkg = Nothing, fr_unusables = [] } ->
      pure (Nothing, MissingModule (mkModuleName spelling))
    _ -> pure (Nothing, UnverifiableDependency)

observeDependency :: PreparedSiteEnvironment -> Map String Id -> Map String Id
  -> DependencyQuery -> DependencyFact
observeDependency environment owned imported query = case query of
  SignatureOwnerQuery -> SignatureOwnerFact
    (mkHomeModule (hsc_home_unit (environmentSignatureContext environment))
      (mkModuleName "Tidepool.CheckedAnnotation"))
  NominalQuery declaration -> snd (environmentDeclarations environment Map.! declaration)
  ProtectedQuery name -> membership (protectedProgressIds authority) name
    progressDeclarations
  TrustedQuery name -> membership (trustedProgressOwners authority) name
    (progressDeclarations ++ [InstallSource, AttachSource])
  SiblingQuery surface -> case lookupPreparedVerbName surface of
    Nothing -> UnverifiableDependency
    Just spec ->
      let recovered = case Map.lookup (vsName spec) (environmentRecoveredSiblings environment) of
            Just (Just sibling, _) | Just surfaceOwner <- nameModule_maybe surface
              , Just siblingOwner <- nameModule_maybe (idName sibling)
              , moduleUnit surfaceOwner == moduleUnit siblingOwner -> Just sibling
            _ -> Nothing
          selected = Map.lookup (vsName spec) owned `orElse`
            Map.lookup (vsName spec) imported `orElse`
            Map.lookup (vsName spec) (environmentInterfaceSiblings environment) `orElse` recovered
      in case selected of
        Just sibling -> identityFact sibling
        Nothing -> case Map.lookup (vsName spec) (environmentRecoveredSiblings environment) of
          Just (_, UnverifiableDependency) -> UnverifiableDependency
          Just (_, fact) -> MembershipFact False [fact]
          Nothing -> NoSelectedSibling
  where
    authority = environmentAuthority environment
    progressDeclarations = [ReplyConstructors, WatchConstructors]
      ++ map ProgressHelper progressAuthorities
    identityFact identifier = case nameModule_maybe (idName identifier) >>= \owner ->
      (owner,) <$> Map.lookup owner (environmentVersions environment) of
        Just (owner, version) -> KnownDeclaration owner version (ValueIdentity identifier)
        Nothing -> UnverifiableDependency
    membership identifiers name declarations =
      let present = getKey (nameUnique name) `Set.member` identifiers
          relevant = [fact | declaration <- declarations
            , let (spelling, _, _) = authorityDeclaration declaration
            , maybe False ((== spelling) . moduleNameString . moduleName) (nameModule_maybe name)
            , let fact = snd (environmentDeclarations environment Map.! declaration)]
      in MembershipFact present relevant
    orElse (Just value) _ = Just value
    orElse Nothing alternative = alternative

-- Resolve the defining declaration through GHC's module/interface authority.
-- Projection compares this exact TyCon, never its rendered module spelling.
resolveRequestSiteTyCon :: HscEnv -> IO (Maybe TyCon)
resolveRequestSiteTyCon env = do
  (thing, _) <- resolveDeclaration env "Tidepool.Internal.RequestSite" (mkTcOcc "RequestSite") True
  pure $ case thing of
    Just (ATyCon tycon) -> Just tycon
    _ -> Nothing

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
  , esDependencyQueries :: !(Set.Set DependencyQuery)
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
-- | The queried objects and this witness come from the same elaboration. The
-- caller cannot supply a dependency census or replace the resolved authority.
elaboratePreparedSitesWithDependencies :: PreparedSiteEnvironment
  -> Map String Id -> Map String Id -> [CoreBind]
  -> IO ([CoreBind], [YieldSite], [PreparedSite], TypeGraph, [SiteRejection], PreparedSiteDependencies)
elaboratePreparedSitesWithDependencies environment owned imported bindings = do
  let IntrinsicCensus _ _ surfaces = censusPreparedIntrinsics [] bindings
      recovered = Map.fromList
        [(vsName spec, sibling)
        | surface <- surfaces, Just spec <- [lookupPreparedVerb surface]
        , Just surfaceOwner <- [nameModule_maybe (idName surface)]
        , Just (Just sibling, _) <- [Map.lookup (vsName spec) (environmentRecoveredSiblings environment)]
        , Just siblingOwner <- [nameModule_maybe (idName sibling)]
        , moduleUnit surfaceOwner == moduleUnit siblingOwner]
      siblings = Map.unions [owned, imported, environmentInterfaceSiblings environment, recovered]
  (rewritten, yields, issued, graph, failures, queries) <-
    elaboratePreparedSitesTracked (environmentSignatureContext environment)
      (environmentAuthority environment) siblings bindings
  let facts = Map.fromSet (observeDependency environment owned imported) queries
  pure (rewritten, yields, issued, graph, failures, PreparedSiteDependencies owned facts)

elaboratePreparedSitesTracked :: HscEnv -> SiteAuthority -> Map String Id -> [CoreBind]
  -> IO ([CoreBind], [YieldSite], [PreparedSite], TypeGraph, [SiteRejection], Set.Set DependencyQuery)
elaboratePreparedSitesTracked env authority siblings bindings = do
  uniques <- mkSplitUniqSupply 's'
  (bindings', final) <- runStateT (traverse rewriteBind bindings)
        (ElaborationState uniques mempty [] [] emptyTypeGraphBuilder []
          (Set.singleton (NominalQuery RequestCarrier)))
  graph <- either throwIO pure (finishTypeGraph (esTypeGraph final))
  pure ( bindings'
       , reverse (esSites final)
       , reverse (esPreparedSites final)
       , graph
       , reverse (esRejections final)
       , esDependencyQueries final)
  where
    recordQuery :: DependencyQuery -> StateT ElaborationState IO ()
    recordQuery query = modify' (\state -> state
      { esDependencyQueries = Set.insert query (esDependencyQueries state) })
    recordWireQuery spec = case vsWireSource spec of
      ResponseResultEvidence -> recordQuery (NominalQuery ResponseWrapper)
      ProgressStateEvidence -> recordQuery (NominalQuery ProgressWrapper)
      _ -> pure ()
    rewriteBind (NonRec binder rhs) =
      NonRec binder <$> rewriteExpr (binder, binderQualName binder) rhs
    rewriteBind (Rec pairs) = Rec <$> traverse (\(binder, rhs) ->
      (binder,) <$> rewriteExpr (binder, binderQualName binder) rhs) pairs

    rewriteExpr origin expression = case expression of
      Var surface | Just _ <- lookupPreparedVerb surface -> rewriteApplication origin expression
      Var identifier -> do
        when (mayBeProtectedName (idName identifier)) (recordQuery (ProtectedQuery (idName identifier)))
        when (getKey (nameUnique (idName identifier)) `Set.member` protectedProgressIds authority) $ do
          recordQuery (TrustedQuery (idName (fst origin)))
          unless (getKey (nameUnique (idName (fst origin))) `Set.member` trustedProgressOwners authority) $
            modify' (\current -> current
              { esRejections = SiteRejection (fst origin)
                  ("raw or site-aware progress operations require compiler-issued typed helper evidence: " ++ T.unpack (snd origin))
                  : esRejections current })
        pure expression
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
          recordQuery (SiblingQuery (idName surface))
          let (topBinder, originName) = origin
          case classifySiteOccurrence siblings spec surface rewrittenArguments of
            Left failure -> do
              modify' (\current -> current
                {esRejections = SiteRejection topBinder
                  (renderSiteFailure (T.unpack originName) spec failure) : esRejections current})
              pure (mkApps headExpr rewrittenArguments)
            Right plan -> do
              recordWireQuery spec
              forM_ (vsDerivedInput spec) $ \(_, source) -> recordWireQuery (spec { vsWireSource = source })
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
                  when (not (null siteInputs) || maybe False (const True) requestTypes)
                    (recordQuery SignatureOwnerQuery)
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
                  (wireNode, graph1) <- either (liftIO . throwIO) pure
                    (runStateT (internType wireType) (esTypeGraph current))
                  (inputNodes, graph2) <- either (liftIO . throwIO) pure
                    (runStateT (traverse internType siteInputs) graph1)
                  let preparedSite = PreparedSite topBinder site (vsDelivery spec)
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
lookupPreparedVerb = lookupPreparedVerbName . idName

lookupPreparedVerbName :: Name -> Maybe VerbSpec
lookupPreparedVerbName name = find matches sitedVerbs
  where
    matches spec =
      occNameString (nameOccName name) == vsName spec
        && case nameModule_maybe name of
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
