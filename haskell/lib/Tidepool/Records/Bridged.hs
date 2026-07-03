{-# LANGUAGE NoImplicitPrelude, DuplicateRecordFields #-}

-- | GENERATED from the Rust bridged-record structs in tidepool-handlers
-- (each carries `#[derive(CoreRecord)]`). DO NOT EDIT BY HAND: the Rust
-- struct is the single source of truth for field order / name / type, and
-- this file is regenerated + verified by the `bridged_records` test
-- (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).
module Tidepool.Records.Bridged
  ( Commit(..), StatusEntry(..), FileDelta(..) ) where

import Prelude (Int, Bool, Eq, Show)
import Data.Text (Text)

data Commit = Commit { sha :: Text, subject :: Text, author :: Text, date :: Text, files :: [Text] } deriving (Show, Eq)
data StatusEntry = StatusEntry { path :: Text, state :: Text } deriving (Show, Eq)
data FileDelta = FileDelta { path :: Text, adds :: Int, dels :: Int, binary :: Bool } deriving (Show, Eq)
