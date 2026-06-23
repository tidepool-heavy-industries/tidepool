-- | Vendored aeson — re-exports construction types and lens accessors.
--
-- Drop-in replacement for Data.Aeson + Data.Aeson.Lens.
module Tidepool.Aeson
  ( -- * Core types (from Tidepool.Aeson.Value)
    Value(..)
  , Key
  , KeyMap
  , Object
  , Array
  , Pair
    -- * Key construction
  , fromText
  , toText
    -- * Value construction
  , object
  , (.=)
  , emptyObject
  , emptyArray
    -- * ToJSON class
  , ToJSON(..)
    -- * FromJSON / structural decode (from Tidepool.Aeson.FromJSON)
  , FromJSON(..)
  , Result(..)
  , fromJSON
  , resultToEither
  , (.:)
  , (.:?)
  , (.!=)
  , withObject
  , withText
  , withArray
  , withBool
  , withDouble
    -- * Lens accessors (from Tidepool.Aeson.Lens)
  , key
  , members
  , nth
  , values
  , _String
  , _Number
  , _Bool
  , _Array
  , _Object
  , _Int
  , _Double
  , _Null
  ) where

import Tidepool.Aeson.Value
import Tidepool.Aeson.Lens
import Tidepool.Aeson.FromJSON
