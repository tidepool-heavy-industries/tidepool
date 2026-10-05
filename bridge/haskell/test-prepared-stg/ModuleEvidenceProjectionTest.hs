module ModuleEvidenceProjectionTest (verifyModuleEvidenceProjection) where

import Tidepool.PreparedStg.Internal (PreparedModule(..), PreparedCoverage(..))
import Control.Monad (forM_, unless)
import Control.Monad.State.Strict (runStateT)
import Data.IntMap.Strict qualified as IntMap
import GHC.Core.TyCon (PrimRep(..))
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Builtin.Types (boolTy)
import GHC.Builtin.Types.Prim (addrPrimTy)
import GHC.Stg.Syntax (GenStgTopBinding(..))
import GHC.Types.Id (mkVanillaGlobal)
import GHC.Types.Name (mkExternalName)
import GHC.Types.Name.Env (emptyNameEnv)
import GHC.Types.Name.Occurrence (mkVarOcc)
import GHC.Types.SrcLoc (noSrcSpan)
import GHC.Types.Unique (mkUnique)
import GHC.Types.Var.Set (emptyVarSet)
import GHC.Unit.Module (mkModule, mkModuleName)
import GHC.Unit.Types (stringToUnit)
import Tidepool.EffectSchema qualified as Effect
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.PreparedSites (PreparedSite(..), SiteRejection(..))
import Tidepool.PreparedStg (pmModule, pmCoverage, pmBindings, pmTagSigs, pmSitedSiblings, pmYieldSites, pmPreparedSites, pmTypeGraph, pmSiteRejections, pmRequestSiteTyCon)
import Tidepool.TypePolicy qualified as TypePolicy

-- Pure projection checks use compiler-owned Ids and graphs, without compiling
-- another source fixture. The interleaved rows exercise module order rather
-- than the order of binder lookup or graph traversal.
verifyModuleEvidenceProjection :: IO ()
verifyModuleEvidenceProjection = do
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
  rejectShape "invalid reachable edge"
    "finite type graph contains an out-of-range node" (prepared
    { preparedPreparedSites = [site alpha 22 "invalid-edge" 0 (raw firstRoot) []]
    , preparedTypeGraph = graph { typeGraphEdges = IntMap.insert (index firstRoot)
        [(TypeBody, TypeNodeId 999999)] (typeGraphEdges graph) }
    })
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
  putStrLn "module evidence projection: 13 checks passed"
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
    , preparedTagSigs = emptyNameEnv
    , preparedSitedSiblings = Map.empty
    , preparedYieldSites = []
    , preparedPreparedSites = [site beta 11 "beta" 1 (raw thirdRoot) [],
                         site alpha 12 "alpha-one" 2 (raw firstRoot) [raw thirdRoot],
                         site alpha 13 "alpha-two" 3 (raw thirdRoot) []]
    , preparedTypeGraph = graph
    , preparedSiteRejections = []
    , preparedRequestSiteTyCon = Nothing
    , preparedAuthorityDependent = False
    , preparedIntrinsicNames = Set.empty
    }
  context = ProjectionContext
    { projectionProfile = "ghc-9.12-prepared-stg"
    , projectionToolchain = "ghc-9.12.2"
    , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
    , projectionRetainedGenerations = Map.empty
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
