{-# LANGUAGE PackageImports #-}

module FormattingDependencyShadow where

import "text" Data.Text (Text)
import Data.Text qualified as Shadow
import Tidepool.Double (renderDouble)

trusted :: Text
trusted = renderDouble 1.5

shadowed :: Text
shadowed = Shadow.pack "1.5"
