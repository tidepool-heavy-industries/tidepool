{-# LANGUAGE ScopedTypeVariables #-}

module WorkerDiagnosticsTest (runWorkerDiagnosticsTests) where

import Control.Exception
  ( AsyncException(..), ErrorCall, Exception(..), IOException, SomeException
  , asyncExceptionFromException, asyncExceptionToException, bracket, evaluate
  , finally, fromException, throwIO, toException, try )
import Control.Monad (forM_, unless)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import GHC.Types.Error (emptyMessages)
import GHC.Types.SourceError (mkSrcErr)
import System.Directory
  ( createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile )
import System.Exit (ExitCode(..))
import System.FilePath ((</>))
import System.IO
  ( hClose, hFlush, openTempFile, readFile', stderr, stdout )
import Tidepool.Binders (CellSourceSpan(..), CellSplitError(..))
import Tidepool.DiagJson
  ( DependencyLoadFailure(..), Diag(..), DiagSeverity(..), InputRejection(..)
  , ReportOutcome(..), SourceRejection(..), diagFromException, renderDiagsJson )
import Tidepool.ExtractUtil (trySynchronous)
import Tidepool.WorkerDiagnostics

-- Cancellation can be wrapped by an application-specific exception type.
data WrappedCancellation = WrappedCancellation deriving (Eq, Show)
instance Exception WrappedCancellation where
  toException = asyncExceptionToException
  fromException = asyncExceptionFromException

runWorkerDiagnosticsTests :: IO ()
runWorkerDiagnosticsTests = withScratch $ \root -> do
  successful <- trySynchronous (pure (7 :: Int))
  unless (either (const False) (== 7) successful) (fail "successful action changed")
  ioFailure <- trySynchronous (readFile' (root </> "absent"))
  case ioFailure of
    Left failure | Just (_ :: IOException) <- fromException failure -> pure ()
    _ -> fail "real file error was not retained as a synchronous failure"
  callFailure <- trySynchronous (evaluate (error "diagnostic test" :: Int))
  case callFailure of
    Left failure | Just (_ :: ErrorCall) <- fromException failure -> pure ()
    _ -> fail "ErrorCall was not retained as a synchronous failure"
  forM_ [ThreadKilled, UserInterrupt] $ \original -> do
    escaped <- try (trySynchronous (throwIO original :: IO ()))
      :: IO (Either AsyncException (Either SomeException ()))
    unless (either (== original) (const False) escaped)
      (fail "standard asynchronous exception was swallowed or changed")
  wrapped <- try (trySynchronous (throwIO WrappedCancellation :: IO ()))
    :: IO (Either WrappedCancellation (Either SomeException ()))
  unless (either (== WrappedCancellation) (const False) wrapped)
    (fail "custom asynchronous exception was swallowed or changed")

  let diagnostic = Diag (Just ("Authored.hs", 4, 2, 4, 9)) DiagError "bad source"
      warning = Diag Nothing DiagWarning "warning with \"quotes\"\nand newline"
      sourceError = mkSrcErr emptyMessages
      cases =
        [ (toException InvalidCellPlanRequest, ReportInputRejected,
            [Diag Nothing DiagError "InvalidCellPlanRequest"], "Error: InvalidCellPlanRequest\n")
        , (toException sourceError, ReportSourceFailure, [],
            "Compilation failed.\n" ++ show sourceError ++ "\n")
        , (toException (DependencySourceFailure [diagnostic]), ReportSourceFailure,
            [diagnostic], "Error: " ++ show (DependencySourceFailure [diagnostic]) ++ "\n")
        , (toException DependencyWorkerFailure, ReportWorkerFailure,
            [diagFromException (toException DependencyWorkerFailure)],
            "Error: DependencyWorkerFailure\n")
        , (toException (SourceRejection "rejected"), ReportSourceFailure,
            [Diag Nothing DiagError "rejected"], "Error: SourceRejection \"rejected\"\n")
        , (toException (LocatedCellRejection (CellSourceSpan 2 3 5 6) "cell"), ReportSourceFailure,
            [Diag (Just ("<cell>", 2, 3, 5, 6)) DiagError "cell"],
            "Error: " ++ show (LocatedCellRejection (CellSourceSpan 2 3 5 6) "cell") ++ "\n")
        , (toException (userError "ordinary worker error"), ReportWorkerFailure,
            [diagFromException (toException (userError "ordinary worker error"))],
            "Error: " ++ show (userError "ordinary worker error") ++ "\n")
        ]
  forM_ cases $ \(failure, outcome, diagnostics, human) -> do
    (code, machine, debug) <- captureChannels root (reportDiags (Left failure))
    unless (code == ExitFailure 1 && machine == renderDiagsJson outcome diagnostics ++ "\n"
        && debug == human) (fail ("failure diagnostic channels changed: " ++ show failure))
  (code, machine, debug) <- captureChannels root (reportDiagsWithWarnings (Right [warning]))
  unless (code == ExitSuccess && machine == renderDiagsJson ReportSuccess [warning] ++ "\n"
      && null debug) (fail "warning success diagnostic channels changed")

  unless (sourceFailureDiagnostics (toException (DependencySourceFailure [diagnostic])) == Just [diagnostic]
      && sourceFailureDiagnostics (toException sourceError) == Just []
      && all ((== Nothing) . sourceFailureDiagnostics)
        [toException DependencyWorkerFailure, toException InvalidCellPlanRequest,
         toException (SourceRejection "contract"), toException ThreadKilled])
    (fail "inspection fallback admitted a non-GHC source failure")
  unless (renderInspectionDiagnostics [diagnostic, warning]
      == "Authored.hs:4:2: bad source\nwarning with \"quotes\"\nand newline")
    (fail "inspection diagnostics lost locations or order")

  forM_ [ (CellLexFailure, Just (1, 1, 1, 1), "GHC could not lex the notebook cell")
        , (CellStatementParseFailure (CellSourceSpan 2 3 4 5), Just (2, 3, 4, 5),
            "GHC could not parse the notebook statement list")
        , (CellPrologueFailure (CellSourceSpan 2 3 4 5) "prologue", Just (2, 3, 4, 5), "prologue")
        , (CellDanglingOperatorFailure (CellSourceSpan 2 3 4 5) "+", Just (2, 3, 4, 5),
            "cell ends with a dangling operator `+`: remove it or supply its right operand")
        , (CellUnsupportedLocalFixity (CellSourceSpan 2 3 4 5), Just (2, 3, 4, 5),
            "local fixity cannot cross prepared item boundaries; put the operator and its fixity in an authored declaration group")
        , (CellHeaderFailure "header", Nothing, "cell check template header: header")
        ] $ \(splitError, location, message) -> do
    failure <- trySynchronous (throwCellSplitError splitError :: IO ())
    case (failure, location) of
      (Left exception, Just expected) -> case fromException exception of
        Just (LocatedCellRejection (CellSourceSpan sl sc el ec) actual)
          | (sl, sc, el, ec) == expected && actual == message -> pure ()
        _ -> fail "cell split rejection lost span or explanation"
      (Left exception, Nothing) | show exception == "user error (" ++ message ++ ")" -> pure ()
      _ -> fail "cell split rejection changed failure category"
  putStrLn "worker diagnostics: synchronous failures, cancellation, classification, spans, warnings and channels passed"

withScratch :: (FilePath -> IO a) -> IO a
withScratch = bracket acquire removeDirectoryRecursive
  where
    acquire = do
      tmp <- getTemporaryDirectory
      (path, handle) <- openTempFile tmp "worker-diagnostics-test"
      hClose handle
      removeFile path
      createDirectory path
      pure path

-- The test runner is serial. Brackets restore both process-global handles even
-- when an assertion or reporting operation fails; writable captures close
-- before GHC opens the same paths for reading.
captureChannels :: FilePath -> IO a -> IO (a, String, String)
captureChannels root action =
  bracket (hDuplicate stdout) hClose $ \savedOut ->
  bracket (hDuplicate stderr) hClose $ \savedErr ->
  bracket (openTempFile root "stdout") (hClose . snd) $ \(outPath, outHandle) ->
  bracket (openTempFile root "stderr") (hClose . snd) $ \(errPath, errHandle) -> do
    result <- (do
      hDuplicateTo outHandle stdout
      hDuplicateTo errHandle stderr
      action) `finally`
        (hFlush stdout `finally`
          (hFlush stderr `finally`
            (hDuplicateTo savedOut stdout `finally` hDuplicateTo savedErr stderr)))
    hClose outHandle
    hClose errHandle
    out <- readFile' outPath
    err <- readFile' errPath
    pure (result, out, err)
