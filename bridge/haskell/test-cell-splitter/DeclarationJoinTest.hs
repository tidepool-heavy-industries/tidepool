module Main (main, tests) where

import Control.Exception (IOException, try)
import Control.Monad (unless)
import Data.List (isInfixOf)
import Test.QuickCheck qualified as QC
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)
import DeclarationJoinCases
import Tidepool.ExecutionSchema qualified as Execution

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "declaration-join"
  [ testCase "indexed declarations match generated histories" declarationIndexProperty
  , testCase "indexed declarations match exhaustive bounded histories" declarationIndexHistories
  , testCase "current typed declaration request roundtrip" wireRoundTripChecks
  , testCase "original package roots" originalProductRootsProof
  , testCase "evaluated original package roots control" originalProductRootsControl
  , testCase "original interfaces fresh consumers and retained family conflicts" declarationJoinScenario
  ]

-- Compile the original GHC identities once, then generate and shrink the
-- selection history independently from the indexed production implementation.
declarationIndexProperty :: IO ()
declarationIndexProperty = withDeclarationIndexOracle $ \classCount familyCount compareHistory -> do
  let generator = (,,)
        <$> QC.listOf (QC.chooseInt (0, classCount - 1))
        <*> QC.sublistOf [0 .. familyCount - 1]
        <*> QC.arbitrary
      shrink (operations, families, visible) =
        [(smaller, families, visible) | smaller <- QC.shrinkList (const []) operations]
        ++ [(operations, smaller, visible) | smaller <- QC.shrinkList (const []) families]
        ++ [(operations, families, False) | visible]
      property = QC.forAllShrink generator shrink $ \(operations, families, visible) ->
        case compareHistory operations families visible of
          Left diagnostic -> QC.counterexample diagnostic False
          Right categories -> QC.checkCoverage $
            QC.cover 10 ("accepted" `elem` categories) "accepted" $
            QC.cover 5 ("ClassInstanceConflict" `elem` categories) "class conflict" $
            QC.cover 5 ("FamilyInstanceConflict" `elem` categories) "family conflict" $
            QC.cover 20 (not visible) "hidden orphan instances" $
            QC.cover 20 visible "visible orphan instances" True
  result <- QC.quickCheckWithResult QC.stdArgs { QC.maxSuccess = 256, QC.maxSize = 18 } property
  unless (QC.isSuccess result) (fail "declaration-index history property failed")

-- The evaluated-only mutant must lose the dictionary's unevaluated package
-- worker. An unexpected exception is a failure, rather than control evidence.
originalProductRootsControl :: IO ()
originalProductRootsControl = do
  result <- try (originalProductRootsProofWith (\global ->
    (Execution.globalIdentity global, Execution.globalRequiredEvaluated global)))
    :: IO (Either IOException ())
  case result of
    Left failure | "unevaluated original dictionary lost its exact package worker definition"
        `isInfixOf` show failure -> pure ()
    Left failure -> fail ("package roots control failed for another reason: " ++ show failure)
    Right () -> fail "evaluated-only package roots control unexpectedly passed"
