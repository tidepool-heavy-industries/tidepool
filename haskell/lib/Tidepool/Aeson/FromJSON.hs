{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE UndecidableInstances #-}
-- | Structural @FromJSON@: decode an already-parsed 'Value' into a typed result.
--
-- This is PURE Haskell over the vendored Double-based 'Value' — it carries none
-- of upstream aeson's @unsafePerformIO@ / exception / text-parser machinery.
-- The text→'Value' step happens Rust-side (the @ParseJson@ effect, serde_json);
-- this module is only the @Value -> a@ half, which runs cleanly on the JIT
-- (typeclass-dictionary dispatch over constructor pattern-matches).
module Tidepool.Aeson.FromJSON
  ( FromJSON(..)
  , GFromJSON(..)
  , genericParseJSON
  , Result(..)
  , fromJSON
  , resultToEither
  , eitherDecode
    -- * Object field accessors (aeson-style)
  , (.:)
  , (.:?)
  , (.!=)
    -- * Type-directed parsers
  , withObject
  , withText
  , withArray
  , withBool
  , withDouble
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import Tidepool.Aeson.Value (Value(..), Object, Array, fromText, toText, eitherDecodeValue)
import Data.Proxy (Proxy(..))
import GHC.Generics
import GHC.TypeLits (TypeError, ErrorMessage(Text, (:<>:)))

-- | The result of a structural decode: a typed value or an error message.
data Result a = Error String | Success a
  deriving (Eq, Show)

instance Functor Result where
  fmap f (Success a) = Success (f a)
  fmap _ (Error e)   = Error e

instance Applicative Result where
  pure = Success
  Success f <*> r = fmap f r
  Error e   <*> _ = Error e

instance Monad Result where
  return = pure
  Success a >>= f = f a
  Error e   >>= _ = Error e

-- | Types decodable from a JSON 'Value'. @parseJSON@ pattern-matches the
-- structural shape; mismatches return 'Error' (no exceptions).
--
-- The default method decodes a single-constructor record generically via
-- 'GHC.Generics' — @data Rec = Rec {..} deriving (Generic, FromJSON)@ decodes a
-- field-name-keyed JSON object into the record. Sum types are rejected at
-- compile time; write an explicit instance for those.
class FromJSON a where
  parseJSON :: Value -> Result a
  default parseJSON :: (Generic a, GFromJSON (Rep a)) => Value -> Result a
  parseJSON = genericParseJSON

-- | Decode a 'Value'. @FromJSON Value@ is the identity, so @fromJSON v :: Result Value@
-- round-trips the raw value — one entry point covers both raw and typed decoding.
fromJSON :: FromJSON a => Value -> Result a
fromJSON = parseJSON

-- | Decode a single-constructor record from a JSON object, keyed by exact
-- selector name. This is the implementation behind the 'FromJSON' default
-- method: @deriving (Generic, FromJSON)@ resolves @parseJSON@ to this.
genericParseJSON :: (Generic a, GFromJSON (Rep a)) => Value -> Result a
genericParseJSON v = to <$> gParseJSON v

-- | Structural decode over a 'GHC.Generics' representation. @M1 D@ (datatype)
-- and @M1 C@ (constructor) are the 'Value'-level layers; the record fields
-- underneath decode from an 'Object' via 'GFromRecord'.
class GFromJSON f where
  gParseJSON :: Value -> Result (f a)

-- | Decode the record fields of one constructor from a JSON 'Object'.
class GFromRecord f where
  gParseRecord :: Object -> Result (f a)

-- Datatype metadata layer: transparent.
instance GFromJSON f => GFromJSON (M1 D d f) where
  gParseJSON v = M1 <$> gParseJSON v

-- Constructor layer: a record decodes from a JSON object.
instance GFromRecord f => GFromJSON (M1 C c f) where
  gParseJSON = withObject "record" (\o -> M1 <$> gParseRecord o)

-- Product: each field group reads its own keys out of the shared object.
instance (GFromRecord a, GFromRecord b) => GFromRecord (a :*: b) where
  gParseRecord o = (:*:) <$> gParseRecord o <*> gParseRecord o

-- Selector leaf: look the field up by its exact selector name, decode via its
-- own 'FromJSON' instance (so nested records recurse through the default).
instance (Selector s, FromJSON c) => GFromRecord (M1 S s (K1 R c)) where
  gParseRecord o = (M1 . K1) <$> (o .: fieldName)
    -- The proxy is a real (non-bottom) 'Proxy' constructor rather than
    -- 'undefined': 'selName' inspects only the phantom selector type @s@, and a
    -- bottom here would be forced by the tree-walking eval oracle (though not by
    -- the JIT), diverging the two engines.
    where fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

-- Nullary constructor: an empty record decodes from any object.
instance GFromRecord U1 where
  gParseRecord _ = Success U1

-- Sum types have no field-name-keyed object form under this decoder.
instance TypeError ('Text "deriving FromJSON via GHC.Generics supports single-constructor records only; "
                    ':<>: 'Text "this type has multiple constructors. Write an explicit FromJSON instance.")
    => GFromJSON (a :+: b) where
  gParseJSON = error "unreachable: sum FromJSON is a compile-time TypeError"

-- | Project a 'Result' to 'Either', carrying the error as 'Text'.
resultToEither :: Result a -> Either Text a
resultToEither (Success a) = Right a
resultToEither (Error e)   = Left (T.pack e)

-- | Decode a JSON document straight into a typed value, aeson-style: @Right a@
-- on success, @Left msg@ on a parse error (from serde_json) or a shape mismatch
-- (from 'fromJSON'). PURE — no effect and no abort, unlike the HTTP @parseJson@
-- verb. Because 'Value' has an identity 'FromJSON' instance,
-- @eitherDecode \@Value@ is the raw parse.
eitherDecode :: FromJSON a => Text -> Either Text a
eitherDecode t = eitherDecodeValue t >>= resultToEither . fromJSON

mismatch :: String -> Value -> Result a
mismatch want v = Error ("expected " ++ want ++ ", got " ++ kindOf v)

kindOf :: Value -> String
kindOf v = case v of
  Object _ -> "object"
  Array _  -> "array"
  String _ -> "string"
  Number _ -> "number"
  NumberI _ -> "number"
  Bool _   -> "bool"
  Null     -> "null"

-- Raw passthrough: lets `parseJson t :: M Value` fall out of the polymorphic helper.
instance FromJSON Value where
  parseJSON = Success

instance FromJSON Bool where
  parseJSON (Bool b) = Success b
  parseJSON v        = mismatch "bool" v

instance FromJSON Text where
  parseJSON (String s) = Success s
  parseJSON v          = mismatch "string" v

instance FromJSON Double where
  parseJSON (Number n)  = Success n
  parseJSON (NumberI n) = Success (fromIntegral n)
  parseJSON v           = mismatch "number" v

-- Truncates toward zero, matching the `_Int` prism (Tidepool.Aeson.Lens).
instance FromJSON Int where
  parseJSON (NumberI n) = Success n
  parseJSON (Number n)  = Success (truncate n)
  parseJSON v           = mismatch "number" v

instance FromJSON a => FromJSON [a] where
  parseJSON (Array xs) = traverse parseJSON xs
  parseJSON v          = mismatch "array" v

instance FromJSON a => FromJSON (Maybe a) where
  parseJSON Null = Success Nothing
  parseJSON v    = Just <$> parseJSON v

instance FromJSON a => FromJSON (Map.Map Text a) where
  parseJSON (Object o) = Map.foldrWithKey step (Success Map.empty) o
    where step k v acc = Map.insert (toText k) <$> parseJSON v <*> acc
  parseJSON v          = mismatch "object" v

-- | Required-field accessor: @o .: "name"@ looks up the key in a decoded
-- 'Object' and decodes it, erroring if the key is absent. Use under
-- 'withObject', exactly like upstream aeson:
--
-- > parseJSON = withObject "Person" $ \o -> Person <$> o .: "name" <*> o .: "age"
(.:) :: FromJSON a => Object -> Text -> Result a
o .: k = case Map.lookup (fromText k) o of
  Just v  -> parseJSON v
  Nothing -> Error ("key " ++ show (T.unpack k) ++ " not present")
infixl 9 .:

-- | Optional-field accessor: a missing key OR an explicit @null@ yields
-- 'Nothing'; a present value is decoded under 'Just'.
(.:?) :: FromJSON a => Object -> Text -> Result (Maybe a)
o .:? k = case Map.lookup (fromText k) o of
  Nothing   -> Success Nothing
  Just Null -> Success Nothing
  Just v    -> Just <$> parseJSON v
infixl 9 .:?

-- | Supply a default for an optional field: @o .:? "k" .!= def@.
(.!=) :: Result (Maybe a) -> a -> Result a
r .!= def = fmap (maybe def id) r
infixl 6 .!=

-- | Run a parser against an object, erroring on any other shape. The first
-- argument names the type being parsed (used only in the error message).
withObject :: String -> (Object -> Result a) -> Value -> Result a
withObject _    f (Object o) = f o
withObject name _ v          = typeMismatch name "object" v

-- | Run a parser against a string.
withText :: String -> (Text -> Result a) -> Value -> Result a
withText _    f (String s) = f s
withText name _ v          = typeMismatch name "string" v

-- | Run a parser against an array.
withArray :: String -> (Array -> Result a) -> Value -> Result a
withArray _    f (Array xs) = f xs
withArray name _ v          = typeMismatch name "array" v

-- | Run a parser against a boolean.
withBool :: String -> (Bool -> Result a) -> Value -> Result a
withBool _    f (Bool b) = f b
withBool name _ v        = typeMismatch name "bool" v

-- | Run a parser against a number (the vendored 'Value' carries 'Double',
-- not 'Scientific').
withDouble :: String -> (Double -> Result a) -> Value -> Result a
withDouble _    f (Number n) = f n
withDouble _    f (NumberI n) = f (fromIntegral n)
withDouble name _ v          = typeMismatch name "number" v

typeMismatch :: String -> String -> Value -> Result a
typeMismatch name want v =
  Error ("parsing " ++ name ++ " failed: expected " ++ want ++ ", got " ++ kindOf v)
