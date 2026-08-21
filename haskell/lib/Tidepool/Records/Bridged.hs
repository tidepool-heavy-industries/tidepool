{-# LANGUAGE NoImplicitPrelude, DuplicateRecordFields, NoFieldSelectors #-}

-- | GENERATED from the Rust bridged-record structs in tidepool-bridge-effects
-- (each carries `#[derive(CoreRecord)]`). DO NOT EDIT BY HAND: the Rust
-- struct is the single source of truth for field order / name / type, and
-- this file is regenerated + verified by the `bridged_records` test
-- (`TIDEPOOL_REGEN_BRIDGED=1 cargo test -p tidepool-handlers bridged_records`).
-- NoFieldSelectors: no field of any record here is exported as a top-level
-- function — access is record-dot only (HasField). Prevents a field name
-- (e.g. CommitDeltas's `commit`) from colliding with an unrelated binding
-- of the same name elsewhere (e.g. Tidepool.Event's `commit` builder).
module Tidepool.Records.Bridged
  ( Commit(..), StatusEntry(..), FileDelta(..), CommitDeltas(..), Proc(..), Hit(..), FileMeta(..) ) where

import Prelude (Int, Bool, Eq, Show)
import Data.Text (Text)

data Commit = Commit { sha :: Text, subject :: Text, author :: Text, date :: Text, files :: [Text] } deriving (Show, Eq)
data StatusEntry = StatusEntry { path :: Text, state :: Text } deriving (Show, Eq)
data FileDelta = FileDelta { path :: Text, adds :: Int, dels :: Int, binary :: Bool } deriving (Show, Eq)
data CommitDeltas = CommitDeltas { commit :: Commit, deltas :: [FileDelta] } deriving (Show, Eq)
data Proc = Proc { exitCode :: Int, stdout :: Text, stderr :: Text } deriving (Show, Eq)
data Hit = Hit { path :: Text, line :: Int, text :: Text } deriving (Show, Eq)
data FileMeta = FileMeta { size :: Int, isFile :: Bool, isDir :: Bool } deriving (Show, Eq)
