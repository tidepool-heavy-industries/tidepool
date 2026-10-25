module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup, requiredInput)
import Test.Tasty (withResource)
import SourceBootCases
import CompilerRequestTest (workerRequestAdmissionBeforeEffects, nativeReceiptIdentityChecks, nativePackageOutputChecks, packageTargetRootDriftRefusal, nativeOutputProvenanceChecks, completedRequestHistories, completedRequestCancellation, completedRequestRelease, completedRequestOwnerBoundaries, completedRequestScopeRelease, checkedRequestOwnerBoundaries, compilerScopeCleanupFailure)
import CompilerRequestTest (checkedPublicModuleHistories, worldAcquisitionDiagnostics, worldRequestSlotBoundaries, worldRequestBoundaries, worldMaterializationBoundaries)
import BoundedReadTest (boundedReadChecks)
import RequestInputsTest (diagnosticCounterCapture, ownedArenaRangeReads, artifactByteOwnership, requestInputHistories, requestInputBoundaries, retainedCompilationPublication, fixtureIssuerCountingHistories, ownedScopeInputReuse, capturedCanonicalPromotionRefusesReadableMissingInputs, validationTimingControl, compilerTimingRoundtrip, compilerTaskAttemptHistories, freshOutputSealSetLaws, worldCellAdmissionParse, worldResidencyBoundaries, worldRecoveryCustody, worldRejectionDiagnosticBoundaries)
import RequestInputsTest (compilerServiceBootBoundaries, worldCanonicalOriginalReuse, worldCarriedImportTypes)
import RequestInputsTest (bufferedImportSyntaxBoundaries, bufferedImportRefinementLineage)
import RequestInputsTest (worldValueTypeHistories)
import SourceBootFixtureSupport (withTiming, withScratch)
import CandidateExecutionSourcesTest (candidateExecutionSourcesTest)
import ExecutionSourceDecodeTest (executionSourceDecodeChecks, executionSourceResolutionBudgetChecks)
import FinalizedCoreTest (finalizedCoreChecks, postloadProviderFrontendOnce, memoIngressSelectionHistory, snapshotExecutableHook, unownedUpToDateDependency, mixedCandidateLoads, canonicalLoadGraphHistories, agentPageDisplayFinalizationHistory)
import ProgressBoundaryTest (progressBoundaryChecks, watchReplyEvidenceChecks, watchReplyWarmAuthorityChecks)
import PhysicalExecutableEpochTest (exactInterfaceOwnerReuse, physicalExecutableEpoch, sourceSummaryContinuation)

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "source-boot"
  [ testCase "worker request admission precedes source scratch executor and RTS effects" workerRequestAdmissionBeforeEffects
  , testCase "compiler receipt symbol parents use null or text" nativeReceiptIdentityChecks
  , testCase "finite native output separates package products with captured interfaces" nativePackageOutputChecks
  , testCase "target package roots are unchanged after stage issuance" packageTargetRootDriftRefusal
  , testCase "native output requires admitted provenance and portable stays explicit" nativeOutputProvenanceChecks
  , testCase "completed compiler request histories" completedRequestHistories
  , testCase "compiler service boot admission boundaries" compilerServiceBootBoundaries
  , testCase "public module checking guards transitive source execution" checkedPublicModuleHistories
  , testCase "world request diagnostics bound UTF-8 and preserve cancellation" worldAcquisitionDiagnostics
  , testCase "world request slots preserve Lib and Val identity" worldRequestSlotBoundaries
  , testCase "world requests require rooted boot hold and fresh executor" worldRequestBoundaries
  , testCase "unissued world materialization preserves failure and cleanup" worldMaterializationBoundaries
  , testCase "completed compiler cancellation preserves facts and fences effects" completedRequestCancellation
  , testCase "closed executor refuses completed cached preparation" completedRequestRelease
  , testCase "completed compilation owners refuse crossed contexts before writing" completedRequestOwnerBoundaries
  , testCase "released compiler scope refuses every prepared operation" completedRequestScopeRelease
  , testCase "checked compilation preserves its issuer through reuse and release" checkedRequestOwnerBoundaries
  , testCase "compiler recovery cleanup failure preserves causes and poisons later scopes" compilerScopeCleanupFailure
  , testCase "diagnostic counter capture separates complete evidence from bounded display" diagnosticCounterCapture
  , testCase "owned arena range reads" ownedArenaRangeReads
  , testCase "artifact byte ownership" artifactByteOwnership
  , testCase "request input custody histories" requestInputHistories
  , testCase "owned scope input reuse" ownedScopeInputReuse
  , testCase "World retained value TYPE closure histories" worldValueTypeHistories
  , testCase "captured canonical promotion refuses readable missing inputs" capturedCanonicalPromotionRefusesReadableMissingInputs
  , testCase "fixture issuer counting histories" fixtureIssuerCountingHistories
  , testCase "retained compilation publication" retainedCompilationPublication
  , testCase "validation timing outcomes" validationTimingControl
  , testCase "compiler timing producer consumer roundtrip" compilerTimingRoundtrip
  , testCase "compiler task attempt histories" compilerTaskAttemptHistories
  , testCase "in-memory GHC import syntax correspondence" bufferedImportSyntaxBoundaries
  , testCase "actual import refinement preserves parent lineage" bufferedImportRefinementLineage
  , testCase "fresh output seal set laws" freshOutputSealSetLaws
  , testCase "request input admission boundaries" requestInputBoundaries
  , testCase "World residency uses shared bytes across END and eviction" worldResidencyBoundaries
  , testCase "failed compiler recovery retains exact World backing custody" worldRecoveryCustody
  , testCase "world cell admission parses one recipe" worldCellAdmissionParse
  , testCase "World interface-only canonical body survives source relocation and cold reuse" worldCanonicalOriginalReuse
  , testCase "World carried imports keep exact types and global Names without source" worldCarriedImportTypes
  , testCase "bounded World rejection diagnostics" worldRejectionDiagnosticBoundaries
  , testCase "bounded artifact reads" boundedReadChecks
  , testCase "resident source summary continuation" sourceSummaryContinuation
  , testCase "physical native executable epoch" physicalExecutableEpoch
  , testCase "exact interface compiler owner reuse" exactInterfaceOwnerReuse
  , testCase "snapshot executable hook" snapshotExecutableHook
  , testCase "mixed candidate load kinds" mixedCandidateLoads
  , testCase "canonical load graph histories" canonicalLoadGraphHistories
  , testCase "Agent PageDisplay finalization history" agentPageDisplayFinalizationHistory
  , testCase "unowned up-to-date dependency" unownedUpToDateDependency
  , testCase "finalized Core" $ finalizedCoreChecks
  , testCase "finalized frontend once" $ finalizedFrontendOnce
  , testCase "post-load provider frontend once" postloadProviderFrontendOnce
  , testCase "memo ingress selection history" memoIngressSelectionHistory
  , testCase "execution source decode" $ executionSourceDecodeChecks
  , testCase "execution source resolution budget" $ executionSourceResolutionBudgetChecks
  , testCase "exact scope binders" $ exactScopeBinders
  , testCase "original constructor metadata closure" $ withEffects originalConstructorMetadataClosure
  , testCase "original package projection" $ originalPackageProjection
  , testCase "original product projection" $ originalProjectionProducts
  , testCase "native graph scheduling equality" nativeGraphSchedulingEquality
  , testCase "candidate compact inventory" $ candidateCompactInventory
  , testCase "candidate GHC load" $ candidateGhcLoad
  , testCase "boot refusal failure isolation" bootRefusalFailureIsolation
  , testCase "SOURCE boot reuse" $ withTiming (withScratch sourceBootReuseAt)
  , testCase "candidate request site siblings" $ candidateRequestSitedSiblings
  , testCase "canonical current source" $ canonicalCurrentSource
  , testCase "canonical mixed source adjacency" canonicalMixedSourceAdjacency
  , testCase "canonical source obligations" $ canonicalSourceObligations
  , testCase "generated scaffold imports" $ generatedScaffoldImports
  , testCase "activation preview original orphan scope" $ activationPreviewOriginalOrphanScope
  , testCase "hydrated site siblings" $ hydratedSiteSiblings
  , testCase "fresh execution recipe" $ freshExecutionRecipeTest
  , testCase "candidate graph descriptors" $ withTiming (withScratch candidateGraphDescriptorsAt)
  , testCase "native checked signatures" $ nativeCheckedSignaturesTest
  , testCase "original home thin interface" originalHomeThinInterfaceTest
  , testCase "package-only thin interface" packageOnlyThinInterfaceTest
  , testCase "wired-in package thin interface" wiredInPackageThinInterfaceTest
  , testCase "host activation purpose" $ hostActivationPurposeTest Nothing
  , testCase "candidate execution sources" $ candidateExecutionSourcesTest
  , testCase "retained exact quoter" $ exactRetainedQuoter
  , testCase "retained execution publication" $ retainedExecutionPublication
  , testCase "retained execution TH counter" $ retainedExecutionThCounter
  , testCase "exact reexport quoter" $ exactReexportQuoter
  , withResource acquireExecutionInstanceFixture releaseExecutionInstanceFixture $ \fixture ->
      testGroup "exact instance execution"
        [ testCase "sealed and qualified controls" $ exactExecutionSealedInstance fixture
        , testCase "transitively imported orphan" $ exactExecutionTransitiveInstance fixture
        , testCase "unrelated provider isolation and explicit import" $ exactExecutionUnrelatedInstance fixture
        , testCase "class parent import and hiding" $ exactExecutionClassInstance fixture
        ]
  , testCase "exact checked value execution" $ exactExecutionValues
  , testCase "exact to ordinary transition" $ exactToOrdinary
  , testCase "checked value imports" $ checkedValueImports
  , testCase "loaded exact metadata" $ exactLoadedMetadata
  , testCase "exact transaction reuse" $ exactTransactionReuse
  , testCase "exact transaction cancellation" $ exactTransactionCancellation
  , testCase "package finder history isolation" packageFinderHistoryIsolation
  , testCase "compiler boot state survives END without HOME publication" compilerBootStateIsolation
  , testCase "lazy SOURCE backedge follows the current target after END" lazySourceBackedge
  , testCase "lazy original home package instances survive context forks" lazyHomePackageInstances
  , testCase "exact transaction candidate reuse" $ exactTransactionCandidateReuse
  , testCase "exact legacy value isolation" $ exactLegacyValueIsolation
  , testCase "quasiquote codegen transition" $ quasiQuoteCodegenTransition
  , testCase "package inputs" $ withTiming packageInputs
  , testCase "session native body demand" sessionNativeBodyDemand
  , testCase "selected home instance edges" $ selectedHomeInstanceEdges
  , testCase "ordered resolution paths" $ resolutionPaths
  , testCase "request resolution search order history" resolutionSearchOrderHistory
  , testCase "original package cohort" $ withEffects $ \effects -> withScratch (originalPackageCohort effects)
  , testCase "checked value type closure" $ withEffects checkedValueTypeClosure
  , testCase "progress boundary" $ withEffects progressBoundaryChecks
  , testCase "watch reply evidence" $ withEffects $ \effects -> withScratch (watchReplyEvidenceChecks effects)
  , testCase "watch reply warm authority" $ withEffects $ \effects -> withScratch (watchReplyWarmAuthorityChecks effects)
  , testCase "exact bash metadata" $ withEffects exactBashMetadata
  , testCase "mixed independent 1" (withTiming (mixedGraph False 1))
  , testCase "mixed independent 10" (withTiming (mixedGraph False 10))
  , testCase "mixed independent 100" (withTiming (mixedGraph False 100))
  , testCase "mixed required independent 10" (withTiming (mixedGraph True 10))
  ]

withEffects :: (FilePath -> IO a) -> IO a
withEffects action = requiredInput "TIDEPOOL_TEST_EFFECTS_DIR" >>= action
