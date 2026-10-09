{-# LANGUAGE OverloadedStrings #-}

module RecoveryEntryScopeTest (entryScopeTests) where

import Control.Exception (SomeException, finally, try)
import Control.Monad (forM_, unless)
import Control.Monad.IO.Class (liftIO)
import Data.Map.Strict qualified as Map
import Data.IORef (newIORef)
import GHC
import GHC.Builtin.Types (nilDataCon, intTy)
import GHC.Core.DataCon (dataConWorkId)
import GHC.Core (CoreBind, CoreExpr, Bind(..), Expr(..), Alt(..))
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.FVs (exprSomeFreeVars)
import GHC.Driver.Session (gopt_set, gopt_unset, updOptLevel)
import GHC.Stg.Syntax qualified as Stg
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Types.Id (idArity, idType, idTagSig_maybe, idCbvMarks_maybe, asNonWorkerLikeId, isDeadEndId, localiseId, setIdArity, setIdDmdSig, setIdType)
import GHC.Types.Demand (nopSig)
import GHC.Types.Name (nameOccName, nameModule_maybe, wiredInNameTyThing_maybe)
import GHC.Types.TyThing (TyThing(..))
import GHC.Types.TypeEnv (emptyTypeEnv)
import GHC.IfaceToCore (tcTopIfaceBindings)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLcl)
import GHC.Unit.Module.ModIface (mi_extra_decls)
import GHC.Utils.Outputable (text)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Types.Name.Env (lookupNameEnv)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName, isId)
import GHC.Types.Unique.Set (nonDetEltsUniqSet)
import GHC.Unit.Types (stringToUnit)
import Tidepool.CertifiedProducts (resolvePackageGlobal)
import Tidepool.ExecutionSchema (SymbolIdentity(..))
import System.Directory
  (createDirectoryIfMissing, getTemporaryDirectory, removeFile, removePathForcibly)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (readProcess)
import Tidepool.FatIface
  (FatIfaceLookup(..), OwnerInterfaceContext, ownerInterfaceLocation, ownerInterfaceTyCons, ownerInterfaceEntries, lookupFatIfaceExact,
   lookupOwnerInterface, newFatIfaceCache, newOwnerInterfaceCache, readExactInterface)
import Tidepool.PreparedStg
  (PreparedModule, RecoveredModuleInput(..), newPreparedBodyCache,
   pmBindings, pmTagSigs, prepareRecoveredBodies, prepareRecoveredModule)
import Tidepool.Test.Runner (TestTree, testCase, testGroup)

assert :: Bool -> String -> IO ()
assert ok message = unless ok (fail message)

entryScopeTests :: TestTree
entryScopeTests = testGroup "exact original entry scope"
  [ testCase "native constructor and function entry facts survive partial preparation" nativeEntries
  , testCase "original CBV worker survives stripped occurrence metadata" originalCbvWorker
  , testCase "wired constructor worker retains exact canonical entry authority" wiredConstructorEntry
  ]

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
        -- An external but nonwired Name still needs its declaring-interface
        -- entry; the GHC wired source cannot manufacture ordinary entries.
        missingExternal <- try (prepareRecoveredModule env
          (RecoveredModuleInput owner (ownerInterfaceLocation context) [] strictLeaf
            (Map.delete (varName (entry "leaf")) entries)))
          :: IO (Either SomeException PreparedModule)
        case missingExternal of
          Left _ -> pure ()
          Right _ -> fail "absent nonwired canonical entry escaped scope refusal"

-- GHC deliberately excludes wired declarations from .hi files. Recover an
-- actual GHC.Types original that refers to the compiler's list worker, then
-- prepare its genuine defining group through the production owner.
wiredConstructorEntry :: IO ()
wiredConstructorEntry = do
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  forM_ [True, False] $ \ignore -> runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags ((if ignore then gopt_set else gopt_unset)
      (updOptLevel 0 flags) Opt_IgnoreInterfacePragmas)
    env <- getSession
    liftIO $ do
      let worker = dataConWorkId nilDataCon
          workerName = varName worker
          doc = text "genuine wired constructor entry control"
      owner <- maybe (fail "wired worker has no exact owner") pure (nameModule_maybe workerName)
      case wiredInNameTyThing_maybe workerName of
        Just (AnId canonical) -> assert
          (varName canonical == workerName && eqType (idType canonical) (idType worker))
          "wired worker Name does not carry its exact GHC-issued Id"
        _ -> fail "wired list worker Name is not an AnId"
      (iface, _) <- readExactInterface env owner >>= either (fail . show) pure
      extra <- maybe (fail "wired owner's original Core is absent") pure (mi_extra_decls iface)
      decoded <- initIfaceCheck doc env $ do
        types <- liftIO (newIORef emptyTypeEnv)
        initIfaceLcl owner doc NotBoot (tcTopIfaceBindings types extra)
      let refersToWorker rhs = any ((== workerName) . varName)
            (nonDetEltsUniqSet (exprSomeFreeVars isId rhs))
      root <- case [binder | binding <- decoded, (binder,rhs) <- pairs binding,
                      nameModule_maybe (varName binder) == Just owner, refersToWorker rhs] of
        binder : _ -> pure binder
        [] -> fail "genuine wired-owner original does not reference the list worker"
      fat <- newFatIfaceCache
      original <- lookupFatIfaceExact env fat (varName root) >>= \case
        FatIfaceFound bindings -> pure bindings
        _ -> fail "selected wired-owner original is absent"
      owners <- newOwnerInterfaceCache
      bodies <- newPreparedBodyCache
      prepared <- prepareRecoveredBodies env owners bodies owner original >>= either (fail . show) pure
      context <- lookupOwnerInterface owners owner >>= maybe (fail "wired owner context absent") pure
      let entries = ownerInterfaceEntries context
      assert (Map.notMember workerName entries)
        "control no longer exercises a wired declaration omitted from the interface"
      canonicalRoot <- maybe (fail "original root absent from declaring interface") pure
        (Map.lookup (varName root) entries)
      assertEntry canonicalRoot prepared
      assert (not (eqType (idType worker) intTy)) "wired worker type fault is ineffective"
      let wrongType = map (mapBindingReferences
            (Map.singleton workerName (setIdType worker intTy))) original
      refused <- try (prepareRecoveredModule env (RecoveredModuleInput owner
        (ownerInterfaceLocation context) [] wrongType entries))
        :: IO (Either SomeException PreparedModule)
      case refused of
        Left _ -> pure ()
        Right _ -> fail "altered wired worker type escaped canonical scope refusal"

-- A genuine package-original WorkerLike Id carries GHC's strict argument
-- contract. Compare unchanged recovery with metadata-stripped occurrences;
-- the canonical interface remains the sole issuer of the CBV marks.
originalCbvWorker :: IO ()
originalCbvWorker = do
  libdir <- trim <$> readProcess "ghc" ["--print-libdir"] ""
  forM_ [True, False] $ \ignore -> runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags ((if ignore then gopt_set else gopt_unset)
      (updOptLevel 0 flags) Opt_IgnoreInterfacePragmas)
    env <- getSession
    liftIO $ do
      let owner = mkModule (stringToUnit "ghc-internal") (mkModuleName "GHC.Internal.List")
          root = SymbolIdentity "ghc-internal" "GHC.Internal.List" "value" "length" Nothing
      (identifier, _) <- resolvePackageGlobal env root >>= either fail pure
      fat <- newFatIfaceCache
      original <- lookupFatIfaceExact env fat (varName identifier) >>= \case
        FatIfaceFound bindings -> pure bindings
        _ -> fail "real length original Core absent"
      owners <- newOwnerInterfaceCache
      bodies <- newPreparedBodyCache
      _ <- prepareRecoveredBodies env owners bodies owner original >>= either (fail . show) pure
      context <- lookupOwnerInterface owners owner >>= maybe (fail "real length owner context absent") pure
      let entries = ownerInterfaceEntries context
          input bindings = RecoveredModuleInput owner (ownerInterfaceLocation context) [] bindings entries
          free = [reference | binding <- original, (_,rhs) <- pairs binding,
            reference <- nonDetEltsUniqSet (exprSomeFreeVars isId rhs)]
      rawWorker <- case [reference | reference <- free, occurrence (varName reference) == "$wlength"] of
        [reference] -> pure reference
        _ -> fail "real length no longer has its omitted strict worker reference"
      worker <- maybe (fail "real strict worker missing from declaring interface") pure
        (Map.lookup (varName rawWorker) entries)
      assert (varName rawWorker == varName worker && eqType (idType rawWorker) (idType worker))
        "real strict worker Name/type differs from canonical entry"
      assert (case idCbvMarks_maybe worker of Just _ -> True; _ -> False)
        "real length dependency lacks compiler-issued CBV worker marks"
      assert (varName worker `notElem` [varName binder | binding <- original, (binder,_) <- pairs binding])
        "CBV control supplies the worker body instead of exercising omitted scope"
      let strippedWorker = asNonWorkerLikeId rawWorker
      assert (case idCbvMarks_maybe strippedWorker of Nothing -> True; _ -> False)
        "CBV control failed to strip actual occurrence worker marks"
      let stripped = map (mapBindingReferences (Map.singleton (varName rawWorker) strippedWorker)) original
      baseline <- prepareRecoveredModule env (input original)
      repaired <- prepareRecoveredModule env (input stripped)
      assert (caseCount baseline >= 2 && caseCount repaired == caseCount baseline)
        "canonical CBV scope lost the genuine strict-argument case rewrite"
      let canonical = entries Map.! varName identifier
      assertEntry canonical baseline
      assertEntry canonical repaired

caseCount :: PreparedModule -> Int
caseCount prepared = sum [countRhs rhs | (Stg.StgTopLifted binding, _) <- pmBindings prepared,
  rhs <- bodies binding]
  where
    bodies (Stg.StgNonRec _ rhs) = [rhs]
    bodies (Stg.StgRec members) = map snd members
    countRhs (Stg.StgRhsClosure _ _ _ _ body _) = countExpr body
    countRhs Stg.StgRhsCon{} = 0
    countExpr (Stg.StgCase scrutinee _ _ alts) =
      1 + countExpr scrutinee + sum (map (countExpr . Stg.alt_rhs) alts)
    countExpr (Stg.StgLet _ binding body) = sum (map countRhs (bodies binding)) + countExpr body
    countExpr (Stg.StgLetNoEscape _ binding body) = sum (map countRhs (bodies binding)) + countExpr body
    countExpr (Stg.StgTick _ body) = countExpr body
    countExpr _ = 0

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
