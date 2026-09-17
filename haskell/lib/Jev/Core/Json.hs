{-# LANGUAGE LambdaCase #-}

-- | The JSON abstraction the core is polymorphic over. Boot packages only.
--
-- A transport supplies its own value type by implementing 'JsonValue'; the
-- package ships an instance for aeson in "Jev.Aeson". The DSL constructs only
-- the values it needs (null, text, numbers, small objects and arrays) and
-- inspects responses through 'jView'. Content an author supplies as
-- structured state, instructions, descriptions, or levels is their @v@ and
-- passes through untouched.
module Jev.Core.Json
  ( JsonValue (..)
  , View (..)
  , structuralEqual
  , lookupKey
  , viewObject
  , viewNumber
  , viewText
  ) where

import Data.List (sortOn)
import Data.Text (Text)

class JsonValue v where
  jNull :: v
  jBool :: Bool -> v
  jNumber :: Double -> v
  jString :: Text -> v
  jArray :: [v] -> v
  jObject :: [(Text, v)] -> v
  jView :: v -> View v
  -- | Exact structural equality, insensitive to object member order. The
  -- default compares through 'jView', which sees numbers as 'Double';
  -- an instance whose numbers are exact should override with its own
  -- equality so validation cannot be fooled by large integers.
  jEqual :: v -> v -> Bool
  jEqual = structuralEqual

-- | One layer of structure. Object members are in the value's own order.
data View v
  = VNull
  | VBool Bool
  | VNumber Double
  | VString Text
  | VArray [v]
  | VObject [(Text, v)]

structuralEqual :: JsonValue v => v -> v -> Bool
structuralEqual a b = case (jView a, jView b) of
  (VNull, VNull) -> True
  (VBool x, VBool y) -> x == y
  (VNumber x, VNumber y) -> x == y
  (VString x, VString y) -> x == y
  (VArray xs, VArray ys) -> length xs == length ys && and (zipWith jEqual xs ys)
  (VObject xs, VObject ys) ->
    let sx = sortOn fst xs
        sy = sortOn fst ys
    in map fst sx == map fst sy && and (zipWith (\(_, x) (_, y) -> jEqual x y) sx sy)
  _ -> False

lookupKey :: JsonValue v => Text -> v -> Maybe v
lookupKey k v = case jView v of
  VObject kv -> lookup k kv
  _ -> Nothing

viewObject :: JsonValue v => v -> Maybe [(Text, v)]
viewObject v = case jView v of
  VObject kv -> Just kv
  _ -> Nothing

viewNumber :: JsonValue v => v -> Maybe Double
viewNumber v = case jView v of
  VNumber n -> Just n
  _ -> Nothing

viewText :: JsonValue v => v -> Maybe Text
viewText v = case jView v of
  VString s -> Just s
  _ -> Nothing
