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
  | Choice { prompt :: Text, options :: [(Text, Text)] }  -- ^ (key, label) pairs.
  | TextIn { prompt :: Text, multiline :: Bool }
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
    Choice p os -> object [ "ui" .= ("choice" :: Text), "prompt" .= p, "options" .= os ]
    TextIn p m  -> object [ "ui" .= ("text_in" :: Text), "prompt" .= p, "multiline" .= m ]
    Badge l k   -> object [ "ui" .= ("badge" :: Text), "label" .= l, "kind" .= k ]

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

-- | A closed set of (key, label) options; the renderer adds an open-prose
-- escape unconditionally (B2 — not represented here).
choice :: Text -> [(Text, Text)] -> Ui
choice = Choice

-- | A free-text prompt; 'True' for a multiline (textarea) input.
textIn :: Text -> Bool -> Ui
textIn = TextIn

-- | A small chip (effect-row \/ fan \/ price \/ state).
badge :: Text -> BadgeKind -> Ui
badge = Badge
