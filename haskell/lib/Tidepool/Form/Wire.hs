{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The JSON transport for 'FormShape' — the Haskell half of a contract
-- whose Rust half is @tidepool-harness@'s @selfharness::operator@ module
-- documentation. That documentation is NORMATIVE: it states what
-- @#[derive(Serialize, Deserialize)]@ with @rename_all = \"snake_case\"@
-- actually produces for the shape enum, one worked example per shape, and
-- this module targets those examples verbatim
-- (@tidepool-runtime\/tests\/generic_form_wire.rs@ asserts against the
-- documented JSON itself, not against a paraphrase of it).
--
-- SHAPE ONLY: the submitted ANSWER is ordinary JSON decoded by the answer
-- type's own 'Tidepool.Aeson.FromJSON.FromJSON' instance — it has no
-- transport of its own and never passes through this module.
--
-- The shape encoding, in one table (@serde@'s externally-tagged
-- representation):
--
-- * a unit variant is a bare string of its snake_case name — @\"string\"@,
--   @\"unit\"@;
-- * a newtype variant is a one-key object — @{\"optional\": …}@;
-- * a struct variant is a one-key object wrapping an object of its fields —
--   @{\"product\": {\"type_key\": …, \"fields\": …}}@.
module Tidepool.Form.Wire
  ( encodeShape
  ) where

import Prelude
import Data.Text (Text)
import qualified Data.Map.Strict as Map

import Tidepool.Aeson.Value (Value (..), scientific)
import Tidepool.Form.Shape
  ( FieldShape (..)
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
