{-# LANGUAGE DataKinds, DeriveAnyClass, TypeOperators #-}
import Tidepool.Prelude
import Tidepool.Agent.Contract

data CaptureRead = CaptureRead deriving (FromJSON, JsonSchema)
data CaptureTools mode = CaptureTools
  { ping :: mode :- Call CaptureRead Int
  , haskell :: mode :- HaskellCell '[]
  }

data InvalidCaptureTools mode = InvalidCaptureTools
  { scalarPing :: mode :- Call Text Int }

data CaptureSyncTools mode = CaptureSyncTools
  { syncPing :: mode :- Sync (Call CaptureRead Int) }

data InvalidCaptureSyncTools mode = InvalidCaptureSyncTools
  { scalarSyncPing :: mode :- Sync (Call Text Int) }
