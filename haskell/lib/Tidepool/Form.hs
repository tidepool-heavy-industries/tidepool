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
-- The type defines the form: there is no second description of the shape to
-- drift from the answer type. The submitted value is
-- ordinary JSON read by the same generic @FromJSON@ every external value
-- uses.
--
-- When the alternatives exist only as runtime VALUES — not as a type's
-- constructors — 'choose' and 'chooseMany' take them as
-- @[(label, value)]@ pairs. That is a different primitive on purpose:
-- @askUser \@T@ covers structure a TYPE defines, 'choose' covers structure a
-- VALUE defines, and neither can express the other.
--
-- All three block for a human and return the typed value directly. A
-- submission that does not fit re-presents the same form; no @Either@
-- reaches the caller.
module Tidepool.Form
  ( askUser
  , choose
  , chooseMany
  , note
  ) where

import Prelude
import Data.Text (Text)
import qualified Data.Map.Strict as Map

import Tidepool.Aeson.FromJSON (Result (..), fromJSON)
import Tidepool.Aeson.Value (Value (..))
import Tidepool.Effects (M, askUserRaw, noteRaw)
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
-- needs to be constructible. The submitted value is ordinary JSON, decoded
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

-- | Post markdown-ish narration to the operator's accumulating feed. Does
-- NOT block: the driver services this by displaying the text and resuming
-- immediately, so a turn can freely interleave narration with forms —
-- @note "why I'm asking this" >> choose [...]@ — without waiting on the
-- operator between the two.
--
-- Use this to say what you are about to ask and why, before presenting a
-- form via 'askUser'\/'choose'\/'chooseMany' — the operator otherwise sees
-- only the form itself, with no stated intent behind it.
note :: Text -> M ()
note = noteRaw

-- | Ask the operator to pick one of a list of runtime alternatives. The
-- 'Text' is what they see; the value they picked is what comes back.
--
-- > lane <- choose [(name, lane) | lane <- lanes, let name = laneName lane]
--
-- Offering nothing has no answer that could be returned, so @choose []@
-- re-presents an empty choice until the driver's re-prompt bound ends it.
-- Use 'chooseMany' (which may legitimately return @[]@) when the list can be
-- empty.
choose :: forall a. [(Text, a)] -> M a
choose options
  | repeatedLabel options = error "choose: labels must be unique"
  | otherwise = awaitChoice
  where
    awaitChoice = do
      submitted <- askUserRaw (encodeShape shape)
      case submitted of
        String label | Just value <- lookup label options -> pure value
        _ -> awaitChoice
    shape =
      SumShape
        "Choice"
        (map (\(label, _) -> VariantShape label (ProductShape "Choice" label [])) options)

-- | Ask the operator to pick ANY NUMBER of a list of runtime alternatives,
-- including none. The picked values come back in the order they were
-- offered.
--
-- > keep <- chooseMany [(idea, idea) | idea <- ideas st]
chooseMany :: forall a. [(Text, a)] -> M [a]
chooseMany options
  | repeatedLabel options = error "chooseMany: labels must be unique"
  | otherwise = awaitChoices
  where
    awaitChoices = do
      submitted <- askUserRaw (encodeShape shape)
      case submitted of
        -- One checkbox per offered label, submitted as a plain JSON object of
        -- booleans, read back in offer order rather than submission order.
        Object picked | Just values <- selected picked -> pure values
        _ -> awaitChoices
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

-- Labels are submitted as object keys (or constructor tags), so duplicates
-- cannot represent distinct choices. Reject programmer error before showing
-- an ambiguous form instead of silently aliasing two values.
repeatedLabel :: [(Text, a)] -> Bool
repeatedLabel = go []
  where
    go _ [] = False
    go seen ((label, _) : rest) = label `elem` seen || go (label : seen) rest
