module Main (main) where

import Control.Exception (bracket)
import Control.Monad (unless)
import Data.List (isInfixOf)
import Data.Set qualified as Set
import System.Directory
  ( createDirectory, createDirectoryIfMissing, getTemporaryDirectory
  , removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.Binders
  ( CellSourcePlan(..), CellAnalysisItem(..), StmtBinders(..), TurnKind(..)
  , analyzeCell, renderCellCheckSource )
import Tidepool.CheckedCell (CheckedSignature(..), CheckedSignatureName(..))
import Tidepool.DeclarationJoin (InstanceInventory(..), DeclarationExport(..), ExportIdentity(..))
import Tidepool.GhcPipeline
  ( CompilePurpose(..), PipelineSelection(..), PreparedPipelineResult(..)
  , PipelineResult(..), CheckedEnvironmentResult(..), runPipelineSessionSelected
  , cellCheckedBinderSignatures )
import Tidepool.PlannedDeclaration
import Tidepool.Session (Generation(..), SessionModule(..), SessionModuleKind(..), sessionModuleString)

main :: IO ()
main = withScratch $ \work -> do
  authored <- readFile "test-planned-declaration/fixtures/cell.hs"
  wrapper <- readFile "test-planned-declaration/fixtures/decl-wrapper.hs"
  checkWrapper <- readFile "test-planned-declaration/fixtures/check-wrapper.hs"
  initial <- analyzeCell checkWrapper authored >>= either (fail . show) pure
  let originalName = sessionModuleString (SessionModule LibMod (Generation 7))
  planned <- either fail pure (preparePlannedDeclaration originalName wrapper initial)
  let originalFile = work </> "Tidepool/Session/Lib/G7.hs"
      checkFile = work </> "PlannedCheck.hs"
  createDirectoryIfMissing True (work </> "Tidepool/Session/Lib")
  writeFile originalFile (plannedSource planned)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
    CertifyHomeProductsCompile Nothing originalFile [work] Nothing
  certified <- certifyPlannedDeclaration planned (prHscEnv (pprPipelineResult original))
    >>= either fail pure
  unless (all ((== originalName) . exportModule . exportHead) (plannedExports certified)
      && all ((/= "__result") . exportOccurrence . exportHead) (plannedExports certified)
      && not (null (inventoryClasses (plannedInstances certified)))
      && not (null (inventoryFamilies (plannedInstances certified)))) $
    fail "original module lost authored exports or instance inventory"
  let checking = plannedCheckPlan planned
      observation item = (cellAnalysisSpan item, cellAnalysisVerdict item, cellAnalysisSourceItems item)
  unless (map observation (cellPlanItems checking) == map observation (cellPlanItems initial)
      && all (null . cellAnalysisSource)
        [item | item <- cellPlanItems checking, sbKind (cellAnalysisVerdict item) == KDecl]) $
    fail "moving declarations changed parser-owned cell observations"
  source <- either fail pure (renderCellCheckSource checkWrapper checking)
  writeFile checkFile source
  checked <- runPipelineSessionSelected CheckedEnvironment Set.empty GeneralCompile
    Nothing checkFile [work] Nothing
  signatures <- cellCheckedBinderSignatures checked
  unless (any (any (\name -> signatureModule name == originalName
      && signatureOccurrence name == "Box") . signatureNames) signatures) $
    fail "checked binding did not retain the original declaration Name"
  unless (not (null (crCheckedBinderPins checked))) $
    fail "declaration, binding and expression cell did not check"
  changed <- analyzeCell checkWrapper (authored ++ "\nother = 99\n") >>= either (fail . show) pure
  stale <- either fail pure (preparePlannedDeclaration originalName wrapper changed)
  refused <- certifyPlannedDeclaration stale (prHscEnv (pprPipelineResult original))
  unless (case refused of Left _ -> True; Right _ -> False) $
    fail "old original interface certified a changed declaration source"
  unless (case preparePlannedDeclaration
      (sessionModuleString (SessionModule ValMod (Generation 7))) wrapper initial of
        Left _ -> True; Right _ -> False) $
    fail "value-module reservation admitted declaration ownership"
  automatic <- readFile "test-planned-declaration/fixtures/automatic.hs"
  automaticPlan <- analyzeCell checkWrapper automatic >>= either (fail . show) pure
  automaticOriginal <- either fail pure (preparePlannedDeclaration originalName wrapper automaticPlan)
  unless ("Generic" `isInfixOf` plannedSource automaticOriginal
      && "displayTree" `isInfixOf` plannedSource automaticOriginal
      && null (cellPlanGenericDeclarations (plannedCheckPlan automaticOriginal))
      && null (cellPlanDisplayTargets (plannedCheckPlan automaticOriginal))) $
    fail "generated helpers did not move with the original declaration group"
  putStrLn "planned original declarations: source identity, inventories and declaration/bind/expression check passed"

withScratch :: (FilePath -> IO a) -> IO a
withScratch action = bracket
  (do root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-planned-declaration"
      hClose handle
      removeFile path
      createDirectory path
      pure path)
  removeDirectoryRecursive action
