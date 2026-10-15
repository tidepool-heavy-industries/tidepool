{-# LANGUAGE DeriveGeneric #-}
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
  , FileReference(..)
  , Publication(..)
  , CommitOutcome(..)
  , snapshotReference
  , contentVersionText
  , writeSnapshotReceipt
  , createFile
  , replaceFile
  ) where

import Prelude hiding (FilePath)
import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)
import Tidepool.FilePath (FilePath)
import Tidepool.Effects.Authored
  ( FsRead, FsWrite(..), FsSnapshot(..), FsWriteReceipt(..), FsPublication(..), readSnapshotRaw, writeCheckedIf )
import Tidepool.Records (FsError, WriteOutcome)
import GHC.Generics (Generic)
import Data.Proxy (Proxy(..))
import qualified Tidepool.Data.Text as T
import Tidepool.Aeson
import Tidepool.Aeson.Schema (JsonSchema(..), objectSchema)
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

-- | A serializable content precondition, not a filesystem grant or read-history
-- certificate. The path is the resolved destination returned by readSnapshot.
data FileReference = FileReference { referencePath :: FilePath, referenceVersion :: ContentVersion }
  deriving (Eq, Show, Generic)
instance ToJSON FileReference where
  toJSON (FileReference path version) = object ["path" .= path, "version" .= version]
instance FromJSON FileReference where
  parseJSON = withObject "FileReference" $ \o -> FileReference <$> o .: "path" <*> o .: "version"
instance JsonSchema FileReference where
  jsonSchema _ = objectSchema Nothing [("path", jsonSchema (Proxy :: Proxy Text), True), ("version", jsonSchema (Proxy :: Proxy ContentVersion), True)]

contentVersionText :: ContentVersion -> Text
contentVersionText (ContentVersion version) = version
instance ToJSON ContentVersion where toJSON = toJSON . contentVersionText
instance FromJSON ContentVersion where
  parseJSON = withText "ContentVersion" $ \text ->
    if T.length text == 64 && T.all (\c -> (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')) text
      then pure (ContentVersion text)
      else Error "ContentVersion must be a 64-character lowercase hexadecimal Blake3 digest"
instance JsonSchema ContentVersion where
  jsonSchema _ = object [("type", String "string"), ("pattern", String "^[0-9a-f]{64}$")]

snapshotReference :: FileSnapshot -> FileReference
snapshotReference snapshot = FileReference (snapshotPath snapshot) (snapshotVersion snapshot)

data Publication = Durable | VisibleDurabilityUnconfirmed Text deriving (Eq, Show, Generic)
instance ToJSON Publication
instance JsonSchema Publication

-- Conflict retains the requested target text; only Published issues a resolved path.
data CommitOutcome
  = Published { writtenFile :: FileReference, publication :: Publication }
  | Conflict { requestedPath :: FilePath, expectedVersion :: Maybe ContentVersion, actualVersion :: Maybe ContentVersion }
  deriving (Eq, Show, Generic)
instance ToJSON CommitOutcome
instance JsonSchema CommitOutcome

-- | The receipt names this publication, not the file's state after return.
-- VisibleDurabilityUnconfirmed is a committed effect: never replay that write.
writeSnapshotReceipt :: Member FsWrite effects => FileSnapshot -> Text -> Eff effects (Either FsError CommitOutcome)
writeSnapshotReceipt snapshot = replaceFile (snapshotReference snapshot)

createFile :: Member FsWrite effects => FilePath -> Text -> Eff effects (Either FsError CommitOutcome)
createFile = writeReceipt Nothing

replaceFile :: Member FsWrite effects => FileReference -> Text -> Eff effects (Either FsError CommitOutcome)
replaceFile (FileReference path version) = writeReceipt (Just version) path

writeReceipt :: Member FsWrite effects => Maybe ContentVersion -> FilePath -> Text -> Eff effects (Either FsError CommitOutcome)
writeReceipt expected path contents = do
  result <- send (FsWriteCas path (contentVersionText <$> expected) contents)
  pure $ case result of
    Left err -> Left err
    Right (Left actual) -> Right (Conflict path expected (ContentVersion <$> actual))
    Right (Right (FsWriteReceipt target digest state)) -> Right (Published
      (FileReference target (ContentVersion digest))
      (case state of FsDurable -> Durable; FsVisibleDurabilityUnconfirmed detail -> VisibleDurabilityUnconfirmed detail))
