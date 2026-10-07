{-# LANGUAGE DeriveGeneric, DeriveAnyClass, TypeOperators #-}
import Tidepool.Prelude
import Tidepool.Agent.Contract
import qualified Tidepool.Effects as Effects

data ScopePing = ScopePing { sentinel :: Int } deriving (FromJSON, JsonSchema)
data ScopeTools mode = ScopeTools { ping :: mode :- Call ScopePing Int }

_ <- say (tshow True)
