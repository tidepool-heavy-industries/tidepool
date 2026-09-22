{-# LANGUAGE OverloadedStrings, PackageImports #-}

module TimeDependencyShadow where

import "text" Data.Text (Text)
import Data.Text qualified as Shadow
import Tidepool.Data.Time (UTCTime, epochMillis, parseISO8601)

trusted :: Int
trusted = either (const (-1)) epochMillis (parseISO8601 "1970-01-01T00:00:00Z")

shadowed :: Text
shadowed = Shadow.pack "1970-01-01T00:00:00Z"
