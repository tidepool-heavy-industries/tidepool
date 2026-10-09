module ModuleEvidenceProjectionTest (verifyModuleEvidenceProjection) where

import Tidepool.PreparedStg.Internal (PreparedModule(..), PreparedCoverage(..), PreparedEntryContext(..))
import Control.Monad (forM_, unless)
import Control.Monad.State.Strict (runStateT)
import Data.List (sort)
import Data.IntMap.Strict qualified as IntMap
import GHC.Core.TyCon (PrimRep(..))
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Builtin.Types (boolTy)
import GHC.Builtin.Types.Prim (addrPrimTy)
import GHC.Stg.Syntax
import GHC.Types.CostCentre (dontCareCCS)
import GHC.Types.Literal (Literal(..))
import GHC.Types.Id (mkVanillaGlobal)
import GHC.Types.Name (mkExternalName, mkSystemName)
import GHC.Types.Name.Env (emptyNameEnv)
import GHC.Types.Name.Occurrence (mkVarOcc)
import GHC.Types.SrcLoc (noSrcSpan)
import GHC.Types.Unique (mkUnique, getKey)
import GHC.Types.Var (varUnique, varName)
import GHC.Types.Var.Set (emptyVarSet, emptyDVarSet, mkVarSet)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Module (mkModule, mkModuleName)
import GHC.Unit.Types (stringToUnit)
import Tidepool.EffectSchema qualified as Effect
import Tidepool.ExecutionProjection
import Tidepool.ExecutionEncode (encodeProjectedGroup)
import Tidepool.ExecutionSchema
import Tidepool.PreparedSites (PreparedSite(..), SiteRejection(..))
import Tidepool.PreparedStg
  ( pmModule, pmCoverage, pmBindings, pmSitedSiblings, pmYieldSites
  , pmPreparedSites, pmTypeGraph, pmSiteRejections, pmRequestSiteTyCon
  , preparedBindingGroups, preparedUsesSiteAuthority
  , preparedRejectsIntrinsic, preparedExpectedEntry )
import Tidepool.TypePolicy qualified as TypePolicy

-- Pure projection checks use compiler-owned Ids and graphs, without compiling
-- another source fixture. The interleaved rows exercise module order rather
-- than the order of binder lookup or graph traversal.
verifyModuleEvidenceProjection :: IO ()
verifyModuleEvidenceProjection = do
  verifyGroupSelection context prepared
  groups <- project prepared
  assert (length groups == 3) "module evidence fixture lost a group"
  (firstBody, secondBody, emptyBody) <- case groups of
    [first, second, empty] -> pure
      (projectedBody first, projectedBody second, projectedBody empty)
    _ -> fail "module evidence fixture lost a group"
  let rows = projectedSites firstBody
      sameHead body root expected = nominalHead (projectedTypes body) root == Just expected
  assert (map siteId rows == [12, 13]
      && map siteOrdinal rows == [2, 3]
      && map siteOrigin rows == ["alpha-one", "alpha-two"]
      && sameHead firstBody (siteWire (head rows)) "Bool"
      && sameHead firstBody (siteWire (last rows)) "Addr#"
      && siteInputs (head rows) == [siteWire (last rows)])
    "group evidence changed original site order or scoped-root rebasing"
  assert (case projectedSites secondBody of
      [row] -> siteId row == 11 && sameHead secondBody (siteWire row) "Addr#"
      _ -> False)
    "group evidence included another owner's rows"
  assert (IntMap.null (typeGraphNodes (projectedTypes emptyBody)) && null (projectedSites emptyBody))
    "site-free group received unrelated module evidence"
  forM_ groups $ \group -> do
    selected <- either (fail . show) pure (projectPreparedModuleGroupsSelected context
      prepared (Just (Set.singleton (projectedOriginalOrdinal group))))
    assert (selected == [group]) "selection changed group evidence identity"
  lazyEmpty <- project (prepared
    { preparedBindings = [last (pmBindings prepared)]
    , preparedTypeGraph = error "site-free projection forced the module type graph"
    })
  assert (all (IntMap.null . typeGraphNodes . projectedTypes . projectedBody) lazyEmpty)
    "site-free projection produced type evidence"
  rejectShape "invalid reachable root"
    "finite type graph contains an out-of-range node" (prepared
    { preparedPreparedSites = [site alpha 21 "invalid-root" 0 999999 []] })
  compareFilterOracle context (prepared
    { preparedPreparedSites = [site alpha 21 "invalid-root" 0 999999 []] })
  rejectShape "invalid reachable edge"
    "finite type graph contains an out-of-range node" (prepared
    { preparedPreparedSites = [site alpha 22 "invalid-edge" 0 (raw firstRoot) []]
    , preparedTypeGraph = graph { typeGraphEdges = IntMap.insert (index firstRoot)
        [(TypeBody, TypeNodeId 999999)] (typeGraphEdges graph) }
    })
  compareFilterOracle context (prepared
    { preparedPreparedSites = [site alpha 22 "invalid-edge" 0 (raw firstRoot) []]
    , preparedTypeGraph = graph { typeGraphEdges = IntMap.insert (index firstRoot)
        [(TypeBody, TypeNodeId 999999)] (typeGraphEdges graph) } })
  rejectShape "duplicate selected site id"
    "duplicate selected prepared site id 23" (prepared
    { preparedPreparedSites = [site alpha 23 "duplicate-one" 0 (raw firstRoot) [],
                         site alpha 23 "duplicate-two" 1 (raw thirdRoot) []] })
  case projectPreparedModuleGroups context (prepared
    { preparedPreparedSites = [site alpha 24 "reachable-defect" 0 (raw thirdRoot) []]
    , preparedTypeGraph = graph { typeGraphNodes = IntMap.map (\node -> case node of
        TypeDeclaration tycon flags (OpaqueDeclaration _ "primitive") restriction ->
          TypeDeclaration tycon flags (ScalarDeclaration (BoxedRep Nothing)) restriction
        _ -> node) (typeGraphNodes graph) } }) of
    Left (InvalidPreparedRepresentation "runtime-polymorphic boxed representation") -> pure ()
    outcome -> fail ("reachable projection defect was skipped: " ++ show outcome)
  case projectPreparedModuleGroups context (prepared
    { preparedBindings = [last (pmBindings prepared)]
    , preparedPreparedSites = []
    , preparedSiteRejections = [SiteRejection alpha "unselected rejection",
                          SiteRejection gamma "empty-root rejection",
                          SiteRejection gamma "later rejection"]
    }) of
    Left (RejectedTypedSite "empty-root rejection") -> pure ()
    outcome -> fail ("empty-root fast path bypassed typed rejection: " ++ show outcome)
  putStrLn "module evidence projection: group selection and evidence checks passed"
 where
  owner = mkModule (stringToUnit "main") (mkModuleName "ModuleEvidence")
  binder unique occurrence = mkVanillaGlobal
    (mkExternalName (mkUnique 'e' unique) owner (mkVarOcc occurrence) noSrcSpan) addrPrimTy
  alpha = binder 1 "alpha"
  beta = binder 2 "beta"
  gamma = binder 3 "gamma"
  site binder_ sid origin ordinal root inputs = PreparedSite
    { psOwner = binder_
    , psSite = Effect.YieldSite sid origin ordinal
        (Effect.SiteType "unused" [] []) [] [] Nothing Nothing
    , psDelivery = Effect.DeliverHostAnswer
    , psWireNode = TypePolicy.TypeNodeId root
    , psInputNodes = map TypePolicy.TypeNodeId inputs
    }
  raw (TypeNodeId value) = value
  index (TypeNodeId value) = fromIntegral value
  (firstRoot, thirdRoot, graph) = case do
      ((first, third), builder) <- runStateT ((,) <$> TypePolicy.internType boolTy
        <*> TypePolicy.internType addrPrimTy) TypePolicy.emptyTypeGraphBuilder
      issued <- TypePolicy.finishTypeGraph builder
      pure (first, third, issued) of
    Right result -> result
    Left failure -> error (show failure)
  prepared = PreparedModule
    { preparedModule = owner
    , preparedCoverage = CompleteSourceModule
    , preparedBindings = [(StgTopStringLit binder_ "fixture", emptyVarSet)
                   | binder_ <- [alpha, beta, gamma]]
    , preparedOriginalTopNames = Set.fromList (map varName [alpha,beta,gamma])
    , preparedTagSigs = emptyNameEnv
    , preparedStableTopSpellings = Map.empty
    , preparedSitedSiblings = Map.empty
    , preparedYieldSites = []
    , preparedPreparedSites = [site beta 11 "beta" 1 (raw thirdRoot) [],
                         site alpha 12 "alpha-one" 2 (raw firstRoot) [raw thirdRoot],
                         site alpha 13 "alpha-two" 3 (raw thirdRoot) []]
    , preparedTypeGraph = graph
    , preparedSiteRejections = []
    , preparedRequestSiteTyCon = Nothing
    , preparedAuthorityDependent = False
    , preparedSiteDependencies = Nothing
    , preparedIntrinsicNames = Set.empty
    , preparedExpectedEntries = ProvisionalEntries Map.empty
    }
  context = ProjectionContext
    { projectionProfile = "ghc-9.12-prepared-stg"
    , projectionToolchain = "ghc-9.12.2"
    , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
    , projectionRetainedGenerations = Map.empty
    , projectionCurrentOriginals = mempty
    , projectionEntry = SymbolIdentity "main" "ModuleEvidence" "value" "alpha" Nothing
    , projectionAuxiliaryRoots = []
    , projectionFormattingAuthority = Nothing
    , projectionTimeAuthority = Nothing
    , projectionJsonAuthority = Nothing
    , projectionTextUnit = Nothing
    }
  project = either (fail . show) pure . projectPreparedModuleGroups context
  assert condition message = unless condition (fail message)
  rejectShape label expected module_ = case projectPreparedModuleGroups context module_ of
    Left (UnsupportedPreparedShape detail) | detail == expected -> pure ()
    outcome -> fail (label ++ " was not rejected: " ++ show outcome)

nominalHead :: TypeGraph -> TypeNodeId -> Maybe Text.Text
nominalHead graph root = do
  body <- child root TypeBody
  declaration <- child body TypeHead
  case node declaration of
    Just (TypeDeclaration identity _ _ _) -> Just (symbolOccurrence identity)
    _ -> Nothing
 where
  node (TypeNodeId raw) = IntMap.lookup (fromIntegral raw) (typeGraphNodes graph)
  child (TypeNodeId raw) role = case [target | (actual, target) <-
      IntMap.findWithDefault [] (fromIntegral raw) (typeGraphEdges graph), actual == role] of
    [target] -> Just target
    _ -> Nothing

-- The reference selector deliberately retains the old full-module filter.
-- Compare complete projection/refusal outcomes and CBOR, not just membership.
verifyGroupSelection :: ProjectionContext -> PreparedModule -> IO ()
verifyGroupSelection context prepared = do
  compareFilterOracle context prepared
  compareFilterOracle context (prepared
    { preparedSiteRejections = [SiteRejection (head owners) "original rejection"] })
  let intrinsic = prepared
        { preparedIntrinsicNames = Set.singleton (varName (head owners)) }
  compareFilterOracle context intrinsic
  case projectPreparedModuleGroups context intrinsic of
    Left (UnelaboratedCompilerIntrinsic _) -> pure ()
    outcome -> fail ("owning group view bypassed intrinsic refusal: " ++ show outcome)
  compareFilterOracle context recursive
  forM_ [take 1 recursiveSymbols, recursiveSymbols] $ \retained -> do
    let retainedContext = context
          { projectionRetainedGenerations = Map.fromList [(symbol, 7) | symbol <- retained] }
    compareFilterOracle retainedContext recursive
    projected <- either (fail . show) pure (projectPreparedModuleGroups retainedContext recursive)
    assert (map projectedOriginalOrdinal projected ==
      if length retained == 1 then [0, 1] else [0])
      "retained recursive selection split a group or renumbered a survivor"
  let rejectedSibling = prepared
        { preparedSiteRejections = [SiteRejection (head owners) "unselected sibling rejection"] }
  selected <- either (fail . show) pure (projectPreparedModuleGroupsSelected context
    rejectedSibling (Just (Set.singleton 2)))
  assert (map projectedOriginalOrdinal selected == [2])
    "selected group inherited an unrelated sibling rejection or changed ordinal"
  let sibling = last owners
      siblingSymbol = SymbolIdentity "main" "ModuleEvidence" "value" "gamma" Nothing
      referring = prepared
        { preparedBindings =
            [(StgTopLifted (StgNonRec first (StgRhsClosure emptyDVarSet dontCareCCS
                ReEntrant [] (StgApp sibling []) addrPrimTy)), emptyVarSet)
            , last (pmBindings prepared)]
        , preparedPreparedSites = [], preparedSiteRejections = [] }
      siblingRetained = context
        { projectionRetainedGenerations = Map.singleton siblingSymbol 9 }
  compareFilterOracle siblingRetained referring
  referred <- either (fail . show) pure (projectPreparedModuleGroups context referring)
  assert (map globalIdentity (projectedGlobals (projectedBody (head referred))) == [siblingSymbol])
    "singleton projection lost the original sibling import identity"
  forM_ (preparedBindingGroups metadata) $ \(_, view) -> do
    (binding, free) <- case pmBindings view of
      [item] -> pure item
      _ -> fail "owning view did not contain exactly one original group"
    assert (sort (map unique (nonDetEltsUniqSet free)) == sort (map unique owners))
      "singleton view lost the original IdSet"
    assert (pmModule view == pmModule metadata && pmCoverage view == pmCoverage metadata
      && Map.map unique (pmSitedSiblings view) == Map.map unique (pmSitedSiblings metadata)
      && pmYieldSites view == pmYieldSites metadata
      && pmRequestSiteTyCon view == pmRequestSiteTyCon metadata
      && pmTypeGraph view == pmTypeGraph metadata
      && map (Effect.ysSite . psSite) (pmPreparedSites view) ==
        map (Effect.ysSite . psSite) (pmPreparedSites metadata)
      && map srMessage (pmSiteRejections view) == map srMessage (pmSiteRejections metadata)
      && preparedUsesSiteAuthority view
      && all (preparedRejectsIntrinsic view) owners
      && map (fmap unique . preparedExpectedEntry view) owners == map (Just . unique) owners
      && length (topBinders binding) == 1)
      "singleton view changed compiler-owned sibling/site/type/entry metadata"
  -- The first view needs only the first input cell; it never searches its tail.
  let prefix = prepared { preparedBindings = head (pmBindings prepared) :
        error "singleton selection scanned later groups" }
  assert (length (pmBindings (snd (head (preparedBindingGroups prefix)))) == 1)
    "singleton selection did not retain one group"
  let collision = prepared
        { preparedBindings =
            [(StgTopStringLit (mkVanillaGlobal
              (mkSystemName (mkUnique 'c' index) (mkVarOcc occurrence)) addrPrimTy)
              "collision", emptyVarSet)
            | (index, occurrence) <- zip [1 ..] ["sat", "sat.1", "sat"]]
        , preparedPreparedSites = [], preparedSiteRejections = [] }
  collisionGroup <- either (fail . show) pure (projectPreparedModuleGroupsSelected context
    collision (Just (Set.singleton 2)))
  assert (map (map symbolOccurrence . projectedBinders) collisionGroup == [["sat.2"]])
    "group selection rebuilt the identity universe after filtering"
  forM_ ([16, 64, 256] :: [Int]) $ \count -> do
    let workload = prepared
          { preparedBindings =
              [(StgTopStringLit (binder (100 + fromIntegral index) ("group" ++ show index))
                  "small group", emptyVarSet) | index <- [1 .. count]]
          , preparedPreparedSites = [], preparedSiteRejections = [] }
        views = preparedBindingGroups workload
        newVisits = sum [length (pmBindings view) | (_, view) <- views]
        -- Count every candidate the independent full-filter oracle examines.
        oldVisits = sum [length (pmBindings workload) | _ <- views]
    compareFilterOracle context workload
    assert (newVisits == count && oldVisits == count * count)
      "many-small-groups selection did not have the expected operation counts"
    putStrLn ("group selection workload: groups=" ++ show count
      ++ " singleton-items=" ++ show newVisits ++ " reference-candidates=" ++ show oldVisits)
 where
  assert condition message = unless condition (fail message)
  unique = getKey . varUnique
  owners = [identifier | (binding, _) <- pmBindings prepared, identifier <- topBinders binding]
  binder index occurrence = mkVanillaGlobal
    (mkExternalName (mkUnique 'g' index) (pmModule prepared) (mkVarOcc occurrence) noSrcSpan) addrPrimTy
  first = binder 1 "recursiveFirst"
  second = binder 2 "recursiveSecond"
  rhs = StgRhsClosure emptyDVarSet dontCareCCS ReEntrant [] (StgLit LitNullAddr) addrPrimTy
  recursive = prepared
    { preparedBindings = [head (pmBindings prepared),
        (StgTopLifted (StgRec [(first, rhs), (second, rhs)]), emptyVarSet)]
    , preparedPreparedSites = [], preparedSiteRejections = [] }
  recursiveSymbols =
    [ SymbolIdentity "main" "ModuleEvidence" "value" occurrence Nothing
    | occurrence <- ["recursiveFirst", "recursiveSecond"] ]
  metadata = prepared
    { preparedBindings = [(binding, mkVarSet owners) | (binding, _) <- pmBindings prepared]
    , preparedSitedSiblings = Map.fromList [(show index, identifier)
        | (index, identifier) <- zip [0 :: Int ..] owners]
    , preparedYieldSites = map psSite (pmPreparedSites prepared)
    , preparedSiteRejections = [SiteRejection (head owners) "retained metadata"]
    , preparedAuthorityDependent = True
    , preparedIntrinsicNames = Set.fromList (map varName owners)
    , preparedExpectedEntries = ProvisionalEntries (Map.fromList [(varName identifier, identifier) | identifier <- owners]) }

compareFilterOracle :: ProjectionContext -> PreparedModule -> IO ()
compareFilterOracle context prepared = do
  let originals = pmBindings prepared
      referenceViews =
        [ (fromIntegral ordinal, prepared { preparedBindings = filter
              (\(candidate, _) -> map varUnique (topBinders candidate) ==
                map varUnique (topBinders binding)) originals })
        | (ordinal, (binding, _)) <- zip [0 :: Int ..] originals ]
      reference = concat <$> traverse (\(ordinal, view) -> do
        graph <- referenceTypeGraph view
        map (\group -> group { projectedOriginalOrdinal = ordinal }) <$>
          projectPreparedModuleGroups context (view { preparedTypeGraph = graph })) referenceViews
      actual = projectPreparedModuleGroups context prepared
  unless (actual == reference)
    (fail ("owning singleton projection differs from full-filter oracle: " ++ show (actual, reference)))
  case (actual, reference) of
    (Right groups, Right expected) -> unless
      (map encodeProjectedGroup groups == map encodeProjectedGroup expected)
      (fail "owning singleton projection changed original group CBOR")
    _ -> pure ()

-- Independent worklist and the old whole-map filters preserve the original
-- key order. Missing roots/edges refuse rather than disappear during lookup.
referenceTypeGraph :: PreparedModule -> Either ProjectionError TypePolicy.TypeGraph
referenceTypeGraph prepared = do
  reached <- walk Set.empty roots
  pure (TypeGraph
    (IntMap.filterWithKey (\key _ -> key `Set.member` reached) (typeGraphNodes graph))
    (IntMap.filterWithKey (\key _ -> key `Set.member` reached) (typeGraphEdges graph)))
 where
  graph = pmTypeGraph prepared
  owners = Set.fromList [getKey (varUnique identifier)
    | (binding, _) <- pmBindings prepared, identifier <- topBinders binding]
  roots = [node | site <- pmPreparedSites prepared
    , getKey (varUnique (psOwner site)) `Set.member` owners
    , node <- psWireNode site : psInputNodes site]
  walk visited [] = Right visited
  walk visited (TypeNodeId raw : pending)
    | key `Set.member` visited = walk visited pending
    | IntMap.member key (typeGraphNodes graph) = walk (Set.insert key visited)
        (map snd (IntMap.findWithDefault [] key (typeGraphEdges graph)) ++ pending)
    | otherwise = Left (UnsupportedPreparedShape
        "finite type graph contains an out-of-range node")
   where key = fromIntegral raw
