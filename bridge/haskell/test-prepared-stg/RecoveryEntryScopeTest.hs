module RecoveryEntryScopeTest (entryScopeTests) where

import Control.Exception (SomeException, finally, try)
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.Map.Strict qualified as Map
import GHC
import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), Alt(..))
import GHC.Core.TyCo.Compare (eqType)
import GHC.Driver.Session (gopt_set, gopt_unset, updOptLevel)
import GHC.Stg.Syntax qualified as Stg
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Types.Id (idArity, idType, idTagSig_maybe, isDeadEndId, localiseId, setIdArity, setIdDmdSig)
import GHC.Types.Demand (nopSig)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name.Env (lookupNameEnv)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName)
import System.Directory
  (createDirectoryIfMissing, getTemporaryDirectory, removeFile, removePathForcibly)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (readProcess)
import Tidepool.FatIface
  (FatIfaceLookup(..), OwnerInterfaceContext(..), lookupFatIfaceExact,
   lookupOwnerInterface, newFatIfaceCache, newOwnerInterfaceCache)
import Tidepool.PreparedStg
  (PreparedModule, RecoveredModuleInput(..), newPreparedBodyCache,
   pmBindings, pmTagSigs, prepareRecoveredBodies, prepareRecoveredModule)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)

assert :: Bool -> String -> IO ()
assert ok message = unless ok (fail message)

entryScopeTests :: TestTree
entryScopeTests = testGroup "exact original entry scope"
  [testCase "native constructor and function entry facts survive partial preparation" nativeEntries]

-- Compile an independent native original, then read its fat Core through the
-- normal recovery owner. The O0 decoding flags vary independently of its
-- declaring interface facts; source HMIs are not an ABI oracle.
nativeEntries :: IO ()
nativeEntries = do
  parent <- getTemporaryDirectory
  withDirectory parent $ \work -> do
    forM_ ["RecoveryTagFixture.hs", "RecoveryTagUse.hs"] $ \name ->
      readFile ("test-prepared-stg" </> name) >>= writeFile (work </> name)
    libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
    forM_ [True, False] $ \ignore -> runGhc (Just libdir) $ do
      flags <- getSessionDynFlags
      _ <- setSessionDynFlags
        (gopt_set (gopt_set (gopt_unset (gopt_unset (updOptLevel 0 flags)
          Opt_OmitInterfacePragmas) Opt_IgnoreInterfacePragmas)
          Opt_WriteInterface) Opt_WriteIfSimplifiedCore)
        { importPaths = [work], hiDir = Just work, objectDir = Just work,
          backend = ncgBackend, ghcLink = NoLink }
      target <- guessTarget (work </> "RecoveryTagUse.hs") Nothing Nothing
      setTargets [target]
      result <- load LoadAllTargets
      case result of Failed -> liftIO (fail "entry scope fixture load failed"); Succeeded -> pure ()
      owner <- findModule (mkModuleName "RecoveryTagFixture") Nothing
      details <- getModuleInfo owner
      names <- maybe (liftIO (fail "fixture module details absent"))
        (pure . modInfoExports) details
      currentFlags <- getSessionDynFlags
      _ <- setSessionDynFlags ((if ignore then gopt_set else gopt_unset)
        currentFlags Opt_IgnoreInterfacePragmas)
      env <- getSession
      liftIO $ do
        fat <- newFatIfaceCache
        owners <- newOwnerInterfaceCache
        bodies <- newPreparedBodyCache
        -- The root Name comes from the genuinely loaded original interface.
        let nameOf wanted = case [name | name <- names, occurrence name == wanted] of
              [name] -> name
              _ -> error ("missing fixture export " ++ wanted)
        raw <- lookupFatIfaceExact env fat (nameOf "strictFunction") >>= \case
          FatIfaceFound bindings -> pure bindings
          _ -> fail "native fixture has no original Core"
        -- Acquire the canonical owner index using the production recovery path.
        _ <- prepareRecoveredBodies env owners bodies owner raw >>= either (fail . show) pure
        context <- lookupOwnerInterface owners owner >>= maybe (fail "canonical owner context absent") pure
        let entries = ownerInterfaceEntries context
            entry wanted = case Map.lookup (nameOf wanted) entries of
              Just identifier -> identifier
              Nothing -> error ("missing canonical fixture entry " ++ wanted)
            input bindings = RecoveredModuleInput owner (ownerInterfaceLocation context)
              [] bindings entries
            getOriginal wanted = lookupFatIfaceExact env fat (nameOf wanted) >>= \case
              FatIfaceFound bindings -> pure bindings
              _ -> fail ("original body absent: " ++ wanted)
            lower bindings = prepareRecoveredModule env (input bindings)
            check wanted = do
              bindings <- getOriginal wanted
              prepared <- lower bindings
              assertEntry (entry wanted) prepared
              assert (lookupNameEnv (pmTagSigs prepared) (varName (entry wanted))
                == idTagSig_maybe (entry wanted))
                ("native result tag changed: " ++ wanted)
        assert (isConstructorEntry (entry "strictFunction")
          && isConstructorEntry (entry "indirectStrictFunction"))
          "strict function controls lack real canonical LFCon entries"
        assert (case importedIdLFInfo (entry "unknownStrictFunction") of LFThunk{} -> True; _ -> False)
          "unknown function control lacks its canonical LFThunk"
        assert (idArity (entry "taggedFunction") == 1)
          "stripped-arity control lacks a genuine native unary function"
        forM_ ["strictLeaf", "lazyLeaf", "strictFunction", "indirectStrictFunction",
               "unknownStrictFunction", "ordinaryCall"] check
        -- CorePrep follows the public alias leaf to its implicit nullary
        -- constructor worker. No tycons/workers are supplied to this subset.
        leaf <- getOriginal "leaf"
        strictLeaf <- getOriginal "strictLeaf"
        paired <- lower (leaf ++ strictLeaf)
        assertEntry (entry "strictLeaf") paired
        -- GlobalId occurrences are replaced directly for this metadata fault:
        -- GHC.Core.Subst intentionally ignores GlobalIds.
        let stripped = Map.singleton (varName (entry "taggedFunction"))
              (setIdArity (entry "taggedFunction") 0)
        -- ordinaryCall makes the saturated call whose result inference needs
        -- canonical arity; a held function field alone would not exercise it.
        forM_ ["strictFunction", "ordinaryCall"] $ \wanted -> do
          original <- getOriginal wanted
          strippedPrepared <- lower (map (mapBindingReferences stripped) original)
          assertEntry (entry wanted) strippedPrepared
          assert (lookupNameEnv (pmTagSigs strippedPrepared) (varName (entry wanted))
            == idTagSig_maybe (entry wanted))
            ("stripped occurrence arity lost its canonical result tag: " ++ wanted)
        assert (isDeadEndId (entry "bottomFunction"))
          "bottoming control lacks a genuine canonical dead-end demand"
        assert (case importedIdLFInfo (entry "bottomStrictFunction") of LFThunk{} -> True; _ -> False)
          "bottoming strict field control lacks its canonical LFThunk"
        bottom <- getOriginal "bottomStrictFunction"
        let strippedDemand = Map.singleton (varName (entry "bottomFunction"))
              (setIdDmdSig (entry "bottomFunction") nopSig)
        bottomPrepared <- lower (map (mapBindingReferences strippedDemand) bottom)
        assertEntry (entry "bottomStrictFunction") bottomPrepared
        assert (lookupNameEnv (pmTagSigs bottomPrepared) (varName (entry "bottomStrictFunction"))
          == idTagSig_maybe (entry "bottomStrictFunction"))
          "stripped bottoming occurrence changed its canonical result tag"
        -- Supplied actual bodies dominate the old declaring-interface scope.
        -- Replace only the genuine leaf body with a same-typed genuine thunk.
        unknownLeaf <- getOriginal "unknownLeaf"
        unknownBody <- singleBody (varName (entry "unknownLeaf")) unknownLeaf
        assert (eqType (idType (entry "leaf")) (idType (entry "unknownLeaf")))
          "actual-body precedence control changed its type"
        altered <- lower (map (replaceBody (varName (entry "leaf")) unknownBody) leaf ++ strictLeaf)
        assert (case rootRhs (varName (entry "strictLeaf")) altered of
          [Stg.StgRhsClosure _ _ Stg.Updatable [] _ _] -> True; _ -> False)
          "canonical scope overrode a supplied actual thunk"
        -- A real omitted reference localised to an internal Name cannot gain
        -- external canonical authority or silently escape original scope lint.
        let missing = Map.singleton (varName (entry "leaf")) (localiseId (entry "leaf"))
        refused <- try (lower (map (mapBindingReferences missing) strictLeaf))
          :: IO (Either SomeException PreparedModule)
        case refused of Left _ -> pure (); Right _ -> fail "missing private reference escaped scope refusal"

isConstructorEntry :: Id -> Bool
isConstructorEntry identifier = case importedIdLFInfo identifier of LFCon{} -> True; _ -> False

assertEntry :: Id -> PreparedModule -> IO ()
assertEntry canonical prepared = assert matches
  ("native entry changed: " ++ occurrence (varName canonical))
  where
    matches = case (importedIdLFInfo canonical, rootRhs (varName canonical) prepared) of
      (LFCon expected, [Stg.StgRhsCon _ actual _ _ _ _]) -> expected == actual
      (LFThunk{}, [Stg.StgRhsClosure _ _ Stg.Updatable [] _ _]) -> True
      (LFReEntrant _ arity _ _, [Stg.StgRhsClosure _ _ Stg.ReEntrant arguments _ _]) -> arity == length arguments
      _ -> False

rootRhs :: Name -> PreparedModule -> [Stg.CgStgRhs]
rootRhs name prepared =
  [rhs | (Stg.StgTopLifted binding, _) <- pmBindings prepared,
   (binder,rhs) <- case binding of Stg.StgNonRec binder rhs -> [(binder,rhs)]; Stg.StgRec members -> members,
   varName binder == name]

singleBody :: Name -> [CoreBind] -> IO CoreExpr
singleBody name bindings = case [rhs | binding <- bindings, (binder,rhs) <- pairs binding, varName binder == name] of
  [rhs] -> pure rhs
  _ -> fail "expected one genuine original body"

pairs :: CoreBind -> [(Id, CoreExpr)]
pairs (NonRec binder rhs) = [(binder,rhs)]
pairs (Rec members) = members

replaceBody :: Name -> CoreExpr -> CoreBind -> CoreBind
replaceBody name body (NonRec binder rhs) = NonRec binder (if varName binder == name then body else rhs)
replaceBody name body (Rec members) = Rec [(binder,if varName binder == name then body else rhs) | (binder,rhs) <- members]

mapBindingReferences :: Map.Map Name Id -> CoreBind -> CoreBind
mapBindingReferences replacements (NonRec binder rhs) = NonRec binder (change rhs)
  where change = mapReferences replacements
mapBindingReferences replacements (Rec members) = Rec [(binder,mapReferences replacements rhs) | (binder,rhs) <- members]

mapReferences :: Map.Map Name Id -> CoreExpr -> CoreExpr
mapReferences known = change
  where
    change (Var identifier) = Var (Map.findWithDefault identifier (varName identifier) known)
    change (App function argument) = App (change function) (change argument)
    change (Lam binder rhs) = Lam binder (change rhs)
    change (Let binding rhs) = Let (mapBindingReferences known binding) (change rhs)
    change (Case rhs binder ty alts) = Case (change rhs) binder ty
      [Alt con args (change body) | Alt con args body <- alts]
    change (Cast rhs coercion) = Cast (change rhs) coercion
    change (Tick tick rhs) = Tick tick (change rhs)
    change other = other

occurrence :: Name -> String
occurrence = occNameString . nameOccName
trim :: String -> String
trim = reverse . dropWhile (`elem` ['\n', '\r', ' ']) . reverse

withDirectory :: FilePath -> (FilePath -> IO a) -> IO a
withDirectory parent action = do
  (path, handle) <- openTempFile parent "tidepool-recovery-entry-scope-"
  hClose handle
  removeFile path
  createDirectoryIfMissing True path
  action path `finally` removePathForcibly path
