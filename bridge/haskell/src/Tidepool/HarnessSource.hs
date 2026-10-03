module Tidepool.HarnessSource
  ( spliceHarnessProfilePragma
  , harnessProfilePragmaLine
  ) where

import Control.Exception (IOException, evaluate, try)
import Control.Monad (foldM, when)
import qualified Data.Map.Strict as Map
import Data.Maybe (fromMaybe)
import System.Directory (createDirectoryIfMissing)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, (</>))
import System.IO (IOMode(ReadMode), hGetContents, withFile)
import System.IO.Error (isDoesNotExistError)
import System.Posix.Files (FileStatus, deviceID, fileID, getFileStatus)
import Tidepool.ExtractRequest (WorkerRequest(..))

-- | Prepend the harness language profile to scratch input copies. Inspection
-- requests profile every singleton fallback and their optional batch source;
-- other modes compile only the first input. Putting the profile in source
-- keeps it visible to GHC downsweep and source-based cache keys. Caller files
-- are never modified; diagnostics are shifted by the inserted line.
spliceHarnessProfilePragma :: WorkerRequest -> IO WorkerRequest
spliceHarnessProfilePragma args = case requestFiles args of
  [] -> pure args
  (file : rest) -> do
    let outDir = fromMaybe (takeDirectory file </> takeBaseName file ++ "_cbor") (requestOutDir args)
        profileCopy directory sourcePath = do
          let scratchPath = directory </> takeFileName sourcePath
          rejectSourceAlias sourcePath scratchPath
          source <- withFile sourcePath ReadMode $ \handle -> do
            contents <- hGetContents handle
            _ <- evaluate (length contents)
            pure contents
          createDirectoryIfMissing True directory
          writeFile scratchPath (harnessProfilePragmaLine ++ "\n" ++ source)
          pure scratchPath
    if null (requestInspections args)
      then do
        scratchPath <- profileCopy outDir file
        pure args { requestFiles = scratchPath : rest }
      else do
        (_, files) <- foldM
          (\(copies, paths) (index, sourcePath) -> do
            copy <- case Map.lookup sourcePath copies of
              Just existing -> pure existing
              Nothing -> profileCopy (outDir </> "inspection-query-" ++ show index) sourcePath
            pure (Map.insert sourcePath copy copies, copy : paths))
          (Map.empty, []) (zip [0 :: Int ..] (file : rest))
        batch <- case requestInspectTypeBatch args of
          Nothing -> pure Nothing
          Just sourcePath -> Just <$> profileCopy (outDir </> "inspection-type-batch") sourcePath
        pure args
          { requestFiles = reverse files
          , requestInspectTypeBatch = batch
          }

-- | Harness language extensions. A cross-language consistency test pins this
-- to the runtime-owned canonical eval dialect.
harnessProfilePragmaLine :: String
harnessProfilePragmaLine =
  "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, KindSignatures, RankNTypes, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, StandaloneDeriving, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot, OverloadedLabels #-}"

-- Existing destination links must not turn scratch rewriting into caller mutation.
rejectSourceAlias :: FilePath -> FilePath -> IO ()
rejectSourceAlias sourcePath scratchPath = do
  source <- getFileStatus sourcePath
  destination <- try (getFileStatus scratchPath) :: IO (Either IOException FileStatus)
  case destination of
    Left exception
      | isDoesNotExistError exception -> pure ()
      | otherwise -> ioError exception
    Right status -> when (deviceID source == deviceID status && fileID source == fileID status) $
      ioError (userError "harness profile scratch path aliases caller source")
