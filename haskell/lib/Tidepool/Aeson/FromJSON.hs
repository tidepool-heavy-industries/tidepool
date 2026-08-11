{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE UndecidableInstances #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE KindSignatures #-}
-- | Structural @FromJSON@: decode an already-parsed 'Value' into a typed result.
--
-- This is PURE Haskell over the vendored Double-based 'Value' — it carries none
-- of upstream aeson's @unsafePerformIO@ / exception / text-parser machinery.
-- The text→'Value' step happens Rust-side (the 'eitherDecodeValue' primop,
-- serde_json); this module adds the @Value -> a@ half, which runs cleanly on
-- the JIT (typeclass-dictionary dispatch over constructor pattern-matches).
module Tidepool.Aeson.FromJSON
  ( FromJSON(..)
  , GFromJSON(..)
  , genericParseJSON
  , Result(..)
  , fromJSON
  , resultToEither
  , eitherDecode
  , decode
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
import qualified Data.Set as Set
import Tidepool.Aeson.Value
  ( Value(..), Object, Array, fromText, toText, eitherDecodeValue
  , GAllFieldsNamed, IsNullarySum
  )
import Tidepool.Aeson.Scientific (toRealFloat, toBoundedInteger, truncateScientific, floatingOrInteger)
import Data.Proxy (Proxy(..))
import GHC.Generics

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
-- The default method decodes generically via 'GHC.Generics':
-- @data Rec = Rec {..} deriving (Generic, FromJSON)@ decodes a
-- field-name-keyed JSON object into the record; @data Mode = Observing |
-- Deciding | Acting deriving (Generic, FromJSON)@ (a nullary sum — every
-- constructor has no fields, i.e. an enum) decodes from the constructor-name
-- string. A sum with payload constructors decodes from aeson's default
-- TaggedObject shape, symmetric with 'Tidepool.Aeson.Value.ToJSON'\'s
-- generic encode.
class FromJSON a where
  parseJSON :: Value -> Result a
  default parseJSON :: (Generic a, GFromJSON (Rep a)) => Value -> Result a
  parseJSON = genericParseJSON

-- | Decode a 'Value'. @FromJSON Value@ is the identity, so @fromJSON v :: Result Value@
-- round-trips the raw value — one entry point covers both raw and typed decoding.
fromJSON :: FromJSON a => Value -> Result a
fromJSON = parseJSON

-- | Decode a single-constructor record from a JSON object, keyed by exact
-- selector name, or a nullary-sum (enum) from its constructor-name string.
-- This is the implementation behind the 'FromJSON' default method:
-- @deriving (Generic, FromJSON)@ resolves @parseJSON@ to this.
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

-- A 'Maybe' field is OPTIONAL: a missing key (or an explicit @null@) decodes
-- as 'Nothing' rather than failing — aeson's `omitNothingFields`-compatible
-- read direction. Without this, a checkpoint written before a field was
-- added could never decode again (the strict '.:' failed on the absent key).
instance {-# OVERLAPPING #-} (Selector s, FromJSON c) => GFromRecord (M1 S s (K1 R (Maybe c))) where
  gParseRecord o = (M1 . K1) <$> (o .:? fieldName)
    where fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

-- Nullary constructor: an empty record decodes from any object.
instance GFromRecord U1 where
  gParseRecord _ = Success U1

-- Sum types: an ALL-nullary sum (every constructor has no fields — an enum)
-- decodes from its bare constructor-name string via 'GSumNullaryFromJSON',
-- matching aeson's default `allNullaryToStringTag = True`. A sum with any
-- non-nullary constructor falls to 'GFromJSONTaggedSum', aeson's default
-- `TaggedObject` shape. 'IsNullarySum' decides which branch of
-- 'GFromJSONSum' applies.
instance GFromJSONSum (IsNullarySum (a :+: b)) (a :+: b) => GFromJSON (a :+: b) where
  gParseJSON = gParseJSONSum (Proxy :: Proxy (IsNullarySum (a :+: b)))

-- 'IsNullarySum' (does every constructor reachable through this sum skeleton
-- carry zero fields?) is shared with "Tidepool.Aeson.Value" — a
-- direction-free closed type family, imported rather than duplicated, since
-- 'ToJSON' and 'FromJSON' must always agree on which branch a sum takes.

-- | Dispatch on whether a sum is all-nullary: 'True' routes to the
-- constructor-name decoder, 'False' to a compile-time rejection.
class GFromJSONSum (allNullary :: Bool) f where
  gParseJSONSum :: Proxy allNullary -> Value -> Result (f a)

instance GSumNullaryFromJSON f => GFromJSONSum 'True f where
  gParseJSONSum _ = gSumNullaryFromJSON

-- | aeson's default `TaggedObject` shape: a JSON object with a `"tag"` field
-- naming the constructor. A nullary constructor needs nothing else; a
-- constructor with fields must be a RECORD (named selectors), whose fields
-- are decoded from the SAME object alongside `"tag"` — matching upstream's
-- "records are unpacked in the tagged object" TaggedObject behavior (aeson
-- `parseNonAllNullarySum`/`FromTaggedObject'`'s `True` (record) instance —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
-- A non-record (positional-field) constructor is not supported: this
-- module's product decoder is selector-name-keyed throughout (see
-- 'GFromRecord'), so a positional field (whose GHC.Generics selector name is
-- empty) fails cleanly with a "key not present" 'Error' rather than upstream's
-- `"contents"`-nested form.
instance GFromJSONTaggedSum f => GFromJSONSum 'False f where
  gParseJSONSum _ = withObject "tagged sum" $ \o -> case Map.lookup tagKey o of
    Just (String tag) -> case gFromJSONTaggedSum tag o of
      Just r  -> r
      Nothing -> badTag tag
    Just v  -> Error ("tag field: expected string, got " ++ kindOf v)
    Nothing -> Error ("key " ++ show (T.unpack tagKey) ++ " not present")
    where
      tagKey = T.pack "tag"
      badTag tag = Error ("expected tag field to be one of " ++ show (gTaggedSumConNames (Proxy :: Proxy f)) ++
                           ", but found tag " ++ show (T.unpack tag))

-- | Match a `"tag"` value against each constructor in a sum, decoding the
-- matched one's fields (if any) from the shared 'Object'. 'Nothing' means
-- this branch's constructor name(s) didn't match — try the sibling branch.
class GFromJSONTaggedSum f where
  gFromJSONTaggedSum :: Text -> Object -> Maybe (Result (f a))
  gTaggedSumConNames :: Proxy f -> [String]

instance (GFromJSONTaggedSum a, GFromJSONTaggedSum b) => GFromJSONTaggedSum (a :+: b) where
  gFromJSONTaggedSum tag o = case gFromJSONTaggedSum tag o of
    Just r  -> Just (L1 <$> r)
    Nothing -> case gFromJSONTaggedSum tag o of
      Just r  -> Just (R1 <$> r)
      Nothing -> Nothing
  gTaggedSumConNames _ = gTaggedSumConNames (Proxy :: Proxy a) ++ gTaggedSumConNames (Proxy :: Proxy b)

-- | A single constructor leaf: decode its fields (via 'GFromRecord', so a
-- nullary constructor succeeds trivially and a record's named fields are
-- read out of the same tagged object) once its name matches `tag`.
instance (Constructor c, GFromRecord f, GAllFieldsNamed f) => GFromJSONTaggedSum (M1 C c f) where
  gFromJSONTaggedSum tag o
    | tag == T.pack name = Just (M1 <$> gParseRecord o)
    | otherwise           = Nothing
    where name = conName (M1 Proxy :: M1 C c Proxy ())
  gTaggedSumConNames _ = [conName (M1 Proxy :: M1 C c Proxy ())]

-- | Decode a nullary-constructors-only sum leaf/branch by matching a JSON
-- string against each constructor's name. Only reachable once
-- 'IsNullarySum' has established every constructor in the sum is nullary.
class GSumNullaryFromJSON f where
  gSumNullaryFromJSON :: Value -> Result (f a)
  gSumNullaryConNames :: Proxy f -> [String]

instance (GSumNullaryFromJSON a, GSumNullaryFromJSON b) => GSumNullaryFromJSON (a :+: b) where
  gSumNullaryFromJSON v = case gSumNullaryFromJSON v of
    Success l -> Success (L1 l)
    Error _   -> case gSumNullaryFromJSON v of
      Success r -> Success (R1 r)
      Error _   -> Error ("expected one of " ++ show allNames)
    where allNames = gSumNullaryConNames (Proxy :: Proxy a) ++ gSumNullaryConNames (Proxy :: Proxy b)
  gSumNullaryConNames _ =
    gSumNullaryConNames (Proxy :: Proxy a) ++ gSumNullaryConNames (Proxy :: Proxy b)

instance Constructor c => GSumNullaryFromJSON (M1 C c U1) where
  gSumNullaryFromJSON v = case v of
    String t | t == T.pack name -> Success (M1 U1)
    _ -> Error ("expected constructor name " ++ show name)
    where name = conName (M1 U1 :: M1 C c U1 ())
  gSumNullaryConNames _ = [conName (M1 U1 :: M1 C c U1 ())]

-- | Project a 'Result' to 'Either', carrying the error as 'Text'.
resultToEither :: Result a -> Either Text a
resultToEither (Success a) = Right a
resultToEither (Error e)   = Left (T.pack e)

-- | Decode a JSON document straight into a typed value, aeson-style: @Right a@
-- on success, @Left msg@ on a parse error (from serde_json) or a shape mismatch
-- (from 'fromJSON'). PURE — no effect and no abort. Because 'Value' has an
-- identity 'FromJSON' instance, @eitherDecode \@Value@ is the raw parse.
eitherDecode :: FromJSON a => Text -> Either Text a
eitherDecode t = eitherDecodeValue t >>= resultToEither . fromJSON

-- | 'eitherDecode' with the error dropped, aeson-style.
decode :: FromJSON a => Text -> Maybe a
decode = either (const Nothing) Just . eitherDecode

mismatch :: String -> Value -> Result a
mismatch want v = Error ("expected " ++ want ++ ", got " ++ kindOf v)

kindOf :: Value -> String
kindOf v = case v of
  Object _ -> "object"
  Array _  -> "array"
  String _ -> "string"
  Number _ -> "number"
  Bool _   -> "bool"
  Null     -> "null"

-- Raw passthrough: lets `eitherDecode t :: Either Text Value` be the raw parse.
instance FromJSON Value where
  parseJSON = Success

instance FromJSON Bool where
  parseJSON (Bool b) = Success b
  parseJSON v        = mismatch "bool" v

instance FromJSON Text where
  parseJSON (String s) = Success s
  parseJSON v          = mismatch "string" v

instance FromJSON Double where
  parseJSON (Number s) = Success (toRealFloat s)
  parseJSON v          = mismatch "number" v

-- Every 'Number' goes through 'toBoundedInteger': an exact integer within
-- 'Int' range succeeds, and everything else — a fractional 'Scientific' or an
-- exact integer outside 'Int' range — is an 'Error', mirroring aeson's
-- bounded-integral parse, which fails a value that is "either floating or
-- will cause over or underflow" (aeson `FromJSON` source,
-- `parseBoundedIntegralFromScientific` —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
instance FromJSON Int where
  parseJSON (Number s) = case toBoundedInteger s of
    Just i  -> Success i
    Nothing
      | s == fromInteger (truncateScientific s) ->
          Error ("Int out of range: " ++ show s)
      | otherwise -> Error ("Int: not an integral value: " ++ show s)
  parseJSON v = mismatch "number" v

-- | A JSON string of EXACTLY one character; a longer or empty string is an
-- 'Error' (aeson `FromJSON Char`'s @parseChar@ —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
instance FromJSON Char where
  parseJSON (String s)
    | T.length s == 1 = Success (T.head s)
    | otherwise        = Error "expected a string of length 1"
  parseJSON v = mismatch "string" v

-- | The bare constructor-name string, matching the 'Tidepool.Aeson.Value.ToJSON'
-- instance's @"LT"@\/@"EQ"@\/@"GT"@ output exactly.
instance FromJSON Ordering where
  parseJSON (String s)
    | s == T.pack "LT" = Success LT
    | s == T.pack "EQ" = Success EQ
    | s == T.pack "GT" = Success GT
    | otherwise = Error ("expected one of \"LT\", \"EQ\", \"GT\", got " ++ show (T.unpack s))
  parseJSON v = mismatch "string" v

-- | Unbounded: an exact integral 'Scientific' of any magnitude succeeds; a
-- fractional value is an 'Error', via the same 'floatingOrInteger' split
-- aeson's @parseIntegralFromScientific@ uses (aeson `FromJSON Integer`
-- routes through `parseIntegral` —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
instance FromJSON Integer where
  parseJSON (Number s) = case floatingOrInteger s :: Either Double Integer of
    Right i -> Success i
    Left _  -> Error ("Integer: not an integral value: " ++ show s)
  parseJSON v = mismatch "number" v

-- | Bounded non-negative integral, same 'toBoundedInteger' shape as 'Int'
-- above: negative, fractional, or out-of-range is an 'Error' (aeson
-- `FromJSON Word` routes through `parseBoundedIntegral` —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
instance FromJSON Word where
  parseJSON (Number s) = case toBoundedInteger s of
    Just w  -> Success w
    Nothing
      | s == fromInteger (truncateScientific s) ->
          Error ("Word out of range: " ++ show s)
      | otherwise -> Error ("Word: not an integral value: " ++ show s)
  parseJSON v = mismatch "number" v

-- | Same shape as the 'Double' instance above, at 'Float' (aeson
-- `FromJSON Float` routes through `parseRealFloat` —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
instance FromJSON Float where
  parseJSON (Number s) = Success (toRealFloat s)
  parseJSON v          = mismatch "number" v

instance {-# OVERLAPPABLE #-} FromJSON a => FromJSON [a] where
  parseJSON (Array xs) = traverse parseJSON xs
  parseJSON v          = mismatch "array" v

-- | Upstream aeson's @String@ instance decodes a JSON string directly, not an
-- array of one-character strings — overlapping the general list instance
-- above the same way 'Tidepool.Aeson.Value.ToJSON' @[Char]@ overlaps
-- 'Tidepool.Aeson.Value.ToJSON' @[a]@
-- (https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
instance {-# OVERLAPPING #-} FromJSON [Char] where
  parseJSON (String s) = Success (T.unpack s)
  parseJSON v          = mismatch "string" v

instance FromJSON a => FromJSON (Maybe a) where
  parseJSON Null = Success Nothing
  parseJSON v    = Just <$> parseJSON v

instance FromJSON a => FromJSON (Map.Map Text a) where
  parseJSON (Object o) = Map.foldrWithKey step (Success Map.empty) o
    where step k v acc = Map.insert (toText k) <$> parseJSON v <*> acc
  parseJSON v          = mismatch "object" v

-- | A JSON array of elements, matching the 'Tidepool.Aeson.Value.ToJSON'
-- instance's @Array . map toJSON . Set.toList@ output.
instance (Ord a, FromJSON a) => FromJSON (Set.Set a) where
  parseJSON (Array xs) = Set.fromList <$> traverse parseJSON xs
  parseJSON v          = mismatch "array" v

-- | JSON @null@, matching this package's 'ToJSON ()' instance and the unit
-- schema exposed to agents. Keeping one spelling matters here: @askUser @()@
-- feeds operator JSON straight back through this decoder.
instance FromJSON () where
  parseJSON Null = Success ()
  parseJSON v    = mismatch "null" v

-- | A JSON ARRAY with an EXACT arity check — a 2-tuple rejects any array
-- whose length isn't 2 (aeson `FromJSON2 (,)` —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html,
-- \"cannot unpack array of length N into a tuple of length 2\"). Tuple
-- coverage stops at 5, matching the 'Tidepool.Aeson.Value.ToJSON' tuple
-- instances already vendored in this module's sibling (upstream aeson goes
-- to 15-tuples; not replicated here).
instance (FromJSON a, FromJSON b) => FromJSON (a, b) where
  parseJSON (Array [x1, x2]) = (,) <$> parseJSON x1 <*> parseJSON x2
  parseJSON (Array xs) = Error ("cannot unpack array of length " ++ show (length xs) ++ " into a tuple of length 2")
  parseJSON v = mismatch "array" v

instance (FromJSON a, FromJSON b, FromJSON c) => FromJSON (a, b, c) where
  parseJSON (Array [x1, x2, x3]) = (,,) <$> parseJSON x1 <*> parseJSON x2 <*> parseJSON x3
  parseJSON (Array xs) = Error ("cannot unpack array of length " ++ show (length xs) ++ " into a tuple of length 3")
  parseJSON v = mismatch "array" v

instance (FromJSON a, FromJSON b, FromJSON c, FromJSON d) => FromJSON (a, b, c, d) where
  parseJSON (Array [x1, x2, x3, x4]) = (,,,) <$> parseJSON x1 <*> parseJSON x2 <*> parseJSON x3 <*> parseJSON x4
  parseJSON (Array xs) = Error ("cannot unpack array of length " ++ show (length xs) ++ " into a tuple of length 4")
  parseJSON v = mismatch "array" v

instance (FromJSON a, FromJSON b, FromJSON c, FromJSON d, FromJSON e) => FromJSON (a, b, c, d, e) where
  parseJSON (Array [x1, x2, x3, x4, x5]) = (,,,,) <$> parseJSON x1 <*> parseJSON x2 <*> parseJSON x3 <*> parseJSON x4 <*> parseJSON x5
  parseJSON (Array xs) = Error ("cannot unpack array of length " ++ show (length xs) ++ " into a tuple of length 5")
  parseJSON v = mismatch "array" v

-- | Upstream's object form: @{\"Left\": x}@ decodes to @Left x@,
-- @{\"Right\": y}@ to @Right y@; anything else is an 'Error' (aeson
-- `FromJSON2 Either` —
-- https://hackage.haskell.org/package/aeson/docs/src/Data.Aeson.Types.FromJSON.html).
instance (FromJSON a, FromJSON b) => FromJSON (Either a b) where
  parseJSON (Object o) = case Map.toList o of
    [(k, v)] | k == T.pack "Left"  -> Left <$> parseJSON v
             | k == T.pack "Right" -> Right <$> parseJSON v
    _ -> eitherShapeError
  parseJSON _ = eitherShapeError

eitherShapeError :: Result a
eitherShapeError = Error "expected an object with a single property where the property key should be either \"Left\" or \"Right\""

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

-- | Run a parser against a number, projecting the 'Scientific' to 'Double'.
withDouble :: String -> (Double -> Result a) -> Value -> Result a
withDouble _    f (Number s) = f (toRealFloat s)
withDouble name _ v          = typeMismatch name "number" v

typeMismatch :: String -> String -> Value -> Result a
typeMismatch name want v =
  Error ("parsing " ++ name ++ " failed: expected " ++ want ++ ", got " ++ kindOf v)
