{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DuplicateRecordFields #-}

-- | The stable home for effect-adjacent decls a bridged record's FIELD
-- embeds (see tidepool-mcp/src/fs_stable.rs). DO NOT EDIT BY HAND: the
-- Rust side is the single source of truth, and this file is regenerated +
-- verified by the `stable_records` test
-- (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).
module Tidepool.Records.Stable
  ( FsError(..), FileRead(..) ) where

import Prelude (Either(..), Eq, Show)
import Data.Text (Text)
import Tidepool.Aeson.Value (ToJSON(..), object, (.=))

data FsError = FsNotFound Text | FsNotUtf8 Text | FsSandbox Text | FsBadRegex Text | FsIo Text | FsNonUtf8Path Text deriving (Show, Eq)
instance ToJSON FsError where
  toJSON e = case e of
    FsNotFound path -> object ["tag" .= ("FsNotFound" :: Text), "path" .= path]
    FsNotUtf8 path -> object ["tag" .= ("FsNotUtf8" :: Text), "path" .= path]
    FsSandbox detail -> object ["tag" .= ("FsSandbox" :: Text), "detail" .= detail]
    FsBadRegex detail -> object ["tag" .= ("FsBadRegex" :: Text), "detail" .= detail]
    FsIo detail -> object ["tag" .= ("FsIo" :: Text), "detail" .= detail]
    FsNonUtf8Path path -> object ["tag" .= ("FsNonUtf8Path" :: Text), "path" .= path]


data FileRead = FileRead { path :: Text, contents :: Either FsError Text } deriving (Show, Eq)
instance ToJSON FileRead where
  toJSON (FileRead p c) = object ["path" .= p, "contents" .= c]
