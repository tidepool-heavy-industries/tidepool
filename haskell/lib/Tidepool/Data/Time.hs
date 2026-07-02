{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Minimal UTC time surface for Tidepool eval.
--
-- 'UTCTime' is an opaque newtype over epoch milliseconds (Int).  All
-- civil-date arithmetic is pure Int -- no Integer, no FFI -- so the full
-- module is JIT-safe.
--
-- NB: must NOT import Tidepool.Prelude (import cycle).
module Tidepool.Data.Time
  ( UTCTime(..)
  , formatISO8601
  , diffUTCTime
  , addUTCTime
  , epochMillis
  ) where

import Prelude
  ( Int, Double, Bool(..), Show(..), Char, String
  , Fractional(..), Semigroup(..)
  , (+), (-), (*), div, mod, fromIntegral, truncate
  , (>), (>=), (<), (<=), otherwise
  , ($), (.)
  )
import Data.Text (Text)
import qualified Tidepool.Data.Text as T

-- | Opaque UTC timestamp at millisecond resolution.
--
-- The 'Show' instance renders ISO-8601 (e.g. @"1970-01-01T00:00:00Z"@).
newtype UTCTime = UTCTime Int

instance Show UTCTime where
  show t = T.unpack (formatISO8601 t)

-- | Howard Hinnant civil_from_days: epoch-days → (year, month, day).
-- Pure Int arithmetic; correct for the full range of 32-bit epoch-days.
civilFromDays :: Int -> (Int, Int, Int)
civilFromDays z0 =
  let z   = z0 + 719468
      -- Hinnant's C version writes `(z >= 0 ? z : z - 146096) / 146097` to
      -- emulate floor with truncating division; Haskell `div` already floors.
      era = z `div` 146097
      doe = z - era * 146097
      yoe = (doe - doe `div` 1460 + doe `div` 36524 - doe `div` 146096) `div` 365
      y   = yoe + era * 400
      doy = doe - (365 * yoe + yoe `div` 4 - yoe `div` 100)
      mp  = (5 * doy + 2) `div` 153
      d   = doy - (153 * mp + 2) `div` 5 + 1
      m   = mp + (if mp < 10 then 3 else -9)
      y'  = y + (if m <= 2 then 1 else 0)
  in (y', m, d)

showInt :: Int -> String -> String
showInt n acc
  | n < 10    = digitChar n : acc
  | otherwise = showInt (n `div` 10) (digitChar (n `mod` 10) : acc)

digitChar :: Int -> Char
digitChar 0 = '0'
digitChar 1 = '1'
digitChar 2 = '2'
digitChar 3 = '3'
digitChar 4 = '4'
digitChar 5 = '5'
digitChar 6 = '6'
digitChar 7 = '7'
digitChar 8 = '8'
digitChar _ = '9'

pad2 :: Int -> Text
pad2 n
  | n < 10    = T.pack ('0' : showInt n "")
  | otherwise = T.pack (showInt n "")

pad4 :: Int -> Text
pad4 n
  | n < 10    = T.pack ('0' : '0' : '0' : showInt n "")
  | n < 100   = T.pack ('0' : '0' : showInt n "")
  | n < 1000  = T.pack ('0' : showInt n "")
  | otherwise = T.pack (showInt n "")

-- | Render a 'UTCTime' as an ISO-8601 string (UTC, no sub-second precision).
--
-- >>> formatISO8601 (UTCTime 0)
-- "1970-01-01T00:00:00Z"
--
-- >>> formatISO8601 (UTCTime 1709164800000)
-- "2024-02-29T00:00:00Z"
formatISO8601 :: UTCTime -> Text
formatISO8601 (UTCTime ms) =
  let totalSecs  = ms `div` 1000
      -- Haskell `div` is FLOOR division (unlike C's truncation), so no
      -- negative-branch adjustment: floor is exactly what civil-date math
      -- needs, and `mod`'s always-non-negative remainder gives secsInDay.
      daysSince  = totalSecs `div` 86400
      secsInDay  = totalSecs `mod` 86400
      (y, mo, d) = civilFromDays daysSince
      hh         = secsInDay `div` 3600
      mm         = (secsInDay `mod` 3600) `div` 60
      ss         = secsInDay `mod` 60
  in pad4 y <> "-" <> pad2 mo <> "-" <> pad2 d
          <> "T" <> pad2 hh <> ":" <> pad2 mm <> ":" <> pad2 ss <> "Z"

-- | Difference in seconds between two 'UTCTime' values (@a - b@).
diffUTCTime :: UTCTime -> UTCTime -> Double
diffUTCTime (UTCTime a) (UTCTime b) =
  fromIntegral (a - b) / 1000.0

-- | Add a number of seconds (may be fractional) to a 'UTCTime'.
addUTCTime :: Double -> UTCTime -> UTCTime
addUTCTime secs (UTCTime ms) =
  UTCTime (ms + (truncate (secs * 1000.0) :: Int))

-- | Extract the raw epoch milliseconds.
epochMillis :: UTCTime -> Int
epochMillis (UTCTime ms) = ms
