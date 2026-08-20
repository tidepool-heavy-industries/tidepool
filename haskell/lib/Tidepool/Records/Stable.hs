{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DuplicateRecordFields #-}

-- | The stable home for effect-adjacent decls a bridged record's FIELD
-- embeds, or an effect's own `errors` ADT (see tidepool-mcp/src/
-- fs_stable.rs). DO NOT EDIT BY HAND: the Rust side is the single source
-- of truth, and this file is regenerated + verified by the `stable_records`
-- test (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).
module Tidepool.Records.Stable
  ( FsError(..), FileRead(..), GitError(..), LlmError(..), HttpError(..) ) where

import Prelude (Either(..), Eq, Int, Show)
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


data GitError = GitBadRevspec Text | GitFailed Int Text deriving (Show, Eq)
instance ToJSON GitError where
  toJSON e = case e of
    GitBadRevspec detail -> object ["tag" .= ("GitBadRevspec" :: Text), "detail" .= detail]
    GitFailed code detail -> object ["tag" .= ("GitFailed" :: Text), "code" .= code, "detail" .= detail]


data LlmError = LlmApi Text | LlmRefusal Text | LlmBudget deriving (Show, Eq)
instance ToJSON LlmError where
  toJSON e = case e of
    LlmApi detail -> object ["tag" .= ("LlmApi" :: Text), "detail" .= detail]
    LlmRefusal detail -> object ["tag" .= ("LlmRefusal" :: Text), "detail" .= detail]
    LlmBudget -> object ["tag" .= ("LlmBudget" :: Text)]


data HttpError = HttpInvalidUrl Text | HttpRestricted Text | HttpNetwork Text | HttpStatus Int Text | HttpTooLarge Int deriving (Show, Eq)
instance ToJSON HttpError where
  toJSON e = case e of
    HttpInvalidUrl detail -> object ["tag" .= ("HttpInvalidUrl" :: Text), "detail" .= detail]
    HttpRestricted detail -> object ["tag" .= ("HttpRestricted" :: Text), "detail" .= detail]
    HttpNetwork detail -> object ["tag" .= ("HttpNetwork" :: Text), "detail" .= detail]
    HttpStatus code body -> object ["tag" .= ("HttpStatus" :: Text), "code" .= code, "body" .= body]
    HttpTooLarge nodes -> object ["tag" .= ("HttpTooLarge" :: Text), "nodes" .= nodes]


data FileRead = FileRead { path :: Text, contents :: Either FsError Text } deriving (Show, Eq)
instance ToJSON FileRead where
  toJSON (FileRead p c) = object ["path" .= p, "contents" .= c]
