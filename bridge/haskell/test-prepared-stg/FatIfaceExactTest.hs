module Main (main) where

import Control.Exception (finally)
import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import GHC
import GHC.Core (Bind(..))
import GHC.Driver.Session (gopt_set, updOptLevel)
import GHC.Tc.Types (tcg_rdr_env)
import GHC.Types.Name (mkExternalName, mkSystemName, nameModule_maybe, nameOccName)
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
  , lookupFatIfaceExact
  , newFatIfaceCache
  )

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = do
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
            $ updOptLevel 0 flags
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
          recAName = findName "FatFixture" "recA" names
          recBName = findName "FatFixture" "recB" names
          thinIdentityName = findName "ThinFixture" "thinIdentity" names
          missingIdentityName = findName "MissingFixture" "missingIdentity" names
      hsc <- getSession
      cache <- liftIO newFatIfaceCache
      localResult <- liftIO (lookupFatIfaceExact hsc cache
        (mkSystemName (mkUnique 'v' 983450) (mkVarOcc "localOnly")))
      liftIO (assert (isNameWithoutModule localResult)
        "name without a module was not reported as typed absence")
      fatIdentityResult <- liftIO (lookupFatIfaceExact hsc cache fatIdentityName)
      liftIO (assert (isFound fatIdentityResult) "fat identity was not recovered")
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
      thinResult <- liftIO (lookupFatIfaceExact hsc cache thinIdentityName)
      liftIO (assert (isNoExtra thinResult)
        "thin interface was not distinguished from an absent binding")
      thinCachedResult <- liftIO (lookupFatIfaceExact hsc cache (missingNameIn thinIdentityName))
      liftIO (assert (isNoExtra thinCachedResult)
        "typed no-extra outcome was not retained in the cache")
      liftIO (removeFile (work </> "MissingFixture.hi"))
      missingResult <- liftIO (lookupFatIfaceExact hsc cache missingIdentityName)
      liftIO (assertLoadFailure "MissingFixture" missingResult)
      missingCachedResult <- liftIO (lookupFatIfaceExact hsc cache missingIdentityName)
      liftIO (assertLoadFailure "MissingFixture" missingCachedResult)
      pure ()

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
  FatIfaceFound (Rec pairs) -> do
    let actual = map (occNameString . nameOccName . varName . fst) pairs
    assert (all (`elem` actual) expected)
      ("recursive group omitted a sibling: " ++ show actual)
  other -> ioError (userError ("expected recursive fat binding, got " ++ showLookup other))

assertLoadFailure :: String -> FatIfaceLookup -> IO ()
assertLoadFailure wanted result = case result of
  FatIfaceLoadFailure modl reason -> do
    assert (moduleNameString (moduleName modl) == wanted)
      ("load failure named the wrong module: " ++ moduleNameString (moduleName modl))
    assert (not (null reason)) "load failure omitted its reason"
  other -> ioError (userError ("expected typed load failure, got " ++ showLookup other))

showLookup :: FatIfaceLookup -> String
showLookup (FatIfaceFound (NonRec binder _)) =
  "non-rec " ++ occNameString (nameOccName (varName binder))
showLookup (FatIfaceFound (Rec pairs)) =
  "recursive " ++ show (map (occNameString . nameOccName . varName . fst) pairs)
showLookup (FatIfaceMissing missing) = show missing
showLookup (FatIfaceLoadFailure modl reason) =
  moduleNameString (moduleName modl) ++ ": " ++ reason

trim :: String -> String
trim = reverse . dropWhile (== '\n') . reverse
