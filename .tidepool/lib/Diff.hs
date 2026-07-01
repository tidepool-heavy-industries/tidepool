{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
-- | Effectful diff verbs over the pure "Tidepool.Patch" core: read the
-- current files, run the pure engine, and (for 'apply'\/'rollback') write
-- back ALL-OR-NOTHING — a single conflict anywhere blocks every write, so the
-- working tree never lands in a half-applied state. Conflicts come back as
-- DATA (a JSON value), not an effect error, mirroring @writeChecked@.
--
-- The @[patch|…|]@ quoter's name is @patch@; this module does NOT export a
-- @patch@ verb (it would shadow the quoter). Big diffs ride the eval @input@
-- field as text and enter via 'applyDiff'\/'planDiff'.
module Diff
  ( plan
  , apply
  , planDiff
  , applyDiff
  , rollback
  , diffFiles
  , genPatchTo
  , DiffOutcome (..)
  , FileApplied (..)
  , DiffFilesOutcome (..)
  , module Tidepool.Patch
  ) where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Tidepool.Patch

-- ---------------------------------------------------------------------------
-- Outcome types — document the shapes the verbs surface, ToJSON reproduces the
-- exact wire JSON the opaque-Value versions used to emit.
-- ---------------------------------------------------------------------------

-- | One file's result in a successful 'apply'.
data FileApplied = FileApplied
  { faFile  :: Text
  , faHunks :: Int
  , faLines :: [Int]
  , faDrift :: [Int]
  } deriving (Eq, Show)

instance ToJSON FileApplied where
  toJSON fa = object
    [ "file"  .= faFile fa
    , "hunks" .= faHunks fa
    , "lines" .= faLines fa
    , "drift" .= faDrift fa ]

-- | Outcome of an atomic 'apply' (also 'applyDiff'\/'rollback'): every file
-- applied (with per-file reports), or blocked by conflicts (nothing written).
data DiffOutcome
  = DiffApplied [FileApplied]
  | DiffConflicts [Conflict]
  deriving (Eq, Show)

instance ToJSON DiffOutcome where
  toJSON (DiffApplied fs)   = object [ "applied" .= True, "files" .= map toJSON fs ]
  toJSON (DiffConflicts cs) = object [ "applied" .= False, "conflicts" .= map toJSON cs ]

-- | Outcome of 'diffFiles': identical content, or a rendered diff with stats.
data DiffFilesOutcome
  = DiffFilesUnchanged Text                    -- ^ path
  | DiffFilesChanged Text Text Int Int Int     -- ^ path, patch, hunks, oldLines, newLines
  deriving (Eq, Show)

instance ToJSON DiffFilesOutcome where
  toJSON (DiffFilesUnchanged p) = object [ "path" .= p, "changed" .= False ]
  toJSON (DiffFilesChanged p patch hunks oldL newL) = object
    [ "path"    .= p
    , "changed" .= True
    , "patch"   .= patch
    , "stats"   .= object
        [ "hunks"    .= hunks
        , "oldLines" .= oldL
        , "newLines" .= newL ] ]

-- | Dry run: read each target file (absent files are 'Nothing') and report
-- only the conflicts the pure engine would raise — no writes.
plan :: Patch -> M [Conflict]
plan p = concatMapM planFile p
  where
    planFile fp = do
      mc <- readTarget fp
      case applyFilePatch fp mc of
        Left cs -> pure cs
        Right _ -> pure []

-- | Apply atomically: read every target, compute every result, and write only
-- when there are zero conflicts across the whole patch. The result is data:
-- @{"applied":true,"files":[…]}@ or @{"applied":false,"conflicts":[…]}@.
apply :: Patch -> M DiffOutcome
apply p = do
  results <- mapM (\fp -> readTarget fp >>= \mc -> pure (fp, applyFilePatch fp mc)) p
  let conflicts = concatMap conflictsOf results
  if null conflicts
    then do
      mapM_ writeResult results
      pure (DiffApplied (map fileReport results))
    else pure (DiffConflicts conflicts)
  where
    conflictsOf (_, Left cs) = cs
    conflictsOf (_, Right _) = []
    writeResult (fp, Right (out, _)) = writeFile (fpPath fp) out
    writeResult (_, Left _)          = pure ()           -- unreachable: zero-conflict guard
    fileReport (fp, Right (_, hrs)) = FileApplied (fpPath fp) (length hrs) (map hrLine hrs) (map hrDrift hrs)
    fileReport (fp, Left _)          = FileApplied (fpPath fp) 0 [] []   -- unreachable

-- | 'plan' from raw diff text (the input lane); a parse error is loud.
planDiff :: Text -> M [Conflict]
planDiff t = case parsePatch t of
  Left e  -> error e
  Right p -> plan p

-- | 'apply' from raw diff text (the input lane); a parse error is loud.
applyDiff :: Text -> M DiffOutcome
applyDiff t = case parsePatch t of
  Left e  -> error e
  Right p -> apply p

-- | Undo a previously-applied patch by applying its inverse (creation patches
-- cannot be inverted — deletion is unsupported).
rollback :: Patch -> M DiffOutcome
rollback p = case invertPatch p of
  Left e   -> error e
  Right ip -> apply ip

-- | Read a file's current content, or 'Nothing' if it does not exist.
readTarget :: FilePatch -> M (Maybe Text)
readTarget fp = do
  exists <- doesFileExist (fpPath fp)
  if exists then fmap Just (readFile (fpPath fp)) else pure Nothing

-- | Diff two existing files (old → new) and return the rendered unified diff
-- plus summary stats as DATA. Reads both paths; the patch is labelled with the
-- OLD path (the side it applies onto), so applying the returned diff to @oldP@
-- makes it match @newP@'s current content. Identical content reports
-- @{"path":…,"changed":false}@.
diffFiles :: Text -> Text -> M DiffFilesOutcome
diffFiles oldP newP = do
  oldC <- readFile oldP
  newC <- readFile newP
  case genPatch oldP oldC newC of
    Left _   -> pure (DiffFilesUnchanged oldP)
    Right fp -> pure (DiffFilesChanged
      oldP
      (renderPatch [fp])
      (length (fpHunks fp))
      (sum (map hunkOldLen (fpHunks fp)))
      (sum (map hunkNewLen (fpHunks fp))))

-- | Read the current content of @path@ and produce the unified diff that turns
-- it into @newContent@ — the \"I have the new content, give me the
-- reviewable\/appliable diff\" verb. An absent file yields a @--- \/dev\/null@
-- creation patch; identical content yields the empty 'Text' (nothing to apply).
genPatchTo :: Text -> Text -> M Text
genPatchTo path newContent = do
  exists <- doesFileExist path
  if exists
    then do
      old <- readFile path
      case genPatch path old newContent of
        Left _   -> pure ""
        Right fp -> pure (renderPatch [fp])
    else pure (renderPatch [creationPatch path newContent])
