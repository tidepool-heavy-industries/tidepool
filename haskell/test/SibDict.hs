{-# LANGUAGE GADTs #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE OverloadedStrings #-}
module SibDict where

import Data.Text (Text)
import qualified Data.Text as T

data K a where
  KInt  :: K Int
  KPrec :: Int -> K Double

-- The sibling-alt refined-dict function: `show` at Int in one alt,
-- `show` at Double in the sibling alt. Each alt uses a dictionary
-- refined by the GADT equality evidence.
useK :: K a -> a -> Text
useK KInt       n = T.pack (show n)
useK (KPrec _p) d = T.pack (show (d + 0.0))

-- NOINLINE opaque seeds defeat -O2 constant folding so the real
-- per-alt dictionary Core survives to runtime.
{-# NOINLINE seedKI #-}
seedKI :: K Int
seedKI = KInt
{-# NOINLINE seedN #-}
seedN :: Int
seedN = 5
{-# NOINLINE seedKP #-}
seedKP :: K Double
seedKP = KPrec 2
{-# NOINLINE seedD #-}
seedD :: Double
seedD = 1.5

-- Program bindings (each a total program the corpus replay evaluates).
progInt :: Text
progInt = useK seedKI seedN

progPrec :: Text
progPrec = useK seedKP seedD

progBoth :: Text
progBoth = useK seedKI seedN <> " | " <> useK seedKP seedD

-- CONTROL 1: plain pack . show, no GADT, no sibling.
packOnly :: Int -> Text
packOnly n = T.pack (show n)

progPackInt :: Text
progPackInt = packOnly seedN

-- CONTROL 2: single-alt GADT dict use (only the Double branch).
useKP :: K a -> a -> Text
useKP (KPrec _p) d = T.pack (show (d + 0.0))
useKP KInt      _ = T.empty

progSingle :: Text
progSingle = useKP seedKP seedD

-- CONTROL 3: Either sibling (non-GADT) control — show at Int vs Double.
useE :: Either Int Double -> Text
useE (Left n)  = T.pack (show n)
useE (Right d) = T.pack (show (d + 0.0))

{-# NOINLINE seedEL #-}
seedEL :: Either Int Double
seedEL = Left 5
{-# NOINLINE seedER #-}
seedER :: Either Int Double
seedER = Right 1.5

progEitherL :: Text
progEitherL = useE seedEL

progEitherR :: Text
progEitherR = useE seedER

-- CONTROL 4: sibling-alt but show at Double in BOTH branches (same dict).
useKPP :: K a -> Double -> Text
useKPP KInt       d = T.pack (show (d + 1.0))
useKPP (KPrec _p) d = T.pack (show (d + 0.0))

progSameDict :: Text
progSameDict = useKPP seedKP seedD
