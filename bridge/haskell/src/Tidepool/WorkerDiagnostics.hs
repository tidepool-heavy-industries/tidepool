{-# LANGUAGE ScopedTypeVariables #-}

-- | Request failure classification and the worker's diagnostic channels.
module Tidepool.WorkerDiagnostics
  ( LocatedCellRejection(..)
  , throwCellSplitError
  , sourceFailureDiagnostics
  , renderInspectionDiagnostics
  , CaptureAssessment(..), renderCompletion
  ) where

import Control.Exception (Exception, SomeException, fromException, throwIO)
import Data.List (intercalate)
import GHC.Types.SourceError (SourceError)
import System.Exit (ExitCode(..))
import Tidepool.RequestCompletion
  ( Completion, BodyOutcome(..), completionResult, completionFailures, completionAsync
  , cleanupStage, cleanupException )
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


-- Capture rejection is independent of body failure and release uncertainty.
-- A bounded diagnostic refusal with confirmed release may reuse the worker.
data CaptureAssessment = CaptureComplete | CaptureBudgetRejected String | CaptureUnavailable

-- Both direct CLI and framed transport call this after their owning releases.
-- This function produces one report; it never writes a terminal channel.
renderCompletion :: Completion [Diag] -> CaptureAssessment -> IO (ExitCode, String, String)
renderCompletion completed capture = do
  case completionAsync completed of
    Just failure -> throwIO failure
    Nothing -> pure ()
  let (bodyOutcome, primary, human, failureCode) = case completionResult completed of
        BodySucceeded warnings -> (ReportSuccess, warnings, "", ExitSuccess)
        BodyFailed failure ->
          let (outcome, diagnostics, debug, code) = failureDiagnostics failure
          in (outcome, diagnostics, debug, code)
        BodyAbortedByCleanup -> (ReportWorkerFailure, [], "", ExitFailure 1)
      releaseDiagnostics =
        [Diag Nothing DiagError ("compiler cleanup " ++ show (cleanupStage failure)
          ++ ": " ++ show (cleanupException failure))
        | failure <- completionFailures completed]
      captureDiagnostics = case capture of
        CaptureComplete -> []
        CaptureBudgetRejected message -> [Diag Nothing DiagError message]
        CaptureUnavailable -> [Diag Nothing DiagError "compiler output capture unavailable"]
      additional = releaseDiagnostics ++ captureDiagnostics
      outcome = if null additional then bodyOutcome else ReportWorkerFailure
      code = if null additional then failureCode else ExitFailure 1
      debug = human ++ concatMap ((++ "\n") . dMessage) additional
  pure (code, renderDiagsJson outcome (primary ++ additional) ++ "\n", debug)

failureDiagnostics :: SomeException -> (ReportOutcome, [Diag], String, ExitCode)
failureDiagnostics e =
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
      human = case fromException e of
        Just (se :: SourceError) -> "Compilation failed.\n" ++ show se ++ "\n"
        Nothing -> "Error: " ++ show e ++ "\n"
      code = case fromException e of
        Just (InvalidWorkerRequest _) -> ExitFailure 2
        _ -> ExitFailure 1
  in (outcome, diags, human, code)
