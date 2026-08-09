{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The structural form algebra: what a derived form LOOKS like
-- ('FormShape') and what an operator SUBMITTED ('FormAnswer'). Both are
-- recursive, and both are keyed by the same datatype/constructor/selector
-- metadata, so a shape and an answer for the same type cannot disagree about
-- field order, constructor tags, or nesting.
--
-- This module is the frozen contract three consumers agree on: the generic
-- interpreter that produces shapes and decodes answers
-- ("Tidepool.Form.GForm"), the @askUser@ surface that ships them across the
-- @AskUser@ effect ("Tidepool.Form"), and the Rust operator seam that renders
-- them (@tidepool-harness@'s @selfharness::operator@). Change the algebra and
-- all three move together.
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

    -- * Answers
  , FormAnswer (..)

    -- * Errors
  , FormError (..)
  , renderFormError
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
-- from @M1 S@ metadata, or — for a positional constructor or tuple — a
-- one-based index rendered as text (@\"1\"@, @\"2\"@, …).
--
-- Positional keys are scoped to THEIR OWN product node. There is no
-- form-wide field counter; two different products both start at @\"1\"@.
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
  | -- | No payload: a nullary constructor's branch. Contributes no control.
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

-- | One alternative within a 'SumShape'. A nullary constructor's shape is
-- 'UnitShape'.
data VariantShape = VariantShape ConstructorKey FormShape
  deriving (Show, Eq)

-- | What the operator submitted, structurally. Mirrors 'FormShape'
-- constructor for constructor.
--
-- The encoding is FROZEN (see
-- @plans\/self-iterating-harness\/16-generic-spike-receipts.md@). Four rules
-- decide every case:
--
-- 1. A single-constructor datatype answers with a bare 'ProductAnswer'.
--    There is no redundant sum wrapper around a record.
-- 2. A multi-constructor datatype ALWAYS answers @'SumAnswer' conKey payload@,
--    including when the chosen branch is nullary.
-- 3. A nullary constructor's payload is 'UnitAnswer', never an empty
--    'ProductAnswer' — so a nullary branch and a zero-field record stay
--    distinguishable.
-- 4. @Maybe@ answers with 'OptionalAnswer', never a constructor pick.
data FormAnswer
  = StringAnswer Text
  | IntAnswer Int
  | NumberAnswer Double
  | BoolAnswer Bool
  | UnitAnswer
  | OptionalAnswer (Maybe FormAnswer)
  | ProductAnswer [(FieldKey, FormAnswer)]
  | SumAnswer ConstructorKey FormAnswer
  deriving (Show, Eq)

-- | Why an answer failed to rebuild a typed value.
--
-- Every constructor carries enough context to name the offending place in
-- the operator's submission. A 'FormError' NEVER reaches the agent: @askUser@
-- re-presents the same form through the existing bounded retry path, so a
-- malformed submission does not consume the continuation.
data FormError
  = -- | The answer's shape did not match the form's: what was expected, and
    -- what arrived.
    ShapeMismatch Text Text
  | -- | A product node was missing a field the shape requires.
    MissingField FieldKey
  | -- | A product node carried a key the shape does not define.
    UnexpectedField FieldKey
  | -- | A product node carried the same key twice.
    DuplicateField FieldKey
  | -- | A sum answer named a constructor this type does not have. Carries
    -- the type's own key and the constructors it DOES have, so the message
    -- can be corrective rather than merely negative.
    UnknownConstructor TypeKey ConstructorKey [ConstructorKey]
  | -- | An error from inside a named field, with the path preserved so a
    -- nested failure reports where it happened rather than at the root.
    InField FieldKey FormError
  | -- | An error from inside a named sum branch.
    InVariant ConstructorKey FormError
  deriving (Show, Eq)

-- | Render a 'FormError' as one operator-facing line, with the nesting path
-- flattened into a dotted prefix.
renderFormError :: FormError -> Text
renderFormError = go ""
  where
    go path err = case err of
      InField k e -> go (path <> k <> ".") e
      InVariant c e -> go (path <> c <> ".") e
      ShapeMismatch expected actual ->
        at path <> "expected " <> expected <> ", got " <> actual
      MissingField k -> at path <> "missing field " <> k
      UnexpectedField k -> at path <> "unexpected field " <> k
      DuplicateField k -> at path <> "duplicate field " <> k
      UnknownConstructor ty c valid ->
        at path <> "unknown constructor " <> c <> " for " <> ty
          <> "; expected one of " <> commas valid
    at "" = ""
    at path = "at " <> path <> ": "
    commas [] = "(none)"
    commas [x] = x
    commas (x : xs) = x <> ", " <> commas xs
