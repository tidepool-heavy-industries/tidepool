{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | The MODEL-boundary structural codec: ONE 'GHC.Generics' traversal that
-- yields both a JSON Schema (for @outputSchema@) and a decoder for the same
-- NAMED-FIELD shape, so the two cannot drift.
--
-- = Why this exists — the wire caveat
--
-- PRD 18 gate 1(b)'s spike (the since-deleted @Tidepool.Agent.CodecSpike@) proved that lists and
-- recursion survive the real JIT. That result transfers unconditionally. Its
-- WIRE SHAPE does not: it encodes every constructor as
-- @{\"tag\": ..., \"fields\": [positional]}@, which is correct for
-- Tidepool↔Tidepool (encode and decode are the same walk, so field ORDER is a
-- safe pairing) and WRONG for Tidepool↔model. On the model boundary the far
-- side knows only a JSON Schema, and there is no JSON Schema that asks a model
-- to emit a positional array of heterogeneous fields — a model writing
-- @outputSchema@-constrained JSON produces named-field objects (observed live:
-- @{\"question\": \"...\"}@). So this module is the opposite polarity: the same
-- inverse-by-construction discipline, a named-field wire shape.
--
-- = What it is NOT built on
--
-- * NOT the vendored @Tidepool.Aeson.Value.ToJSON@ \/
--   @Tidepool.Aeson.FromJSON.FromJSON@ generic defaults. (Historical: at the
--   time those rejected payload sums on encode; they are symmetric now. This
--   module still differs by POLICY — snake_case field names, tagged objects
--   even for all-nullary sums, a schema, and JSONPath-carrying decode errors
--   — and folding it onto the shared implementation behind small options is
--   the structural-cleanup plan's step 3 remainder.)
-- * NOT the generic-surface @Symbol@-metadata substrate.
--
-- Base 'GHC.Generics' directly, over @Tidepool.Aeson.Value@.
--
-- = The encoding
--
-- __Single-constructor record__ — a plain object, keys are the snake_case
-- selector names ('Tidepool.Agent.Contract.toSnakeCase', imported, not
-- duplicated):
--
-- > {"additionalProperties": false,
-- >  "properties": {"note_file": {"type": "string"}, ...},
-- >  "required": ["note_file", ...],
-- >  "type": "object"}
--
-- No @tag@: a model asked for one fixed record should not have to emit a
-- redundant discriminator. (This is a DELIBERATE difference from CodecSpike,
-- which gives a single-constructor type the same tagged shape a sum leaf gets.
-- The cost is stated plainly: a type that later gains a second constructor
-- changes its wire shape. On this boundary that is the right trade — the shape
-- is what a model is asked to produce, and a schema that demands a constant
-- field is noise.)
--
-- __Multi-constructor sum__ — a tag-discriminated object per constructor,
-- @oneOf@ at the top:
--
-- > {"oneOf": [ {"additionalProperties": false,
-- >              "properties": {"tag": {"enum": ["Completed"], "type": "string"}, ...},
-- >              "required": ["tag", ...],
-- >              "type": "object"}, ... ]}
--
-- A NULLARY constructor is the zero-field case of that same shape — an object
-- carrying only @tag@, never a bare string. One shape per constructor form, by
-- construction (the mistake ledger's item 8: three shapes for three
-- constructor forms is how a codec grows a decoder that disagrees with its
-- schema).
--
-- __@const@ vs @enum@ for the tag.__ The tag is spelled
-- @{\"type\": \"string\", \"enum\": [\"Completed\"]}@, not
-- @{\"const\": \"Completed\"}@. Both are draft-07; the singleton @enum@ is the
-- form every structured-output validator in this family accepts, while @const@
-- support is uneven. Nothing downstream reads the tag schema except the model
-- host, so the conservative spelling costs nothing. Recorded here because the
-- lane spec asked which was chosen.
--
-- __Positional (non-record) constructors with fields are a compile-time
-- 'TypeError'__ — see 'GModelCon'. A model cannot be asked for positional
-- fields, so this is a scope edge, not an omission. @data Plan = Step Text |
-- Seq [Plan]@ (positional constructors) does NOT derive 'ModelCodec';
-- give the constructors record selectors if they must cross this boundary.
--
-- __Leaves__: 'Text' → @string@, 'Int' → @integer@, 'Bool' → @boolean@,
-- 'Double' → @number@, @[a]@ → @array@\/@items@, @Maybe a@ in FIELD position →
-- dropped from @required@ (a missing key, or an explicit @null@, decodes as
-- 'Nothing'). Nested 'ModelCodec' records compose.
--
-- Caveat for strict structured-output validators that require every property
-- to appear in @required@: an optional field is expressed here by ABSENCE from
-- @required@, per the lane spec. If such a validator ever rejects the schema,
-- the fix is schema-side (require the key, type it
-- @{\"oneOf\": [payload, {\"type\": \"null\"}]}@) — the decoder already accepts
-- an explicit @null@, so it needs no change.
--
-- = Property ORDER on the wire
--
-- @Tidepool.Aeson.Value.Object@ is a @Data.Map.Strict@, so @properties@ (and
-- every schema object) renders with its keys SORTED, not in declaration order.
-- @required@ is a JSON array and DOES preserve declaration order (@tag@ first
-- for a sum). Every example in this module is written in the order it actually
-- renders — worth knowing before pinning one.
--
-- = Decode is loud
--
-- Every mismatch — wrong tag, unknown tag, missing required field, wrong JSON
-- kind, non-object where an object is required — is a descriptive 'Left'
-- carrying a JSONPath-ish location (@$.caveats[2]@). Never a default, never a
-- sentinel. Unrecognized EXTRA keys are the one thing decode tolerates: the
-- schema already says @additionalProperties: false@, so a conforming model
-- sends none, and turning a harmless model quirk into a spawn failure buys
-- nothing.
--
-- = Worked example pairs (the acceptance-dev pins)
--
-- These are exact — @modelSchema (Proxy :: Proxy WorkerResult)@ renders to:
--
-- > {"oneOf":
-- >   [{"additionalProperties": false,
-- >     "properties": {"caveats": {"items": {"type": "string"}, "type": "array"},
-- >                    "summary": {"type": "string"},
-- >                    "tag": {"enum": ["Completed"], "type": "string"}},
-- >     "required": ["tag", "summary", "caveats"],
-- >     "type": "object"},
-- >    {"additionalProperties": false,
-- >     "properties": {"blocker": {"type": "string"},
-- >                    "evidence": {"items": {"type": "string"}, "type": "array"},
-- >                    "tag": {"enum": ["Blocked"], "type": "string"}},
-- >     "required": ["tag", "blocker", "evidence"],
-- >     "type": "object"}]}
--
-- and @decodeModel :: Value -> Either Text WorkerResult@ behaves so:
--
-- > {"tag":"Completed","summary":"ported the handler","caveats":[]}
-- >   => Right (Completed "ported the handler" [])
-- >
-- > {"tag":"Blocked","blocker":"no credentials","evidence":["auth.json absent"]}
-- >   => Right (Blocked "no credentials" ["auth.json absent"])
-- >
-- > {"tag":"Finished","summary":"x","caveats":[]}
-- >   => Left "$: unknown tag \"Finished\"; expected one of: Completed, Blocked"
-- >
-- > {"summary":"x","caveats":[]}
-- >   => Left "$: missing \"tag\" (expected one of: Completed, Blocked)"
-- >
-- > {"tag":"Completed","summary":"x"}
-- >   => Left "$.caveats: required field is missing"
-- >
-- > {"tag":"Completed","summary":42,"caveats":[]}
-- >   => Left "$.summary: expected string, got number"
-- >
-- > {"tag":"Completed","summary":"x","caveats":["a",7]}
-- >   => Left "$.caveats[1]: expected string, got number"
-- >
-- > {"tag":7,"summary":"x","caveats":[]}
-- >   => Left "$.tag: expected string, got number"
-- >
-- > "nope"   (a JSON string, not an object)
-- >   => Left "$: expected an object, got string"
--
-- 'ReviewNote' pins the single-constructor polarity — snake_case selectors,
-- 'Int', 'Bool', and an optional field. @modelSchema (Proxy :: Proxy ReviewNote)@:
--
-- > {"additionalProperties": false,
-- >  "properties": {"note_blocking": {"type": "boolean"},
-- >                 "note_file": {"type": "string"},
-- >                 "note_fix": {"type": "string"},
-- >                 "note_line": {"type": "integer"}},
-- >  "required": ["note_file", "note_line", "note_blocking"],
-- >  "type": "object"}
--
-- > {"note_file":"a.rs","note_line":12,"note_blocking":true,"note_fix":"drop it"}
-- >   => Right (ReviewNote "a.rs" 12 True (Just "drop it"))
-- >
-- > {"note_file":"a.rs","note_line":12,"note_blocking":false}
-- >   => Right (ReviewNote "a.rs" 12 False Nothing)         -- absent optional
-- >
-- > {"note_file":"a.rs","note_line":12,"note_blocking":false,"note_fix":null}
-- >   => Right (ReviewNote "a.rs" 12 False Nothing)         -- explicit null
-- >
-- > {"note_file":"a.rs","note_line":1.5,"note_blocking":false}
-- >   => Left "$.note_line: expected integer, got number 1.5"
-- >
-- > {"note_line":12,"note_blocking":false}
-- >   => Left "$.note_file: required field is missing"
--
-- 'roundTripsModel' is the pure Haskell acceptance shape: @decodeModel
-- (encodeModel x) == Right x@ for every constructor of both types.
module Tidepool.Agent.ModelCodec
  ( -- * The codec
    ModelCodec (..)
  , roundTripsModel

    -- * Generic hierarchy (ONE traversal — schema, encode, and decode)
  , GModelCodec (..)
  , GModelSum (..)
  , GModelCon (..)
  , GField (..)
  , IsMaybe
  , ModelField (..)

    -- * Proof types
  , WorkerResult (..)
  , ReviewNote (..)
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import Data.Kind (Type)
import Data.Proxy (Proxy (..))
import GHC.Generics
import GHC.TypeLits (KnownSymbol, symbolVal, TypeError, ErrorMessage (..))
import Tidepool.Aeson.Value (Value (..), Object, object, scientific, fromFloatDigits, toRealFloat)
import Tidepool.Aeson.Scientific (toBoundedInteger)
import Tidepool.Agent.Contract (toSnakeCase)

-- ---------------------------------------------------------------------------
-- The class
-- ---------------------------------------------------------------------------

-- | A type that can cross the model boundary: it publishes a JSON Schema a
-- model can be constrained by, and reads back the named-field JSON such a
-- model emits.
--
-- All three methods come from the SAME 'GModelCodec' traversal, so the schema
-- and the decoder cannot disagree about a field name, an optionality, or a
-- constructor tag. 'encodeModel' exists for exactly one reason — it makes
-- @decodeModel . encodeModel@ a pure Haskell round-trip test ('roundTripsModel')
-- that needs no model and no JIT. It is free: the traversal that computes the
-- schema already visits every leaf.
--
-- Derive it with an empty instance:
--
-- > data WorkerResult = Completed { summary :: Text, caveats :: [Text] }
-- >                   | Blocked   { blocker :: Text, evidence :: [Text] }
-- >   deriving (Show, Eq, Generic)
-- >
-- > instance ModelCodec WorkerResult
class ModelCodec a where
  -- | The JSON Schema a model must conform to when producing an @a@. The
  -- 'Proxy' carries the type only — no value is needed, or even constructible.
  modelSchema :: Proxy a -> Value
  default modelSchema :: (Generic a, GModelCodec (Rep a)) => Proxy a -> Value
  modelSchema _ = gSchema (Proxy :: Proxy (Rep a))

  -- | Read the named-field JSON a conforming model emits. Loud on every
  -- mismatch; the 'Text' is a human-readable message with a JSONPath-ish
  -- location.
  decodeModel :: Value -> Either Text a
  default decodeModel :: (Generic a, GModelCodec (Rep a)) => Value -> Either Text a
  decodeModel v = to <$> gDecode rootPath v

  -- | Produce the exact shape 'modelSchema' describes. Same traversal, so this
  -- is a genuine inverse of 'decodeModel', not a parallel hand-written encoder.
  encodeModel :: a -> Value
  default encodeModel :: (Generic a, GModelCodec (Rep a)) => a -> Value
  encodeModel = gEncode . from

-- | @decodeModel (encodeModel x) == Right x@ — the pure acceptance shape. A
-- model is not involved: this checks that the traversal's two directions agree
-- with each other, which is the invariant the whole module exists to hold.
roundTripsModel :: (ModelCodec a, Eq a) => a -> Bool
roundTripsModel x = decodeModel (encodeModel x) == Right x

-- ---------------------------------------------------------------------------
-- Paths and error rendering
-- ---------------------------------------------------------------------------

-- | The root of a decode path. Every 'decodeModel' starts here, INCLUDING a
-- nested one — 'reroot' splices the outer location in when composing.
rootPath :: Text
rootPath = "$"

-- | Name a 'Value''s JSON kind, for "expected X, got Y" messages.
kindOf :: Value -> Text
kindOf v = case v of
  Object _ -> "object"
  Array _ -> "array"
  String _ -> "string"
  Number _ -> "number"
  Bool _ -> "bool"
  Null -> "null"

-- | Attach an outer location to an error produced by a NESTED 'decodeModel'.
--
-- A nested decode starts its own path at 'rootPath', so its message reads
-- @\"$.line: ...\"@; splicing @path@ in place of that leading @$@ yields
-- @\"$.note.line: ...\"@ rather than the doubled @\"$.note: $.line: ...\"@ a
-- naive prefix would give. A leaf error (@\"expected string, got number\"@)
-- carries no path, so it is prefixed with a separator instead.
reroot :: Text -> Either Text a -> Either Text a
reroot _ (Right x) = Right x
reroot path (Left e) = Left (case T.unpack e of
  ('$' : rest) -> path <> T.pack rest
  _ -> path <> ": " <> e)

showT :: Int -> Text
showT = T.pack . show

-- ---------------------------------------------------------------------------
-- Leaves
-- ---------------------------------------------------------------------------

instance ModelCodec Text where
  modelSchema _ = object [("type", String "string")]
  encodeModel = String
  decodeModel (String s) = Right s
  decodeModel v = Left ("expected string, got " <> kindOf v)

instance ModelCodec Int where
  modelSchema _ = object [("type", String "integer")]
  encodeModel n = Number (scientific (fromIntegral n) 0)
  decodeModel (Number s) = case toBoundedInteger s of
    Just i -> Right i
    Nothing -> Left ("expected integer, got number " <> T.pack (show s))
  decodeModel v = Left ("expected integer, got " <> kindOf v)

instance ModelCodec Bool where
  modelSchema _ = object [("type", String "boolean")]
  encodeModel = Bool
  decodeModel (Bool b) = Right b
  decodeModel v = Left ("expected boolean, got " <> kindOf v)

instance ModelCodec Double where
  modelSchema _ = object [("type", String "number")]
  encodeModel = Number . fromFloatDigits
  decodeModel (Number s) = Right (toRealFloat s)
  decodeModel v = Left ("expected number, got " <> kindOf v)

-- | Recursion through a list is the shape gate 1(b) proved survives the JIT;
-- this instance's own @ModelCodec a@ context is that same self-referential
-- dictionary, in the named-field polarity. The element index is part of the
-- error path (@$[2]@), so a bad element in a long array names itself.
instance ModelCodec a => ModelCodec [a] where
  modelSchema _ = object
    [ ("type", String "array")
    , ("items", modelSchema (Proxy :: Proxy a))
    ]
  encodeModel = Array . map encodeModel
  decodeModel (Array xs) = go (0 :: Int) xs
    where
      go _ [] = Right []
      go i (v : vs) = case reroot (rootPath <> "[" <> showT i <> "]") (decodeModel v) of
        Left e -> Left e
        Right x -> (x :) <$> go (i + 1) vs
  decodeModel v = Left ("expected array, got " <> kindOf v)

-- | 'Maybe' OUTSIDE field position (inside a list, say) is a nullable value.
-- In FIELD position the optionality is expressed by absence from @required@
-- instead — see 'GField'.
instance ModelCodec a => ModelCodec (Maybe a) where
  modelSchema _ = object
    [ ("oneOf", Array [modelSchema (Proxy :: Proxy a), object [("type", String "null")]])
    ]
  encodeModel Nothing = Null
  encodeModel (Just x) = encodeModel x
  decodeModel Null = Right Nothing
  decodeModel v = Just <$> decodeModel v

-- ---------------------------------------------------------------------------
-- Schema assembly
-- ---------------------------------------------------------------------------

-- | One named field of one constructor, as the single traversal sees it:
-- everything the schema needs (name, payload schema, whether it is required)
-- in the same value the decoder keys off.
data ModelField = ModelField
  { mfName :: Text
  , mfSchema :: Value
  , mfRequired :: Bool
  }
  deriving (Eq, Show)

-- | The object schema for ONE constructor. @Just tag@ adds the discriminator
-- (sum leaf); 'Nothing' omits it (single-constructor type).
conObjectSchema :: Maybe Text -> [ModelField] -> Value
conObjectSchema mtag fields =
  object
    [ ("type", String "object")
    , ("properties", Object (Map.fromList (tagProperty ++ [(mfName f, mfSchema f) | f <- fields])))
    , ("required", Array (map String (tagRequired ++ [mfName f | f <- fields, mfRequired f])))
    , ("additionalProperties", Bool False)
    ]
  where
    tagProperty = case mtag of
      Nothing -> []
      Just t -> [("tag", object [("type", String "string"), ("enum", Array [String t])])]
    tagRequired = case mtag of
      Nothing -> []
      Just _ -> ["tag"]

-- A selector's source name is normalized before it reaches JSON. Two source
-- names can therefore collapse to one key, and a sum payload can collide
-- with the discriminator. `Map.fromList` would silently choose a winner;
-- reject the schema instead so encode, decode, and the advertised contract
-- remain one-to-one.
validateConFields :: Maybe Text -> [ModelField] -> Either Text ()
validateConFields mtag fields =
  case firstRepeated [] (map mfName fields) of
    Just name -> Left ("duplicate normalized model field name \"" <> name <> "\"")
    Nothing -> case mtag of
      Just _ | any ((== "tag") . mfName) fields ->
        Left "model sum payload field \"tag\" collides with the constructor discriminator"
      _ -> Right ()
  where
    firstRepeated _ [] = Nothing
    firstRepeated seen (name : rest)
      | name `elem` seen = Just name
      | otherwise = firstRepeated (name : seen) rest

-- Schema and encode call `error` where decode returns `Left`: a collision is
-- a property of the TYPE, and the schema is built at agent construction —
-- before any model call — so the partial branches fail fast on a type decode
-- could never have accepted. Decode stays total because it judges model
-- OUTPUT at runtime, where a `Left` must flow back as an ordinary result.
-- The collision rules themselves are pinned by the decode-path tests in
-- `tidepool-runtime/tests/agent_mode_encoding.rs`.
checkedConSchema :: Maybe Text -> [ModelField] -> Value
checkedConSchema mtag fields = case validateConFields mtag fields of
  Left problem -> error ("Tidepool.Agent.ModelCodec: " ++ T.unpack problem)
  Right () -> conObjectSchema mtag fields

checkedConEncode :: Maybe Text -> [ModelField] -> [(Text, Value)] -> Value
checkedConEncode mtag fields pairs = case validateConFields mtag fields of
  Left problem -> error ("Tidepool.Agent.ModelCodec: " ++ T.unpack problem)
  Right () -> Object (Map.fromList (tagPair ++ pairs))
  where
    tagPair = case mtag of
      Nothing -> []
      Just tag -> [("tag", String tag)]

-- ---------------------------------------------------------------------------
-- Generic traversal — top level
-- ---------------------------------------------------------------------------

-- | Dispatch over a 'Generic' 'Rep': strip the datatype-metadata layer, then
-- either the single-constructor case or a multi-constructor sum. All three
-- methods live on ONE class so that adding a case cannot add it to the schema
-- without adding it to the decoder.
class GModelCodec (f :: Type -> Type) where
  gSchema :: Proxy f -> Value
  gEncode :: f p -> Value
  -- | The 'Text' is the path of the value being decoded, for error messages.
  gDecode :: Text -> Value -> Either Text (f p)

instance GModelCodec f => GModelCodec (M1 D d f) where
  gSchema _ = gSchema (Proxy :: Proxy f)
  gEncode (M1 x) = gEncode x
  gDecode path v = M1 <$> gDecode path v

-- | Single constructor: a bare named-field object, no discriminator.
instance GModelCon f => GModelCodec (M1 C c f) where
  gSchema _ = checkedConSchema Nothing fields
    where fields = gConFields (Proxy :: Proxy f)
  gEncode (M1 x) = checkedConEncode Nothing fields (gConEncode x)
    where fields = gConFields (Proxy :: Proxy f)
  gDecode path (Object o) = do
    validateConFields Nothing (gConFields (Proxy :: Proxy f))
    M1 <$> gConDecode path o
  gDecode path v = Left (path <> ": expected an object, got " <> kindOf v)

-- | Multi-constructor sum: @oneOf@ of per-constructor tagged objects. The tag
-- is read FIRST and dispatched on, so a wrong tag reports itself by name
-- instead of degenerating into a pile of per-branch field errors.
instance (GModelSum a, GModelSum b) => GModelCodec (a :+: b) where
  gSchema _ = object [("oneOf", Array (gSumSchemas (Proxy :: Proxy a) ++ gSumSchemas (Proxy :: Proxy b)))]
  gEncode (L1 x) = gSumEncode x
  gEncode (R1 x) = gSumEncode x
  gDecode path (Object o) = case Map.lookup "tag" o of
    Just (String tag) -> case gSumDecode path tag o of
      Just r -> r
      Nothing -> Left (path <> ": unknown tag \"" <> tag <> "\"; expected one of: " <> tagList)
    Just other -> Left (path <> ".tag: expected string, got " <> kindOf other)
    Nothing -> Left (path <> ": missing \"tag\" (expected one of: " <> tagList <> ")")
    where
      tagList = T.intercalate ", " (gSumTags (Proxy :: Proxy a) ++ gSumTags (Proxy :: Proxy b))
  gDecode path v = Left (path <> ": expected an object, got " <> kindOf v)

-- | One branch of a '(:+:)' tree. 'gSumDecode' returns 'Nothing' for "that is
-- not my tag" and @Just (Left ...)@ for "my tag, but the body is wrong" — the
-- distinction is what lets the top-level sum decoder report an unknown tag
-- crisply while still surfacing a real field error verbatim.
class GModelSum (f :: Type -> Type) where
  gSumSchemas :: Proxy f -> [Value]
  gSumTags :: Proxy f -> [Text]
  gSumEncode :: f p -> Value
  gSumDecode :: Text -> Text -> Object -> Maybe (Either Text (f p))

instance (GModelSum a, GModelSum b) => GModelSum (a :+: b) where
  gSumSchemas _ = gSumSchemas (Proxy :: Proxy a) ++ gSumSchemas (Proxy :: Proxy b)
  gSumTags _ = gSumTags (Proxy :: Proxy a) ++ gSumTags (Proxy :: Proxy b)
  gSumEncode (L1 x) = gSumEncode x
  gSumEncode (R1 x) = gSumEncode x
  gSumDecode path tag o = case gSumDecode path tag o of
    Just r -> Just (fmap L1 r)
    Nothing -> fmap (fmap R1) (gSumDecode path tag o)

instance (Constructor c, GModelCon f) => GModelSum (M1 C c f) where
  gSumSchemas _ = [checkedConSchema (Just tag) fields]
    where
      tag = conNameOf (Proxy :: Proxy c)
      fields = gConFields (Proxy :: Proxy f)
  gSumTags _ = [conNameOf (Proxy :: Proxy c)]
  gSumEncode m@(M1 x) = checkedConEncode (Just (T.pack (conName m))) fields (gConEncode x)
    where fields = gConFields (Proxy :: Proxy f)
  gSumDecode path tag o
    | tag == conNameOf (Proxy :: Proxy c) = Just $ do
        validateConFields (Just tag) (gConFields (Proxy :: Proxy f))
        M1 <$> gConDecode path o
    | otherwise = Nothing

-- | The constructor's source name, as it appears in the @tag@. No
-- normalization: a constructor name is already the model-facing vocabulary,
-- and snake_casing it would make @Completed@ read as a field.
conNameOf :: forall (c :: Meta). Constructor c => Proxy c -> Text
conNameOf _ = T.pack (conName (M1 Proxy :: M1 C c Proxy ()))

-- ---------------------------------------------------------------------------
-- Generic traversal — one constructor's named fields
-- ---------------------------------------------------------------------------

-- | The fields of ONE constructor, BY NAME. Unlike
-- "Tidepool.Agent.CodecSpike"'s positional product walk, decode here looks
-- each field up in the object rather than consuming a positional array — which
-- is why a product needs no arity bookkeeping and no @splitAt@.
class GModelCon (f :: Type -> Type) where
  gConFields :: Proxy f -> [ModelField]
  gConEncode :: f p -> [(Text, Value)]
  gConDecode :: Text -> Object -> Either Text (f p)

instance GModelCon U1 where
  gConFields _ = []
  gConEncode U1 = []
  gConDecode _ _ = Right U1

instance (GModelCon a, GModelCon b) => GModelCon (a :*: b) where
  gConFields _ = gConFields (Proxy :: Proxy a) ++ gConFields (Proxy :: Proxy b)
  gConEncode (x :*: y) = gConEncode x ++ gConEncode y
  gConDecode path o = (:*:) <$> gConDecode path o <*> gConDecode path o

-- | A RECORD field: it has a selector name, so it has a wire name
-- ('toSnakeCase' of the selector) and can be looked up.
instance
  (KnownSymbol nm, GField (IsMaybe c) c) =>
  GModelCon (M1 S ('MetaSel ('Just nm) su ss ds) (K1 R c))
  where
  gConFields _ =
    [ ModelField
        { mfName = selectorWireName (Proxy :: Proxy nm)
        , mfSchema = gFieldSchema (Proxy :: Proxy (IsMaybe c)) (Proxy :: Proxy c)
        , mfRequired = gFieldRequired (Proxy :: Proxy (IsMaybe c)) (Proxy :: Proxy c)
        }
    ]
  gConEncode (M1 (K1 x)) = case gFieldEncode (Proxy :: Proxy (IsMaybe c)) x of
    Just v -> [(selectorWireName (Proxy :: Proxy nm), v)]
    Nothing -> []
  gConDecode path o =
    M1 . K1 <$> gFieldDecode (Proxy :: Proxy (IsMaybe c)) fieldPath (Map.lookup name o)
    where
      name = selectorWireName (Proxy :: Proxy nm)
      fieldPath = path <> "." <> name

-- | A POSITIONAL constructor field — rejected at compile time.
--
-- A JSON Schema can ask a model for a named object or an array of ONE element
-- type; it cannot ask for a heterogeneous positional tuple whose slots are
-- distinguished only by index. So @data Plan = Step Text | Seq [Plan]@ has no
-- model-boundary encoding, and saying so at the type level beats emitting a
-- schema no model can satisfy. This is the deliberate scope edge; the fix is
-- to give the constructor record selectors.
instance
  TypeError
    ( 'Text "Tidepool.Agent.ModelCodec: constructor field has no record selector."
        ':$$: 'Text "A model produces NAMED-FIELD JSON, so every field crossing this boundary needs a name;"
        ':$$: 'Text "a JSON Schema cannot ask for a positional constructor's fields."
        ':$$: 'Text "Give the constructor record selectors, e.g. `Step { stepAction :: Text }`."
    ) =>
  GModelCon (M1 S ('MetaSel 'Nothing su ss ds) (K1 R c))
  where
  gConFields _ = error "unreachable: positional constructor field is a compile-time TypeError"
  gConEncode _ = error "unreachable: positional constructor field is a compile-time TypeError"
  gConDecode _ _ = error "unreachable: positional constructor field is a compile-time TypeError"

-- | The wire name of a record selector: 'toSnakeCase', imported from
-- "Tidepool.Agent.Contract" (ONE camel-to-snake conversion in the codebase —
-- tool names and record fields must normalize identically or an authored name
-- means two different things depending on where it appears).
selectorWireName :: forall nm. KnownSymbol nm => Proxy nm -> Text
selectorWireName p = toSnakeCase (T.pack (symbolVal p))

-- ---------------------------------------------------------------------------
-- Field-position dispatch: optional vs required
-- ---------------------------------------------------------------------------

-- | Is this field type a 'Maybe'? Drives 'GField' — the same type-family
-- dispatch @Tidepool.Aeson.Value@'s @IsNullarySum@ uses, chosen over
-- overlapping instances for the same reason: it errors better and there is no
-- instance-resolution order to reason about.
type family IsMaybe (a :: Type) :: Bool where
  IsMaybe (Maybe a) = 'True
  IsMaybe a = 'False

-- | How a field of type @a@ behaves in RECORD position. The 'Bool' index is
-- @'IsMaybe' a@: an optional field leaves @required@, tolerates an absent key
-- AND an explicit @null@, and is omitted entirely when encoding 'Nothing'.
class GField (opt :: Bool) a where
  gFieldSchema :: Proxy opt -> Proxy a -> Value
  gFieldRequired :: Proxy opt -> Proxy a -> Bool
  gFieldEncode :: Proxy opt -> a -> Maybe Value
  -- | The 'Text' is this field's path; the @Maybe Value@ is the object lookup,
  -- so "the key was absent" is a case this decides rather than a caller.
  gFieldDecode :: Proxy opt -> Text -> Maybe Value -> Either Text a

instance ModelCodec a => GField 'False a where
  gFieldSchema _ _ = modelSchema (Proxy :: Proxy a)
  gFieldRequired _ _ = True
  gFieldEncode _ x = Just (encodeModel x)
  gFieldDecode _ path (Just v) = reroot path (decodeModel v)
  gFieldDecode _ path Nothing = Left (path <> ": required field is missing")

-- | The optional case. The field's schema is its PAYLOAD's schema — the
-- optionality lives in @required@, not in the type (see the module header for
-- the strict-validator caveat).
instance ModelCodec a => GField 'True (Maybe a) where
  gFieldSchema _ _ = modelSchema (Proxy :: Proxy a)
  gFieldRequired _ _ = False
  gFieldEncode _ Nothing = Nothing
  gFieldEncode _ (Just x) = Just (encodeModel x)
  gFieldDecode _ _ Nothing = Right Nothing
  gFieldDecode _ _ (Just Null) = Right Nothing
  gFieldDecode _ path (Just v) = reroot path (Just <$> decodeModel v)

-- ---------------------------------------------------------------------------
-- Proof types
-- ---------------------------------------------------------------------------

-- | PRD 18's worker result — the acceptance type for lane 1's one-cycle
-- spawn. A sum of two records: it exercises the tag discriminator, the
-- named-field record polarity, and a list leaf in one shape. See the module
-- header for its exact schema and decode pairs.
data WorkerResult
  = Completed {summary :: Text, caveats :: [Text]}
  | Blocked {blocker :: Text, evidence :: [Text]}
  deriving (Show, Eq, Generic)

instance ModelCodec WorkerResult

-- | The single-constructor polarity, with the leaves 'WorkerResult' does not
-- reach: a selector that actually changes under 'toSnakeCase'
-- (@noteFile@ → @note_file@), an 'Int', a 'Bool', and an OPTIONAL field.
data ReviewNote = ReviewNote
  { noteFile :: Text
  , noteLine :: Int
  , noteBlocking :: Bool
  , noteFix :: Maybe Text
  }
  deriving (Show, Eq, Generic)

instance ModelCodec ReviewNote
