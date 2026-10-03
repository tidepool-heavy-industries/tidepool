-- | Immutable output shared by interface certification and executable preparation.
module Tidepool.FinalizedModule (FinalizedModule(..)) where

import GHC.Unit.Home.ModInfo (HomeModInfo)
import GHC.Unit.Module.ModGuts (CgGuts)

-- | The interface and Core have one finalization owner. Retaining this pair
-- permits later preparation from the exact result that supplied its interface,
-- without replaying source or retaining a mutable compiler session.
data FinalizedModule = FinalizedModule
  { finalizedHomeModInfo :: HomeModInfo
  , finalizedTidyGuts :: CgGuts
  }

