{-# LANGUAGE PartialTypeSignatures #-}

module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (SomeException, bracket, fromException, try)
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.List (isInfixOf, nub)
import Data.IORef (newIORef)
import Data.Map.Strict qualified as Map
import GHC
import GHC.Driver.Env (hscUpdateHPT, hsc_HPT, hsc_units)
import GHC.Driver.Env.Types (HscEnv(hsc_unit_env))
import GHC.Driver.Monad (Session(..), reflectGhc)
import GHC.Driver.Session (PackageFlag(..))
import GHC.Tc.Types (TcGblEnv, tcg_rdr_env, tcg_type_env)
import GHC.Types.TypeEnv (emptyTypeEnv, typeEnvIds)
import GHC.Types.Name (nameOccName, nameModule_maybe)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Reader (GlobalRdrEnv, globalRdrEnvElts, greName)
import GHC.Types.PkgQual (PkgQual(..))
import GHC.Types.SourceError (SourceError)
import GHC.Types.Unique.Map (addToUniqMap, listToUniqMap)
import GHC.Unit.Env (ue_setUnits)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), delFromHpt, lookupHpt, mapHpt)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Module (moduleUnit)
import GHC.Unit.Module.ModIface (mi_exports, mi_module)
import GHC.Unit.State (UnitState(..), ModuleOrigin(..), UnusableUnit(..), UnusableUnitReason(..))
import GHC.Unit.Types (stringToUnitId)
import System.Directory (createDirectory, createDirectoryIfMissing, getTemporaryDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.IO.Error (ioeGetErrorString, isUserError)
import System.Process (callProcess, readProcess)
import Tidepool.DiagJson (diagsFromSourceError)
import Tidepool.ExtractRequest (InspectionRequest (..), StructuredInspection(..), StructuredNameScope(..), StructuredNameNamespace(..), InspectionProvenance(..))
import Tidepool.Introspection

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test"
  [ testCase "compiled introspection searches" scenario,
    testCase "bounded browse dedupes before the budget" browseScenario,
    testCase "compiled scoped identifier selection" structuredScenario,
    testCase "public module absence differs from unavailable information" moduleLookupScenario,
    testCase "hidden package lookup preserves source diagnostics" hiddenPackageScenario,
    testCase "UnitState provider faults do not establish module absence" providerFaultScenario,
    testCase "advertised names require information after exact selection" advertisedNameScenario
  ]

-- These fixtures use the production inspection entrypoint. HPT and UnitState
-- mutations below are explicit fault injection; ordinary absence, uncompiled
-- source, hidden packages and empty exports use real compiler inputs.
withQueryFixture :: (FilePath -> HscEnv -> TcGblEnv -> IO ()) -> IO ()
withQueryFixture action = bracket temporary removeDirectoryRecursive $ \root -> do
  writeFile (root </> "QueryHonestyGood.hs")
    "module QueryHonestyGood where\nsharedName :: Int\nsharedName = 1\n"
  writeFile (root </> "QueryHonestyBroken.hs")
    "module QueryHonestyBroken where\nsharedName :: Int\nsharedName = 2\n"
  writeFile (root </> "EmptyExports.hs") "module EmptyExports () where\nhidden = True\n"
  writeFile (root </> "UncompiledQueryModule.hs")
    "module UncompiledQueryModule where\nuncompiled = True\n"
  let source = root </> "QueryHonestyHost.hs"
  writeFile source (unlines
    [ "module QueryHonestyHost (localValue) where"
    , "import qualified QueryHonestyGood as Good"
    , "import qualified QueryHonestyBroken as Broken"
    , "import EmptyExports ()"
    , "localValue :: Int"
    , "localValue = Good.sharedName"
    , "hiddenValue :: Int"
    , "hiddenValue = Broken.sharedName"
    ])
  libdir <- reverse . dropWhile (`elem` ['\n','\r']) . reverse <$> readProcess "ghc" ["--print-libdir"] ""
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags {importPaths=root:importPaths flags,hiDir=Just root,objectDir=Just root}
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    loaded <- load LoadAllTargets
    case loaded of Failed -> liftIO (fail "query honesty fixture did not load"); Succeeded -> pure ()
    summary <- getModSummary (mkModuleName "QueryHonestyHost")
    typed <- typecheckModule =<< parseModule summary
    let (checked,_) = tm_internals_ typed
    environment <- getSession
    liftIO (action root environment checked)
  where
    temporary = do
      parent <- getTemporaryDirectory
      (path,handle) <- openTempFile parent "tidepool-query-honesty"
      hClose handle
      removeFile path
      createDirectory path
      pure path

publicQuery :: String -> StructuredNameNamespace -> String -> StructuredInspection
publicQuery owner namespace name = StructuredInspection (StructuredPublicModule owner)
  namespace name (InspectionProvenance 23 "query-honesty")

inspectQueries :: HscEnv -> TcGblEnv -> [InspectionRequest] -> IO [InspectionResult]
inspectQueries environment checked = runInspection environment checked (tcg_rdr_env checked) Map.empty

withQueryEnvironment :: HscEnv -> Ghc a -> IO a
withQueryEnvironment environment action = do
  session <- Session <$> newIORef environment
  reflectGhc action session

assertQuery :: String -> Bool -> IO ()
assertQuery label condition = unless condition (fail label)

expectUnavailable :: IO [InspectionResult] -> IO ()
expectUnavailable action = do
  outcome <- try action :: IO (Either SomeException [InspectionResult])
  case outcome of
    Left failure | Just exception <- fromException failure
      , isUserError exception && "information unavailable" `isInfixOf` ioeGetErrorString exception -> pure ()
    other -> fail ("expected unavailable inspection information: " ++ show other)

expectSourceFailure :: IO [InspectionResult] -> IO ()
expectSourceFailure action = do
  outcome <- try action :: IO (Either SomeException [InspectionResult])
  case outcome of
    Left failure | Just exception <- (fromException failure :: Maybe SourceError)
      , not (null (diagsFromSourceError exception)) -> pure ()
    other -> fail ("expected original GHC source diagnostics: " ++ show other)

moduleLookupScenario :: IO ()
moduleLookupScenario = withQueryFixture $ \_ environment checked -> do
  withQueryEnvironment environment $ do
    absent <- liftIO (findImportedModule environment (mkModuleName "MissingQueryModule") NoPkgQual)
    liftIO (assertQuery "missing module must be a clean Finder miss" (case absent of
      NotFound {fr_pkg=Nothing,fr_mods_hidden=[],fr_pkgs_hidden=[],fr_unusables=[]} -> True
      _ -> False))
    present <- liftIO (findImportedModule environment (mkModuleName "UncompiledQueryModule") NoPkgQual)
    owner <- case present of Found _ mdl -> pure mdl; _ -> liftIO (fail "uncompiled home source was not found")
    information <- getModuleInfo owner
    liftIO (assertQuery "uncompiled source must lack module information" (case information of Nothing -> True; _ -> False))
    empty <- findModule (mkModuleName "EmptyExports") Nothing >>= getModuleInfo
    liftIO (assertQuery "empty-export control must have real module information"
      (case empty of Just info -> null (modInfoExports info); _ -> False))
  absent <- inspectQueries environment checked
    [ InspectModule "MissingQueryModule" False
    , InspectStructuredInfoOf (publicQuery "MissingQueryModule" StructuredValueName "missing")
    , InspectModule "EmptyExports" False
    , InspectStructuredTypeOf (publicQuery "EmptyExports" StructuredValueName "hidden")
    ]
  case absent of
    [ InspectionModuleNotFound "MissingQueryModule" [StructuredPublicModule "MissingQueryModule"]
      , InspectionStructuredError (StructuredUnknownModule "MissingQueryModule")
      , InspectionBrowse "EmptyExports" False []
      , InspectionStructuredError (StructuredUnknown _)
      ] -> pure ()
    other -> fail ("module absence/empty exports were conflated: " ++ show other)
  forM_ [InspectModule "UncompiledQueryModule" False,
      InspectStructuredInfoOf (publicQuery "UncompiledQueryModule" StructuredValueName "uncompiled")] $ \query ->
    expectUnavailable (inspectQueries environment checked [query])
  -- HPT fault: the checked current module intentionally has no loaded HMI,
  -- matching the production metadata leaf's actual shape.
  let noTarget = hscUpdateHPT (\table -> delFromHpt table (mkModuleName "QueryHonestyHost")) environment
  assertQuery "current-target control retained an HMI"
    (case lookupHpt (hsc_HPT noTarget) (mkModuleName "QueryHonestyHost") of Nothing -> True; _ -> False)
  local <- inspectQueries noTarget checked
    [ InspectModule "QueryHonestyHost" False
    , InspectStructuredInfoOf (publicQuery "QueryHonestyHost" StructuredValueName "localValue")
    , InspectStructuredTypeOf (publicQuery "QueryHonestyHost" StructuredValueName "localValue")
    , InspectStructuredInfoOf (publicQuery "QueryHonestyHost" StructuredValueName "hiddenValue")
    ]
  case local of
    [ InspectionBrowse "QueryHonestyHost" False entries, InspectionStructuredInfo _,
        InspectionStructuredType _, InspectionStructuredError (StructuredUnknown _) ]
      | map infoName entries == ["localValue"] -> pure ()
    other -> fail ("checked current target required an HMI or leaked private exports: " ++ show other)
  forM_ ["Tidepool.Session.Val.G1","Tidepool.Session.Malformed"] $ \reserved -> do
    result <- inspectQueries environment checked [InspectModule reserved False,
      InspectStructuredInfoOf (publicQuery reserved StructuredValueName "private")]
    case result of
      [InspectionModuleNotFound name [], InspectionStructuredError (StructuredUnknownModule name')]
        | name == reserved && name' == reserved -> pure ()
      other -> fail ("reserved session namespace was inspected: " ++ show other)

hiddenPackageScenario :: IO ()
hiddenPackageScenario = withQueryFixture $ \_ environment checked -> withQueryEnvironment environment $ do
  flags <- getSessionDynFlags
  _ <- setSessionDynFlags flags {packageFlags=HidePackage "filepath":packageFlags flags}
  hidden <- getSession
  found <- liftIO (findImportedModule hidden (mkModuleName "System.FilePath") NoPkgQual)
  liftIO (assertQuery "hidden package control did not retain a hidden provider"
    (case found of NotFound {fr_pkgs_hidden=providers} -> not (null providers); _ -> False))
  liftIO $ forM_ [InspectModule "System.FilePath" False,
      InspectStructuredInfoOf (publicQuery "System.FilePath" StructuredValueName "takeFileName")] $ \query ->
    expectSourceFailure (inspectQueries hidden checked [query])

providerFaultScenario :: IO ()
providerFaultScenario = withQueryFixture $ \_ environment checked -> withQueryEnvironment environment $ do
  first <- findModule (mkModuleName "Data.List") Nothing
  second <- findModule (mkModuleName "GHC.Types") Nothing
  liftIO (assertQuery "ambiguous-provider control requires distinct actual owners" (moduleUnit first /= moduleUnit second))
  let exposed = ModOrigin (Just True) [] [] False
      -- UnitState fault injection exercises real Finder refusal, without
      -- pretending ordered home-source paths produce package ambiguity.
      faults =
        [ ("AmbiguousQueryModule",[(first,exposed),(second,exposed)],\result -> case result of FoundMultiple providers -> length providers == 2; _ -> False)
        , ("UnusableQueryModule",[(first,ModUnusable (UnusableUnit (moduleUnit first)
            (BrokenDependencies [stringToUnitId "missing-query-dependency"]) False))],\result -> case result of NotFound {fr_unusables=providers} -> not (null providers); _ -> False)
        , ("HiddenQueryModule",[(first,ModHidden)],\result -> case result of NotFound {fr_mods_hidden=providers} -> not (null providers); _ -> False)
        ]
  liftIO $ forM_ faults $ \(requested,providers,precondition) -> do
    let units = hsc_units environment
        changed = units {moduleNameProvidersMap=addToUniqMap (moduleNameProvidersMap units)
          (mkModuleName requested) (listToUniqMap providers)}
        faulty = environment {hsc_unit_env=ue_setUnits changed (hsc_unit_env environment)}
    found <- findImportedModule faulty (mkModuleName requested) NoPkgQual
    assertQuery ("provider fault precondition failed: " ++ requested) (precondition found && not (isCleanModuleLookupMiss found))
    forM_ [InspectModule requested False,
        InspectStructuredInfoOf (publicQuery requested StructuredValueName "missing")] $ \query ->
      expectSourceFailure (inspectQueries faulty checked [query])

advertisedNameScenario :: IO ()
advertisedNameScenario = withQueryFixture $ \_ environment checked -> do
  original <- withQueryEnvironment environment $ do
    owner <- findModule (mkModuleName "QueryHonestyGood") Nothing
    information <- getModuleInfo owner >>= maybe (liftIO (fail "good module information missing")) pure
    case [name | name <- modInfoExports information,occNameString (nameOccName name) == "sharedName"] of
      [name] -> lookupName name >>= \found -> case found of
        Just (AnId identifier) -> pure identifier
        _ -> liftIO (fail "good sharedName did not resolve to an Id")
      _ -> liftIO (fail "good module has no unique exported sharedName")
  -- HPT fault injection retains both advertised export vectors but removes
  -- the declaration environment. The target's own checked locals stay intact.
  let breakHome requested = hscUpdateHPT (mapHpt (\home ->
        if moduleName (mi_module (hm_iface home)) == mkModuleName requested
          then home {hm_details=(hm_details home) {md_types=emptyTypeEnv}}
          else home))
      faulty = breakHome "QueryHonestyBroken" environment
  withQueryEnvironment faulty $ do
    home <- case lookupHpt (hsc_HPT faulty) (mkModuleName "QueryHonestyBroken") of
      Just value -> pure value
      Nothing -> liftIO (fail "broken-name control lost its HMI")
    liftIO (assertQuery "broken-name control lost advertised exports"
      (not (null (mi_exports (hm_iface home))) && not (null (md_exports (hm_details home)))))
    information <- getModuleInfo (mi_module (hm_iface home)) >>= maybe (liftIO (fail "broken-name control lost ModuleInfo")) pure
    case modInfoExports information of
      [name] -> do
        liftIO (assertQuery "broken-name control advertised another occurrence"
          (occNameString (nameOccName name) == "sharedName"))
        lookupName name >>= \found -> liftIO (assertQuery "broken advertised Name still resolved"
          (case found of Nothing -> True; _ -> False))
      _ -> liftIO (fail "broken-name control needs one advertised Name")
  let current namespace name = StructuredInspection StructuredCurrentScope namespace name (InspectionProvenance 23 "query-honesty")
  forM_ [ InspectModule "QueryHonestyBroken" False
    , InspectModuleBounded "QueryHonestyBroken" True 1
    , InspectStructuredInfoOf (publicQuery "QueryHonestyBroken" StructuredValueName "sharedName")
    , InspectStructuredInfoOf (current StructuredValueName "Broken.sharedName")
    , InspectKeyedAvailability (current StructuredValueName "sharedName") "QueryHonestyBroken"
    ] $ \query -> expectUnavailable (inspectQueries faulty checked [query])
  expectUnavailable $ withQueryEnvironment faulty $ do
    owner <- findModule (mkModuleName "QueryHonestyBroken") Nothing
    information <- getModuleInfo owner >>= maybe (liftIO (fail "broken export information disappeared")) pure
    (answer,_) <- inspectPublicModuleExports False
      (publicQuery "QueryHonestyBroken" StructuredValueName "sharedName") (modInfoExports information)
    pure [answer]
  controls <- inspectQueries faulty checked
    [ InspectStructuredInfoOf (publicQuery "QueryHonestyBroken" StructuredValueName "trulyMissing")
    , InspectKeyedAvailability (current StructuredValueName "sharedName") "QueryHonestyGood"
    , InspectKeyedAvailability (current StructuredValueName "sharedName") "UnselectedOwner"
    , InspectKeyedAvailability (current StructuredFieldName "sharedName") "QueryHonestyGood"
    , InspectStructuredInfoOf (publicQuery "QueryHonestyGood" StructuredFieldName "sharedName")
    , InspectStructuredInfoOf (current StructuredFieldName "Good.sharedName")
    ]
  case controls of
    [ InspectionStructuredError (StructuredUnknown _), InspectionRowFit _ Available,
        InspectionRowFit _ Unknown, InspectionRowFit _ Unknown,
        InspectionStructuredError (StructuredUnknown _), InspectionStructuredError (StructuredUnknown _) ] -> pure ()
    other -> fail ("missing symbol, foreign owner or value-as-field control changed: " ++ show other)
  -- The held-world alias owns an actual Id even if ordinary HPT lookup of its
  -- original Name is unavailable. Preserve that typed route before lookup.
  let aliasFault = breakHome "QueryHonestyGood" faulty
  withQueryEnvironment aliasFault $ lookupName (getName original) >>= \found ->
    liftIO (assertQuery "canonical alias control still has ordinary lookup information"
      (case found of Nothing -> True; _ -> False))
  aliases <- withInspectionWork $ \work -> withQueryEnvironment environment $
    runWorldInspectionGhc work aliasFault checked (tcg_rdr_env checked) Map.empty
      (Map.singleton "localValue" original)
      [ InspectStructuredInfoOf (current StructuredValueName "localValue")
      , InspectKeyedAvailability (current StructuredValueName "sharedName") "QueryHonestyGood"
      ]
  case aliases of
    [InspectionStructuredInfo info, InspectionRowFit ref Available]
      | inspectedIdentifier info == IdentifierRef "QueryHonestyGood" "sharedName" ValueIdentifier
          && ref == inspectedIdentifier info -> pure ()
    other -> fail ("checked canonical alias required ordinary HPT lookup: " ++ show other)

-- These identities come from an actual checked GHC module. Duplicate record
-- fields deliberately have distinct Names with the same public rendering;
-- repeated qualified imports deliberately reach the same actual Name.
structuredScenario :: IO ()
structuredScenario = do
  root <- (</> "tidepool-introspection-identity-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  writeFile (root </> "QualifiedSource.hs") qualifiedFixture
  let source = root </> "StructuredIdentityFixture.hs"
  writeFile source (unlines
    [ "{-# LANGUAGE DuplicateRecordFields #-}"
    , "module StructuredIdentityFixture where"
    , "import qualified QualifiedSource as Q"
    , "import qualified QualifiedSource as R"
    , "candidatePlain :: a -> a"
    , "candidatePlain value = value"
    , "data LeftRecord = LeftRecord { sharedField :: Int }"
    , "data RightRecord = RightRecord { sharedField :: Bool }"
    ])
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  (answers,fieldNames,qualifiedNames) <- withInspectionWork $ \work -> runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags flags {importPaths=root:importPaths flags,hiDir=Just root,objectDir=Just root}
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    loaded <- load LoadAllTargets
    case loaded of Failed -> liftIO (fail "structured identity fixture did not load"); Succeeded -> pure ()
    summary <- getModSummary (mkModuleName "StructuredIdentityFixture")
    typed <- typecheckModule =<< parseModule summary
    let (checked,_) = tm_internals_ typed
        reader = tcg_rdr_env checked
        names occurrence = nub [greName entry | entry <- globalRdrEnvElts reader
          , occNameString (nameOccName (greName entry)) == occurrence]
        query namespace name = StructuredInspection StructuredCurrentScope namespace name (InspectionProvenance 17 "checked-identity")
    value <- case [identifier | identifier <- typeEnvIds (tcg_type_env checked)
        , occNameString (nameOccName (getName identifier)) == "candidatePlain"] of
      [identifier] -> pure identifier
      _ -> liftIO (fail "actual value identity was not unique")
    environment <- getSession
    answers <- runWorldInspectionGhc work environment checked reader Map.empty
      (Map.singleton "candidatePlain" value)
      [ InspectStructuredInfoOf (query StructuredValueName "Q.onlyQualified")
      , InspectStructuredTypeOf (query StructuredTypeName "candidatePlain")
      , InspectStructuredInfoOf (query StructuredValueName "Q.%%")
      , InspectKeyedAvailability (query StructuredValueName "onlyQualified") "QualifiedSource"
      , InspectKeyedAvailability (query StructuredFieldName "sharedField") "StructuredIdentityFixture"
      , InspectStructuredInfoOf (query StructuredAnyName "sharedField")
      , InspectStructuredInfoOf (query StructuredValueName "R.onlyQualified")
      , InspectStructuredInfoOf (query StructuredValueName ".")
      , InspectStructuredInfoOf (query StructuredValueName "Q..%")
      ]
    pure (answers,names "sharedField",names "onlyQualified")
  let publicSpelling name = (moduleNameString . moduleName <$> nameModule_maybe name,occNameString (nameOccName name))
  unless (length fieldNames == 2 && length (nub fieldNames) == 2
      && length (nub (map publicSpelling fieldNames)) == 1) $
    fail "duplicate-field control must retain distinct actual Names with equal public spelling"
  unless (length qualifiedNames == 1) $
    fail "repeated imports must reach one actual Name"
  case answers of
    [ InspectionStructuredInfo qualified
      , InspectionStructuredError (StructuredUnknown missing)
      , InspectionStructuredInfo operator
      , InspectionRowFit fit Available
      , InspectionStructuredError (StructuredUnsupported _)
      , InspectionStructuredError (StructuredAmbiguous _ fields)
      , InspectionStructuredInfo repeated
      , InspectionStructuredInfo composition
      , InspectionStructuredInfo dotted
      ] | inspectedIdentifier qualified == IdentifierRef "QualifiedSource" "onlyQualified" ValueIdentifier
          && inspectedIdentifier repeated == inspectedIdentifier qualified
          && structuredNamespace missing == StructuredTypeName
          && inspectedIdentifier operator == IdentifierRef "QualifiedSource" "%%" ValueIdentifier
          && fit == inspectedIdentifier qualified
          && identifierName (inspectedIdentifier composition) == "."
          && identifierNamespace (inspectedIdentifier composition) == ValueIdentifier
          && inspectedIdentifier dotted == IdentifierRef "QualifiedSource" ".%" ValueIdentifier
          && length fields == 2 && head fields == last fields -> pure ()
    other -> fail ("structured identity selection lost namespace/alias/ambiguity: " ++ show other)
  where trim = reverse . dropWhile (`elem` ['\n','\r']) . reverse

scenario :: IO ()
scenario = do
  root <- (</> "tidepool-introspection-search-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "InspectionSearchFixture.hs"
  writeFile (root </> "QualifiedSource.hs") qualifiedFixture
  writeFile (root </> "AmbigA.hs") ambiguousAFixture
  writeFile (root </> "AmbigB.hs") ambiguousBFixture
  writeFile source fixture
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  (repeatMatches, wildMatches, anyMatches, collisionMatches, numMatches, plainMatches, rankMatches, qualifiedMatches, ambiguousMatches) <-
    runGhc (Just libdir) $ do
      flags <- getSessionDynFlags
      _ <- setSessionDynFlags
        flags
          { importPaths = root : importPaths flags,
            hiDir = Just root,
            objectDir = Just root
          }
      target <- guessTarget source Nothing Nothing
      setTargets [target]
      _ <- load LoadAllTargets
      summary <- getModSummary (mkModuleName "InspectionSearchFixture")
      parsed <- parseModule summary
      typed <- typecheckModule parsed {pm_parsed_source=normalizeInspectionWildcards (pm_parsed_source parsed)}
      let (tcEnv, _) = tm_internals_ typed
          rdrEnv = tcg_rdr_env tcEnv
      hscEnv <- getSession
      repeatMatches <- matchesFor hscEnv tcEnv rdrEnv "queryRepeat"
      wildMatches <- matchesFor hscEnv tcEnv rdrEnv "queryWild"
      anyMatches <- matchesFor hscEnv tcEnv rdrEnv "queryAny"
      collisionMatches <- matchesFor hscEnv tcEnv rdrEnv "queryCollision"
      numMatches <- matchesFor hscEnv tcEnv rdrEnv "queryNum"
      plainMatches <- matchesFor hscEnv tcEnv rdrEnv "queryPlain"
      rankMatches <- matchesFor hscEnv tcEnv rdrEnv "queryRank"
      qualifiedMatches <- matchesFor hscEnv tcEnv rdrEnv "queryQualified"
      ambiguousMatches <- matchesFor hscEnv tcEnv rdrEnv "queryAmbiguous"
      pure
        ( repeatMatches,
          wildMatches,
          anyMatches,
          collisionMatches,
          numMatches,
          plainMatches,
          rankMatches,
          qualifiedMatches,
          ambiguousMatches
        )
  let repeatNames = map typeMatchName repeatMatches
      wildNames = map typeMatchName wildMatches
  unless ("queryRepeat" `notElem` repeatNames && "queryWild" `notElem` wildNames) $
    fail "reserved query binder leaked into its own results"
  unless ("same" `elem` repeatNames) $
    fail ("repeated variable query missed same: " ++ show repeatMatches)
  unless
    ( any
        (\result -> typeMatchName result == "same" && typeMatchQuality result == TypeMatchExact)
        repeatMatches
    )
    (fail ("alpha-equivalent signature was not exact: " ++ show repeatMatches))
  unless ("differentFixed" `notElem` repeatNames) $
    fail ("repeated variable query accepted fixed mismatch: " ++ show repeatMatches)
  unless ("wildUseful" `elem` wildNames) $
    fail ("independent wildcards missed wildUseful: " ++ show wildMatches)
  unless ("candidateMismatch" `elem` map typeMatchName anyMatches) $
    fail ("_ -> _ missed Int -> Bool: " ++ show anyMatches)
  unless ("candidateCollision" `elem` map typeMatchName collisionMatches) $
    fail ("fresh wildcard collided with explicit spelling: " ++ show collisionMatches)
  unless (isSorted (map matchKey repeatMatches) && isSorted (map matchKey wildMatches)) $
    fail "matches were not deterministic by quality/name/module/signature"
  unless (isExact "candidateNum" numMatches) $
    fail ("alpha-equivalent constrained signature was not exact: " ++ show numMatches)
  unless (hasAvailability "candidatePlain" Available numMatches) $
    fail ("constrained query lost usable plain candidate: " ++ show numMatches)
  -- `Num b => b -> b` is usable against `a -> a`; the instance comes from the
  -- call site. It ranks below an unconstrained match and above an unknown.
  unless (hasAvailability "candidateNum" Polymorphic plainMatches) $
    fail ("plain query lost call-site constrained candidate: " ++ show plainMatches)
  unless ("candidateMismatch" `notElem` map typeMatchName plainMatches) $
    fail ("repeated query variable accepted Int -> Bool: " ++ show plainMatches)
  unless (isExact "candidateRank" rankMatches) $
    fail ("alpha-equivalent nested forall was not exact: " ++ show rankMatches)
  unless ("candidateMonoRank" `notElem` map typeMatchName rankMatches) $
    fail ("nested forall matched monomorphic argument: " ++ show rankMatches)
  let qualifiedNames =
        filter (== "Q.onlyQualified") (map typeMatchName qualifiedMatches)
      ambiguousNames =
        filter (`elem` ["A.clash", "B.clash"]) (map typeMatchName ambiguousMatches)
  unless (qualifiedNames == ["Q.onlyQualified"]) $
    fail ("qualified-only import did not retain its usable alias: " ++ show qualifiedMatches)
  let operatorNames = filter (== "(Q.%%)") (map typeMatchName qualifiedMatches)
  unless (operatorNames == ["(Q.%%)"]) $
    fail ("qualified operator was not a usable expression: " ++ show qualifiedMatches)
  unless (ambiguousNames == ["A.clash", "B.clash"]) $
    fail ("ambiguous imports did not choose qualified spellings: " ++ show ambiguousMatches)
  unless (any (\result -> typeMatchName result == "candidateNum"
      && any ((== "Num") . identifierName) (typeMatchReferences result)) numMatches) $
    fail "constrained type search lost its compiler-resolved class reference"
  unless (any (\result -> typeMatchName result == "wildUseful"
      && all (`elem` map identifierName (typeMatchReferences result)) ["Int", "Maybe", "Char"]) wildMatches) $
    fail "nested type search lost its compiler-resolved nominal references"
  unless (any (\result -> typeMatchName result == "candidatePromoted"
      && any (\reference -> identifierName reference == "True"
          && identifierNamespace reference == ConstructorIdentifier) (typeMatchReferences result)) anyMatches) $
    fail "promoted constructor reference lost its constructor namespace"
  let returnedUse = root </> "ReturnedUse.hs"
  qualified <- case qualifiedNames of
    [name] -> pure name
    _ -> fail "qualified result changed after validation"
  writeFile returnedUse (returnedFixture qualified (ambiguousNames ++ operatorNames))
  callProcess "ghc" ["-fno-code", "-i" ++ root, returnedUse]
  where
    trim = reverse . dropWhile (`elem` ['\n', '\r']) . reverse
    matchKey result =
      ( typeMatchAvailability result,
        typeMatchQuality result,
        typeMatchName result,
        typeMatchModule result,
        typeMatchSignature result
      )
    isExact wanted =
      any
        (\result -> typeMatchName result == wanted && typeMatchQuality result == TypeMatchExact)
    hasAvailability wanted availability =
      any
        (\result -> typeMatchName result == wanted && typeMatchAvailability result == availability)

-- A budgeted browse reports the retained distinct entries, so a source larger
-- than the budget reads as "at least budget". The browsed module is imported
-- rather than the checked target, exercising `modInfoExports`; the fixture also
-- exports one type twice (through `Wrapper(..)` and `Wrapper(Wrapped)`) so a
-- future regression that counted export items rather than entities would
-- under-fill the budget.
browseScenario :: IO ()
browseScenario = do
  root <- (</> "tidepool-introspection-browse-test") <$> getTemporaryDirectory
  createDirectoryIfMissing True root
  let source = root </> "InspectionBrowseHost.hs"
      library = root </> "InspectionBrowseLibrary.hs"
      budget = 3
  writeFile library browseLibrary
  writeFile source browseHost
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  unless (boundedDistinctNames Nothing [3 :: Int, 1, 3, 2, 1] == [3, 1, 2]) $
    fail "names must keep first-seen order"
  -- The budget composes after deduplication. GHC's export APIs currently
  -- dedupe repeated items themselves, so this pure assertion is what pins the
  -- ordering: budgeting first would return [1, 2] for a budget of three.
  unless (boundedDistinctNames (Just 3) [1 :: Int, 1, 2, 2, 3, 3, 4] == [1, 2, 3]) $
    fail "the budget must count distinct candidates"
  results <- runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags
      flags
        { importPaths = root : importPaths flags,
          hiDir = Just root,
          objectDir = Just root
        }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "InspectionBrowseHost")
    typed <- typecheckModule =<< parseModule summary
    let (tcEnv, _) = tm_internals_ typed
    hscEnv <- getSession
    liftIO $
      runInspection hscEnv tcEnv (tcg_rdr_env tcEnv) Map.empty
        [InspectModuleBounded "InspectionBrowseLibrary" False budget]
  case results of
    [InspectionBrowse moduleName False entries] -> do
      unless (moduleName == "InspectionBrowseLibrary") $
        fail ("browse named another module: " ++ moduleName)
      let names = map infoName entries
      unless (names == ["Wrapper", "alpha", "beta"]) $
        fail ("a truncated browse must retain the first budget distinct names: " ++ show entries)
      unless (length (nub names) == budget) $
        fail ("retained names were not distinct: " ++ show names)
    other -> fail ("expected one expanded browse result: " ++ show other)
  where
    trim = reverse . dropWhile (`elem` ['\n', '\r']) . reverse

browseLibrary :: String
browseLibrary =
  unlines
    [ "module InspectionBrowseLibrary",
      "  ( Wrapper (..),",
      "    Wrapper (Wrapped),",
      "    alpha,",
      "    beta",
      "  ) where",
      "",
      "data Wrapper = Wrapped Int",
      "",
      "alpha :: Int",
      "alpha = 1",
      "",
      "beta :: Bool",
      "beta = True",
      ""
    ]

browseHost :: String
browseHost =
  unlines
    [ "module InspectionBrowseHost where",
      "import InspectionBrowseLibrary",
      "",
      "hostValue :: Int",
      "hostValue = alpha"
    ]

matchesFor :: (GhcMonad m) => HscEnv -> TcGblEnv -> GlobalRdrEnv -> String -> m [TypeMatch]
matchesFor hscEnv tcEnv rdrEnv wanted = do
  (binder, query) <- findId rdrEnv wanted
  searchTypeMatches hscEnv tcEnv rdrEnv binder query

findId :: (GhcMonad m) => GlobalRdrEnv -> String -> m (Name, Type)
findId rdrEnv wanted = do
  let names =
        [ greName entry
        | entry <- globalRdrEnvElts rdrEnv,
          occNameString (nameOccName (greName entry)) == wanted
        ]
  found <- mapM lookupName names
  case [(getName identifier, idType identifier) | Just (AnId identifier) <- found] of
    result : _ -> pure result
    [] -> error ("missing fixture identifier " ++ wanted)

isSorted :: (Ord a) => [a] -> Bool
isSorted values = and (zipWith (<=) values (drop 1 values))

fixture :: String
fixture =
  unlines
    [ "{-# LANGUAGE PartialTypeSignatures #-}",
      "{-# LANGUAGE RankNTypes, DataKinds #-}",
      "module InspectionSearchFixture where",
      "import qualified QualifiedSource as Q",
      "import Data.Proxy (Proxy)",
      "import AmbigA",
      "import AmbigB",
      "import qualified AmbigA as A",
      "import qualified AmbigB as B",
      "queryRepeat :: a -> (a, a)",
      "queryRepeat value = (value, value)",
      "same :: b -> (b, b)",
      "same value = (value, value)",
      "differentFixed :: Int -> (Int, String)",
      "differentFixed = undefined",
      "queryWild :: _ -> Maybe _",
      "queryWild = undefined",
      "wildUseful :: Int -> Maybe String",
      "wildUseful = undefined",
      "queryAny :: _ -> _",
      "queryAny = undefined",
      "queryCollision :: forall __inspection_w0. __inspection_w0 -> _",
      "queryCollision = undefined",
      "candidateCollision :: Int -> Bool",
      "candidateCollision = undefined",
      "queryNum :: Num a => a -> a",
      "queryNum = id",
      "candidateNum :: Num b => b -> b",
      "candidateNum = id",
      "candidatePlain :: b -> b",
      "candidatePlain = id",
      "queryPlain :: a -> a",
      "queryPlain = id",
      "candidatePromoted :: Proxy 'True -> Bool",
      "candidatePromoted = undefined",
      "candidateMismatch :: Int -> Bool",
      "candidateMismatch = undefined",
      "queryRank :: (forall a. a -> a) -> Int",
      "queryRank _ = 0",
      "candidateRank :: (forall b. b -> b) -> Int",
      "candidateRank _ = 0",
      "candidateMonoRank :: (Int -> Int) -> Int",
      "candidateMonoRank _ = 0",
      "queryQualified :: Int -> Bool",
      "queryQualified = undefined",
      "queryAmbiguous :: Int -> Bool",
      "queryAmbiguous = undefined"
    ]

qualifiedFixture :: String
qualifiedFixture =
  unlines
    [ "module QualifiedSource (onlyQualified, (%%), (.%)) where",
      "onlyQualified :: Int -> Bool",
      "onlyQualified = undefined",
      "(%%) :: Int -> Bool",
      "(%%) = undefined",
      "(.%) :: Int -> Bool",
      "(.%) = undefined"
    ]

ambiguousAFixture :: String
ambiguousAFixture =
  unlines
    [ "module AmbigA (clash) where",
      "clash :: Int -> Bool",
      "clash = undefined"
    ]

ambiguousBFixture :: String
ambiguousBFixture =
  unlines
    [ "module AmbigB (clash) where",
      "clash :: Int -> Bool",
      "clash = undefined"
    ]

returnedFixture :: String -> [String] -> String
returnedFixture qualified ambiguous =
  unlines $
    [ "module ReturnedUse where",
      "import qualified QualifiedSource as Q",
      "import AmbigA",
      "import AmbigB",
      "import qualified AmbigA as A",
      "import qualified AmbigB as B",
      "qualifiedResult :: Int -> Bool",
      "qualifiedResult = " ++ qualified
    ]
      ++ concat
        [ [ "ambiguousResult" ++ show index ++ " :: Int -> Bool",
            "ambiguousResult" ++ show index ++ " = " ++ spelling
          ]
        | (index, spelling) <- zip [(1 :: Int) ..] ambiguous
        ]
