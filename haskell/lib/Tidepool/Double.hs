{-# LANGUAGE NoImplicitPrelude #-}
-- | Stable extractor intrinsics for rendering 'Double' values.
--
-- Translate replaces these exact, qualified bindings with managed-Text
-- primitives. Their bodies are deliberately unreachable.
module Tidepool.Double (renderDouble, renderDoublePrec) where

import Data.Text (Text)
import Prelude (Double, Int, error)

{-# OPAQUE renderDouble #-}
renderDouble :: Double -> Text
renderDouble _ = error "Tidepool.Double.renderDouble: extractor intrinsic was not lowered"

{-# OPAQUE renderDoublePrec #-}
renderDoublePrec :: Int -> Double -> Text
renderDoublePrec _ _ = error "Tidepool.Double.renderDoublePrec: extractor intrinsic was not lowered"
