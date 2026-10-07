{-# LANGUAGE DeriveGeneric, DeriveAnyClass, TypeOperators #-}
import Tidepool.Prelude
import Tidepool.Agent.Contract

data ScopePing = ScopePing { sentinel :: Int } deriving (Generic, FromJSON, JsonSchema)
data ScopeTools mode = ScopeTools { ping :: mode :- Call ScopePing Int } deriving Generic

pure True
