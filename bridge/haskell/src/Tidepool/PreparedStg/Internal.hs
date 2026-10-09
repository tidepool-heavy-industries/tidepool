-- | Compiler-owned prepared representation. Structural projection tests may
-- inspect malformed IR here; this data issues no source or execution admission.
module Tidepool.PreparedStg.Internal (PreparedModule(..), PreparedCoverage(..), PreparedEntryContext(..)) where

import Data.Map.Strict (Map)
import Data.Set (Set)
import Data.Text (Text)
import GHC.Core.TyCon (TyCon)
import GHC.Stg.Pipeline (StgCgInfos)
import GHC.Stg.Syntax (CgStgTopBinding)
import GHC.Types.Name (Name)
import GHC.Types.Var (Id)
import GHC.Types.Var.Set (IdSet)
import GHC.Unit.Types (Module)
import Tidepool.FatIface (OwnerInterfaceContext)
import Tidepool.EffectSchema (YieldSite)
import Tidepool.PreparedSites (PreparedSite, SiteRejection, PreparedSiteDependencies)
import Tidepool.TypePolicy (TypeGraph)

-- | A missing top in a complete source module is a producer defect. A package
-- body subset can still reference unavailable package bodies; those remain
-- explicit globals with recovery diagnostics, not fabricated home definitions.
data PreparedCoverage = CompleteSourceModule | ExactBodySubset
  deriving (Eq, Show)

-- Direct typed Core adapters may carry provisional entries. Canonical units
-- retain their exact declaring owner; assembly never accepts provisional maps.
data PreparedEntryContext
  = ProvisionalEntries (Map Name Id)
  | DeclaringEntries OwnerInterfaceContext

-- | Prepared output and projection evidence for one defining module.
data PreparedModule = PreparedModule
  { preparedModule :: Module
  , preparedCoverage :: PreparedCoverage
  , preparedBindings :: [(CgStgTopBinding, IdSet)]
  -- | Exact top Names before CorePrep can introduce floated bindings.
  , preparedOriginalTopNames :: Set Name
  -- | Canonical package-unit spellings issued before subset selection. Source
  -- modules retain their normal compiler identity allocation.
  , preparedStableTopSpellings :: Map Name Text
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
  , preparedSiteDependencies :: Maybe PreparedSiteDependencies
  , preparedIntrinsicNames :: Set Name
  -- | Exact package declaring Ids, separate from provisional STG shapes.
  -- Recovery uses the latter to discover dependencies before final emission.
  , preparedExpectedEntries :: PreparedEntryContext
  }
