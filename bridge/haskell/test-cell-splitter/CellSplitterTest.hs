module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup, requiredInput)
import CellSplitterCases
import CellProgramStateTest (cellProgramStateChecks)
import CheckedAdmissionTest (checkedAdmissionChecks)
import HarnessSourceTest (harnessSourceChecks)
import InspectionRunnerTest (inspectionRunnerChecks)
import WorkerDiagnosticsTest (runWorkerDiagnosticsTests)
import QuasiQuoteOccurrencesTest (quasiQuoteOccurrenceChecks)
import TypedSegmentCases
  ( typedSegmentNativePreparation, typedSegmentRecordMetadataProperty
  , typedSegmentRewriteSemantics
  )
import TypedSessionCases (typedSessionHydrationPublicationChecks, typedSessionPrefixProperties)
import UnreachableCompileTimeTest (unreachableCompileTimeCompilation)
import Tidepool.ExtractUtil (getLibdir)
import GHC (runGhc, getSessionDynFlags)
import GHC.Driver.Config.Parser (initParserOpts)
import GHC.Driver.Session (DynFlags, parseDynamicFilePragma)
import GHC.Parser.Header (getOptions)
import GHC.Data.StringBuffer (stringToStringBuffer)
import Control.Monad.IO.Class (liftIO)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "cell-splitter"
  [ testCase "compilerLifecycleCompilation" compilerLifecycleCompilation
  , testCase "memoLifecycleCompilation" memoLifecycleCompilation
  , testCase "metadataCompilation" metadataCompilation
  , testCase "preparedSessionLeafCompilation" preparedSessionLeafCompilation
  , testCase "quasiQuoteOccurrenceChecks" quasiQuoteOccurrenceChecks
  , testCase "quasiQuoteSourceReuseCompilation" quasiQuoteSourceReuseCompilation
  , testCase "pinnedQuasiQuoteSourceCompilation" pinnedQuasiQuoteSourceCompilation
  , testCase "untrackedCompileTimeCompilation" untrackedCompileTimeCompilation
  , testCase "generatedScaffoldIdentityChecks" generatedScaffoldIdentityChecks
  , testCase "checkedAdmissionChecks" checkedAdmissionChecks
  , testCase "cellProgramStateChecks" cellProgramStateChecks
  , testCase "certificationRequestValidation" certificationRequestValidation
  , testCase "checkingSourceRequestRoundTrip" checkingSourceRequestRoundTrip
  , testCase "requestShapeValidation" requestShapeValidation
  , testCase "requestFieldOrdering" requestFieldOrdering
  , testCase "dependencyQualifierChecks" dependencyQualifierChecks
  , testCase "harnessSourceChecks" harnessSourceChecks
  , testCase "inspectionRunnerChecks" inspectionRunnerChecks
  , testCase "runWorkerDiagnosticsTests" runWorkerDiagnosticsTests
  , testCase "orderedInferenceSegments" orderedInferenceSegments
  , testCase "compilerDefaultRecipeChecks" compilerDefaultRecipeChecks
  , testCase "programOriginalImportsCompilation" programOriginalImportsCompilation
  , testCase "functionValueInterfaceCompilation" functionValueInterfaceCompilation
  , testCase "sigmaValueInterfaceCompilation" sigmaValueInterfaceCompilation
  , testCase "constructorEvidenceClassification" constructorEvidenceClassification
  , testCase "typedSegmentNativePreparation" typedSegmentNativePreparation
  , testCase "typedSegmentRecordMetadataProperty" typedSegmentRecordMetadataProperty
  , testCase "typedSegmentRewriteSemantics" typedSegmentRewriteSemantics
  , testCase "typedSessionHydrationPublicationChecks" typedSessionHydrationPublicationChecks
  , typedSessionPrefixProperties
  , testCase "sessionValueFinalizedDependency" sessionValueFinalizedDependency
  , testCase "sessionFixitiesCompilation" sessionFixitiesCompilation
  , testCase "unreachableCompileTimeCompilation" unreachableCompileTimeCompilation
  , testCase "mixedInspectionCompilation" mixedInspectionCompilation
  , testCase "checkedLoadBoundaryCompilation" checkedLoadBoundaryCompilation
  , testCase "dependencyEvidenceCompilation" dependencyEvidenceCompilation
  , testCase "validationMemoCompilation" validationMemoCompilation
  , testCase "pathInsensitiveWitnessCompilation" pathInsensitiveWitnessCompilation
  , testCase "requestOwnedParserDefaults" requestOwnedParserDefaults
  , testCase "interfaceMeasurementDiagnostics" interfaceMeasurementDiagnostics
  , testCase "multilineLetCompilation" multilineLetCompilation
  , testCase "ambiguousOccurrenceHintCompilation" ambiguousOccurrenceHintCompilation
  , testCase "ordinary structural display" $
      requiredInput "TIDEPOOL_TEST_EFFECTS_DIR" >>= structuralDisplayCompilation OrdinaryDisplayTest
  , testCase "explicit Generic derivation recovery" explicitGenericDerivationRecovery
  , testCase "qualified and standalone Generic recovery" qualifiedAndStandaloneGenericRecovery
  , testCase "authored Generic conflicts remain errors" authoredGenericConflictsRemainErrors
  , testCase "legacy display scope refusal" $
      requiredInput "TIDEPOOL_TEST_EFFECTS_DIR" >>= structuralDisplayCompilation LegacyDisplayScopeTest
  , testGroup "parser" [testCase "lexical islands" $ withParserFlags $ \_flags lexicalFlags -> lexicalIslands lexicalFlags
      , testCase "semicolon statement refinement" $ withParserFlags $ \flags _lexicalFlags -> semicolonStatementRefinement flags
      , testCase "comments pragmas and layout" $ withParserFlags $ \_flags lexicalFlags -> commentsPragmasAndLayout lexicalFlags
      , testCase "declarations form one cell item" $ withParserFlags $ \flags _lexicalFlags -> declarationsBecomeOneCellItem flags
      , testCase "prologue plans" $ withParserFlags $ \flags _lexicalFlags -> prologuePlans flags
      , testCase "automatic generic plans" $ withParserFlags $ \flags _lexicalFlags -> automaticGenericPlans flags
      , testCase "standalone deriving without a declaration" $ withParserFlags $ \flags _lexicalFlags -> noStandaloneDerivingLeavesCellUntouched flags
      , testCase "dangling operator cells" $ withParserFlags $ \flags _lexicalFlags -> danglingOperatorCells flags
      , testCase "multiline let placement" $ withParserFlags $ \flags _lexicalFlags -> multilineLetPlacement flags
      ]
  ]

withParserFlags :: (DynFlags -> DynFlags -> IO a) -> IO a
withParserFlags action = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    let (_, lexicalOptions) = getOptions (initParserOpts flags)
          (stringToStringBuffer
            "{-# LANGUAGE QuasiQuotes, MultilineStrings, LambdaCase #-}\n")
          "<cell-test>"
    (lexicalFlags, _, _) <- parseDynamicFilePragma flags lexicalOptions
    liftIO (action flags lexicalFlags)
