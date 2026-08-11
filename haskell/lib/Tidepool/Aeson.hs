-- | Vendored aeson — re-exports construction types and lens accessors.
--
-- Drop-in replacement for Data.Aeson + Data.Aeson.Lens.
--
-- "Tidepool.Aeson.Schema" is deliberately NOT re-exported here: upstream
-- aeson has no schema class, and this umbrella is in every eval's compile
-- closure (via "Tidepool.Prelude") while @JsonSchema@ is consumed only by the
-- agent surfaces. Import it directly, or get it from
-- "Tidepool.Agent.Contract", which re-exports it.
module Tidepool.Aeson
  ( -- * Core types (from Tidepool.Aeson.Value)
    Value(..)
  , Scientific
  , scientific
  , coefficient
  , base10Exponent
  , fromFloatDigits
  , toRealFloat
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
  , eitherDecode
  , decode
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
  , _Integer
  , _Double
  , _Null
  ) where

import Tidepool.Aeson.Value
import Tidepool.Aeson.Lens
import Tidepool.Aeson.FromJSON
