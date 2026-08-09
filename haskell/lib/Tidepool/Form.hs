{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Ask a human, get a typed value.
--
-- @askUser \@T@ presents a form and returns @T@. Define @T@ with ordinary
-- records and constructors, derive @Generic@ for it and any nested custom
-- types, and end fields in @Text@, @Int@, @Double@, or @Bool@. Constructors
-- are choices, record fields are named inputs, and @Maybe a@ is optional.
--
-- > data Environment = Development | Staging | Production deriving (Generic)
-- >
-- > data DeployRequest = DeployRequest
-- >   { service     :: Text
-- >   , environment :: Environment
-- >   , replicas    :: Int
-- >   , releaseNote :: Maybe Text
-- >   } deriving (Generic)
-- >
-- > request <- askUser @DeployRequest
--
-- The type IS the form: there is no second description of the shape to drift
-- from the answer type, and nothing to import, derive, or annotate beyond
-- @Generic@.
--
-- When the alternatives exist only as runtime VALUES — not as a type's
-- constructors — 'choose' and 'chooseMany' take them as
-- @[(label, value)]@ pairs. That is a different primitive on purpose:
-- @askUser \@T@ covers structure a TYPE defines, 'choose' covers structure a
-- VALUE defines, and neither can express the other.
--
-- All three BLOCK for a human and return the typed value directly. A
-- submission that does not fit re-presents the SAME form; no @Either@
-- reaches the caller.
module Tidepool.Form
  ( askUser
  , choose
  , chooseMany
  ) where

import Prelude
import Data.Text (Text)

import Tidepool.Effects (M, askUserRaw)
import Tidepool.Form.GForm (DerivedForm, decodeForm, formShape)
import Tidepool.Form.Shape
  ( FieldShape (..)
  , FormAnswer (..)
  , FormShape (..)
  , VariantShape (..)
  )
import Tidepool.Form.Wire (decodeAnswer, encodeShape)

-- | Present a form derived from @a@'s own structure and return the @a@ the
-- operator built.
--
-- The shape is derived from type metadata alone — no @a@ exists yet, or even
-- needs to be constructible — and the answer is rebuilt through the SAME
-- generic traversal, so the form and the value cannot disagree about field
-- order, constructor tags, or nesting.
--
-- A malformed submission re-presents the same form by recursion. The retry
-- is entirely here: the caller sees @M a@, never a failure to handle. (The
-- driver bounds the consecutive re-presentations, so a non-interactive gate
-- cannot spin forever.)
askUser :: forall a. DerivedForm a => M a
askUser = do
  submitted <- askUserRaw (encodeShape (formShape @a))
  case decodeAnswer submitted of
    Just answer -> case decodeForm @a answer of
      Right value -> pure value
      Left _ -> askUser @a
    Nothing -> askUser @a

-- | Ask the operator to pick ONE of a list of runtime alternatives. The
-- 'Text' is what they see; the value they picked is what comes back.
--
-- > lane <- choose [(name, lane) | lane <- lanes, let name = laneName lane]
--
-- Offering nothing has no answer that could be returned, so @choose []@
-- re-presents an empty choice until the driver's re-prompt bound ends it.
-- Use 'chooseMany' (which may legitimately return @[]@) when the list can be
-- empty.
choose :: forall a. [(Text, a)] -> M a
choose options = do
  submitted <- askUserRaw (encodeShape shape)
  case decodeAnswer submitted of
    Just (SumAnswer label UnitAnswer) | Just value <- lookup label options -> pure value
    _ -> choose options
  where
    shape = SumShape "Choice" (map (\(label, _) -> VariantShape label UnitShape) options)

-- | Ask the operator to pick ANY NUMBER of a list of runtime alternatives,
-- including none. The picked values come back in the order they were
-- offered.
--
-- > keep <- chooseMany [(idea, idea) | idea <- ideas st]
chooseMany :: forall a. [(Text, a)] -> M [a]
chooseMany options = do
  submitted <- askUserRaw (encodeShape shape)
  case decodeAnswer submitted of
    Just (ProductAnswer picked) | Just values <- selected picked -> pure values
    _ -> chooseMany options
  where
    shape =
      ProductShape
        "Choices"
        "Choices"
        (map (\(label, _) -> FieldShape label BoolShape) options)
    -- One checkbox per offered label, read back in OFFER order rather than
    -- submission order — so a repeated label cannot silently reorder or drop
    -- the values it stands for.
    selected submittedFields = go options
      where
        go [] = Just []
        go ((label, value) : rest) = case lookup label submittedFields of
          Just (BoolAnswer True) -> fmap (value :) (go rest)
          Just (BoolAnswer False) -> go rest
          _ -> Nothing
