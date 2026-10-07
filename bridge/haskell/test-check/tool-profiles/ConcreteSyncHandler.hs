{-# LANGUAGE DataKinds, DeriveGeneric, OverloadedStrings #-}
module ConcreteSyncHandler where
import Control.Monad.Freer (Eff, send)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (ContextReadWrite (..))

-- Ignoring the record's mode parameter must not mislabel a sync-row handler
-- as an async tool with the empty base profile.
data Tools mode = Tools
  { disguised :: Presented (Tool (Eff '[ContextReadWrite]) Text Text)
  } deriving (Generic)

tools :: Tools (AsServerT (Eff '[]))
tools = Tools (presentWith id $ tool "Invalid concrete sync handler" $ \model -> send (SetNextModelWith model) >> pure model)

invalid = compileInstalledTools tools
