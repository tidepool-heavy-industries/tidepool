{-# LANGUAGE DeriveGeneric, DeriveAnyClass #-}
import qualified GHC.Generics as G
import qualified CellDisplayReexports as R
import qualified CellDisplayExternal as Foreign
import Tidepool.Aeson (FromJSON)
import Tidepool.Aeson.Schema (JsonSchema)

data Qualified = Qualified { qualifiedSentinel :: Int }
  deriving (G.Generic, FromJSON, JsonSchema)
data Standalone = Standalone { standaloneSentinel :: Int }
deriving instance G.Generic Standalone
deriving instance FromJSON Standalone
deriving instance JsonSchema Standalone
data Reexported = Reexported { reexportedSentinel :: Int }
  deriving (R.Generic, FromJSON, JsonSchema)
data Automatic = Automatic { automaticSentinel :: Int }
  deriving (FromJSON, JsonSchema)
data ForeignIdentity = ForeignIdentity { foreignSentinel :: Int }
  deriving (FromJSON, JsonSchema)
instance Foreign.Generic ForeignIdentity where foreignGeneric _ = ()
