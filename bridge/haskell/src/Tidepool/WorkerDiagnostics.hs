{-# LANGUAGE ScopedTypeVariables #-}

-- | Request failure classification and the worker's diagnostic channels.
module Tidepool.WorkerDiagnostics
  ( LocatedCellRejection(..)
  , throwCellSplitError
  , sourceFailureDiagnostics
  , renderInspectionDiagnostics
  , reportDiags
  , reportDiagsWithWarnings
  ) where

import Control.Exception (Exception, SomeException, fromException, throwIO)
import Data.List (intercalate)
import GHC.Types.SourceError (SourceError)
import System.Exit (ExitCode(..))
import System.IO (hPutStrLn, stderr)
import Tidepool.FamilyConsistency (FamilyConsistencyRejection, renderFamilyConsistencyRejection)
import Tidepool.Binders (CellSplitError(..), CellSourceSpan(..))
import Tidepool.DiagJson
  ( ReportOutcome(..), DiagSeverity(..), Diag(..), SourceRejection(..)
  , InputRejection(..), DependencyLoadFailure(..), diagsFromSourceError
  , diagFromException, renderDiagsJson )

data LocatedCellRejection = LocatedCellRejection CellSourceSpan String
  deriving Show
instance Exception LocatedCellRejection

throwCellSplitError :: CellSplitError -> IO a
throwCellSplitError errorValue = case errorValue of
  CellPrologueFailure sourceSpan message ->
    throwIO (LocatedCellRejection sourceSpan message)
  CellLexFailure ->
    throwIO (LocatedCellRejection (CellSourceSpan 1 1 1 1)
      "GHC could not lex the notebook cell")
  CellStatementParseFailure sourceSpan ->
    throwIO (LocatedCellRejection sourceSpan
      "GHC could not parse the notebook statement list")
  CellDanglingOperatorFailure sourceSpan operatorText ->
    throwIO (LocatedCellRejection sourceSpan
      ("cell ends with a dangling operator `" ++ operatorText
        ++ "`: remove it or supply its right operand"))
  CellUnsupportedLocalFixity sourceSpan ->
    throwIO (LocatedCellRejection sourceSpan
      "local fixity cannot cross prepared item boundaries; put the operator and its fixity in an authored declaration group")
  CellHeaderFailure message -> fail ("cell check template header: " ++ message)

-- Both direct checking and GHC dependency loading retain real source
-- diagnostics. Only this structural distinction permits a source alternative;
-- input, protocol and worker failures never choose another template.
sourceFailureDiagnostics :: SomeException -> Maybe [Diag]
sourceFailureDiagnostics exception = case fromException exception of
  Just (sourceError :: SourceError) -> Just (diagsFromSourceError sourceError)
  Nothing -> case fromException exception of
    Just (DependencySourceFailure diagnostics) -> Just diagnostics
    _ -> Nothing

renderInspectionDiagnostics :: [Diag] -> String
renderInspectionDiagnostics = intercalate "\n" . map render
  where
    render diagnostic = location diagnostic ++ dMessage diagnostic
    location diagnostic = case dFile diagnostic of
      Just (file, line, column, _, _) -> file ++ ":" ++ show line ++ ":" ++ show column ++ ": "
      Nothing -> ""


-- | Emit one JSON report on stdout and a human-readable failure on stderr.
-- The caller owns process exit; failures return exit code 1.
reportDiags :: Either SomeException () -> IO ExitCode
reportDiags = reportDiagsWithWarnings . fmap (const [])

reportDiagsWithWarnings :: Either SomeException [Diag] -> IO ExitCode
reportDiagsWithWarnings (Left e) = do
  let (outcome, diags) = case fromException e of
        Just (rejection :: InputRejection) -> (ReportInputRejected, [Diag Nothing DiagError (show rejection)])
        Nothing -> case fromException e of
          Just (se :: SourceError) -> (ReportSourceFailure, diagsFromSourceError se)
          Nothing -> case fromException e of
            Just (DependencySourceFailure diagnostics) -> (ReportSourceFailure, diagnostics)
            Just DependencyWorkerFailure -> (ReportWorkerFailure, [diagFromException e])
            Nothing -> case fromException e of
              Just (SourceRejection message) ->
                (ReportSourceFailure, [Diag Nothing DiagError message])
              Nothing -> case fromException e of
                Just (LocatedCellRejection (CellSourceSpan sl sc el ec) message) ->
                  (ReportSourceFailure,
                    [Diag (Just ("<cell>", sl, sc, el, ec)) DiagError message])
                Nothing -> case fromException e of
                  Just (rejection :: FamilyConsistencyRejection) ->
                    (ReportSourceFailure, [Diag Nothing DiagError (renderFamilyConsistencyRejection rejection)])
                  Nothing -> (ReportWorkerFailure, [diagFromException e])
  putStrLn (renderDiagsJson outcome diags)
  -- Debug copy for humans only; stdout (above) is the authoritative machine
  -- contract.
  case fromException e of
    Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
    Nothing -> hPutStrLn stderr $ "Error: " ++ show e
  pure (ExitFailure 1)
reportDiagsWithWarnings (Right warnings) =
  putStrLn (renderDiagsJson ReportSuccess warnings) >> pure ExitSuccess
