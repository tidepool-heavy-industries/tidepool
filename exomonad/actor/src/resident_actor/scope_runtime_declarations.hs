{-# LANGUAGE DeriveGeneric, DeriveAnyClass, TypeOperators #-}
import Tidepool.Prelude
import Tidepool.Agent.Contract

data ScopePing = ScopePing { sentinel :: Int } deriving (FromJSON, JsonSchema)
data ScopeTools mode = ScopeTools { ping :: mode :- Call ScopePing Int }

_ <- say (tshow True)
