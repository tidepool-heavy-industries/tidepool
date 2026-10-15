{-# LANGUAGE DataKinds, DeriveGeneric, TypeOperators #-}
module ReloadSync where
import Prelude
import GHC.Generics (Generic)
import Control.Monad.Freer (Eff)
import Tidepool.Agent.Contract
import Tidepool.Agent.ToolEffects (SyncEffects)
import Tidepool.Effects (Source, SourceStatus, SourceError, sourceStatus)

-- Inspection remains available to ordinary asynchronous source tools.
inspectSource :: Eff '[Source] (Either SourceError SourceStatus)
inspectSource = sourceStatus

-- Renaming the authored field preserves its synchronous capability.
data Tools mode = Tools
  { activateDraft :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  } deriving (Generic)

spec :: AgentSpec Tools '[]
spec = defaultSpec { specTools = Tools standardReload }

-- The ordinary notebook facade retains the compatible default wrapper.
compatible :: Eff (SyncEffects '[]) (Either ReloadError ReloadOutcome)
compatible = reloadSource []
