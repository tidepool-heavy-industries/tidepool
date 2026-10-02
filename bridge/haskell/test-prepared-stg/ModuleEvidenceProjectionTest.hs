{-# LANGUAGE PatternSynonyms #-}

module ModuleEvidenceProjectionTest (verifyModuleEvidenceProjection) where

import Control.Monad (forM_, unless)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import GHC.Builtin.Types (boolTy, boolTyCon)
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
import Tidepool.PreparedStg (PreparedModule(..), PreparedCoverage(..))
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
  let firstNode = TypeUnconstructible "first" "A"
      thirdNode = TypeUnconstructible "third" "C"
      row sid origin ordinal root inputs = SiteRow sid origin ordinal HostAnswer root inputs
  assert (projectedTypes firstBody == [firstNode, thirdNode]
      && projectedSites firstBody ==
        [row 12 "alpha-one" 2 (TypeNodeId 0) [TypeNodeId 1],
         row 13 "alpha-two" 3 (TypeNodeId 1) []])
    "group evidence changed original node/site order or rebasing"
  assert (projectedTypes secondBody == [thirdNode]
      && projectedSites secondBody == [row 11 "beta" 1 (TypeNodeId 0) []])
    "group evidence included another owner's rows"
  assert (null (projectedTypes emptyBody) && null (projectedSites emptyBody))
    "site-free group received unrelated module evidence"
  forM_ groups $ \group -> do
    selected <- either (fail . show) pure (projectPreparedModuleGroupsSelected context
      prepared (Just (Set.singleton (projectedOriginalOrdinal group))))
    assert (selected == [group]) "selection changed group evidence identity"
  lazyEmpty <- project (prepared
    { pmBindings = [last (pmBindings prepared)]
    , pmTypeGraph = error "site-free projection forced the module type graph"
    })
  assert (all (null . projectedTypes . projectedBody) lazyEmpty)
    "site-free projection produced type evidence"
  rejectShape "invalid reachable root"
    "prepared type graph contains an out-of-range node" (prepared
    { pmPreparedSites = [site alpha 21 "invalid-root" 0 99 []] })
  rejectShape "invalid reachable edge"
    "prepared type graph contains an out-of-range node" (prepared
    { pmPreparedSites = [site alpha 22 "invalid-edge" 0 0 []]
    , pmTypeGraph = TypePolicy.TypeGraph
        [TypePolicy.DataG boolTy boolTyCon [TypePolicy.TypeNodeId 99] []]
    })
  rejectShape "duplicate selected site id"
    "duplicate selected prepared site id 23" (prepared
    { pmPreparedSites = [site alpha 23 "duplicate-one" 0 0 [],
                         site alpha 23 "duplicate-two" 1 2 []] })
  case projectPreparedModuleGroups context (prepared
    { pmPreparedSites = [site alpha 24 "reachable-defect" 0 1 []] }) of
    Left (InvalidPreparedRepresentation "unreachable defect") -> pure ()
    outcome -> fail ("reachable projection defect was skipped: " ++ show outcome)
  case projectPreparedModuleGroups context (prepared
    { pmBindings = [last (pmBindings prepared)]
    , pmPreparedSites = []
    , pmSiteRejections = [SiteRejection alpha "unselected rejection",
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
        (Effect.SiteType "unused" [] []) [] [] Nothing
    , psDelivery = Effect.DeliverHostAnswer
    , psWireNode = TypePolicy.TypeNodeId root
    , psInputNodes = map TypePolicy.TypeNodeId inputs
    }
  prepared = PreparedModule
    { pmModule = owner
    , pmCoverage = CompleteSourceModule
    , pmBindings = [(StgTopStringLit binder_ "fixture", emptyVarSet)
                   | binder_ <- [alpha, beta, gamma]]
    , pmTagSigs = emptyNameEnv
    , pmSitedSiblings = Map.empty
    , pmYieldSites = []
    , pmPreparedSites = [site beta 11 "beta" 1 2 [],
                         site alpha 12 "alpha-one" 2 0 [2],
                         site alpha 13 "alpha-two" 3 2 []]
    , pmTypeGraph = TypePolicy.TypeGraph
        [ TypePolicy.UnconstructibleG "first" "A"
        , TypePolicy.ProjectionDefectG "unreachable defect"
        , TypePolicy.UnconstructibleG "third" "C" ]
    , pmSiteRejections = []
    , pmEffectRequestTypeIds = Set.empty
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
