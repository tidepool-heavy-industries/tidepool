{-# LANGUAGE DefaultSignatures #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeOperators #-}
{-# LANGUAGE UndecidableInstances #-}

-- | PRD 18 gate 1(b) spike: prove that a list-carrying type and a genuinely
-- RECURSIVE ADT round-trip through a structural codec on the real
-- extract\/JIT — the polarity the operator-form interpreter rejects and
-- this interpreter requires.
--
-- This is a STANDALONE 'GHC.Generics' traversal, deliberately NOT built on
-- 'Tidepool.Aeson.Value.ToJSON'\/'Tidepool.Aeson.FromJSON.FromJSON'\'s own
-- generic defaults: those reject every sum type with a non-nullary
-- constructor at compile time (\"deriving ... via GHC.Generics supports
-- single-constructor records only\"), so neither 'WorkerResult' (a sum of
-- two records) nor 'Plan' (a sum of positional constructors) below could
-- derive through them. Per
-- @plans\/post-restart\/agent-lanes\/dev-structural-codec.md@ this module
-- also must not touch the generic-surface lane's Symbol-metadata substrate
-- (still mid-fold, off-limits) — it uses base 'GHC.Generics' directly and
-- rolls its own occurs-free traversal.
--
-- Encoding, chosen against the mistake ledger at
-- @plans\/post-restart\/codex-review-2026-08-08.md@ item 8 (nullary
-- constructors becoming bare strings while records become @_con@ and
-- positional constructors a third shape — three shapes for three
-- constructor forms): every constructor, of ANY arity or shape (nullary,
-- positional, record), becomes ONE shape —
-- @{\"tag\": \<constructor name\>, \"fields\": [\<field values, in
-- declaration order\>]}@. A nullary constructor is simply the @fields: []@
-- case of the same traversal, never a special-cased sentinel string.
-- Record field names are not carried in the wire shape (that would need a
-- second, differently-shaped encoding for the positional\/nullary cases
-- this codec also has to support); field ORDER — read by the exact same
-- 'GHC.Generics' walk that wrote it — is what makes decode the inverse of
-- encode by construction, not a hand-maintained pairing.
--
-- Loud rejection over silent coercion: every shape mismatch (wrong tag,
-- wrong field count, wrong JSON kind) returns a descriptive 'Left', never a
-- sentinel value or a silently-defaulted field.
module Tidepool.Agent.CodecSpike
  ( Structural(..)
  , WorkerResult(..)
  , Plan(..)
  , roundTrips
  ) where

import Prelude
import Data.Text (Text)
import qualified Tidepool.Data.Text as T
import qualified Data.Map.Strict as Map
import Data.Proxy (Proxy(..))
import GHC.Generics
import Tidepool.Aeson.Value (Value(..), object)

-- | Structural values encode to\/decode from a plain aeson-style 'Value'
-- ('Tidepool.Aeson.Value' — the pragmatic choice: it is what backends
-- speak, and @tidepool_agent::seam::DynamicToolDeclaration@ already carries
-- @serde_json::Value@; a dedicated @StructuralValue@ would only duplicate
-- this six-constructor type for no expressive gain here).
--
-- 'encodeS' and 'decodeS' are INVERSE BY CONSTRUCTION: both default methods
-- are derived from the SAME 'GHC.Generics' traversal
-- ('GStructural'\/'GStructuralSum'\/'GStructuralProd' below), so a schema
-- and its decoder cannot silently drift the way a hand-written encoder\/
-- decoder pair could.
class Structural a where
  encodeS :: a -> Value
  default encodeS :: (Generic a, GStructural (Rep a)) => a -> Value
  encodeS = gEncode . from

  decodeS :: Value -> Either Text a
  default decodeS :: (Generic a, GStructural (Rep a)) => Value -> Either Text a
  decodeS v = to <$> gDecode v

-- Base leaves --------------------------------------------------------------

instance Structural Text where
  encodeS = String
  decodeS (String s) = Right s
  decodeS v = Left ("expected string, got " <> kindOf v)

-- | Recursion through a list is the shape under test: 'Plan'\'s @Seq [Plan]@
-- constructor bottoms out here, and this instance's own context
-- (@Structural a@) is exactly the self-referential dictionary the gate asks
-- whether the JIT can elaborate and run.
instance Structural a => Structural [a] where
  encodeS = Array . map encodeS
  decodeS (Array xs) = traverse decodeS xs
  decodeS v = Left ("expected array, got " <> kindOf v)

kindOf :: Value -> Text
kindOf v = case v of
  Object _ -> "object"
  Array _ -> "array"
  String _ -> "string"
  Number _ -> "number"
  Bool _ -> "bool"
  Null -> "null"

-- | One tagged shape for every constructor: @{"tag": name, "fields": [...]}@.
taggedValue :: Text -> [Value] -> Value
taggedValue tag fields = object [("tag", String tag), ("fields", Array fields)]

-- | Match a tagged 'Value' against ONE expected constructor name, then hand
-- its @"fields"@ array to the caller. Every failure path (wrong top-level
-- shape, missing key, wrong field-array kind, wrong tag) is a descriptive
-- 'Left' — never a silent default.
decodeTagged :: Text -> ([Value] -> Either Text a) -> Value -> Either Text a
decodeTagged expected k (Object o) =
  case (Map.lookup "tag" o, Map.lookup "fields" o) of
    (Just (String tag), Just (Array fields))
      | tag == expected -> k fields
      | otherwise -> Left ("expected tag " <> expected <> ", got " <> tag)
    (Just (String _), Just other) ->
      Left ("expected \"fields\" to be an array, got " <> kindOf other)
    (Nothing, _) -> Left "malformed tagged value: missing \"tag\""
    (_, Nothing) -> Left "malformed tagged value: missing \"fields\""
    _ -> Left "malformed tagged value"
decodeTagged _ _ v = Left ("expected a tagged object, got " <> kindOf v)

-- Generic traversal ---------------------------------------------------------

-- | Top-level dispatch over a 'Generic' 'Rep': strip the datatype-metadata
-- layer, then either the single-constructor case (no '(:+:)' at this level)
-- or a multi-constructor sum.
class GStructural f where
  gEncode :: f p -> Value
  gDecode :: Value -> Either Text (f p)

instance GStructural f => GStructural (M1 D d f) where
  gEncode (M1 x) = gEncode x
  gDecode v = M1 <$> gDecode v

-- Single-constructor type: same tag+fields shape a sum leaf would produce,
-- so a type gaining a second constructor later does not change its wire
-- shape.
instance (Constructor c, GStructuralProd f) => GStructural (M1 C c f) where
  gEncode m@(M1 x) = taggedValue (T.pack (conName m)) (gEncodeProd x)
  gDecode = decodeTagged expected (\fields -> M1 <$> gDecodeProd fields)
    where expected = T.pack (conName (M1 Proxy :: M1 C c Proxy ()))

instance (GStructuralSum a, GStructuralSum b) => GStructural (a :+: b) where
  gEncode = gEncodeSum
  gDecode = gDecodeSum

-- | A leaf inside a '(:+:)' tree: the same per-constructor tag+fields shape
-- as the single-constructor 'GStructural' instance, plus name-matching so
-- the sum decoder can pick the right branch.
class GStructuralSum f where
  gEncodeSum :: f p -> Value
  gDecodeSum :: Value -> Either Text (f p)

instance (GStructuralSum a, GStructuralSum b) => GStructuralSum (a :+: b) where
  gEncodeSum (L1 x) = gEncodeSum x
  gEncodeSum (R1 x) = gEncodeSum x
  gDecodeSum v = case gDecodeSum v of
    Right l -> Right (L1 l)
    Left eL -> case gDecodeSum v of
      Right r -> Right (R1 r)
      Left eR -> Left (eL <> "; " <> eR)

instance (Constructor c, GStructuralProd f) => GStructuralSum (M1 C c f) where
  gEncodeSum m@(M1 x) = taggedValue (T.pack (conName m)) (gEncodeProd x)
  gDecodeSum = decodeTagged expected (\fields -> M1 <$> gDecodeProd fields)
    where expected = T.pack (conName (M1 Proxy :: M1 C c Proxy ()))

-- | Fields of ONE constructor, in declaration order, regardless of whether
-- they are named (record) or positional — this traversal never inspects
-- 'Selector' metadata, so a record and a positional constructor of the same
-- arity produce the identical wire shape (one shape for every constructor
-- form, per the module haddock).
class GStructuralProd f where
  gArity :: Proxy f -> Int
  gEncodeProd :: f p -> [Value]
  gDecodeProd :: [Value] -> Either Text (f p)

instance GStructuralProd U1 where
  gArity _ = 0
  gEncodeProd U1 = []
  gDecodeProd [] = Right U1
  gDecodeProd vs = Left ("expected 0 fields, got " <> T.pack (show (length vs)))

instance (GStructuralProd a, GStructuralProd b) => GStructuralProd (a :*: b) where
  gArity _ = gArity (Proxy :: Proxy a) + gArity (Proxy :: Proxy b)
  gEncodeProd (a :*: b) = gEncodeProd a ++ gEncodeProd b
  gDecodeProd vs
    | length vs == total = (:*:) <$> gDecodeProd xs <*> gDecodeProd ys
    | otherwise =
        Left ("expected " <> T.pack (show total) <> " fields, got " <> T.pack (show (length vs)))
    where
      n = gArity (Proxy :: Proxy a)
      total = n + gArity (Proxy :: Proxy b)
      (xs, ys) = splitAt n vs

instance Structural c => GStructuralProd (M1 S s (K1 R c)) where
  gArity _ = 1
  gEncodeProd (M1 (K1 c)) = [encodeS c]
  gDecodeProd [v] = M1 . K1 <$> decodeS v
  gDecodeProd vs = Left ("expected 1 field, got " <> T.pack (show (length vs)))

-- Proof shapes ---------------------------------------------------------------

-- | Shape 1: a list-carrying record-and-sum, PRD 18's own worker result —
-- @Completed@\/@Blocked@ are both records, so this also proves the record
-- case (fields visited in declaration order, not by selector name).
data WorkerResult
  = Completed {summary :: Text, caveats :: [Text]}
  | Blocked {blocker :: Text, evidence :: [Text]}
  deriving (Show, Eq, Generic)

instance Structural WorkerResult

-- | Shape 2: a genuinely RECURSIVE ADT whose recursion goes THROUGH a list
-- — the shape a real @DevPlan@\/review tree has, stressing lists and
-- recursion at once. @Seq [Plan]@ is where 'Structural'\'s self-referential
-- dictionary (@Structural Plan@ needs @Structural [Plan]@ needs
-- @Structural Plan@) has to actually elaborate and run.
data Plan
  = Step Text
  | Seq [Plan]
  deriving (Show, Eq, Generic)

instance Structural Plan

-- | Encode, decode, and compare against the original — the acceptance shape
-- every proof case drives on the real JIT. Reconstruction happens inside
-- 'decodeS'; this only checks its result.
roundTrips :: (Structural a, Eq a) => a -> Bool
roundTrips x = case decodeS (encodeS x) of
  Right y -> y == x
  Left _ -> False
