module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Exception (finally)
import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import GHC
import GHC.Core (Bind(..), maybeUnfoldingTemplate)
import GHC.Driver.Env (HscEnv)
import GHC.Driver.Session (gopt_set, gopt_unset, updOptLevel)
import GHC.Types.Id (Id, realIdUnfolding)
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Types.Name (mkExternalName, mkSystemName, nameModule_maybe, nameOccName, isExternalName)
import GHC.Types.Name.Occurrence (mkVarOcc, occNameString)
import GHC.Types.Name.Reader (globalRdrEnvElts, greName)
import GHC.Types.Unique (mkUnique)
import GHC.Types.Var (varName)
import System.Directory
  ( createDirectoryIfMissing
  , getTemporaryDirectory
  , removeFile
  , removePathForcibly
  )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (callProcess, readProcess)
import Tidepool.FatIface
  ( FatIfaceLookup(..)
  , FatIfaceMissing(..)
  , lookupFatIfaceExact, lookupFatIfaceBodies
  , newFatIfaceCache
  , OwnerInterfaceContext(..), newOwnerInterfaceCache, lookupOwnerInterface
  )
import Tidepool.Resolve (ExactBodyLookup(..), recoverExactBody)
import Tidepool.PreparedStg (newPreparedBodyCache, prepareRecoveredBodies, pmBindings)
import Tidepool.ExecutionProjection (topBinders)

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "test-prepared-stg"
  [testCase "exact interface cache lifecycle" scenario]

scenario :: IO ()
scenario = do
  tmp <- getTemporaryDirectory
  withTempDirectory tmp $ \work -> do
    let fixtures = "test-prepared-stg" </> "fat-iface-fixtures"
        fatSource = work </> "FatFixture.hs"
        thinSource = work </> "ThinFixture.hs"
        missingSource = work </> "MissingFixture.hs"
        useSource = work </> "FatIfaceUse.hs"
    mapM_ (copyFixture fixtures work)
      ["FatFixture.hs", "ThinFixture.hs", "MissingFixture.hs", "FatIfaceUse.hs"]
    let ghc = "ghc"
    compileFixture ghc work ["-fwrite-if-simplified-core"] fatSource
    compileFixture ghc work [] thinSource
    compileFixture ghc work ["-fwrite-if-simplified-core"] missingSource
    libdir <- trim <$> readProcess ghc ["--print-libdir"] ""
    runGhc (Just libdir) $ do
      flags <- getSessionDynFlags
      let fatFlags =
            (`gopt_set` Opt_WriteInterface)
            $ (`gopt_set` Opt_WriteIfSimplifiedCore)
            $ gopt_unset (updOptLevel 0 flags) Opt_IgnoreInterfacePragmas
      _ <- setSessionDynFlags fatFlags
        { importPaths = work : importPaths flags
        , hiDir = Just work
        , objectDir = Just work
        }
      target <- guessTarget useSource Nothing Nothing
      setTargets [target]
      _ <- load LoadAllTargets
      summary <- getModSummary (mkModuleName "FatIfaceUse")
      parsed <- parseModule summary
      typed <- typecheckModule parsed
      let (tcEnv, _) = tm_internals_ typed
          names = map greName (globalRdrEnvElts (tcg_rdr_env tcEnv))
          fatIdentityName = findName "FatFixture" "fatIdentity" names
          privateCallerName = findName "FatFixture" "privateCaller" names
          recAName = findName "FatFixture" "recA" names
          recBName = findName "FatFixture" "recB" names
          thinIdentityName = findName "ThinFixture" "thinIdentity" names
          missingIdentityName = findName "MissingFixture" "missingIdentity" names
      hsc <- getSession
      thinId <- lookupName thinIdentityName >>= \case
        Just (AnId identifier) -> pure identifier
        _ -> fail "thin identity has no genuine exported Id"
      missingId <- lookupName missingIdentityName >>= \case
        Just (AnId identifier) -> pure identifier
        _ -> fail "missing-interface identity has no genuine exported Id"
      cache <- liftIO newFatIfaceCache
      localResult <- liftIO (lookupFatIfaceExact hsc cache
        (mkSystemName (mkUnique 'v' 983450) (mkVarOcc "localOnly")))
      liftIO (assert (isNameWithoutModule localResult)
        "name without a module was not reported as typed absence")
      fatIdentityResult <- liftIO (lookupFatIfaceExact hsc cache fatIdentityName)
      liftIO (assert (isFound fatIdentityResult) "fat identity was not recovered")
      privateResult <- liftIO (lookupFatIfaceExact hsc cache privateCallerName)
      liftIO $ assertPrivateScope privateCallerName privateResult
      privateOwner <- maybe (fail "private scope fixture has no owner") pure
        (nameModule_maybe privateCallerName)
      privateGroups <- case privateResult of
        FatIfaceFound groups -> pure groups
        _ -> fail "private defining closure disappeared"
      privateOwners <- liftIO newOwnerInterfaceCache
      privateBodies <- liftIO newPreparedBodyCache
      privatePrepared <- liftIO $ prepareRecoveredBodies hsc privateOwners privateBodies
        privateOwner privateGroups >>= either (fail . show) pure
      liftIO $ assert (any ((== "privateHelper") . occNameString . nameOccName . varName)
        (concatMap (topBinders . fst) (pmBindings privatePrepared)))
        "native preparation omitted the defining private helper"
      forward <- liftIO (lookupFatIfaceBodies hsc cache privateOwner [privateCallerName, fatIdentityName])
      reverseRoots <- liftIO (lookupFatIfaceBodies hsc cache privateOwner
        [fatIdentityName, privateCallerName, privateCallerName])
      liftIO $ assert (foundNames forward == foundNames reverseRoots)
        "root permutation/duplication changed original group membership or defining order"
      recAResult <- liftIO (lookupFatIfaceExact hsc cache recAName)
      recBResult <- liftIO (lookupFatIfaceExact hsc cache recBName)
      liftIO (assertRecGroup ["recA", "recB"] recAResult)
      liftIO (assertRecGroup ["recA", "recB"] recBResult)
      absentResult <- liftIO (lookupFatIfaceExact hsc cache (missingNameIn fatIdentityName))
      liftIO (assert (isBindingAbsent absentResult)
        "loaded fat interface did not distinguish an absent binding")
      -- Loading the use site under fat flags can rebuild every source
      -- dependency. Restore this deliberately thin fixture before testing the
      -- raw-reader outcome; the fat cache has already observed its artifact.
      liftIO (compileFixture ghc work [] thinSource)
      (thinDeclaring, missingDeclaring) <- liftIO $ (,)
        <$> declaringId hsc thinId <*> declaringId hsc missingId
      thinResult <- liftIO (lookupFatIfaceExact hsc cache thinIdentityName)
      liftIO (assert (isNoExtra thinResult)
        "thin interface was not distinguished from an absent binding")
      thinCachedResult <- liftIO (lookupFatIfaceExact hsc cache (missingNameIn thinIdentityName))
      liftIO (assert (isNoExtra thinCachedResult)
        "typed no-extra outcome was not retained in the cache")
      thinRecovery <- liftIO (recoverExactBody hsc cache thinDeclaring)
      liftIO $ case thinRecovery of
        MissingExactBody name NoExtraDeclarations -> assert (name == thinIdentityName)
          "thin recovery refusal changed the exact defining identity"
        _ -> fail "optimizer unfolding bypassed missing original fat Core"
      liftIO (removeFile (work </> "MissingFixture.hi"))
      missingResult <- liftIO (lookupFatIfaceExact hsc cache missingIdentityName)
      liftIO (assertLoadFailure "MissingFixture" missingResult)
      missingCachedResult <- liftIO (lookupFatIfaceExact hsc cache missingIdentityName)
      liftIO (assertLoadFailure "MissingFixture" missingCachedResult)
      missingRecovery <- liftIO (recoverExactBody hsc cache missingDeclaring)
      liftIO $ case missingRecovery of
        BodyInterfaceFailure owner reason -> do
          assert (Just owner == nameModule_maybe missingIdentityName)
            "read failure changed the exact defining owner"
          assert (not (null reason)) "read failure lost its reason"
        _ -> fail "optimizer unfolding bypassed an unreadable defining interface"
      pure ()

-- Use the production defining-interface loader rather than a use-site Id
-- whose optimization metadata the frontend may have omitted.
declaringId :: HscEnv -> Id -> IO Id
declaringId env requested = do
  owner <- maybe (fail "INLINE fixture has no defining owner") pure
    (nameModule_maybe (varName requested))
  owners <- newOwnerInterfaceCache
  bodies <- newPreparedBodyCache
  _ <- prepareRecoveredBodies env owners bodies owner [] >>= either (fail . show) pure
  context <- lookupOwnerInterface owners owner
  original <- case [identifier | Just defining <- [context]
      , identifier <- ownerInterfaceEntries defining
      , varName identifier == varName requested] of
    [identifier] -> pure identifier
    _ -> fail "INLINE fixture lost its genuine defining Id"
  case maybeUnfoldingTemplate (realIdUnfolding original) of
    Just _ -> pure original
    Nothing -> fail "INLINE defining-interface control has no optimizer unfolding"

copyFixture :: FilePath -> FilePath -> FilePath -> IO ()
copyFixture fixtures work name = readFile (fixtures </> name) >>= writeFile (work </> name)

withTempDirectory :: FilePath -> (FilePath -> IO a) -> IO a
withTempDirectory parent action = do
  (path, handle) <- openTempFile parent "tidepool-fat-iface-exact-"
  hClose handle
  removeFile path
  createDirectoryIfMissing True path
  action path `finally` removePathForcibly path

compileFixture :: FilePath -> FilePath -> [String] -> FilePath -> IO ()
compileFixture ghc work extra source = callProcess ghc
  (["-v0", "-fforce-recomp", "-c", source, "-odir", work, "-hidir", work] ++ extra)

findName :: String -> String -> [Name] -> Name
findName wantedModule wantedOccurrence names = case
    [ name
    | name <- names
    , Just modl <- [nameModule_maybe name]
    , moduleNameString (moduleName modl) == wantedModule
    , occNameString (nameOccName name) == wantedOccurrence
    ] of
  [name] -> name
  found -> error ("expected one " ++ wantedModule ++ "." ++ wantedOccurrence
    ++ ", got " ++ show (length found))

missingNameIn :: Name -> Name
missingNameIn name = case nameModule_maybe name of
  Just modl -> mkExternalName
    (mkUnique 'v' 983451) modl (mkVarOcc "notInExtraDecls") noSrcSpan
  Nothing -> mkSystemName (mkUnique 'v' 983452) (mkVarOcc "notInExtraDecls")

isFound :: FatIfaceLookup -> Bool
isFound FatIfaceFound{} = True
isFound _ = False

isNoExtra :: FatIfaceLookup -> Bool
isNoExtra (FatIfaceMissing NoExtraDeclarations) = True
isNoExtra _ = False

isBindingAbsent :: FatIfaceLookup -> Bool
isBindingAbsent (FatIfaceMissing BindingAbsent) = True
isBindingAbsent _ = False

isNameWithoutModule :: FatIfaceLookup -> Bool
isNameWithoutModule (FatIfaceMissing NameWithoutModule) = True
isNameWithoutModule _ = False

assertRecGroup :: [String] -> FatIfaceLookup -> IO ()
assertRecGroup expected result = case result of
  FatIfaceFound groups -> do
    let actual = [map (occNameString . nameOccName . varName . fst) pairs | Rec pairs <- groups]
    assert (any (\members -> all (`elem` members) expected) actual)
      ("recursive group omitted a sibling: " ++ show actual)
  other -> ioError (userError ("expected recursive fat binding, got " ++ showLookup other))

-- Private defining tops have internal Names and cannot be discovered by
-- the external recovery worklist. Their exact original groups must travel
-- with the selected body, without admitting unrelated definitions.
assertPrivateScope :: Name -> FatIfaceLookup -> IO ()
assertPrivateScope requested result = case result of
  FatIfaceFound groups -> do
    let identifiers = concatMap groupBinders groups
        helpers = [identifier | identifier <- identifiers,
          occNameString (nameOccName (varName identifier)) == "privateHelper"]
    assert (any ((== requested) . varName) identifiers)
      "private closure omitted its selected exported body"
    assert (length helpers == 1 && all (not . isExternalName . varName) helpers)
      "private closure did not retain exactly its genuine internal top helper"
    assert (not (any ((== "fatIdentity") . occNameString . nameOccName . varName) identifiers))
      "private closure admitted an unrelated exported group"
  other -> fail ("private defining scope was unavailable: " ++ showLookup other)

foundNames :: FatIfaceLookup -> [[Name]]
foundNames (FatIfaceFound groups) = map (map varName . groupBinders) groups
foundNames _ = []

groupBinders :: CoreBind -> [Id]
groupBinders (NonRec identifier _) = [identifier]
groupBinders (Rec pairs) = map fst pairs

assertLoadFailure :: String -> FatIfaceLookup -> IO ()
assertLoadFailure wanted result = case result of
  FatIfaceLoadFailure modl reason -> do
    assert (moduleNameString (moduleName modl) == wanted)
      ("load failure named the wrong module: " ++ moduleNameString (moduleName modl))
    assert (not (null reason)) "load failure omitted its reason"
  other -> ioError (userError ("expected typed load failure, got " ++ showLookup other))

showLookup :: FatIfaceLookup -> String
showLookup found@FatIfaceFound{} = "original groups " ++ show (map (map (occNameString . nameOccName)) (foundNames found))
showLookup (FatIfaceMissing missing) = show missing
showLookup (FatIfaceLoadFailure modl reason) =
  moduleNameString (moduleName modl) ++ ": " ++ reason

trim :: String -> String
trim = reverse . dropWhile (== '\n') . reverse
