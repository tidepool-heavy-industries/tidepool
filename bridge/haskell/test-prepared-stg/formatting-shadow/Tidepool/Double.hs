{-# LANGUAGE NoImplicitPrelude #-}
module Tidepool.Double (renderDouble, renderDoublePrec) where

import Data.Text (Text)
import Data.Text qualified as Text
import Prelude (Double, Int, show, showsPrec, (++))

{-# OPAQUE renderDouble #-}
renderDouble :: Double -> Text
renderDouble value = Text.pack (show value ++ " shadow")

{-# OPAQUE renderDoublePrec #-}
renderDoublePrec :: Int -> Double -> Text
renderDoublePrec precedence value = Text.pack (showsPrec precedence value " shadow")
