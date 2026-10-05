-- | Compiler-owned prepared representation. Structural projection tests may
-- inspect malformed IR here; this data issues no source or execution admission.
module Tidepool.PreparedStg.Internal (PreparedModule(..), PreparedCoverage(..)) where

import Data.Map.Strict (Map)
import Data.Set (Set)
import GHC.Core.TyCon (TyCon)
import GHC.Stg.Pipeline (StgCgInfos)
import GHC.Stg.Syntax (CgStgTopBinding)
import GHC.Types.Name (Name)
import GHC.Types.Var (Id)
import GHC.Types.Var.Set (IdSet)
import GHC.Unit.Types (Module)
import Tidepool.EffectSchema (YieldSite)
import Tidepool.PreparedSites (PreparedSite, SiteRejection)
import Tidepool.TypePolicy (TypeGraph)

-- | A missing top in a complete source module is a producer defect. A package
-- body subset can still reference unavailable package bodies; those remain
-- explicit globals with recovery diagnostics, not fabricated home definitions.
data PreparedCoverage = CompleteSourceModule | ExactBodySubset
  deriving (Eq, Show)

-- | Prepared output and projection evidence for one defining module.
data PreparedModule = PreparedModule
  { preparedModule :: Module
  , preparedCoverage :: PreparedCoverage
  , preparedBindings :: [(CgStgTopBinding, IdSet)]
  , preparedTagSigs :: StgCgInfos
  -- | Defining-module sibling Ids only, replayed after memo validity checks.
  , preparedSitedSiblings :: Map String Id
  , preparedYieldSites :: [YieldSite]
  , preparedPreparedSites :: [PreparedSite]
  , preparedTypeGraph :: TypeGraph
  -- | Typed sites that failed elaboration, raised only if projection
  -- reaches their top binder.
  , preparedSiteRejections :: [SiteRejection]
  , preparedRequestSiteTyCon :: Maybe TyCon
  , preparedAuthorityDependent :: Bool
  , preparedIntrinsicNames :: Set Name
  }

