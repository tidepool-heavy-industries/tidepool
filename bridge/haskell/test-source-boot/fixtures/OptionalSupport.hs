module OptionalSupport where

import qualified Data.Time.Calendar as Calendar

optional :: Bool
optional = Calendar.isLeapYear 2024
