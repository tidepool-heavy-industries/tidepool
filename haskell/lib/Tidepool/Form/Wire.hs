{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The JSON transport for 'FormShape' and 'FormAnswer' — the Haskell half of
-- a contract whose Rust half is @tidepool-harness@'s
-- @selfharness::operator@ module documentation. That documentation is
-- NORMATIVE: it states what @#[derive(Serialize, Deserialize)]@ with
-- @rename_all = \"snake_case\"@ actually produces for those two enums, one
-- worked example per shape, and this module targets those examples verbatim
-- (@tidepool-runtime\/tests\/generic_form_wire.rs@ asserts against the
-- documented JSON itself, not against a paraphrase of it).
--
-- This is an INTERNAL transport, not an aeson contract. Nothing here is a
-- @ToJSON@\/@FromJSON@ instance and nothing routes through one: the agent
-- never authors, derives, or reads this encoding — it writes an ordinary ADT
-- and gets one back (see @plans\/self-iterating-harness\/14-generic-derived-askuser-prd.md@,
-- which rejects @FromJSON@\/@ToJSON@ by decision record).
--
-- The encoding, in one table (@serde@'s externally-tagged representation):
--
-- * a unit variant is a bare string of its snake_case name — @\"string\"@,
--   @\"unit\"@;
-- * a newtype variant is a one-key object — @{\"optional\": …}@;
-- * a struct variant is a one-key object wrapping an object of its fields —
--   @{\"sum\": {\"constructor\": …, \"payload\": …}}@;
-- * a product ANSWER's fields are an ARRAY of @[key, value]@ pairs, not an
--   object: a JSON object silently collapses a duplicate key on parse, and
--   'Tidepool.Form.Shape.DuplicateField' has to stay detectable.
module Tidepool.Form.Wire
  ( encodeShape
  , encodeAnswer
  , decodeAnswer
  ) where

import Prelude
import Data.Text (Text)
import qualified Data.Map.Strict as Map

import Tidepool.Aeson.Scientific (fromFloatDigits, toBoundedInteger, toRealFloat)
import Tidepool.Aeson.Value (Value (..), scientific)
import Tidepool.Form.Shape
  ( FieldShape (..)
  , FormAnswer (..)
  , FormShape (..)
  , VariantShape (..)
  )

-- | A one-key object — the externally-tagged carrier every non-unit variant
-- of both algebras uses.
tagged :: Text -> Value -> Value
tagged k v = Object (Map.singleton k v)

-- ---------------------------------------------------------------------------
-- Shape (Haskell -> operator)

-- | Put a derived form on the wire. The operator's renderer reads exactly
-- this.
encodeShape :: FormShape -> Value
encodeShape shape = case shape of
  StringShape -> String "string"
  IntShape -> String "int"
  NumberShape -> String "number"
  BoolShape -> String "bool"
  UnitShape -> String "unit"
  OptionalShape inner -> tagged "optional" (encodeShape inner)
  ProductShape ty con fields ->
    tagged "product" $
      Object
        ( Map.fromList
            [ ("type_key", String ty)
            , ("constructor", String con)
            , ("fields", Array (map encodeFieldShape fields))
            ]
        )
  SumShape ty variants ->
    tagged "sum" $
      Object
        ( Map.fromList
            [ ("type_key", String ty)
            , ("variants", Array (map encodeVariantShape variants))
            ]
        )

encodeFieldShape :: FieldShape -> Value
encodeFieldShape (FieldShape key shape) =
  Object (Map.fromList [("key", String key), ("shape", encodeShape shape)])

encodeVariantShape :: VariantShape -> Value
encodeVariantShape (VariantShape con shape) =
  Object (Map.fromList [("constructor", String con), ("shape", encodeShape shape)])

-- ---------------------------------------------------------------------------
-- Answer (operator -> Haskell)

-- | Put an answer on the wire. The decode direction is what @askUser@ runs;
-- this exists so both directions of the frozen encoding are stated once, in
-- one place, and can be tested against each other.
encodeAnswer :: FormAnswer -> Value
encodeAnswer answer = case answer of
  StringAnswer t -> tagged "string" (String t)
  IntAnswer i -> tagged "int" (Number (scientific (toInteger i) 0))
  NumberAnswer d -> tagged "number" (Number (fromFloatDigits d))
  BoolAnswer b -> tagged "bool" (Bool b)
  UnitAnswer -> String "unit"
  OptionalAnswer Nothing -> tagged "optional" Null
  OptionalAnswer (Just v) -> tagged "optional" (encodeAnswer v)
  ProductAnswer kvs -> tagged "product" (Array (map encodeAnswerPair kvs))
  SumAnswer con payload ->
    tagged "sum" $
      Object
        ( Map.fromList
            [ ("constructor", String con)
            , ("payload", encodeAnswer payload)
            ]
        )

encodeAnswerPair :: (Text, FormAnswer) -> Value
encodeAnswerPair (key, value) = Array [String key, encodeAnswer value]

-- | Read what the operator submitted. 'Nothing' means the submission was not
-- a well-formed answer AT ALL — malformed transport, as distinct from a
-- well-formed answer that does not fit the requested type (which
-- 'Tidepool.Form.GForm.decodeForm' rejects as a
-- 'Tidepool.Form.Shape.FormError'). Both re-present the same form; neither
-- reaches the caller.
decodeAnswer :: Value -> Maybe FormAnswer
decodeAnswer value = case value of
  String "unit" -> Just UnitAnswer
  Object o -> case Map.toList o of
    [("string", String t)] -> Just (StringAnswer t)
    [("int", Number n)] -> fmap IntAnswer (toBoundedInteger n)
    [("number", Number n)] -> Just (NumberAnswer (toRealFloat n))
    [("bool", Bool b)] -> Just (BoolAnswer b)
    [("optional", Null)] -> Just (OptionalAnswer Nothing)
    [("optional", inner)] -> fmap (OptionalAnswer . Just) (decodeAnswer inner)
    [("product", Array items)] -> fmap ProductAnswer (decodeAnswerPairs items)
    [("sum", Object fields)] -> decodeSum fields
    _ -> Nothing
  _ -> Nothing

decodeSum :: Map.Map Text Value -> Maybe FormAnswer
decodeSum fields = case (Map.lookup "constructor" fields, Map.lookup "payload" fields) of
  (Just (String con), Just payload) -> fmap (SumAnswer con) (decodeAnswer payload)
  _ -> Nothing

-- | A product answer's @[[key, value], …]@ array. Order is preserved (it is
-- the shape's declaration order) and duplicates survive as duplicates — the
-- decoder is what decides whether either is acceptable.
decodeAnswerPairs :: [Value] -> Maybe [(Text, FormAnswer)]
decodeAnswerPairs [] = Just []
decodeAnswerPairs (item : rest) = case item of
  Array [String key, value] -> case (decodeAnswer value, decodeAnswerPairs rest) of
    (Just v, Just vs) -> Just ((key, v) : vs)
    _ -> Nothing
  _ -> Nothing
