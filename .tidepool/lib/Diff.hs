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
  , module Tidepool.Patch
  ) where

import Tidepool.Prelude hiding (error)
import Tidepool.Effects
import Tidepool.Patch

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
apply :: Patch -> M Value
apply p = do
  results <- mapM (\fp -> readTarget fp >>= \mc -> pure (fp, applyFilePatch fp mc)) p
  let conflicts = concatMap conflictsOf results
  if null conflicts
    then do
      mapM_ writeResult results
      pure (object [ "applied" .= True, "files" .= map fileReport results ])
    else pure (object [ "applied" .= False, "conflicts" .= conflicts ])
  where
    conflictsOf (_, Left cs) = cs
    conflictsOf (_, Right _) = []
    writeResult (fp, Right (out, _)) = writeFile (fpPath fp) out
    writeResult (_, Left _)          = pure ()           -- unreachable: zero-conflict guard
    fileReport (fp, Right (_, hrs)) = object
      [ "file"  .= fpPath fp
      , "hunks" .= length hrs
      , "lines" .= map hrLine hrs
      , "drift" .= map hrDrift hrs ]
    fileReport (fp, Left _) = object [ "file" .= fpPath fp ]   -- unreachable

-- | 'plan' from raw diff text (the input lane); a parse error is loud.
planDiff :: Text -> M [Conflict]
planDiff t = case parsePatch t of
  Left e  -> error e
  Right p -> plan p

-- | 'apply' from raw diff text (the input lane); a parse error is loud.
applyDiff :: Text -> M Value
applyDiff t = case parsePatch t of
  Left e  -> error e
  Right p -> apply p

-- | Undo a previously-applied patch by applying its inverse (creation patches
-- cannot be inverted — deletion is unsupported).
rollback :: Patch -> M Value
rollback p = case invertPatch p of
  Left e   -> error e
  Right ip -> apply ip

-- | Read a file's current content, or 'Nothing' if it does not exist.
readTarget :: FilePatch -> M (Maybe Text)
readTarget fp = do
  exists <- doesFileExist (fpPath fp)
  if exists then fmap Just (readFile (fpPath fp)) else pure Nothing
