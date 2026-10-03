-- | Immutable output shared by interface certification and executable preparation.
module Tidepool.FinalizedModule (FinalizedModule(..), homeInterfaceUsageOwners) where

import GHC.Unit.Home.ModInfo (HomeModInfo)
import GHC.Unit.Module.ModGuts (CgGuts)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_usages)
import GHC.Unit.Module (moduleUnit, moduleName, moduleNameString)
import GHC.Unit.Module.Deps (Usage(..))
import GHC.Unit.Types (unitIdString, unitString, toUnitId)
import GHC.Driver.Env (HscEnv, hsc_all_home_unit_ids)
import Data.Set qualified as Set

-- | The interface and Core have one finalization owner. Retaining this pair
-- permits later preparation from the exact result that supplied its interface,
-- without replaying source or retaining a mutable compiler session.
data FinalizedModule = FinalizedModule
  { finalizedHomeModInfo :: HomeModInfo
  , finalizedTidyGuts :: CgGuts
  }

-- GHC records indirect home type uses independently of authored imports.
-- These edges retain interfaces and grant no native execution authority.
homeInterfaceUsageOwners :: HscEnv -> ModIface -> [(String, String)]
homeInterfaceUsageOwners env iface = Set.toAscList (Set.delete self (Set.fromList
  (concatMap owner (mi_usages iface))))
  where
    self = (unitString (moduleUnit (mi_module iface)), moduleNameString (moduleName (mi_module iface)))
    owner UsageHomeModule{usg_mod_name = name, usg_unit_id = unit} =
      [(unitIdString unit, moduleNameString name)]
    owner UsageHomeModuleInterface{usg_mod_name = name, usg_unit_id = unit} =
      [(unitIdString unit, moduleNameString name)]
    owner UsagePackageModule{usg_mod = required}
      | toUnitId (moduleUnit required) `Set.member` hsc_all_home_unit_ids env =
          [(unitString (moduleUnit required),moduleNameString (moduleName required))]
    owner _ = []
