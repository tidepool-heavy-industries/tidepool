{-# LANGUAGE DataKinds, DeriveAnyClass, TypeOperators #-}
import Tidepool.Prelude
import Tidepool.Agent.Contract

data CaptureRead = CaptureRead deriving (FromJSON, JsonSchema)
data CaptureTools mode = CaptureTools
  { ping :: mode :- Call CaptureRead Int
  , haskell :: mode :- HaskellCell '[]
  }
