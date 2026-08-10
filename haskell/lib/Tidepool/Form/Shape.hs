{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The structural form algebra: what a derived form LOOKS like
-- ('FormShape'). PRESENTATION METADATA only — the submitted answer is
-- ordinary JSON, decoded by the answer type's own
-- 'Tidepool.Aeson.FromJSON.FromJSON' instance; there is no parallel answer
-- language.
--
-- This module is the frozen contract three consumers agree on: the generic
-- interpreter that produces shapes ("Tidepool.Form.GForm"), the @askUser@
-- surface that ships them across the @AskUser@ effect ("Tidepool.Form"), and
-- the Rust operator seam that renders them (@tidepool-harness@'s
-- @selfharness::operator@). Change the algebra and all three move together.
--
-- The algebra is deliberately small. Everything an agent can ask a human for
-- is a leaf, an optional, a keyed product, or a keyed sum — see
-- @plans\/self-iterating-harness\/14-generic-derived-askuser-prd.md@ for why
-- lists, recursion, and defaults are out of v1.
module Tidepool.Form.Shape
  ( -- * Keys
    TypeKey
  , ConstructorKey
  , FieldKey

    -- * Shapes
  , FormShape (..)
  , FieldShape (..)
  , VariantShape (..)
  ) where

import Prelude
import Data.Text (Text)

-- | A datatype's own name — the form's title, and the key a sum is
-- identified by. Taken verbatim from @M1 D@ metadata.
type TypeKey = Text

-- | A constructor's name — the stable key for one branch of a sum, and the
-- identity of a product node. Taken verbatim from @M1 C@ metadata.
type ConstructorKey = Text

-- | A field's key within one product node: the exact record selector name
-- from @M1 S@ metadata. (Positional fields have no key and are rejected at
-- compile time — the key doubles as the submitted JSON field name.)
type FieldKey = Text

-- | The structural description of a form, derived from a type's generic
-- representation without ever seeing a value of that type.
--
-- Recursive by construction: a product may contain sums, a sum branch may
-- carry a product, and either nests until it bottoms out in a leaf.
data FormShape
  = -- | A single-line string leaf. @Text@ only — @String@ is deliberately
    -- unsupported (Tidepool is Text-first, and @[Char]@ would collide with
    -- the future collection meaning of lists).
    StringShape
  | -- | A bounded integral leaf.
    IntShape
  | -- | A numeric leaf.
    NumberShape
  | -- | A boolean leaf. Rendered as a checkbox or toggle — NOT as a
    -- two-constructor enum, even though @Bool@ has a generic representation.
    -- The user-facing meaning outranks the implementation structure.
    BoolShape
  | -- | The JSON unit value @()@. Contributes no control and submits @null@.
    UnitShape
  | -- | An optional shape, from @Maybe a@. A blessed container, not an
    -- ordinary sum: the operator sees an optional control, never a
    -- @Nothing@\/@Just@ constructor picker. Nested @Maybe (Maybe a)@ is
    -- rejected at compile time — three states cannot be communicated
    -- cleanly by one optional control.
    OptionalShape FormShape
  | -- | A product: one constructor's fields, in declaration order.
    -- Presentation order IS declaration order.
    ProductShape TypeKey ConstructorKey [FieldShape]
  | -- | A sum: alternatives in constructor-declaration order. An all-nullary
    -- sum is an enum and may render as one compact choice control; a
    -- payload-bearing sum renders as a choice followed by the selected
    -- branch's nested form.
    --
    -- The generic representation's balanced @:+:@ tree is an implementation
    -- detail and MUST NOT leak into this list's order.
    SumShape TypeKey [VariantShape]
  deriving (Show, Eq)

-- | One named input within a 'ProductShape'.
data FieldShape = FieldShape FieldKey FormShape
  deriving (Show, Eq)

-- | One alternative within a 'SumShape'. A nullary constructor is an empty
-- 'ProductShape'; 'UnitShape' is reserved for an actual @()@ value.
data VariantShape = VariantShape ConstructorKey FormShape
  deriving (Show, Eq)
