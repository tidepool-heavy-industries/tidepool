{-# LANGUAGE DataKinds, DeriveGeneric, DeriveAnyClass, TypeOperators #-}
import Tidepool.Prelude
import Tidepool.Agent.Contract

data CaptureRead = CaptureRead deriving (Generic, FromJSON, JsonSchema)
data CaptureTools mode = CaptureTools
  { ping :: mode :- Call CaptureRead Int
  , haskell :: mode :- HaskellCell '[]
  } deriving Generic
