module Main (main) where

import Control.Exception (bracket, IOException, try)
import Control.Monad (forM_, unless)
import Data.List (isInfixOf, isPrefixOf)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import System.Directory
  ( createDirectory, createDirectoryIfMissing, getTemporaryDirectory
  , removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import GHC
  ( runGhc, setSession, setTargets, guessTarget, depanal, mgModSummaries
  , ms_mod_name, parseModule, typecheckModule, getSession, tm_internals_, ParsedModule )
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Driver.Env (HscEnv)
import GHC.Types.SourceError (SourceError)
import GHC.Unit.Module (moduleNameString)
import Control.Monad.IO.Class (liftIO)
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
import Tidepool.ExtractUtil (getLibdir)
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
  imported <- readFile "test-planned-declaration/fixtures/imported-cell.hs"
  importedCheckWrapper <- readFile "test-planned-declaration/fixtures/import-check-wrapper.hs"
  importedDeclWrapper <- readFile "test-planned-declaration/fixtures/import-decl-wrapper.hs"
  importedPlan <- analyzeCell importedCheckWrapper imported >>= either (fail . show) pure
  importedOriginal <- either fail pure (preparePlannedDeclaration originalName importedDeclWrapper importedPlan)
  foreignSource <- readFile "test-planned-declaration/fixtures/Foreign.hs"
  writeFile (work </> "Foreign.hs") foreignSource
  writeFile originalFile (plannedSource importedOriginal)
  importedProducts <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
    CertifyHomeProductsCompile Nothing originalFile [work] Nothing
  let importedEnv = prHscEnv (pprPipelineResult importedProducts)
  importedInventory <- certifyPlannedDeclaration importedOriginal importedEnv >>= either fail pure
  importedCheck <- either fail pure (renderCellCheckSource importedCheckWrapper (plannedCheckPlan importedOriginal))
  writeFile checkFile importedCheck
  importedSignatures <- checkImports checkFile importedEnv importedInventory (Just certified)
  let hasOriginal occurrence = any (any (\name -> signatureModule name == originalName
        && signatureOccurrence name == occurrence) . signatureNames) importedSignatures
  unless (hasOriginal "Box" && any (any ((== "Foreign") . signatureModule) . signatureNames) importedSignatures) $
    fail "import refinement changed original or qualified imported type identities"
  forM_ ["import Foreign", "import Foreign hiding (hidden)"] $ \replacement -> do
    writeFile checkFile (replaceForeignImport replacement importedCheck)
    _ <- checkImports checkFile importedEnv importedInventory Nothing
    pure ()
  forbidden <- analyzeCell importedCheckWrapper (imported ++ "\nForeign.hidden\n") >>= either (fail . show) pure
  forbiddenOriginal <- either fail pure (preparePlannedDeclaration originalName importedDeclWrapper forbidden)
  forbiddenSource <- either fail pure (renderCellCheckSource importedCheckWrapper (plannedCheckPlan forbiddenOriginal))
  forM_ ["import Foreign (Box(..), Remaining(..), ForeignRecord(..), (<+>))", "import Foreign hiding (hidden)"] $ \selection -> do
    writeFile checkFile (replaceForeignImport selection forbiddenSource)
    refusedQualification <- try (checkImports checkFile importedEnv importedInventory Nothing)
      :: IO (Either SourceError [CheckedSignature])
    unless (case refusedQualification of Left _ -> True; Right _ -> False) $
      fail "qualified clone widened the original import selection"
  duplicateFields <- readFile "test-planned-declaration/fixtures/DuplicateFields.hs"
  writeFile (work </> "DuplicateFields.hs") duplicateFields
  fieldCell <- readFile "test-planned-declaration/fixtures/field-cell.hs"
  fieldCheckWrapper <- readFile "test-planned-declaration/fixtures/field-check-wrapper.hs"
  fieldPlan <- analyzeCell fieldCheckWrapper fieldCell >>= either (fail . show) pure
  fieldOriginal <- either fail pure (preparePlannedDeclaration originalName wrapper fieldPlan)
  writeFile originalFile (plannedSource fieldOriginal)
  fieldProducts <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
    CertifyHomeProductsCompile Nothing originalFile [work] Nothing
  let fieldEnv = prHscEnv (pprPipelineResult fieldProducts)
  fieldInventory <- certifyPlannedDeclaration fieldOriginal fieldEnv >>= either fail pure
  fieldCheck <- either fail pure (renderCellCheckSource fieldCheckWrapper (plannedCheckPlan fieldOriginal))
  writeFile checkFile fieldCheck
  _ <- checkImports checkFile fieldEnv fieldInventory Nothing
  putStrLn "planned original declarations: source identity, inventories, import shadowing and declaration/bind/expression check passed"

replaceForeignImport :: String -> String -> String
replaceForeignImport replacement = unlines . map replace . lines
  where
    replace line | "import Foreign " `isPrefixOf` line = replacement
                 | otherwise = line

checkImports
  :: FilePath -> HscEnv -> PlannedDeclarationInventory
  -> Maybe PlannedDeclarationInventory -> IO [CheckedSignature]
checkImports checkFile importedEnv importedInventory staleInventory = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    setSession importedEnv
    target <- guessTarget checkFile Nothing Nothing
    setTargets [target]
    graph <- depanal [] False
    summary <- case [item | item <- mgModSummaries graph, moduleNameString (ms_mod_name item) == "PlannedCheck"] of
      [item] -> pure item
      _ -> fail "checking module summary absent"
    parsed <- parseModule summary
    transformed <- liftIO (transformPlannedDeclarationImports importedInventory importedEnv parsed)
    forM_ staleInventory $ \stale -> do
      absent <- liftIO (try (transformPlannedDeclarationImports stale importedEnv parsed) :: IO (Either IOException ParsedModule))
      unless (case absent of Left _ -> True; Right _ -> False) $
        fail "stale original inventory admitted changed interface"
    typed <- typecheckModule transformed
    env <- getSession
    let (tcg, _) = tm_internals_ typed
        checkedImport = CheckedEnvironmentResult env tcg (tcg_rdr_env tcg) Map.empty Nothing [] []
    liftIO (cellCheckedBinderSignatures checkedImport)

withScratch :: (FilePath -> IO a) -> IO a
withScratch action = bracket
  (do root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-planned-declaration"
      hClose handle
      removeFile path
      createDirectory path
      pure path)
  removeDirectoryRecursive action
