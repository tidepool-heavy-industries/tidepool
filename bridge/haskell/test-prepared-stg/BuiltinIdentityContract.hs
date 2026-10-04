{-# LANGUAGE PackageImports #-}
module BuiltinIdentityContract where

import qualified "text" Data.Text.Internal as ActualText
import qualified "base" Numeric.Natural as ActualNatural
import qualified Data.Text.Internal as ForeignText
import qualified GHC.Num.Integer as ForeignInteger
import qualified GHC.Num.Natural as ForeignNatural

realText :: ActualText.Text -> ActualText.Text
realText value = value

foreignText :: ForeignText.Text -> ForeignText.Text
foreignText value = value

realInteger :: Integer -> Integer
realInteger value = value

foreignInteger :: ForeignInteger.Integer -> ForeignInteger.Integer
foreignInteger value = value

realNatural :: ActualNatural.Natural -> ActualNatural.Natural
realNatural value = value

foreignNatural :: ForeignNatural.Natural -> ForeignNatural.Natural
foreignNatural value = value
