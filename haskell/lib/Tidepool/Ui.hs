{-# LANGUAGE OverloadedStrings #-}

-- | The @Ui@ eDSL — UI described as DATA. R0 is first-order: no monadic
-- sequencing, no @Dialog@ effect, no @uiOf@\/@[form|]@ (those are R1 —
-- see @plans\/harness-r0\/50-ui-edsl\/SPEC.md@). Haskell speaks elicitation
-- semantics (card\/prose\/code\/choice\/text\/badge); Datastar\/HTML
-- vocabulary exists only in the tidepool-web renderer.
--
-- WIRE CONTRACT: the JSON shape here is a cross-language contract PINNED by
-- the Rust mirror @tidepool-harness\/src\/ui.rs@ — its @wire_shape_is_stable@
-- test is canonical. The shape matches exactly: externally tagged by a
-- @"ui"@ field, snake_case tag\/kind values, @options@ as 2-element arrays.
--
-- One thing does NOT match byte-for-byte: object key ORDER. The vendored
-- 'Tidepool.Aeson.Value.object' is backed by @Data.Map.Strict@, which always
-- iterates keys in ascending order — it structurally cannot reproduce the
-- Rust struct's declaration-order (tag-first) field sequence. This is
-- harmless: JSON object member order carries no semantics, and every
-- consumer (serde on the Rust side, this module's own JIT probe) compares
-- structurally, never as a literal string.
module Tidepool.Ui
  ( Ui (..)
  , BadgeKind (..)
    -- * Smart constructors
  , card
  , prose
  , code
  , choice
  , textIn
  , keyedText
  , keyedTextInitial
  , keyedChoice
  , keyedChoiceSelected
  , keyedMultiChoice
  , badge
  ) where

import Prelude
import Data.Text (Text)

import Tidepool.Aeson.Value (ToJSON (..), Value (..), object, (.=))

-- | UI-as-data. Every 'Choice' renders with an open-prose escape path in
-- addition to its options — a renderer-side law (B2), not represented in
-- this type.
data Ui
  = Card   { title :: Text, body :: [Ui] }
  | Prose  { text :: Text }                              -- ^ Markdown.
  | Code   { lang :: Text, source :: Text }               -- ^ Fenced source block.
    -- | @options@ are (option-key, label) pairs. @key@ is the FORM-FIELD key:
    -- 'Just' makes this a field of an enclosing form (renders as a radio group
    -- submitted under @values.<key>@); 'Nothing' is a standalone one-click
    -- choice. (@Tidepool.Form@ sets it; hand-authored @choice@ leaves it off.)
    -- @selected@ is the option-key pre-selected as the radio group's default.
  | Choice { prompt :: Text, options :: [(Text, Text)], key :: Maybe Text, selected :: Maybe Text }
    -- | @initial@ seeds the input's starting text (a draft to edit).
  | TextIn { prompt :: Text, multiline :: Bool, key :: Maybe Text, initial :: Maybe Text }
    -- | Checkboxes over (option-key, label) pairs — a FORM-FIELD widget,
    -- keyed exactly like a keyed 'Choice': submitted under @values.<key>@ as
    -- an ARRAY of the checked option-keys.
  | MultiChoice { prompt :: Text, options :: [(Text, Text)], key :: Maybe Text }
  | Badge  { label :: Text, kind :: BadgeKind }
  deriving (Eq, Show)

-- | Effect-row \/ fan \/ price \/ state chips.
data BadgeKind = EffectRow | Fan | Price | State
  deriving (Eq, Show)

instance ToJSON Ui where
  toJSON u = case u of
    Card t b    -> object [ "ui" .= ("card" :: Text), "title" .= t, "body" .= b ]
    Prose t     -> object [ "ui" .= ("prose" :: Text), "text" .= t ]
    Code l s    -> object [ "ui" .= ("code" :: Text), "lang" .= l, "source" .= s ]
    Choice p os mk msel -> object ([ "ui" .= ("choice" :: Text), "prompt" .= p, "options" .= os ] ++ keyField mk ++ selectedField msel)
    TextIn p m mk mi    -> object ([ "ui" .= ("text_in" :: Text), "prompt" .= p, "multiline" .= m ] ++ keyField mk ++ initialField mi)
    MultiChoice p os mk -> object ([ "ui" .= ("multi_choice" :: Text), "prompt" .= p, "options" .= os ] ++ keyField mk)
    Badge l k   -> object [ "ui" .= ("badge" :: Text), "label" .= l, "kind" .= k ]
    where
      -- Emit the form-field key only when present (matches the Rust mirror's
      -- `skip_serializing_if`, so standalone widgets stay wire-identical).
      keyField = maybe [] (\k -> [ "key" .= (k :: Text) ])
      selectedField = maybe [] (\s -> [ "selected" .= (s :: Text) ])
      initialField = maybe [] (\i -> [ "initial" .= (i :: Text) ])

instance ToJSON BadgeKind where
  toJSON EffectRow = String "effect_row"
  toJSON Fan       = String "fan"
  toJSON Price     = String "price"
  toJSON State     = String "state"

-- ---------------------------------------------------------------------------
-- Smart constructors — ergonomic eval authoring
-- ---------------------------------------------------------------------------

-- | A titled card grouping child elements.
card :: Text -> [Ui] -> Ui
card = Card

-- | Markdown prose.
prose :: Text -> Ui
prose = Prose

-- | A fenced source block: language, then source text.
code :: Text -> Text -> Ui
code = Code

-- | A standalone one-click choice: (option-key, label) options; the renderer
-- adds an open-prose escape unconditionally (B2 — not represented here).
choice :: Text -> [(Text, Text)] -> Ui
choice p os = Choice p os Nothing Nothing

-- | A standalone free-text prompt; 'True' for a multiline (textarea) input.
textIn :: Text -> Bool -> Ui
textIn p m = TextIn p m Nothing Nothing

-- | A FORM-FIELD text input, submitted under @values.<key>@ (used by
-- 'Tidepool.Form'): field key, prompt, multiline flag.
keyedText :: Text -> Text -> Bool -> Ui
keyedText k p m = TextIn p m (Just k) Nothing

-- | A FORM-FIELD text input seeded with an initial (editable) value: field
-- key, prompt, multiline flag, initial text.
keyedTextInitial :: Text -> Text -> Bool -> Text -> Ui
keyedTextInitial k p m initial = TextIn p m (Just k) (Just initial)

-- | A FORM-FIELD radio choice, submitted under @values.<key>@ (used by
-- 'Tidepool.Form'): field key, prompt, (option-key, label) options.
keyedChoice :: Text -> Text -> [(Text, Text)] -> Ui
keyedChoice k p os = Choice p os (Just k) Nothing

-- | A FORM-FIELD radio choice with a pre-selected default option-key.
keyedChoiceSelected :: Text -> Text -> [(Text, Text)] -> Text -> Ui
keyedChoiceSelected k p os sel = Choice p os (Just k) (Just sel)

-- | A FORM-FIELD checkbox group, submitted under @values.<key>@ as an ARRAY
-- of the checked option-keys (used by 'Tidepool.Form'\'s @multiChoiceField@):
-- field key, prompt, (option-key, label) options.
keyedMultiChoice :: Text -> Text -> [(Text, Text)] -> Ui
keyedMultiChoice k p os = MultiChoice p os (Just k)

-- | A small chip (effect-row \/ fan \/ price \/ state).
badge :: Text -> BadgeKind -> Ui
badge = Badge
