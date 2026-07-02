{-# LANGUAGE FlexibleInstances #-}
-- | Structural @FromJSON@: decode an already-parsed 'Value' into a typed result.
--
-- This is PURE Haskell over the vendored Double-based 'Value' — it carries none
-- of upstream aeson's @unsafePerformIO@ / exception / text-parser machinery.
-- The text→'Value' step happens Rust-side (the @ParseJson@ effect, serde_json);
-- this module is only the @Value -> a@ half, which runs cleanly on the JIT
-- (typeclass-dictionary dispatch over constructor pattern-matches).
module Tidepool.Aeson.FromJSON
  ( FromJSON(..)
  , Result(..)
  , fromJSON
  , resultToEither
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
import Tidepool.Aeson.Value (Value(..), Object, Array, fromText, toText)

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
class FromJSON a where
  parseJSON :: Value -> Result a

-- | Decode a 'Value'. @FromJSON Value@ is the identity, so @fromJSON v :: Result Value@
-- round-trips the raw value — one entry point covers both raw and typed decoding.
fromJSON :: FromJSON a => Value -> Result a
fromJSON = parseJSON

-- | Project a 'Result' to 'Either', carrying the error as 'Text'.
resultToEither :: Result a -> Either Text a
resultToEither (Success a) = Right a
resultToEither (Error e)   = Left (T.pack e)

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
