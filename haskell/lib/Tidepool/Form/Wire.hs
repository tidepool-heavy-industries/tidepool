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
  , encodeShapeAnnotated
  ) where

import Prelude
import Data.Text (Text)
import qualified Data.Map.Strict as Map

import Tidepool.Aeson.Value (Value (..), scientific)
import Tidepool.Form.Shape
  ( FieldKey
  , FieldShape (..)
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
-- Doc-carrying shape ("Tidepool.Form"'s @formShapeWith@\/@askUserWith@)

-- | 'encodeShape', plus a ROOT title and per-field help merged in as an
-- extra @"doc"@ key: on the outermost @"product"@\/@"sum"@ object for the
-- title, and on a matching entry of a @"fields"@ array for that field's
-- help. Absent title\/docs leave the wire byte-identical to 'encodeShape' —
-- the merge only ever ADDS a key, never changes one 'encodeShape' already
-- writes.
--
-- Field docs apply at every product 'encodeShape' reaches from the ROOT
-- without crossing a nested field's own shape — the root product's fields,
-- or (for a root sum) each variant's product's fields — matching where a
-- @HasField name r t@ selector for @r@'s own field actually lives. A docs
-- key present for a name no product at that level has is simply unused.
encodeShapeAnnotated :: Maybe Text -> Map.Map FieldKey Text -> FormShape -> Value
encodeShapeAnnotated mTitle fieldDocs shape = case shape of
  ProductShape ty con fields ->
    withDoc $
      tagged "product" $
        Object
          ( Map.fromList
              [ ("type_key", String ty)
              , ("constructor", String con)
              , ("fields", Array (map annotateField fields))
              ]
          )
  SumShape ty variants ->
    withDoc $
      tagged "sum" $
        Object
          ( Map.fromList
              [ ("type_key", String ty)
              , ("variants", Array (map annotateVariant variants))
              ]
          )
  other -> encodeShape other
  where
    -- A root product/sum's OWN doc (the title) lands on the inner object
    -- `tagged` just wrapped — the same object `encodeShape` would have
    -- produced, with one key added.
    withDoc v = case mTitle of
      Nothing -> v
      Just t -> case v of
        Object outer -> Object (Map.map (addDoc t) outer)
        _ -> v
    addDoc t (Object inner) = Object (Map.insert "doc" (String t) inner)
    addDoc _ other = other

    -- A variant's payload is itself a product (or, for a nullary
    -- constructor, an empty one) — annotate its fields the same way, but
    -- never give the variant itself a "doc" (titles are root-only).
    annotateVariant (VariantShape con vshape) =
      Object (Map.fromList [("constructor", String con), ("shape", annotateNested vshape)])
    annotateNested (ProductShape ty con fields) =
      tagged "product" $
        Object
          ( Map.fromList
              [ ("type_key", String ty)
              , ("constructor", String con)
              , ("fields", Array (map annotateField fields))
              ]
          )
    annotateNested other = encodeShape other

    annotateField (FieldShape key fshape) = case Map.lookup key fieldDocs of
      Nothing -> Object (Map.fromList [("key", String key), ("shape", encodeShape fshape)])
      Just doc ->
        Object
          ( Map.fromList
              [ ("key", String key)
              , ("shape", encodeShape fshape)
              , ("doc", String doc)
              ]
          )
