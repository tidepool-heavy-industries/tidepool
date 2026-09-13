{-# LANGUAGE NoImplicitPrelude #-}
-- | Stable extractor intrinsics for rendering 'Double' values.
--
-- Backends may replace these exact bindings with managed-Text primitives.
-- Their reference bodies must have the real returning semantics: OPAQUE keeps
-- calls intact, but does not hide bottoming demand information from GHC.
module Tidepool.Double (renderDouble, renderDoublePrec) where

import Data.Text (Text)
import Data.Text qualified as Text
import Prelude (Double, Int, show, showsPrec)

{-# OPAQUE renderDouble #-}
renderDouble :: Double -> Text
renderDouble value = Text.pack (show value)

{-# OPAQUE renderDoublePrec #-}
renderDoublePrec :: Int -> Double -> Text
renderDoublePrec precedence value = Text.pack (showsPrec precedence value "")
