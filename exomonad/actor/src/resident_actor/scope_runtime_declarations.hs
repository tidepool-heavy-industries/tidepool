{-# LANGUAGE DeriveGeneric, DeriveAnyClass, TypeOperators #-}
import Tidepool.Prelude
import Tidepool.Agent.Contract
import qualified Tidepool.Effects as Effects

data ScopePing = ScopePing { sentinel :: Int } deriving (FromJSON, JsonSchema)
data ScopeTools mode = ScopeTools { ping :: mode :- Call ScopePing Int }
data ScopeTargetTools mode = ScopeTargetTools
  { acknowledge :: mode :- Call ScopePing ()
  , respond :: mode :- Call ScopePing ()
  }

_ <- say (tshow True)
