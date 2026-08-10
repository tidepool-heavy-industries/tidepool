{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}

-- | Ask a human, get a typed value.
--
-- @askUser \@T@ presents a form and returns @T@. Define @T@ with ordinary
-- records and constructors, derive @Generic@ and @FromJSON@ (the generic
-- default — no method to write) for it and any nested custom types, and end
-- fields in @Text@, @Int@, @Double@, or @Bool@. Constructors are choices,
-- record fields are named inputs, and @Maybe a@ is optional.
--
-- > data Environment = Development | Staging | Production deriving (Generic, FromJSON)
-- >
-- > data DeployRequest = DeployRequest
-- >   { service     :: Text
-- >   , environment :: Environment
-- >   , replicas    :: Int
-- >   , releaseNote :: Maybe Text
-- >   } deriving (Generic, FromJSON)
-- >
-- > request <- askUser @DeployRequest
--
-- The type IS the form: there is no second description of the shape to
-- drift from the answer type, and no second decode — the submitted value is
-- ordinary JSON read by the same generic @FromJSON@ every external value
-- uses.
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
import qualified Data.Map.Strict as Map

import Tidepool.Aeson.FromJSON (Result (..), fromJSON)
import Tidepool.Aeson.Value (Value (..))
import Tidepool.Effects (M, askUserRaw)
import Tidepool.Form.GForm (DerivedForm, formShape)
import Tidepool.Form.Shape
  ( FieldShape (..)
  , FormShape (..)
  , VariantShape (..)
  )
import Tidepool.Form.Wire (encodeShape)

-- | Present a form derived from @a@'s own structure and return the @a@ the
-- operator built.
--
-- The shape is derived from type metadata alone — no @a@ exists yet, or even
-- needs to be constructible. The submitted value is ORDINARY JSON, decoded
-- by @a@'s own 'Tidepool.Aeson.FromJSON.FromJSON' instance (normally the
-- generic default): the collector renders record objects, tagged record
-- sums, bare strings for enums, and @null@\/omission for optionals — the
-- exact shapes that decode accepts. There is no second answer language.
--
-- A malformed submission re-presents the same form by recursion. The retry
-- is entirely here: the caller sees @M a@, never a failure to handle. (The
-- driver bounds the consecutive re-presentations, so a non-interactive gate
-- cannot spin forever.)
askUser :: forall a. DerivedForm a => M a
askUser = do
  submitted <- askUserRaw (encodeShape (formShape @a))
  case fromJSON submitted of
    Success value -> pure value
    Error _ -> askUser @a

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
  case submitted of
    -- An all-nullary chooser submits the picked label as a bare string —
    -- the same wire an enum type's generic decode reads.
    String label | Just value <- lookup label options -> pure value
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
  case submitted of
    -- One checkbox per offered label, submitted as a plain JSON object of
    -- booleans, read back in OFFER order rather than submission order — so
    -- a repeated label cannot silently reorder or drop the values it
    -- stands for.
    Object picked | Just values <- selected picked -> pure values
    _ -> chooseMany options
  where
    shape =
      ProductShape
        "Choices"
        "Choices"
        (map (\(label, _) -> FieldShape label BoolShape) options)
    selected picked = go options
      where
        go [] = Just []
        go ((label, value) : rest) = case Map.lookup label picked of
          Just (Bool True) -> fmap (value :) (go rest)
          Just (Bool False) -> go rest
          _ -> Nothing
