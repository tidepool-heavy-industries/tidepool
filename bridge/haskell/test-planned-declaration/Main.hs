module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (bracket, IOException, try)
import Control.Monad (forM_, unless)
import Data.List (isInfixOf, isPrefixOf, nub)
import Data.ByteString qualified as BS
import Crypto.Hash.SHA256 qualified as SHA256
import Numeric (showHex)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import System.Directory
  ( createDirectory, createDirectoryIfMissing, getTemporaryDirectory
  , removeDirectoryRecursive, removeFile )
import System.FilePath ((</>), takeDirectory)
import System.IO (hClose, openTempFile)
import GHC
  ( runGhc, setSession, setTargets, guessTarget, depanal, mgModSummaries
  , ms_mod_name, parseModule, typecheckModule, getSession, tm_internals_, ParsedModule )
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Driver.Env (HscEnv)
import GHC.Builtin.Types (intTy, boolTy)
import GHC.Types.Name (nameModule_maybe)
import GHC.Types.Name.Occurrence (mkVarOcc, mkTcOcc, mkDataOcc, mkRecFieldOcc)
import GHC.Data.FastString (mkFastString)
import GHC.Types.Name.Reader (RdrName(..), mkRdrUnqual, globalRdrEnvElts, greName, greRdrNames)
import GHC.Unit.Module (moduleName, mkModuleName)
import GHC.Unit.Module.ModIface (mi_iface_hash, mi_final_exts)
import GHC.Utils.Fingerprint (Fingerprint(..))
import GHC.Types.SourceError (SourceError)
import GHC.Unit.Module (moduleNameString)
import Control.Monad.IO.Class (liftIO)
import Tidepool.Binders
  ( CellSourcePlan(..), CellAnalysisItem(..), StmtBinders(..), TurnKind(..)
  , analyzeCell, renderCellCheckSource )
import Tidepool.CheckedCell (CheckedSignature(..), CheckedSignatureName(..))
import Tidepool.DeclarationJoin (InstanceInventory(..), DeclarationExport(..), ExportIdentity(..))
import Tidepool.GhcPipeline
  ( CompilePurpose(..), PipelineSelection(..), pprPipelineResult, prHscEnv, prTargetRdrEnv
  , CheckedEnvironmentResult(..), runPipelineSessionSelected, cellCheckedBinderSignatures
  , cellGeneratedInstanceRecipe )
import Tidepool.CheckedPrefixImports
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.Identity (stableVarId)
import Tidepool.PlannedDeclaration
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.Session
  ( Generation(..), SessionModule(..), SessionModuleKind(..), sessionModuleString
  , parseSessionModule, isReservedSessionModuleName
  , mkThinSessionIface, writeSessionIface, injectSessionIface, sessionHiPath, sessionBinderName, registerSessionInterfaceLocation )

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test-planned-declaration"
  [ testCase "reserved session module identity" checkSessionModuleNames
  , testCase "certified original declaration lifecycle" scenario
  ]

scenario :: IO ()
scenario = withScratch $ \work -> do
  authored <- readFile "test-planned-declaration/fixtures/cell.hs"
  wrapper <- readFile "test-planned-declaration/fixtures/decl-wrapper.hs"
  checkWrapper <- readFile "test-planned-declaration/fixtures/check-wrapper.hs"
  initial <- analyzeCell checkWrapper authored >>= either (fail . show) pure
  let originalName = sessionModuleString (SessionModule LibMod (Generation 7))
  planned <- either (fail . show) pure (preparePlannedDeclaration originalName wrapper initial)
  let originalFile = work </> "Tidepool/Session/Lib/G7.hs"
      checkFile = work </> "PlannedCheck.hs"
  createDirectoryIfMissing True (work </> "Tidepool/Session/Lib")
  retainedFamily <- readFile "test-planned-declaration/fixtures/RetainedFamily.hs"
  writeFile (work </> "RetainedFamily.hs") retainedFamily
  writeFile originalFile (plannedSource planned)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
    CertifyHomeProductsCompile Nothing originalFile [work] Nothing
  certified <- certifyPlannedDeclaration planned (prHscEnv (pprPipelineResult original))
    >>= either fail pure
  hydrated <- hydratePlannedDeclarationInventory (plannedOriginalOwner certified)
    (plannedInterfaceFingerprint certified) (prHscEnv (pprPipelineResult original))
    >>= either fail pure
  unless (hydrated == certified && plannedSourceMatches planned certified
      && plannedSourceMatches planned hydrated) $
    fail "exact hydrated original changed its declaration inventory"
  forM_ [(plannedOriginalOwner certified, "stale-interface")
    ,((fst (plannedOriginalOwner certified), "Tidepool.Session.Lib.G8")
      ,plannedInterfaceFingerprint certified)
    ,(("foreign-unit", originalName), plannedInterfaceFingerprint certified)] $ \(owner, fingerprint) -> do
      refusedHydration <- hydratePlannedDeclarationInventory owner fingerprint
        (prHscEnv (pprPipelineResult original))
      unless (case refusedHydration of Left _ -> True; Right _ -> False) $
        fail "stale or absent original interface admitted hydrated declaration inventory"
  unless (snd (plannedOriginalOwner certified) == originalName
      && not (null (plannedInterfaceFingerprint certified))
      && all (`elem` plannedFamilyClosure certified) (inventoryFamilies (plannedInstances certified))
      && any ((== "RetainedFamily") . exportModule) (plannedFamilyClosure certified)
      && "\"record_parent\":" `isInfixOf` renderPlannedDeclarationInventory certified
      && "\"family_closure\":" `isInfixOf` renderPlannedDeclarationInventory certified
      && "\"selected_axioms\":" `isInfixOf` renderPlannedDeclarationInventory certified) $
    fail "planned original receipt omitted compiler inventory or exact owner evidence"
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
  late <- analyzeCell checkWrapper (authored ++ "\nother = 99\n") >>= either (fail . show) pure
  unless (case preparePlannedDeclaration originalName wrapper late of
      Left UnsupportedDeclarationOrder -> True; _ -> False) $
    fail "narrow original-declaration operation admitted a declaration after execution"
  -- Keep the source-inventory refusal inside this operation's leading group.
  changed <- analyzeCell checkWrapper (unlines
      [if line == "let value = Box 42" then "other = 99\n" ++ line else line
      | line <- lines authored]) >>= either (fail . show) pure
  stale <- either (fail . show) pure (preparePlannedDeclaration originalName wrapper changed)
  unless (not (plannedSourceMatches stale certified)) $
    fail "same-owner inventory admitted a different planned source"
  refused <- certifyPlannedDeclaration stale (prHscEnv (pprPipelineResult original))
  unless (case refused of Left _ -> True; Right _ -> False) $
    fail "old original interface certified a changed declaration source"
  unless (case preparePlannedDeclaration
      (sessionModuleString (SessionModule ValMod (Generation 7))) wrapper initial of
        Left _ -> True; Right _ -> False) $
    fail "value-module reservation admitted declaration ownership"
  automatic <- readFile "test-planned-declaration/fixtures/automatic.hs"
  automaticPlan <- analyzeCell checkWrapper automatic >>= either (fail . show) pure
  automaticOriginal <- either (fail . show) pure (preparePlannedDeclaration originalName wrapper automaticPlan)
  unless ("Generic" `isInfixOf` plannedSource automaticOriginal
      && "displayTree" `isInfixOf` plannedSource automaticOriginal
      && null (cellPlanGenericDeclarations (plannedCheckPlan automaticOriginal))
      && null (cellPlanStructuralDisplayTargets (plannedCheckPlan automaticOriginal))) $
    fail "generated helpers did not move with the original declaration group"
  imported <- readFile "test-planned-declaration/fixtures/imported-cell.hs"
  importedCheckWrapper <- readFile "test-planned-declaration/fixtures/import-check-wrapper.hs"
  importedDeclWrapper <- readFile "test-planned-declaration/fixtures/import-decl-wrapper.hs"
  importedPlan <- analyzeCell importedCheckWrapper imported >>= either (fail . show) pure
  importedOriginal <- either (fail . show) pure (preparePlannedDeclaration originalName importedDeclWrapper importedPlan)
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
  forbiddenOriginal <- either (fail . show) pure (preparePlannedDeclaration originalName importedDeclWrapper forbidden)
  forbiddenSource <- either fail pure (renderCellCheckSource importedCheckWrapper (plannedCheckPlan forbiddenOriginal))
  forM_ ["import Foreign (Box(..), Remaining(..), ForeignRecord(..), (<+>))", "import Foreign hiding (hidden)"] $ \selection -> do
    writeFile checkFile (replaceForeignImport selection forbiddenSource)
    refusedQualification <- try (checkImports checkFile importedEnv importedInventory Nothing)
      :: IO (Either SourceError [CheckedSignature])
    unless (case refusedQualification of Left _ -> True; Right _ -> False) $
      fail "qualified clone widened the original import selection"
  checkCompletedValueRefinement work importedEnv importedInventory
  duplicateFields <- readFile "test-planned-declaration/fixtures/DuplicateFields.hs"
  writeFile (work </> "DuplicateFields.hs") duplicateFields
  fieldCell <- readFile "test-planned-declaration/fixtures/field-cell.hs"
  fieldCheckWrapper <- readFile "test-planned-declaration/fixtures/field-check-wrapper.hs"
  fieldPlan <- analyzeCell fieldCheckWrapper fieldCell >>= either (fail . show) pure
  fieldOriginal <- either (fail . show) pure (preparePlannedDeclaration originalName wrapper fieldPlan)
  writeFile originalFile (plannedSource fieldOriginal)
  fieldProducts <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
    CertifyHomeProductsCompile Nothing originalFile [work] Nothing
  let fieldEnv = prHscEnv (pprPipelineResult fieldProducts)
  fieldInventory <- certifyPlannedDeclaration fieldOriginal fieldEnv >>= either fail pure
  fieldCheck <- either fail pure (renderCellCheckSource fieldCheckWrapper (plannedCheckPlan fieldOriginal))
  writeFile checkFile fieldCheck
  _ <- checkImports checkFile fieldEnv fieldInventory Nothing
  checkOriginalDeclarationShadow work
  putStrLn "planned original declarations: source identity, inventories, import shadowing and declaration/bind/expression check passed"

-- Exercise the original's pre-renamer seam, before its inventory can exist.
-- Both target hooks resolve an ordinary source predecessor. Exact retained
-- inventory refinement is exercised separately by checkImports; this fixture
-- checks parser-owned shadowing and qualified Names before that inventory exists.
checkOriginalDeclarationShadow :: FilePath -> IO ()
checkOriginalDeclarationShadow work = do
  let directory = work </> "original-shadow"
      previousName = sessionModuleString (SessionModule LibMod (Generation 6))
      originalName = sessionModuleString (SessionModule LibMod (Generation 10))
      previousFile = directory </> "Tidepool/Session/Lib/G6.hs"
      originalFile = directory </> "Tidepool/Session/Lib/G10.hs"
  createDirectoryIfMissing True (takeDirectory previousFile)
  previousSource <- readFile "test-planned-declaration/fixtures/shadow-previous.hs"
  foreignSource <- readFile "test-planned-declaration/fixtures/Foreign.hs"
  writeFile previousFile previousSource
  writeFile (directory </> "Foreign.hs") foreignSource
  authored <- readFile "test-planned-declaration/fixtures/shadow-cell.hs"
  wrapper <- readFile "test-planned-declaration/fixtures/shadow-decl-wrapper.hs"
  plan <- analyzeCell wrapper authored >>= either (fail . show) pure
  original <- either (fail . show) pure (preparePlannedDeclaration originalName wrapper plan)
  unless (not (null (cellPlanGenericDeclarations plan))
      && not (null (cellPlanStructuralDisplayTargets plan))
      && "import qualified Tidepool.Inspection.Display as " `isInfixOf` plannedSource original
      && ".Generic" `isInfixOf` plannedSource original
      && ".Display" `isInfixOf` plannedSource original) $
    fail "original shadow fixture lost genuine generated Generic or Display companions"
  writeFile originalFile (plannedSource original)
  checked <- runPipelineSessionSelected CheckedEnvironment Set.empty
    (GeneratedInstanceCheck (cellGeneratedInstanceRecipe plan) OriginalDeclarationCompile)
    Nothing originalFile [directory,"lib"] Nothing
  assertShadowNames originalName previousName (crTargetRdrEnv checked)
  prepared <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
    (ProgramItemCompile True [] [] []) Nothing originalFile [directory,"lib"] Nothing
  _ <- certifyPlannedDeclaration original (prHscEnv (pprPipelineResult prepared)) >>= either fail pure
  assertShadowNames originalName previousName (prTargetRdrEnv (pprPipelineResult prepared))
  where
    assertShadowNames originalName previousName reader = do
      let owners spelling = nub [moduleNameString (moduleName owner)
            | entry <- globalRdrEnvElts reader, spelling `elem` greRdrNames entry
            , Just owner <- [nameModule_maybe (greName entry)]]
          qualified owner occurrence = Qual (mkModuleName owner) occurrence
      forM_ [mkTcOcc "Input", mkTcOcc "Tagged", mkVarOcc "make", mkVarOcc "project", mkVarOcc "tag"
        , mkDataOcc "Record", mkTcOcc "ConstructorOnly", mkTcOcc "Maybe", mkTcOcc "Box"
        , mkTcOcc "Text", mkTcOcc "Double", mkTcOcc "Int"] $ \occurrence ->
          unless (owners (mkRdrUnqual occurrence) == [originalName]) $
            fail "parsed original declaration did not replace its unqualified imported occurrence"
      unless (owners (qualified previousName (mkTcOcc "Input")) == [previousName]
          && owners (qualified previousName (mkVarOcc "project")) == [previousName]
          && owners (qualified "Selected" (mkTcOcc "Box")) == ["Foreign"]
          && owners (qualified "OriginalText" (mkTcOcc "Text")) == ["Data.Text.Internal"]
          && owners (mkRdrUnqual (mkDataOcc "ConstructorOnly")) == [previousName]
          && owners (mkRdrUnqual (mkRecFieldOcc (mkFastString "Record") "field")) == [originalName]
          && owners (mkRdrUnqual (mkRecFieldOcc (mkFastString "OtherRecord") "field")) == [previousName]) $
        fail "parsed original shadowing changed a qualified owner, namespace, or unrelated record parent"

checkCompletedValueRefinement :: FilePath -> HscEnv -> PlannedDeclarationInventory -> IO ()
checkCompletedValueRefinement work baseEnv original = do
  let owner = SessionModule ValMod (Generation 9)
      dependencyOwner = SessionModule ValMod (Generation 8)
      ownerName = sessionModuleString owner
      occurrence = mkVarOcc "id"
      binderIdentity = stableVarId (sessionBinderName baseEnv owner occurrence)
  thin <- mkThinSessionIface baseEnv owner [(occurrence, intTy), (mkVarOcc "other", intTy)]
  unless (mi_iface_hash (mi_final_exts thin) == Fingerprint 0 0) $
    fail "thin value fixture does not exercise the zero fingerprint case"
  writeSessionIface baseEnv work owner thin
  dependencyThin <- mkThinSessionIface baseEnv dependencyOwner [(mkVarOcc "older", intTy)]
  writeSessionIface baseEnv work dependencyOwner dependencyThin
  previousEnv <- injectSessionIface work owner baseEnv
  bytes <- BS.readFile (sessionHiPath work owner)
  dependencyBytes <- BS.readFile (sessionHiPath work dependencyOwner)
  let digestOf input = concatMap (\byte -> let digits = showHex byte ""
        in replicate (2 - length digits) '0' ++ digits) (BS.unpack (SHA256.hash input))
      digest = digestOf bytes
      requested = CompletedValueImport (fst (plannedOriginalOwner original)) ownerName
        (sessionHiPath work owner) digest [("id", binderIdentity)]
      dependency = ExactIfaceArtifact (fst (plannedOriginalOwner original))
        (sessionModuleString dependencyOwner) (sessionHiPath work dependencyOwner)
        (digestOf dependencyBytes) []
      target = work </> "PrefixCheck.hs"
  (hydrated, completed) <- hydrateCompletedValueImportsWithDependencies [dependency]
    [requested] previousEnv >>= either fail pure
  source <- readFile "test-planned-declaration/fixtures/prefix-check.hs"
  writeFile target source
  checkCompletedImports target hydrated original completed
  forM_ [requested {completedValueIfaceSha256 = replicate 64 '0'}
    ,requested {completedValueBinders = [("id", 0)]}
    ,requested {completedValueModule = "Tidepool.Session.Val.G10"}
    ,requested {completedValueBinders = [("absent", binderIdentity)]}] $ \invalid -> do
      refused <- hydrateCompletedValueImports [invalid] previousEnv
      unless (case refused of Left _ -> True; Right _ -> False) $
        fail "changed bytes, owner or native binder admitted completed value imports"
  duplicated <- hydrateCompletedValueImports [requested, requested] previousEnv
  unless (case duplicated of Left _ -> True; Right _ -> False) $
    fail "duplicate completed value winners were admitted"
  forM_ [[dependency {exactSha256 = replicate 64 '0'}]
    , [dependency, dependency]
    , [ExactIfaceArtifact (completedValueUnit requested) ownerName
        (completedValueIfacePath requested) digest []]] $ \invalidDependencies -> do
      refused <- hydrateCompletedValueImportsWithDependencies invalidDependencies [requested] previousEnv
      unless (case refused of Left _ -> True; Right _ -> False) $
        fail "changed or duplicate dependency bytes admitted completed value hydration"
  -- Identical owner, bytes and fingerprint0 still do not authorize the HPT
  -- allocation from an earlier injection or a separately reconstructed read.
  (recreated, recreatedCompleted) <- hydrateCompletedValueImports [requested] hydrated >>= either fail pure
  checkCompletedImports target recreated original recreatedCompleted
  poisonedThin <- mkThinSessionIface baseEnv owner [(occurrence, boolTy), (mkVarOcc "other", intTy)]
  unless (mi_iface_hash (mi_final_exts poisonedThin) == Fingerprint 0 0) $
    fail "changed thin value interface ceased to exercise the zero fingerprint case"
  writeSessionIface baseEnv work owner poisonedThin
  poisonedEnv <- injectSessionIface work owner baseEnv
  BS.writeFile (sessionHiPath work owner) bytes
  -- A previous same-owner, same-zero-hash allocation has different native
  -- details. The captured Int bytes must replace it before issuing this cap.
  (repaired, repairedCompleted) <- hydrateCompletedValueImportsWithDependencies [dependency]
    [requested] poisonedEnv >>= either fail pure
  checkCompletedImports target repaired original repairedCompleted
  forM_ [previousEnv, recreated, poisonedEnv] $ \stale -> do
    refused <- try (checkCompletedImports target stale original completed) :: IO (Either IOException ())
    unless (case refused of Left _ -> True; Right _ -> False) $
      fail "stale zero-fingerprint HPT accepted an unrelated readback allocation"
  writeFile target (unlines [if line == "import Tidepool.Session.Val.G9 (id)"
    then "import Tidepool.Session.Val.G9 (id, other)" else line | line <- lines source])
  widened <- try (checkCompletedImports target hydrated original completed) :: IO (Either IOException ())
  unless (case widened of Left _ -> True; Right _ -> False) $
    fail "completed value import widened beyond its certified prefix winners"

checkCompletedImports
  :: FilePath -> HscEnv -> PlannedDeclarationInventory -> CompletedValueImports -> IO ()
checkCompletedImports file env original completed = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    setSession env
    target <- guessTarget file Nothing Nothing
    setTargets [target]
    graph <- depanal [mkModuleName "Tidepool.Session.Val.G8", mkModuleName "Tidepool.Session.Val.G9"] False
    summary <- case [item | item <- mgModSummaries graph, moduleNameString (ms_mod_name item) == "PrefixCheck"] of
      [item] -> pure item
      _ -> fail "completed-prefix checking module absent"
    parsed <- parseModule summary
    current <- getSession
    -- Downsweep can replace finder state; register the retained source-less
    -- location in the consuming request, as the session pipeline does.
    liftIO (registerSessionInterfaceLocation (sessionHiPath (takeDirectory file)
      (SessionModule ValMod (Generation 9))) (SessionModule ValMod (Generation 9)) current)
    liftIO (registerSessionInterfaceLocation (sessionHiPath (takeDirectory file)
      (SessionModule ValMod (Generation 8))) (SessionModule ValMod (Generation 8)) current)
    transformed <- liftIO (transformPlannedDeclarationImportsWithCompleted original completed current parsed)
    typed <- typecheckModule transformed
    let (tcg, _) = tm_internals_ typed
        owners spelling = nub [moduleNameString (moduleName owner)
          | entry <- globalRdrEnvElts (tcg_rdr_env tcg)
          , spelling `elem` greRdrNames entry
          , Just owner <- [nameModule_maybe (greName entry)]]
    unless (owners (mkRdrUnqual (mkVarOcc "id")) == ["Tidepool.Session.Val.G9"]
        && owners (Qual (mkModuleName "Tidepool.Session.Lib.G7") (mkVarOcc "id")) == ["Tidepool.Session.Lib.G7"]
        && owners (Qual (mkModuleName "Tidepool.Session.Val.G8") (mkVarOcc "older")) == ["Tidepool.Session.Val.G8"]
        && owners (Qual (mkModuleName "Foreign") (mkVarOcc "hidden")) == ["Foreign"]) $
      fail "completed value refinement lost native replacement or preserved qualification"

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

-- Keep the generation boundary matched with the runtime identifier owner.
checkSessionModuleNames :: IO ()
checkSessionModuleNames = do
  forM_ [LibMod, ValMod] $ \kind ->
    forM_ [0, 1, maxBound] $ \generation -> do
      let owner = SessionModule kind (Generation generation)
      unless (parseSessionModule (sessionModuleString owner) == Just owner) $
        fail "session module identity did not roundtrip"
  unless (parseSessionModule "Tidepool.Session.Lib.G0001"
      == Just (SessionModule LibMod (Generation 1))) $
    fail "leading-zero generation did not normalize"
  forM_ ["Tidepool.Session.Lib.G", "Tidepool.Session.Lib.G-1"
    , "Tidepool.Session.Lib.G+1", "Tidepool.Session.Lib.G1.tail"
    , "Tidepool.Session.Lib.G18446744073709551616", "Tidepool.Session.Lib.G١"
    , "Tidepool.Session.Other.G1"] $ \name ->
      unless (parseSessionModule name == Nothing && isReservedSessionModuleName name) $
        fail "invalid reserved module acquired a session identity or escaped the namespace"
  forM_ ["Other.Lib.G1", "Tidepool.Sessionish.Lib.G1"] $ \name ->
    unless (parseSessionModule name == Nothing && not (isReservedSessionModuleName name)) $
      fail "ordinary module was classified as a session module"
