{-# LANGUAGE OverloadedStrings #-}

module TimeIntrinsicContract where

import Data.Text (Text)
import Tidepool.Data.Time (UTCTime, epochMillis, parseISO8601)

parsedMillis :: Either Text UTCTime -> Int
parsedMillis = either (const (-1)) epochMillis

directUtc :: Int
directUtc = parsedMillis (parseISO8601 "1970-01-01T00:00:00Z")

offsetUtc :: Int
offsetUtc = parsedMillis (parseISO8601 "2026-07-01T19:24:22-07:00")

higherOrder :: Int
higherOrder = applyParser parseISO8601 "2024-02-29T00:00:00Z"
  where
    applyParser parser = parsedMillis . parser

invalidError :: Text
invalidError = either id (const "unexpected success") (parseISO8601 "not-a-time")
