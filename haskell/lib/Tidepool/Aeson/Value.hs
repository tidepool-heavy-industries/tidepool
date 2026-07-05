{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE UndecidableInstances #-}
-- | Vendored aeson Value type with construction.
--
-- This module provides the core JSON Value type and construction helpers.
--
-- Differences from upstream aeson:
--   - Array uses [Value] instead of V.Vector Value (avoids Array# primop)
--   - KeyMap uses Data.Map.Strict instead of HashMap (avoids hash primops)
module Tidepool.Aeson.Value
  ( -- * Core types
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
    -- * Decoding (primop anchor; the public decoder is
    --   'Tidepool.Aeson.FromJSON.eitherDecode')
  , eitherDecodeValue
    -- * ToJSON class
  , ToJSON(..)
  , GToJSON(..)
  , genericToJSON
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Tidepool.Aeson.Scientific
  ( Scientific, scientific, coefficient, base10Exponent
  , fromFloatDigits, toRealFloat )
import GHC.Generics
import GHC.TypeLits (TypeError, ErrorMessage(Text, (:<>:)))

-- | A JSON object key. Transparently Text (like 'FilePath') — object keys are
-- just Text, so @KM.lookup "k" m@ and @KM.keys@ work directly with no wrapper.
type Key = Text

-- | Convert Text to a Key. Identity (kept for call-site compatibility).
fromText :: Text -> Key
fromText = id

-- | Convert a Key back to Text. Identity (kept for call-site compatibility).
toText :: Key -> Text
toText = id

-- | KeyMap backed by Data.Map.Strict (avoids HashMap primop issues).
type KeyMap v = Map.Map Key v

-- | A JSON object.
type Object = KeyMap Value

-- | A JSON array (uses list instead of Vector to avoid Array# primops).
type Array = [Value]

-- | A key-value pair for building objects.
type Pair = (Key, Value)

-- | A JSON value. @Number@ carries an exact 'Scientific'
-- (@coefficient * 10 ^ base10Exponent@) — integer and fractional JSON numbers
-- share one aeson-faithful representation, with no @Double@ round-trip and no
-- Int64 cap (arbitrary-precision integers run on the JIT via @tidepool-bignum@).
data Value
  = Object !Object
  | Array Array
  | String !Text
  | Number !Scientific
  | Bool !Bool
  | Null
  deriving (Eq, Ord, Show)

-- | Construct a JSON object from key-value pairs.
object :: [Pair] -> Value
object = Object . Map.fromList

-- | Pair a text key with a JSON-encodable value.
(.=) :: ToJSON v => Text -> v -> Pair
k .= v = (k, toJSON v)
infixr 8 .=

-- | Empty JSON object.
emptyObject :: Value
emptyObject = Object Map.empty

-- | Empty JSON array.
emptyArray :: Value
emptyArray = Array []

-- | Decode a JSON document into an @'Either' 'Text' 'Value'@ — @Right v@ on
-- success, @Left msg@ (the serde_json parse error) on failure. This is the
-- internal primop anchor that the public @'Tidepool.Aeson.FromJSON.eitherDecode'@
-- is derived from — prefer that on the surface.
--
-- PURE — no effect. Calls to @eitherDecodeValue@ are intercepted in the
-- extractor (Translate.hs) and lowered to the @JsonDecode@ primop, which
-- dispatches to Rust @serde_json@ and builds the ADT directly on the heap. That
-- is why this can run inside a pure fold (e.g. JSONL-as-pure-fold) without an
-- effect handler. The body below is a stub that never actually runs at a call
-- site (the primop replaces it); it exists only so the name type-checks and
-- stays an opaque 'Var' for the interceptor to spot.
--
-- OPAQUE (not merely NOINLINE): the interceptor matches the name
-- @eitherDecodeValue@, but returning @Either@ makes GHC's CPR worker/wrapper
-- split it into @$weitherDecodeValue@ at -O2 — which the interceptor would miss,
-- leaving the stub to run (every decode → @Left@). OPAQUE forbids w/w (and
-- specialization/inlining), so the wrapper name survives verbatim into every
-- caller's Core.
{-# OPAQUE eitherDecodeValue #-}
eitherDecodeValue :: Text -> Either Text Value
eitherDecodeValue _ = Left T.empty

-- | A class for types that can be converted to JSON Value.
--
-- The default method encodes a single-constructor record generically via
-- 'GHC.Generics' — @data Rec = Rec {..} deriving (Generic, ToJSON)@ builds a
-- field-name-keyed JSON object. Sum types are rejected at compile time.
class ToJSON a where
  toJSON :: a -> Value
  default toJSON :: (Generic a, GToJSON (Rep a)) => a -> Value
  toJSON = genericToJSON

-- | Encode a single-constructor record as a field-name-keyed JSON object. This
-- is the implementation behind the 'ToJSON' default method.
genericToJSON :: (Generic a, GToJSON (Rep a)) => a -> Value
genericToJSON = gToJSON . from

-- | Encode a 'GHC.Generics' representation. @M1 D@/@M1 C@ are the outer layers;
-- the record fields underneath emit @[Pair]@ via 'GToRecord'.
class GToJSON f where
  gToJSON :: f a -> Value

-- | Emit the record fields of one constructor as JSON object pairs.
class GToRecord f where
  gToRecord :: f a -> [Pair]

-- Datatype metadata layer: transparent.
instance GToJSON f => GToJSON (M1 D d f) where
  gToJSON (M1 x) = gToJSON x

-- Constructor layer: a record becomes a JSON object.
instance GToRecord f => GToJSON (M1 C c f) where
  gToJSON (M1 x) = object (gToRecord x)

-- Product: concatenate the pairs from both field groups.
instance (GToRecord a, GToRecord b) => GToRecord (a :*: b) where
  gToRecord (a :*: b) = gToRecord a ++ gToRecord b

-- Selector leaf: one pair, keyed by exact selector name.
instance (Selector s, ToJSON c) => GToRecord (M1 S s (K1 R c)) where
  gToRecord m@(M1 (K1 c)) = [(T.pack (selName m), toJSON c)]

-- Nullary constructor: an empty record is an empty object.
instance GToRecord U1 where
  gToRecord _ = []

-- Sum types have no field-name-keyed object form under this encoder.
instance TypeError ('Text "deriving ToJSON via GHC.Generics supports single-constructor records only; "
                    ':<>: 'Text "this type has multiple constructors. Write an explicit ToJSON instance.")
    => GToJSON (a :+: b) where
  gToJSON = error "unreachable: sum ToJSON is a compile-time TypeError"

instance ToJSON Value where
  toJSON = id

instance ToJSON Text where
  toJSON = String

instance ToJSON Int where
  toJSON n = Number (scientific (fromIntegral n) 0)

instance ToJSON Double where
  toJSON = Number . fromFloatDigits

instance ToJSON Float where
  toJSON = Number . fromFloatDigits

instance ToJSON Bool where
  toJSON = Bool

instance {-# OVERLAPPABLE #-} ToJSON a => ToJSON [a] where
  toJSON = Array . map toJSON

instance {-# OVERLAPPING #-} ToJSON [Char] where
  toJSON cs = String (T.pack cs)

instance ToJSON a => ToJSON (Maybe a) where
  toJSON Nothing  = Null
  toJSON (Just a) = toJSON a

instance ToJSON () where
  toJSON () = Null

instance ToJSON Integer where
  -- Exact at any magnitude: 'Scientific' carries the full 'Integer' coefficient.
  toJSON n = Number (scientific n 0)

instance ToJSON Word where
  toJSON n = Number (scientific (fromIntegral n) 0)

instance ToJSON Char where
  toJSON c = String (T.singleton c)

instance ToJSON Ordering where
  toJSON LT = String "LT"
  toJSON EQ = String "EQ"
  toJSON GT = String "GT"

instance (ToJSON a, ToJSON b) => ToJSON (Either a b) where
  toJSON (Left a)  = Object (Map.singleton "Left" (toJSON a))
  toJSON (Right b) = Object (Map.singleton "Right" (toJSON b))

instance (ToJSON a, ToJSON b) => ToJSON (a, b) where
  toJSON (a, b) = Array [toJSON a, toJSON b]

instance (ToJSON a, ToJSON b, ToJSON c) => ToJSON (a, b, c) where
  toJSON (a, b, c) = Array [toJSON a, toJSON b, toJSON c]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d) => ToJSON (a, b, c, d) where
  toJSON (a, b, c, d) = Array [toJSON a, toJSON b, toJSON c, toJSON d]

instance (ToJSON a, ToJSON b, ToJSON c, ToJSON d, ToJSON e) => ToJSON (a, b, c, d, e) where
  toJSON (a, b, c, d, e) = Array [toJSON a, toJSON b, toJSON c, toJSON d, toJSON e]

instance ToJSON a => ToJSON (Map.Map Text a) where
  toJSON m = Object (Map.map toJSON m)  -- keys are already Text (= Key)

instance ToJSON a => ToJSON (Set.Set a) where
  toJSON = Array . map toJSON . Set.toList

