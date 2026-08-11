{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE MultiParamTypeClasses #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | A JSON Schema DESCRIPTION of what the vendored generic JSON defaults
-- produce.
--
-- This module derives a schema and nothing else — there is no codec here.
-- Values cross the boundary through 'Tidepool.Aeson.Value.ToJSON' and
-- 'Tidepool.Aeson.FromJSON.FromJSON'; this class only publishes the SHAPE
-- those two agree on, for the boundaries that need to hand a JSON Schema to
-- someone else (an agent tool's @input_schema@, a subagent's
-- @outputSchema@).
--
-- = Why it reads the same metadata
--
-- Every rule below is the schema-side statement of a rule the encoder and
-- decoder already implement, over the same 'GHC.Generics' representation and
-- the same selector\/constructor names:
--
-- * a single-constructor record → a JSON object keyed by VERBATIM selector
--   names (@GToRecord@\/@GFromRecord@ use @selName@ unchanged — there is no
--   name normalization anywhere on this path);
-- * an all-nullary sum (an enum) → @{"type": "string", "enum": [...]}@,
--   because those encode as a bare constructor-name string;
-- * a sum with any payload constructor → @oneOf@ of one tagged object per
--   constructor, each carrying @tag@ plus that constructor's record fields
--   — aeson's @TaggedObject@ shape, which is what the two defaults use;
-- * a @Maybe@ FIELD → its payload's schema, omitted from @required@,
--   because the decoder accepts an absent key AND an explicit @null@ there;
-- * a payload constructor with positional fields, or with a field literally
--   named @tag@, → a compile-time 'TypeError', via the SAME
--   'GAllFieldsNamed' witness "Tidepool.Aeson.Value" and
--   "Tidepool.Aeson.FromJSON" use. A type this class accepts is therefore a
--   type those two accept.
--
-- The consequence worth stating: a schema drift can only come from editing
-- this module against the other two, not from a second traversal that
-- silently disagrees about a name or an optionality.
--
-- = The two deliberate strictnesses
--
-- @additionalProperties: false@ is emitted for every object. The decoder
-- IGNORES unknown keys (a harmless producer quirk should not fail a decode),
-- but the schema is an instruction to the producer, and the producer should
-- be told to send exactly these keys.
--
-- The tag is spelled @{"type": "string", "enum": ["Completed"]}@, not
-- @{"const": "Completed"}@. Both are draft-07; the singleton @enum@ is the
-- form structured-output validators accept uniformly, while @const@ support
-- is uneven.
--
-- = Property ORDER
--
-- 'Tidepool.Aeson.Value.Object' is a @Data.Map.Strict@, so @properties@
-- renders with its keys SORTED. @required@ is a JSON array and DOES preserve
-- declaration order (@tag@ first for a sum). Worth knowing before pinning a
-- rendering.
module Tidepool.Aeson.Schema
  ( -- * The class
    JsonSchema (..)

    -- * Generic hierarchy
  , GJsonSchema (..)
  , GJsonSchemaSum (..)
  , GTaggedSchemas (..)
  , GNullaryNames (..)
  , GSchemaRecord (..)
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import Data.Kind (Type)
import Data.Proxy (Proxy (..))
import GHC.Generics
import Tidepool.Aeson.Value (Value (..), object, GAllFieldsNamed, IsNullarySum)

-- ---------------------------------------------------------------------------
-- The class
-- ---------------------------------------------------------------------------

-- | The JSON Schema describing @a@'s generic JSON encoding.
--
-- Derive it with @deriving (Generic, JsonSchema)@ (needs @DeriveAnyClass@ at
-- the use site) or an empty @instance JsonSchema T@. The 'Proxy' carries the
-- type only — no value is needed, or even constructible.
class JsonSchema a where
  jsonSchema :: Proxy a -> Value
  default jsonSchema :: (Generic a, GJsonSchema (Rep a)) => Proxy a -> Value
  jsonSchema _ = gJsonSchema (Proxy :: Proxy (Rep a))

-- ---------------------------------------------------------------------------
-- Leaves
-- ---------------------------------------------------------------------------

instance JsonSchema Text where
  jsonSchema _ = object [("type", String "string")]

instance JsonSchema Bool where
  jsonSchema _ = object [("type", String "boolean")]

instance JsonSchema Int where
  jsonSchema _ = object [("type", String "integer")]

instance JsonSchema Integer where
  jsonSchema _ = object [("type", String "integer")]

instance JsonSchema Word where
  jsonSchema _ = object [("type", String "integer")]

instance JsonSchema Double where
  jsonSchema _ = object [("type", String "number")]

instance JsonSchema Float where
  jsonSchema _ = object [("type", String "number")]

instance JsonSchema Char where
  jsonSchema _ = object [("type", String "string")]

-- | @()@ encodes as JSON @null@ (matching this package's @ToJSON ()@ and
-- @FromJSON ()@), so its schema is the null type.
instance JsonSchema () where
  jsonSchema _ = object [("type", String "null")]

-- | A raw 'Value' field accepts any JSON, and the empty schema is how draft-07
-- spells "anything".
instance JsonSchema Value where
  jsonSchema _ = object []

instance {-# OVERLAPPABLE #-} JsonSchema a => JsonSchema [a] where
  jsonSchema _ =
    object
      [ ("type", String "array")
      , ("items", jsonSchema (Proxy :: Proxy a))
      ]

-- | @[Char]@ encodes as a JSON string, not an array of one-character strings
-- — overlapping the list instance exactly as 'Tidepool.Aeson.Value.ToJSON'
-- does.
instance {-# OVERLAPPING #-} JsonSchema [Char] where
  jsonSchema _ = object [("type", String "string")]

-- | 'Maybe' OUTSIDE field position is just its payload: the encoder writes
-- @null@ for 'Nothing' and the decoder reads @null@ back. In FIELD position
-- the optionality is additionally expressed by absence from @required@ — see
-- 'GSchemaRecord'.
instance JsonSchema a => JsonSchema (Maybe a) where
  jsonSchema _ = jsonSchema (Proxy :: Proxy a)

-- ---------------------------------------------------------------------------
-- Object assembly
-- ---------------------------------------------------------------------------

-- | The object schema for ONE constructor. @Just tag@ adds the discriminator
-- (a leaf of a payload sum); 'Nothing' omits it (a single-constructor type,
-- which the encoder writes with no @tag@ either).
objectSchema :: Maybe Text -> [(Text, Value, Bool)] -> Value
objectSchema mtag fields =
  object
    [ ("type", String "object")
    , ("properties", Object (Map.fromList (tagProperty ++ [(n, s) | (n, s, _) <- fields])))
    , -- Only genuinely required fields: a 'Maybe' field is optional and must
      -- NOT appear here — a schema that lists an optional key in @required@
      -- forces the producer to invent a value for it.
      ("required", Array (map String (tagRequired ++ [n | (n, _, req) <- fields, req])))
    , ("additionalProperties", Bool False)
    ]
  where
    tagProperty = case mtag of
      Nothing -> []
      Just t -> [("tag", object [("type", String "string"), ("enum", Array [String t])])]
    tagRequired = case mtag of
      Nothing -> []
      Just _ -> ["tag"]

-- ---------------------------------------------------------------------------
-- Generic traversal
-- ---------------------------------------------------------------------------

-- | Describe a 'GHC.Generics' representation. Layer for layer the same walk
-- 'Tidepool.Aeson.Value.GToJSON' performs: transparent @M1 D@, object-from-
-- record @M1 C@, and an 'IsNullarySum'-directed split at @(:+:)@.
class GJsonSchema (f :: Type -> Type) where
  gJsonSchema :: Proxy f -> Value

instance GJsonSchema f => GJsonSchema (M1 D d f) where
  gJsonSchema _ = gJsonSchema (Proxy :: Proxy f)

instance GSchemaRecord f => GJsonSchema (M1 C c f) where
  gJsonSchema _ = objectSchema Nothing (gSchemaFields (Proxy :: Proxy f))

instance GJsonSchemaSum (IsNullarySum (a :+: b)) (a :+: b) => GJsonSchema (a :+: b) where
  gJsonSchema = gJsonSchemaSum (Proxy :: Proxy (IsNullarySum (a :+: b)))

-- | Dispatch on whether a sum is all-nullary — the same 'IsNullarySum' the
-- encoder and decoder branch on, so the schema names the shape they use.
class GJsonSchemaSum (allNullary :: Bool) f where
  gJsonSchemaSum :: Proxy allNullary -> Proxy f -> Value

instance GNullaryNames f => GJsonSchemaSum 'True f where
  gJsonSchemaSum _ p =
    object
      [ ("type", String "string")
      , ("enum", Array (map String (gNullaryNames p)))
      ]

instance GTaggedSchemas f => GJsonSchemaSum 'False f where
  gJsonSchemaSum _ p = object [("oneOf", Array (gTaggedSchemas p))]

-- | Constructor names of an all-nullary sum, in declaration order — the exact
-- strings the encoder emits and the decoder matches.
class GNullaryNames (f :: Type -> Type) where
  gNullaryNames :: Proxy f -> [Text]

instance (GNullaryNames a, GNullaryNames b) => GNullaryNames (a :+: b) where
  gNullaryNames _ = gNullaryNames (Proxy :: Proxy a) ++ gNullaryNames (Proxy :: Proxy b)

instance Constructor c => GNullaryNames (M1 C c U1) where
  gNullaryNames _ = [T.pack (conName (M1 Proxy :: M1 C c Proxy ()))]

-- | One tagged object per constructor of a payload sum, in declaration order.
class GTaggedSchemas (f :: Type -> Type) where
  gTaggedSchemas :: Proxy f -> [Value]

instance (GTaggedSchemas a, GTaggedSchemas b) => GTaggedSchemas (a :+: b) where
  gTaggedSchemas _ = gTaggedSchemas (Proxy :: Proxy a) ++ gTaggedSchemas (Proxy :: Proxy b)

-- | 'GAllFieldsNamed' is imported, not restated: a payload constructor with
-- positional fields, or with a field named @tag@, fails HERE with the same
-- 'GHC.TypeLits.TypeError' the encoder and decoder raise. That is what makes
-- "if it has a schema, it encodes and decodes" a type-level fact rather than
-- a convention.
instance (Constructor c, GSchemaRecord f, GAllFieldsNamed f) => GTaggedSchemas (M1 C c f) where
  gTaggedSchemas _ = [objectSchema (Just tag) (gSchemaFields (Proxy :: Proxy f))]
    where
      tag = T.pack (conName (M1 Proxy :: M1 C c Proxy ()))

-- | Per field: (name, schema, required?). The name is the VERBATIM selector
-- name, matching @GToRecord@\/@GFromRecord@. A 'Maybe' field reports
-- @required = False@; everything else 'True'.
class GSchemaRecord (f :: Type -> Type) where
  gSchemaFields :: Proxy f -> [(Text, Value, Bool)]

instance (GSchemaRecord a, GSchemaRecord b) => GSchemaRecord (a :*: b) where
  gSchemaFields _ = gSchemaFields (Proxy :: Proxy a) ++ gSchemaFields (Proxy :: Proxy b)

instance GSchemaRecord U1 where
  gSchemaFields _ = []

instance (Selector s, JsonSchema c) => GSchemaRecord (M1 S s (K1 R c)) where
  gSchemaFields _ = [(fieldName, jsonSchema (Proxy :: Proxy c), True)]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))

instance {-# OVERLAPPING #-} (Selector s, JsonSchema c) => GSchemaRecord (M1 S s (K1 R (Maybe c))) where
  gSchemaFields _ = [(fieldName, jsonSchema (Proxy :: Proxy c), False)]
    where
      fieldName = T.pack (selName (M1 Proxy :: M1 S s Proxy ()))
