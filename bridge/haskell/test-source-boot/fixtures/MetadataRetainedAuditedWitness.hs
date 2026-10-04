{-# LANGUAGE EmptyDataDecls #-}
{-# LANGUAGE QuasiQuotes #-}
module MetadataRetainedWitness (RetainedWitness, witnessValue) where

import Data.Text (Text)
import Tidepool.QQ.Validate (uri)

data RetainedWitness

witnessValue :: Text
witnessValue = [uri|https://example.com/retained|]
