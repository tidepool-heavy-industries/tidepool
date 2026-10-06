{-# LANGUAGE FlexibleInstances #-}
module CandidateDemandInstance where

import CandidateDemandClass
import qualified Data.Time.Calendar as Calendar

instance Available Int where
  {-# NOINLINE available #-}
  available year = if Calendar.isLeapYear (toInteger year) then year - 1982 else 0

{-# NOINLINE sibling #-}
sibling :: Int -> Int
sibling value = value + 2
