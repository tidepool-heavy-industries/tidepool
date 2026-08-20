{-# LANGUAGE AllowAmbiguousTypes #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE KindSignatures #-}
{-# LANGUAGE OverloadedLabels #-}
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

    -- * Doc-carrying forms
  , FieldRef
  , FieldAnn
  , help
  , FormAnn
  , title
  , field
  , formShapeWith
  , askUserWith
  ) where

import Prelude
import Data.Proxy (Proxy (..))
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import GHC.Records (HasField)
import GHC.TypeLits (KnownSymbol, Symbol, symbolVal)
import GHC.OverloadedLabels (IsLabel (..))

import Tidepool.Aeson.FromJSON (Result (..), fromJSON)
import Tidepool.Aeson.Value (Value (..))
import Tidepool.Effects (M, askUserRaw, noteRaw)
import Tidepool.Form.GForm (DerivedForm, formShape)
import Tidepool.Form.Shape
  ( FieldKey
  , FieldShape (..)
  , FormShape (..)
  , VariantShape (..)
  )
import Tidepool.Form.Wire (encodeShape, encodeShapeAnnotated)

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

-- ---------------------------------------------------------------------------
-- Doc-carrying forms
--
-- 'askUser' derives a form's SHAPE from a type alone; the surface below lets
-- an author attach a title and per-field help prose to that same derivation
-- without writing a second description of the shape. A @#field@ is a typed
-- reference to one of @r@'s own record fields (via 'IsLabel'); 'field' pairs
-- it with help text, erasing the name to the plain 'FieldKey' the wire
-- already carries. A typo'd @#field@ — one that does not name a real field
-- of @r@ — fails to compile: the 'HasField' constraint on 'field' is the
-- check, not a convenience.

-- | A typed reference to a field, produced by @OverloadedLabels@
-- (@#fieldName@) and carrying only the field's NAME — @field@'s own
-- signature carries the record type @r@ and field type @t@, resolved from
-- the label via @HasField@ once @r@ is known from context.
--
-- Deliberately not parameterized over @r@\/@t@ itself: an @IsLabel@ instance
-- whose head repeats a type variable inside a nested application (as a
-- @FieldRef r name t@-shaped carrier would) leaves those OTHER variables
-- ambiguous at the @#label@ use site. Keeping the label-carrier down to its
-- symbol alone sidesteps that: nothing about resolving @#fieldName@ needs
-- @r@ or @t@ at all.
data FieldRef (name :: Symbol) = FieldRef

-- The @name ~ name'@ indirection (rather than @IsLabel name (FieldRef
-- name)@ directly) is load-bearing, not decorative: GHC's ambiguity check
-- for a class method does not treat "the wanted's argument structurally
-- matches the instance head" as enough to pin a metavariable — only a
-- genuinely unconstrained instance parameter (here, @name'@, which appears
-- ONLY as the class's own first parameter, nowhere inside @FieldRef@) lets
-- instance selection proceed before @name@ itself is known, deferring
-- @name ~ name'@ to ordinary equality solving once context (here, @field@'s
-- own @HasField@ constraint) pins it down. Confirmed empirically (plain
-- @ghc@, no tidepool involved): the direct form (@IsLabel name (FieldRef
-- name)@) leaves every use site "Ambiguous type variable", even fully
-- monomorphic ones — a vanilla GHC 9.12 @OverloadedLabels@ inference limit,
-- and the GHC user's guide names this exact workaround for it.
--
-- KNOWN LIMITATION (2026-08-20): this equality constraint is also
-- currently un-runnable through tidepool-extract specifically when 'field'
-- is called from a DIFFERENT module than this instance (i.e. every real
-- use — an eval body calling into the stdlib) — extraction fails with
-- "Dangling NVar reference(s) ... Eq# [GHC.Types]", confirmed via
-- @TIDEPOOL_DANGLING_DEBUG=1@ to be the boxed equality-coercion witness
-- GHC's desugarer builds for the @(name ~ name')@ dictionary at the call
-- site. Reproduced down to a 3-line same-shape repro; SAME-MODULE use (the
-- instance and its use site in one file) extracts and runs fine, isolating
-- this to cross-module dictionary passing for an equality superclass
-- specifically — plain `ghc -O2` (even with tidepool-extract's exact
-- `-fno-full-laziness -fno-cpr-anal` flags) erases the same coercion
-- entirely, so this is a real gap in tidepool-extract's Core→CBOR
-- reachability (most likely: no 'wiredInDataCons'-style entry for the
-- wired-in boxed-equality witness), not a mistake in this instance. Fixing
-- it needs a change in @haskell/src/Tidepool/Translate.hs@, outside this
-- module's scope — until then, 'field' typechecks correctly but a real
-- @askUserWith@\/@formShapeWith@ call using it traps at extract time.
-- 'title'\/'help'\/'formShapeWith'\/'askUserWith' with NO 'field' use are
-- unaffected (confirmed working end to end).
instance (name ~ name', KnownSymbol name) => IsLabel name' (FieldRef name) where
  fromLabel = FieldRef

-- | Help prose for one field. Combine with '<>' to prefer the right-hand
-- (later) text over the left; 'mempty' carries none.
newtype FieldAnn t = FieldAnn (Maybe Text)

instance Semigroup (FieldAnn t) where
  FieldAnn a <> FieldAnn b = FieldAnn (b `orElse` a)
    where
      orElse (Just x) _ = Just x
      orElse Nothing y = y

instance Monoid (FieldAnn t) where
  mempty = FieldAnn Nothing

-- | Help text shown alongside a field's control.
help :: Text -> FieldAnn t
help = FieldAnn . Just

-- | One annotation on a form derived from @r@: either the form's own title,
-- or one field's help text (name already erased to a plain 'FieldKey').
data FormAnn r
  = FormTitleAnn Text
  | FormFieldAnn FieldKey (Maybe Text)

-- | Give the derived form a title.
title :: Text -> FormAnn r
title = FormTitleAnn

-- | Attach help text to one of @r@'s own fields. @name@ must actually be a
-- field of @r@ — 'HasField' is the compile-time check; a typo'd @#field@
-- fails to compile naming that class.
field ::
  forall name r t.
  (KnownSymbol name, HasField name r t) =>
  FieldRef name ->
  FieldAnn t ->
  FormAnn r
field FieldRef (FieldAnn h) = FormFieldAnn (T.pack (symbolVal (Proxy @name))) h

-- | The derived wire shape for @a@ with the given title\/field docs merged
-- in — pure, so tests can assert its exact JSON. With no annotations this is
-- byte-identical to @encodeShape (formShape \@a)@ ('askUser''s own shape).
formShapeWith :: forall a. DerivedForm a => [FormAnn a] -> Value
formShapeWith anns = encodeShapeAnnotated mTitle fieldDocs (formShape @a)
  where
    mTitle = case [t | FormTitleAnn t <- anns] of
      [] -> Nothing
      ts -> Just (last ts)
    fieldDocs =
      Map.fromList [(k, t) | FormFieldAnn k (Just t) <- anns]

-- | Like 'askUser', but the presented form carries the given title\/field
-- docs. Same retry-on-malformed-submission behavior.
askUserWith :: forall a. DerivedForm a => [FormAnn a] -> M a
askUserWith anns = do
  submitted <- askUserRaw (formShapeWith @a anns)
  case fromJSON submitted of
    Success value -> pure value
    Error _ -> askUserWith @a anns
