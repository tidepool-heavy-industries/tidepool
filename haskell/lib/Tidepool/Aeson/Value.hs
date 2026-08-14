{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE UndecidableInstances #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE ScopedTypeVariables #-}
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
  , GAllFieldsNamed
  , IsNullarySum
  , IsRecordCon
  , genericToJSON
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Tidepool.Aeson.Scientific
  ( Scientific, scientific, coefficient, base10Exponent
  , fromFloatDigits, toRealFloat, isFiniteDouble )
import Data.Kind (Type)
import Data.Proxy (Proxy(..))
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
-- The default method encodes generically via 'GHC.Generics':
-- @data Rec = Rec {..} deriving (Generic, ToJSON)@ builds a field-name-keyed
-- JSON object; @data Mode = Observing | Deciding | Acting deriving (Generic,
-- ToJSON)@ (a nullary sum — every constructor has no fields, i.e. an enum)
-- encodes each constructor as its bare name string. A sum with payload
-- constructors encodes as aeson's default @TaggedObject@ shape — a JSON
-- object with a @"tag"@ field naming the constructor and the constructor's
-- RECORD fields alongside it (a POSITIONAL payload constructor nests its
-- fields under @"contents"@ instead — bare for one field, an array for
-- several, aeson's default) — SYMMETRIC with what
-- 'Tidepool.Aeson.FromJSON.FromJSON'\'s default decodes.
class ToJSON a where
  toJSON :: a -> Value
  default toJSON :: (Generic a, GToJSON (Rep a)) => a -> Value
  toJSON = genericToJSON

-- | Encode a single-constructor record as a field-name-keyed JSON object, a
-- nullary-sum (enum) as its constructor-name string, or a payload sum as a
-- tagged object. This is the implementation behind the 'ToJSON' default
-- method.
genericToJSON :: (Generic a, GToJSON (Rep a)) => a -> Value
genericToJSON = gToJSON . from

-- | Encode a 'GHC.Generics' representation. @M1 D@/@M1 C@ are the outer layers;
-- the record fields underneath emit @[Pair]@ via 'GToRecord'.
class GToJSON f where
  gToJSON :: f a -> Value

-- | Emit the record fields of one constructor as JSON object pairs.
class GToRecord f where
  gToRecord :: f a -> [Pair]

instance GToJSON f => GToJSON (M1 D d f) where
  gToJSON (M1 x) = gToJSON x

instance GToRecord f => GToJSON (M1 C c f) where
  gToJSON (M1 x) = object (gToRecord x)

instance (GToRecord a, GToRecord b) => GToRecord (a :*: b) where
  gToRecord (a :*: b) = gToRecord a ++ gToRecord b

instance (Selector s, ToJSON c) => GToRecord (M1 S s (K1 R c)) where
  gToRecord m@(M1 (K1 c)) = [(T.pack (selName m), toJSON c)]

instance GToRecord U1 where
  gToRecord _ = []

-- Sum types: a NULLARY sum (every constructor has no fields — an enum)
-- encodes as its constructor-name string via 'GSumNullaryToJSON'; a sum with
-- payload constructors encodes as the tagged-object shape via
-- 'GToJSONTaggedSum'. 'IsNullarySum' decides which branch of 'GToJSONSum'
-- applies — mirroring the decode split in "Tidepool.Aeson.FromJSON".
instance GToJSONSum (IsNullarySum (a :+: b)) (a :+: b) => GToJSON (a :+: b) where
  gToJSON = gToJSONSum (Proxy :: Proxy (IsNullarySum (a :+: b)))

-- | Does every constructor reachable through this sum skeleton carry zero
-- fields (@M1 C c U1@)? Computed structurally over the '(:+:)' tree so it
-- works for any number of constructors, not just two.
type family IsNullarySum (f :: Type -> Type) :: Bool where
  IsNullarySum (a :+: b) = IsNullarySumAnd (IsNullarySum a) (IsNullarySum b)
  IsNullarySum (M1 C c U1) = 'True
  IsNullarySum (M1 C c f) = 'False

type family IsNullarySumAnd (a :: Bool) (b :: Bool) :: Bool where
  IsNullarySumAnd 'True 'True = 'True
  IsNullarySumAnd a b = 'False

-- | Does this constructor's payload use RECORD syntax (named selectors)?
-- Haskell guarantees all-or-nothing per constructor, so inspecting the first
-- field suffices. A nullary constructor counts as a (trivially empty) record
-- — its encoding is the tag-only object either way. Shared by the encode
-- ("Tidepool.Aeson.Value"), decode ("Tidepool.Aeson.FromJSON"), and schema
-- ("Tidepool.Aeson.Schema") sides so all three take the same branch.
type family IsRecordCon (f :: Type -> Type) :: Bool where
  IsRecordCon U1 = 'True
  IsRecordCon (a :*: b) = IsRecordCon a
  IsRecordCon (M1 S ('MetaSel ('Just n) su ss ds) f) = 'True
  IsRecordCon (M1 S ('MetaSel 'Nothing su ss ds) f) = 'False

-- | Dispatch on whether a sum is all-nullary: 'True' routes to the
-- constructor-name encoder, 'False' to the tagged-object encoder.
class GToJSONSum (allNullary :: Bool) f where
  gToJSONSum :: Proxy allNullary -> f a -> Value

instance GSumNullaryToJSON f => GToJSONSum 'True f where
  gToJSONSum _ = gSumNullaryToJSON

instance GToJSONTaggedSum f => GToJSONSum 'False f where
  gToJSONSum _ = gToJSONTaggedSum

-- | aeson's default @TaggedObject@ shape, encode side: one JSON object per
-- value carrying @"tag": <constructor name>@ plus the constructor's payload —
-- the exact shape "Tidepool.Aeson.FromJSON"\'s @GFromJSONTaggedSum@ decodes,
-- so a payload sum round-trips through the two defaults. A nullary
-- constructor in a mixed sum is an object carrying only @tag@. A RECORD
-- constructor's named fields sit alongside @tag@ (upstream's "records are
-- unpacked in the tagged object"); a POSITIONAL constructor's fields nest
-- under @"contents"@ — the single value bare for one field, an array for
-- several — matching upstream's non-record TaggedObject encoding.
-- 'IsRecordCon' picks the branch per constructor.
class GToJSONTaggedSum f where
  gToJSONTaggedSum :: f a -> Value

instance (GToJSONTaggedSum a, GToJSONTaggedSum b) => GToJSONTaggedSum (a :+: b) where
  gToJSONTaggedSum (L1 x) = gToJSONTaggedSum x
  gToJSONTaggedSum (R1 x) = gToJSONTaggedSum x

instance (Constructor c, GToJSONTaggedCon (IsRecordCon f) f) => GToJSONTaggedSum (M1 C c f) where
  gToJSONTaggedSum m@(M1 x) =
    object
      ((T.pack "tag", String (T.pack (conName m)))
         : gTaggedConPairs (Proxy :: Proxy (IsRecordCon f)) x)

-- | One constructor's payload as tagged-object pairs, dispatched on
-- 'IsRecordCon': record fields inline beside @tag@, positional fields under
-- one @"contents"@ key.
class GToJSONTaggedCon (isRecord :: Bool) f where
  gTaggedConPairs :: Proxy isRecord -> f a -> [Pair]

instance (GToRecord f, GAllFieldsNamed f) => GToJSONTaggedCon 'True f where
  gTaggedConPairs _ = gToRecord

instance GToPositional f => GToJSONTaggedCon 'False f where
  gTaggedConPairs _ x = [(T.pack "contents", wrap (gToPositional x))]
    where
      wrap [v] = v
      wrap vs = Array vs

-- | A positional payload's field values, in declaration order.
class GToPositional f where
  gToPositional :: f a -> [Value]

instance (GToPositional a, GToPositional b) => GToPositional (a :*: b) where
  gToPositional (a :*: b) = gToPositional a ++ gToPositional b

instance ToJSON c => GToPositional (M1 S s (K1 R c)) where
  gToPositional (M1 (K1 c)) = [toJSON c]

-- | For RECORD payload constructors only (the 'IsRecordCon' @'True@ branch):
-- proof that no field is named @tag@ (reserved for the discriminator). The
-- positional-field instance below is unreachable for a well-formed type —
-- Haskell constructors are all-record or all-positional, and positional
-- constructors take the @"contents"@ branch — and is kept as a defensive
-- backstop.
class GAllFieldsNamed (f :: Type -> Type)
instance GAllFieldsNamed U1
instance (GAllFieldsNamed a, GAllFieldsNamed b) => GAllFieldsNamed (a :*: b)
instance
  {-# OVERLAPPING #-}
  TypeError
    ( 'Text "generic JSON: a record field named `tag` cannot appear in a payload constructor of a sum; "
        ':<>: 'Text "`tag` is reserved for the constructor discriminator. Rename the field."
    ) =>
  GAllFieldsNamed (M1 S ('MetaSel ('Just "tag") su ss ds) (K1 R c))
instance {-# OVERLAPPABLE #-} GAllFieldsNamed (M1 S ('MetaSel ('Just name) su ss ds) (K1 R c))
instance
  TypeError ('Text "generic JSON: a payload constructor in a sum must use record syntax "
             ':<>: 'Text "(named fields) — positional fields have no JSON key.")
    => GAllFieldsNamed (M1 S ('MetaSel 'Nothing su ss ds) (K1 R c))

-- | Encode a nullary-constructors-only sum leaf/branch as its constructor
-- name. Only reachable once 'IsNullarySum' has established every constructor
-- in the sum is nullary.
class GSumNullaryToJSON f where
  gSumNullaryToJSON :: f a -> Value

instance (GSumNullaryToJSON a, GSumNullaryToJSON b) => GSumNullaryToJSON (a :+: b) where
  gSumNullaryToJSON (L1 x) = gSumNullaryToJSON x
  gSumNullaryToJSON (R1 x) = gSumNullaryToJSON x

instance Constructor c => GSumNullaryToJSON (M1 C c U1) where
  gSumNullaryToJSON m = String (T.pack (conName m))

instance ToJSON Value where
  toJSON = id

instance ToJSON Text where
  toJSON = String

instance ToJSON Int where
  toJSON n = Number (scientific (fromIntegral n) 0)

-- Non-finite Doubles have no numeric JSON representation; upstream aeson
-- encodes them as Null, so we match that instead of letting 'fromFloatDigits'
-- parse the letters of "NaN"/"Infinity" as decimal digits (see
-- 'Tidepool.Aeson.Scientific.isFiniteDouble').
instance ToJSON Double where
  toJSON d
    | isFiniteDouble d = Number (fromFloatDigits d)
    | otherwise         = Null

instance ToJSON Float where
  toJSON f
    | isFiniteDouble (realToFrac f) = Number (fromFloatDigits f)
    | otherwise                      = Null

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
