{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
-- | Ordinary installed Sync handlers over version-checked filesystem effects.
module Persona.FileTools
  ( ReadInput(..), LineRange(..), ReadReceipt(..), WriteInput(..), EditInput(..)
  , FileToolError(..), PreviewReceipt(..)
  , readFileTool, writeFileTool, editFileTool, previewEditTool
  ) where
import Prelude hiding (FilePath)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson
import Tidepool.Aeson.Schema (JsonSchema)
import Tidepool.FilePath (FilePath)
import Tidepool.Effects.Authored (FsRead, FsWrite)
import Tidepool.Records (FsError)
import qualified Tidepool.File as File
import qualified Tidepool.Text.Navigation as Navigation
import qualified Persona.Patch as Patch

data LineRange = LineRange { firstLine :: Int, lastLine :: Int } deriving (Eq, Show, Generic)
instance FromJSON LineRange
instance ToJSON LineRange
instance JsonSchema LineRange

data ReadInput = ReadInput
  { path :: FilePath, expectedVersion :: Maybe File.ContentVersion, range :: Maybe LineRange }
  deriving (Eq, Show, Generic)
instance FromJSON ReadInput
instance ToJSON ReadInput
instance JsonSchema ReadInput

data ReadReceipt = ReadReceipt
  { file :: File.FileReference, content :: Text, selectedRange :: Maybe LineRange
  , omittedBefore :: Bool, omittedAfter :: Bool }
  deriving (Eq, Show, Generic)
instance ToJSON ReadReceipt
instance JsonSchema ReadReceipt

-- | Create-only or replace against the explicit earlier-read reference.
data WriteInput
  = Create { path :: FilePath, contents :: Text }
  | Replace { file :: File.FileReference, contents :: Text }
  deriving (Eq, Show, Generic)
instance FromJSON WriteInput
instance ToJSON WriteInput
instance JsonSchema WriteInput

data EditInput = EditInput { file :: File.FileReference, edits :: [Patch.Edit] }
  deriving (Eq, Show, Generic)
instance FromJSON EditInput
instance ToJSON EditInput
instance JsonSchema EditInput

data FileToolError
  = StorageFailure FsError
  | ReadVersionMismatch File.ContentVersion File.FileReference
  | InvalidRange Text
  | PatchRejected Patch.PatchError
  deriving (Eq, Show, Generic)
instance ToJSON FileToolError
instance JsonSchema FileToolError

data PreviewReceipt = PreviewReceipt
  { file :: File.FileReference, replacements :: [Patch.Edit], preview :: Text }
  deriving (Eq, Show, Generic)
instance ToJSON PreviewReceipt
instance JsonSchema PreviewReceipt

-- | A range is an excerpt of the whole-file version. Expected versions refuse
-- drift; this operation never retrieves a different historical body silently.
readFileTool :: Member FsRead effects => ReadInput -> Eff effects (Either FileToolError ReadReceipt)
readFileTool (ReadInput target expected selection) = do
  observed <- File.readSnapshot target
  pure $ case observed of
    Left err -> Left (StorageFailure err)
    Right snapshot ->
      let reference = File.snapshotReference snapshot
          body = File.snapshotText snapshot
      in case expected of
        Just version | version /= File.snapshotVersion snapshot -> Left (ReadVersionMismatch version reference)
        _ -> case selection of
          Nothing -> Right (ReadReceipt reference body Nothing False False)
          Just selected@(LineRange first last') ->
            case Navigation.retainedWindow (File.contentVersionText (File.snapshotVersion snapshot)) first last' body of
              Left err -> Left (InvalidRange (renderRangeError err))
              Right slice -> Right (ReadReceipt reference (Navigation.sliceText slice) (Just selected)
                (Navigation.retainedBefore slice) (Navigation.retainedAfter slice))
  where
    renderRangeError Navigation.InvalidLineWindow = "Use a positive 1-based inclusive line range"
    renderRangeError Navigation.LineOutsideCapture = "Line range is outside the captured file"
    renderRangeError Navigation.EmptyNeedle = "Invalid line selection"

writeFileTool :: Member FsWrite effects => WriteInput -> Eff effects (Either FsError File.CommitOutcome)
writeFileTool (Create target body) = File.createFile target body
writeFileTool (Replace reference body) = File.replaceFile reference body

previewEditTool :: Member FsRead effects => EditInput -> Eff effects (Either FileToolError PreviewReceipt)
previewEditTool (EditInput reference edits') = do
  prepared <- Patch.prepare reference edits'
  pure $ case prepared of
    Left err -> Left (PatchRejected err)
    Right proposal -> Right (PreviewReceipt (Patch.preparedReference proposal) edits' (Patch.preview proposal))

editFileTool :: (Member FsRead effects, Member FsWrite effects)
  => EditInput -> Eff effects (Either FileToolError File.CommitOutcome)
editFileTool (EditInput reference edits') = do
  prepared <- Patch.prepare reference edits'
  case prepared of
    Left err -> pure (Left (PatchRejected err))
    Right proposal -> do
      committed <- Patch.commitReceipt proposal
      pure $ case committed of Left err -> Left (StorageFailure err); Right receipt -> Right receipt
