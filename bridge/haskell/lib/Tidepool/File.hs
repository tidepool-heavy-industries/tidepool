{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Version-checked file editing. Snapshots retain their resolved destination,
-- contents and content version together; they do not grant filesystem access.
module Tidepool.File
  ( FileSnapshot
  , ContentVersion
  , readSnapshot
  , snapshotPath
  , snapshotText
  , snapshotVersion
  , writeSnapshot
  ) where

import Prelude hiding (FilePath)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import Tidepool.FilePath (FilePath)
import Tidepool.Effects.Authored
  ( FsRead, FsWrite, FsSnapshot(..), readSnapshotRaw, writeCheckedIf )
import Tidepool.Records (FsError, WriteOutcome)
import Tidepool.Inspection.Display (Display(..), displayRecord)

-- | Content identity, not a monotonic revision. Restoring identical contents
-- restores the same version even if intervening writes occurred.
newtype ContentVersion = ContentVersion Text deriving (Eq, Show)

-- The constructor is private so ordinary edits cannot mix a snapshot's
-- destination, version and observed contents. Runtime grants still authorize
-- every read and write independently.
data FileSnapshot = FileSnapshot FilePath Text ContentVersion

instance Display ContentVersion where
  displayTree (ContentVersion version) =
    displayRecord 0 "ContentVersion" [("digest", displayTree version)]

instance Display FileSnapshot where
  displayTree (FileSnapshot target body version) = displayRecord 0 "FileSnapshot"
    [("path", displayTree target), ("contents", displayTree body), ("version", displayTree version)]

snapshotPath :: FileSnapshot -> FilePath
snapshotPath (FileSnapshot path _ _) = path

snapshotText :: FileSnapshot -> Text
snapshotText (FileSnapshot _ body _) = body

snapshotVersion :: FileSnapshot -> ContentVersion
snapshotVersion (FileSnapshot _ _ version) = version

-- | Capture UTF-8 contents and the Blake3 version of exactly those bytes in
-- one observation. Missing files, invalid UTF-8, sandbox and storage failures
-- remain typed errors. The path is the resolved absolute destination, so a
-- later change to a symlink used for this read does not redirect the edit.
-- Snapshot reads, ordinary filesystem-effect writes and conditional writes
-- serialize through the same parent-directory lock. External writers that
-- bypass it are outside that guarantee.
readSnapshot :: Member FsRead effects
  => FilePath -> Eff effects (Either FsError FileSnapshot)
readSnapshot path = fmap capture <$> readSnapshotRaw path
  where
    capture (FsSnapshot target body version) =
      FileSnapshot target body (ContentVersion version)

-- | Write to the captured destination only while its contents still match
-- the captured version. A conflict writes nothing; missing files are not
-- created implicitly. The current runtime grant is checked again at commit.
-- @FsDurabilityUnknown@ means the write is visible: do not replay it.
writeSnapshot :: Member FsWrite effects
  => FileSnapshot -> Text -> Eff effects (Either FsError WriteOutcome)
writeSnapshot (FileSnapshot target _ (ContentVersion version)) =
  writeCheckedIf (Just version) target
