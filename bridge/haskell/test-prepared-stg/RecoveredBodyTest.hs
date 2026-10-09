{-# LANGUAGE GADTs #-}

module Main (main, tests) where

import Tidepool.PreparedStg.Internal (PreparedModule(..))
import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Control.Exception (evaluate)
import Data.List (intercalate, sortOn)
import Data.IORef (newIORef)
import Data.Set qualified as Set
import Data.Map.Strict qualified as Map
import Data.Text qualified as Text
import GHC
import GHC.Driver.Env (hsc_HPT)
import GHC.IfaceToCore (tcTopIfaceBindings)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLcl)
import GHC.Types.TypeEnv (emptyTypeEnv)
import GHC.Unit.Module.ModIface (mi_extra_decls)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Unit.Home.ModInfo (lookupHpt)
import Tidepool.FinalizedModule (FinalizedModule(..))
import GHC.Core (Bind(..), bindersOfBinds, maybeUnfoldingTemplate)
import GHC.Core.Opt.Arity (manifestArity)
import GHC.Unit.Module.ModGuts (CgGuts(..))
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.Utils qualified as CoreUtils
import GHC.Driver.Session (updOptLevel, gopt_unset, GeneralFlag(Opt_IgnoreInterfacePragmas))
import GHC.Driver.Main (hscTidy)
import GHC.Stg.Syntax qualified as Stg
import GHC.Types.Id (idName, idArity, realIdUnfolding)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (varName, varType)
import GHC.Utils.Outputable (ppr, showSDocUnsafe, text)
import System.Directory (getCurrentDirectory)
import System.FilePath ((</>))
import System.Exit (ExitCode(..))
import System.Process (proc, readCreateProcessWithExitCode)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), preparedTargetReferences
  , preparedTopIdentities, preparedRootIdentity, projectPreparedTarget, prepareProjection
  , projectSelectedCandidateWithHostBindings, candidateGlobals, finalizePreparedCandidate )
import Tidepool.ExecutionSchema
  ( Architecture(..), Alternative(..), Atom(..), Endianness(..), Expr(..), Group(..)
  , HeapBinding(..), HeapRhs(..), GlobalDecl(..), JoinBinding(..), OperationDecl(..)
  , OperationId(..), OperationIdentity(..), ResultContract(..), RuntimeRep(..)
  , Signature(..), SignatureId(..), SymbolIdentity(..), TargetDescriptor(..)
  , TopBinding(..), ValueRef(..), WireProgram(..) )
import Tidepool.FatIface
  ( FatIfaceLookup(..), newFatIfaceCache, lookupFatIfaceExact
  , readExactInterface
  , OwnerInterfaceContext, ownerInterfaceLocation, ownerInterfaceTyCons, ownerInterfaceEntries, newOwnerInterfaceCache, lookupOwnerInterface, evictOwnerInterfaceMatching )
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult(..)
  , PipelineResult(prHscEnv), runPipelineSelected )
import Tidepool.PreparedRecovery (RecoveredClosure(closureModules, closureFailures), recoverPreparedClosure)
import Tidepool.PreparedStg
  ( pmModule, pmBindings, RecoveredModuleFailure(..), newPreparedBodyCache, prepareModule
  , prepareRecoveredBodies, RecoveredModuleInput(..), prepareRecoveredModule, preparedExpectedEntry )
import Tidepool.Resolve
  ( ExactBodyLookup(..), recoverExactBody )

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "recovered-body"
  [ testCase "original recovered base package body" assertRecoveredBaseBody
  , testCase "original recovered hsc package body" assertRecoveredExecutionStackBody
  , testCase "original recovered entry contracts" assertRecoveredEntryContracts
  , testCase "original recovered dictionary defining body" assertRecoveredDictionaryBody
  , testCase "original recovered body closure" assertAllRecoveredBodies
  , testCase "owner interface cache reuse eviction and retry" $ do
      root <- getCurrentDirectory
      libdir <- trim <$> readProcessGhc ["--print-libdir"]
      assertSemigroupSubset root libdir
  ]

-- base has its own executable definitions as well as reexports. Demand the
-- actual defining Id without optimizer inlining so an absent fat body refuses.
assertRecoveredBaseBody :: IO ()
assertRecoveredBaseBody = do
  root <- getCurrentDirectory
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (gopt_unset (updOptLevel 0 flags) Opt_IgnoreInterfacePragmas)
      { importPaths = [root </> "test-prepared-stg"] ++ importPaths flags
      , backend = noBackend, ghcLink = NoLink }
    target <- guessTarget (root </> "test-prepared-stg" </> "RecoveredBaseCaller.hs") Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "RecoveredBaseCaller")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    desugared <- desugarModule typed
    env <- getSession
    (guts, _) <- liftIO $ hscTidy env (coreModule desugared)
    home <- maybe (fail "base fixture lacks its finalized home interface") pure
      (lookupHpt (hsc_HPT env) (ms_mod_name summary))
    prepared <- liftIO $ prepareModule env (ms_location summary) mempty (FinalizedModule home guts)
    let context = ProjectionContext
          { projectionProfile = Text.pack "recovered-base-body"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty, projectionCurrentOriginals = mempty
          , projectionEntry = SymbolIdentity (Text.pack "main") (Text.pack "RecoveredBaseCaller")
              (Text.pack "value") (Text.pack "caller") Nothing
          , projectionAuxiliaryRoots = [], projectionFormattingAuthority = Nothing
          , projectionTimeAuthority = Nothing, projectionJsonAuthority = Nothing
          , projectionTextUnit = Nothing }
    defining <- case [identifier | identifier <- preparedTargetReferences context [prepared]
          , occNameString (nameOccName (varName identifier)) == "toList"
          , Just owner <- [nameModule_maybe (varName identifier)]
          , moduleNameString (moduleName owner) == "Data.List.NonEmpty"] of
      [identifier] -> pure identifier
      _ -> fail "authored nonempty caller lost its actual base defining Id"
    let identity = preparedRootIdentity defining
        rooted = context {projectionAuxiliaryRoots = [identity]}
    cache <- liftIO newFatIfaceCache
    exact <- liftIO $ recoverExactBody env cache defining
    liftIO $ case exact of
      ExactBody owner bindings -> do
        assert (Just owner == nameModule_maybe (varName defining))
          "base body changed its defining owner"
        assert (varName defining `elem` map varName (bindersOfBinds bindings))
          "base body lacks its exact defining binder"
      MissingExactBody _ reason -> fail ("base defining Core is absent: " ++ show reason)
      BodyInterfaceFailure _ reason -> fail ("base defining interface is unreadable: " ++ reason)
      BodyTypeMismatch{} -> fail "base defining body type is incompatible"
      UnsupportedBodyCapability{} -> fail "base toList has unsupported body capability"
    owners <- liftIO newOwnerInterfaceCache
    bodies <- liftIO newPreparedBodyCache
    closure <- liftIO $ recoverPreparedClosure env cache owners bodies rooted [prepared]
    liftIO $ do
      assert (null (closureFailures closure)) ("base package closure failed: " ++ show (closureFailures closure))
      program <- either (fail . ("base native projection failed: " ++) . show) pure
        (projectPreparedTarget rooted (closureModules closure))
      let definitions = [symbol | group <- programBindings program
            , TopBinding symbol _ <- case group of
                NonRecursive binding -> [binding]
                Recursive bindings -> bindings]
      assert (identity `elem` definitions)
        "native base projection omitted its required exact defining root"
      case [signature | group <- programBindings program
          , TopBinding symbol (HeapBinding _ (Function (SignatureId index) _ _ _)) <- case group of
              NonRecursive binding -> [binding]
              Recursive bindings -> bindings
          , symbol == identity
          , signature : _ <- [drop (fromIntegral index) (programSignatures program)]] of
        [signature] -> assert (signatureArguments signature == [LiftedRefRep])
          "native base toList lost its unary callable contract"
        _ -> fail "native base toList did not emit its exact defining function"

-- The pinned boot-library producer must retain original Core from hsc2hs
-- inputs too. This pure worker is demanded by ordinary error rendering.
assertRecoveredExecutionStackBody :: IO ()
assertRecoveredExecutionStackBody = do
  root <- getCurrentDirectory
  prepared <- runPipelineSelected PreparedStg
    (root </> "test-prepared-stg" </> "RecoveredExecutionStackCaller.hs") []
  let env = prHscEnv (pprPipelineResult prepared)
      home = pprModules prepared
      context = ProjectionContext
        { projectionProfile = Text.pack "recovered-hsc-body"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionCurrentOriginals = mempty
        , projectionEntry = SymbolIdentity (Text.pack "main") (Text.pack "RecoveredExecutionStackCaller")
            (Text.pack "value") (Text.pack "caller") Nothing
        , projectionAuxiliaryRoots = [], projectionFormattingAuthority = Nothing
        , projectionTimeAuthority = Nothing, projectionJsonAuthority = Nothing
        , projectionTextUnit = Nothing }
  owner <- case [modul | identifier <- preparedTargetReferences context home
      , Just modul <- [nameModule_maybe (varName identifier)]
      , moduleNameString (moduleName modul) == "GHC.Internal.ExecutionStack.Internal"] of
    first : _ -> pure first
    [] -> fail "authored stack renderer did not demand its genuine package owner"
  cache <- newFatIfaceCache
  owners <- newOwnerInterfaceCache
  bodies <- newPreparedBodyCache
  _ <- prepareRecoveredBodies env owners bodies owner [] >>= either (fail . show) pure
  declaring <- lookupOwnerInterface owners owner
  worker <- case [identifier | Just defining <- [declaring]
      , identifier <- Map.elems (ownerInterfaceEntries defining)
      , occurrence identifier == "$wshowLocation"] of
    [identifier] -> pure identifier
    _ -> fail "defining interface lost its real stack-location worker"
  required <- case importedIdLFInfo worker of
    LFReEntrant _ arity _ _ -> pure arity
    _ -> fail "defining stack-location worker is not callable"
  assert (required == 4) "pinned stack-location worker changed its entry contract"
  exact <- recoverExactBody env cache worker
  case exact of
    ExactBody defining _ -> assert (defining == owner) "hsc body changed its defining owner"
    MissingExactBody _ reason -> fail ("hsc defining Core is absent: " ++ show reason)
    BodyInterfaceFailure _ reason -> fail ("hsc defining interface is unreadable: " ++ reason)
    BodyTypeMismatch{} -> fail "hsc defining body type is incompatible"
    UnsupportedBodyCapability{} -> fail "pure stack renderer has unsupported body capability"
  closure <- recoverPreparedClosure env cache owners bodies context home
  assert (null (closureFailures closure))
    ("hsc package closure failed: " ++ show (closureFailures closure))
  program <- either (fail . ("hsc native projection failed: " ++) . show) pure
    (projectPreparedTarget context (closureModules closure))
  case [signature | group <- programBindings program, top <- groupItems group
      , TopBinding symbol (HeapBinding _ (Function (SignatureId index) _ _ _)) <- [top]
      , symbolModule symbol == Text.pack "GHC.Internal.ExecutionStack.Internal"
      , symbolOccurrence symbol == Text.pack "$wshowLocation"
      , signature : _ <- [drop (fromIntegral index) (programSignatures program)]] of
    [signature] -> assert (signatureArguments signature == replicate required LiftedRefRep)
      "native stack-location worker lost its defining callable contract"
    _ -> fail "native stack renderer did not emit its demanded defining worker"
  where
    occurrence = occNameString . nameOccName . varName
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops

-- Optimizer DFun recipes need not preserve an exported allocation/entry shape.
-- The defining fat body does, independently of discovery order.
assertRecoveredDictionaryBody :: IO ()
assertRecoveredDictionaryBody = do
  root <- getCurrentDirectory
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (gopt_unset (updOptLevel 0 flags) Opt_IgnoreInterfacePragmas)
      { importPaths = [root </> "test-prepared-stg", root </> "lib"] ++ importPaths flags
      , backend = noBackend, ghcLink = NoLink }
    target <- guessTarget (root </> "test-prepared-stg" </> "RecoveredEntryCaller.hs") Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "RecoveredEntryCaller")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    desugared <- desugarModule typed
    env <- getSession
    (guts, _) <- liftIO $ hscTidy env (coreModule desugared)
    home <- maybe (fail "dictionary fixture has no finalized home interface") pure
      (lookupHpt (hsc_HPT env) (ms_mod_name summary))
    prepared <- liftIO $ prepareModule env (ms_location summary) mempty (FinalizedModule home guts)
    let context = ProjectionContext
          { projectionProfile = Text.pack "recovered-dictionary-order"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty
          , projectionCurrentOriginals = mempty
          , projectionEntry = SymbolIdentity (Text.pack "main") (Text.pack "RecoveredEntryCaller")
              (Text.pack "value") (Text.pack "applicativeDictionary") Nothing
          , projectionAuxiliaryRoots = [], projectionFormattingAuthority = Nothing
          , projectionTimeAuthority = Nothing, projectionJsonAuthority = Nothing
          , projectionTextUnit = Nothing }
    cache <- liftIO newFatIfaceCache
    dictionary <- case [identifier | identifier <- preparedTargetReferences context [prepared]
          , occurrence identifier == "$fApplicativeEff"] of
      [identifier] -> pure identifier
      _ -> fail "authored dictionary did not retain its exact package reference"
    exact <- liftIO $ recoverExactBody env cache dictionary
    liftIO $ case exact of
      ExactBody _ group -> putStrLn ("production dictionary input from defining fat Core: "
        ++ showSDocUnsafe (ppr group))
      _ -> fail "genuine dictionary exact body was unavailable"
    owners <- liftIO newOwnerInterfaceCache
    bodies <- liftIO newPreparedBodyCache
    closure <- liftIO $ recoverPreparedClosure env cache owners bodies context [prepared]
    recovered <- case [modul | modul <- closureModules closure
          , moduleNameString (moduleName (pmModule modul)) == "Control.Monad.Freer.Internal"] of
      [modul] -> pure modul
      _ -> fail ("genuine dictionary owner was not recovered: " ++ show (closureFailures closure))
    let owner = pmModule recovered
        wanted = Set.fromList [varName identifier
          | (Stg.StgTopLifted binding, _) <- pmBindings recovered
          , (identifier, _) <- stgPairs binding]
    (iface, _) <- liftIO $ readExactInterface env owner
      >>= either (const (fail "dictionary defining interface is unreadable")) pure
    ifaceBindings <- maybe (fail "dictionary owner has no canonical fat Core") pure (mi_extra_decls iface)
    original <- liftIO $ initIfaceCheck (text "dictionary order oracle") env $ do
      scope <- liftIO $ newIORef emptyTypeEnv
      initIfaceLcl owner (text "dictionary order oracle") NotBoot (tcTopIfaceBindings scope ifaceBindings)
    let selected = [group | group <- original
          , any ((`Set.member` wanted) . varName) (bindersOfBinds [group])]
        symbolOrder = sortOn (map (occNameString . nameOccName . varName) . bindersOfBinds . pure) selected
    liftIO $ do
      assert (any ((== "$fApplicativeEff") . occurrence) (bindersOfBinds selected))
        "authored dictionary did not retain its genuine Applicative instance"
      assert (any ((== "$fFunctorEff") . occurrence) (bindersOfBinds selected))
        "completed dictionary closure omitted its strict Functor superclass"
      putStrLn ("production recovered dictionary shapes: " ++ shapes recovered)
      putStrLn ("defining group order: " ++ show (map (map occurrence . bindersOfBinds . pure) selected))
      canonicalCache <- newPreparedBodyCache
      canonical <- prepareRecoveredBodies env owners canonicalCache owner selected >>= either (fail . show) pure
      sortedCache <- newPreparedBodyCache
      sorted <- prepareRecoveredBodies env owners sortedCache owner symbolOrder >>= either (fail . show) pure
      putStrLn ("canonical dictionary shapes: " ++ shapes canonical)
      putStrLn ("symbol-sorted dictionary shapes: " ++ shapes sorted)
      assert (isConstructor "$fApplicativeEff" canonical)
        "canonical defining order failed the genuine Applicative constructor entry"
      assert (isConstructor "$fApplicativeEff" recovered)
        "production recovered Applicative dictionary is not its canonical constructor entry"
      assert (isConstructor "$fApplicativeEff" sorted)
        "dictionary constructor entry depends on discovered group order"
      identities <- either (fail . show) pure (preparedTopIdentities [recovered])
      entry <- case [identity | identity <- identities,
          symbolOccurrence identity == Text.pack "$fApplicativeEff"] of
        [identity] -> pure identity
        _ -> fail "recovered dictionary lost its exact native identity"
      let dictionaryContext = context { projectionEntry = entry }
      native <- either (fail . ("original dictionary native projection failed: " ++) . show) pure
        (projectPreparedTarget dictionaryContext (closureModules closure))
      assert (any (emitsConstructor entry) (concatMap groupItems (programBindings native)))
        "original dictionary native projection did not emit its canonical constructor"
      recipe <- maybe (fail "dictionary refusal control lost its genuine DFun unfolding") pure
        (maybeUnfoldingTemplate (realIdUnfolding dictionary))
      recipeCache <- newPreparedBodyCache
      reconstructed <- prepareRecoveredBodies env owners recipeCache owner [NonRec dictionary recipe]
        >>= either (fail . show) pure
      case projectPreparedTarget dictionaryContext [reconstructed] of
        Left (RecoveredEntryContractMismatch symbol Nothing True
            (Just (Signature [] (Returns [LiftedRefRep]))) False)
          | symbol == entry -> pure ()
        Left failure -> fail ("reconstructed DFun recipe refused for the wrong reason: " ++ show failure)
        Right _ -> fail "optimizer DFun recipe replaced the canonical constructor entry"
  where
    occurrence = occNameString . nameOccName . varName
    stgPairs (Stg.StgNonRec binder rhs) = [(binder, rhs)]
    stgPairs (Stg.StgRec pairs) = pairs
    pairs modul = [pair | (Stg.StgTopLifted binding, _) <- pmBindings modul, pair <- stgPairs binding]
    isConstructor wanted modul = any (\(identifier, rhs) -> occurrence identifier == wanted
      && case rhs of Stg.StgRhsCon{} -> True; _ -> False) (pairs modul)
    shapes modul = intercalate "; " [occurrence identifier ++ "=" ++ showSDocUnsafe (ppr rhs)
      | (identifier, rhs) <- pairs modul, occurrence identifier `elem` ["$fApplicativeEff", "$fFunctorEff"]]
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops
    emitsConstructor wanted (TopBinding symbol (HeapBinding _ Constructor{})) = symbol == wanted
    emitsConstructor _ _ = False

-- Both controls use genuine finalized Core and package interface declarations.
-- Package decoding and optimization can change qApp's Core shape independently
-- of its canonical callable entry; a function-valued CAF must stay zero.
assertRecoveredEntryContracts :: IO ()
assertRecoveredEntryContracts = do
  root <- getCurrentDirectory
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (updOptLevel 0 flags)
      { importPaths = [root </> "test-prepared-stg", root </> "lib"] ++ importPaths flags
      , backend = noBackend, ghcLink = NoLink }
    target <- guessTarget (root </> "test-prepared-stg" </> "RecoveredEntryCaller.hs") Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "RecoveredEntryCaller")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    desugared <- desugarModule typed
    env <- getSession
    (guts, _) <- liftIO $ hscTidy env (coreModule desugared)
    home <- maybe (fail "entry fixture has no finalized home interface") pure
      (lookupHpt (hsc_HPT env) (ms_mod_name summary))
    prepared <- liftIO $ prepareModule env (ms_location summary) mempty (FinalizedModule home guts)
    let context = ProjectionContext
          { projectionProfile = Text.pack "recovered-entry-contracts"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty
          , projectionCurrentOriginals = mempty
          , projectionEntry = SymbolIdentity (Text.pack "main") (Text.pack "RecoveredEntryCaller")
              (Text.pack "value") (Text.pack "caller") Nothing
          , projectionAuxiliaryRoots = [], projectionFormattingAuthority = Nothing
          , projectionTimeAuthority = Nothing, projectionJsonAuthority = Nothing
          , projectionTextUnit = Nothing }
        references = preparedTargetReferences context [prepared]
    requested <- case [identifier | identifier <- references, occurrence identifier == "qApp"] of
      [identifier] -> pure identifier
      identifiers -> fail ("expected one genuine qApp reference, found " ++ show (length identifiers))
    cache <- liftIO newFatIfaceCache
    result <- liftIO $ recoverExactBody env cache requested
    (owner, body) <- case result of
      ExactBody modul group -> pure (modul, group)
      _ -> fail "genuine package qApp exact Core was unavailable"
    let rawSelected = [(identifier, rhs) | (identifier, rhs) <- concatMap bindPairs body,
          varName identifier == varName requested]
    liftIO $ case rawSelected of
      [(identifier, rhs)] -> putStrLn ("genuine qApp defining body: Id arity=" ++ show (idArity identifier)
        ++ ", manifest arity=" ++ show (manifestArity rhs))
      _ -> fail "qApp exact group did not retain exactly its selected binder"
    owners <- liftIO newOwnerInterfaceCache
    bodies <- liftIO newPreparedBodyCache
    recovered <- liftIO $ prepareRecoveredBodies env owners bodies owner body
      >>= either (fail . show) pure
    declaring <- liftIO $ lookupOwnerInterface owners owner
    original <- case [identifier | Just context <- [declaring], identifier <- Map.elems (ownerInterfaceEntries context),
          varName identifier == varName requested] of
      [identifier] -> pure identifier
      _ -> fail "the production owner did not retain qApp's defining declaration"
    required <- case importedIdLFInfo original of
      LFReEntrant _ arity _ _ -> pure arity
      _ -> fail "the genuine defining qApp interface is not callable"
    liftIO $ assert (required == 2 && idArity original == 2)
      "the pinned defining qApp interface no longer has its two-argument entry"
    liftIO $ assert (entryArity "qApp" recovered == Just (Stg.ReEntrant, required))
      "recovered qApp changed its original callable entry contract"
    qAppIdentities <- either (fail . show) pure (preparedTopIdentities [recovered])
    qAppEntry <- case [identity | identity <- qAppIdentities,
          symbolOccurrence identity == Text.pack "qApp"] of
      [identity] -> pure identity
      _ -> fail "recovered qApp did not retain its exact native identity"
    native <- either (fail . ("genuine qApp native projection failed: " ++) . show) pure
      (projectPreparedTarget (context { projectionEntry = qAppEntry }) [recovered])
    liftIO $ case
        [ signature | group <- programBindings native, top <- groupItems group
        , TopBinding symbol (HeapBinding _ (Function (SignatureId index) _ _ _)) <- [top]
        , symbol == qAppEntry
        , signature : _ <- [drop (fromIntegral index) (programSignatures native)] ] of
      [signature] -> assert (signatureArguments signature == replicate required LiftedRefRep)
        "recovered qApp native function does not offer its canonical entry arguments"
      _ -> fail "recovered qApp native projection did not emit its canonical function"
    let allPairs = concatMap bindPairs (cg_binds guts)
        caf = [(identifier, rhs) | (identifier, rhs) <- allPairs, occurrence identifier == "functionResult"]
    liftIO $ case caf of
      [(identifier, rhs)] -> assert (idArity identifier == 0 && manifestArity rhs == 0)
        "genuine function-result CAF control no longer has a zero-argument entry"
      _ -> fail "expected one genuine function-result CAF"
    subset <- liftIO $ prepareRecoveredModule env
      (RecoveredModuleInput (cg_module guts) (ms_location summary) (cg_tycons guts)
        (cg_binds guts) (Map.fromList [(varName binder,binder) | binder <- bindersOfBinds (cg_binds guts)]))
    liftIO $ case (entryArity "functionResult" prepared, entryArity "functionResult" subset) of
      (Just (original, 0), Just (recoveredUpdate, 0)) -> assert
        (original /= Stg.ReEntrant && recoveredUpdate == original)
        "the function-result CAF was eta-expanded into a callable"
      _ -> fail "function-result CAF entry mismatch"
  where
    occurrence = occNameString . nameOccName . varName
    bindPairs (NonRec binder body) = [(binder, body)]
    bindPairs (Rec pairs) = pairs
    entryArity wanted prepared = case
        [ (update, length parameters)
        | (Stg.StgTopLifted binding, _) <- pmBindings prepared
        , (identifier, Stg.StgRhsClosure _ _ update parameters _ _) <- stgPairs binding
        , occurrence identifier == wanted ] of
      [entry] -> Just entry
      _ -> Nothing
    stgPairs (Stg.StgNonRec binder rhs) = [(binder, rhs)]
    stgPairs (Stg.StgRec pairs) = pairs
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops

assertAllRecoveredBodies :: IO ()
assertAllRecoveredBodies = do
  root <- getCurrentDirectory
  let source = root </> "test-prepared-stg" </> "RecoveredBody.hs"
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (updOptLevel 0 flags)
      { importPaths = root : importPaths flags
      , backend = noBackend
      , ghcLink = NoLink
      }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    summary <- getModSummary (mkModuleName "RecoveredBody")
    parsed <- parseModule summary
    typed <- typecheckModule parsed
    desugared <- desugarModule typed
    hsc <- getSession
    (callerGuts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
    home <- maybe (fail "loaded caller has no finalized home interface") pure
      (lookupHpt (hsc_HPT hsc) (ms_mod_name summary))
    caller <- liftIO $ prepareModule hsc (ms_location summary) mempty
      (FinalizedModule home callerGuts)
    let entry = callerEntry caller
        context = ProjectionContext
          { projectionProfile = Text.pack "w5-b2-recovered-body"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
              (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty
          , projectionCurrentOriginals = mempty
          , projectionEntry = entry
          , projectionAuxiliaryRoots = []
          , projectionFormattingAuthority = Nothing
          , projectionTimeAuthority = Nothing
          , projectionJsonAuthority = Nothing
          , projectionTextUnit = Nothing
          }
        references = preparedTargetReferences context [caller]
    liftIO $ assert (any isFst references)
      ("-O0 caller did not retain a real package fst reference: "
        ++ intercalate ", " (map (showSDocUnsafe . ppr . idName) references))
    let fstIds = filter isFst references
    cache <- liftIO newFatIfaceCache
    ownerCache <- liftIO newOwnerInterfaceCache
    bodyCache <- liftIO newPreparedBodyCache
    recovered <- liftIO $ recoverFirst hsc cache fstIds
    case recovered of
      (fstId, ExactBody owner body) -> do
        preparedResult <- liftIO $ prepareRecoveredBodies hsc ownerCache bodyCache owner body
        recoveredModule <- case preparedResult of
          Left failure -> liftIO $ ioError (userError
            ("defining-context preparation failed: " ++ show failure))
          Right prepared -> pure prepared
        liftIO $ do
          assert (pmModule recoveredModule == owner)
            "recovered body was prepared under the caller module"
          assert (moduleNameString (moduleName owner) == recoveredModuleName fstId)
            "recovered body module identity did not come from the Id"
          case projectPreparedTarget context [caller, recoveredModule] of
            Left failure -> ioError (userError
              ("caller + exact body projection failed: " ++ show failure))
            Right program -> assert (hasRecoveredTop owner fstId program)
              "projection omitted the recovered defining top"
      (fstId, other) -> liftIO $ ioError (userError
        ("real package fst had no exact body (actual Id "
          ++ showSDocUnsafe (ppr (idName fstId)) ++ "): " ++ showLookup other))
  assertSemigroupSubset root libdir
  assertRecoveredKindRep root
  assertPatErrorBody root
  assertRaiseContracts root
  assertBottomingApplications root
  where
    callerEntry prepared = case
      [ identity
      | identity <- either (error . show) id (preparedTopIdentities [prepared])
      , symbolOccurrence identity == Text.pack "caller"
      ] of
      [identity] -> identity
      found -> error ("expected one caller entry, got " ++ show found)

    isFst identifier = occNameString (nameOccName (varName identifier)) == "fst"

    recoverFirst _ _ [] = error "recoverFirst called with no fst Id"
    recoverFirst hsc cache (identifier : rest) = do
      result <- recoverExactBody hsc cache identifier
      case result of
        exact@(ExactBody _ _) -> pure (identifier, exact)
        _ | null rest -> pure (identifier, result)
          | otherwise -> recoverFirst hsc cache rest

    recoveredModuleName identifier = case nameModule_maybe (varName identifier) of
      Just owner -> moduleNameString (moduleName owner)
      Nothing -> error "fst Id unexpectedly had no defining module"

    hasRecoveredTop owner identifier program = any matches
      [ symbol
      | group <- programBindings program
      , top <- groupItems group
      , symbol <- [topSymbol top]
      ]
      where
        wantedOccurrence = Text.pack (occNameString (nameOccName (varName identifier)))
        matches symbol = symbolModule symbol == Text.pack (moduleNameString (moduleName owner))
          && symbolOccurrence symbol == wantedOccurrence
        topSymbol (TopBinding symbol _) = symbol
        groupItems (NonRecursive top) = [top]
        groupItems (Recursive tops) = tops

    showLookup (ExactBody owner _) = "defining body in " ++ renderModule owner
    showLookup (MissingExactBody name reason) = "missing " ++ renderName name ++ ": " ++ show reason
    showLookup (BodyInterfaceFailure owner reason) = "interface failure in " ++ renderModule owner ++ ": " ++ reason
    showLookup (BodyTypeMismatch owner name requested candidate reason) =
      "type mismatch in " ++ renderModule owner ++ " for " ++ renderName name
        ++ ": " ++ requested ++ " vs " ++ candidate
        ++ "; " ++ reason
    showLookup (UnsupportedBodyCapability name) = "unsupported body " ++ renderName name

    renderModule = showSDocUnsafe . ppr
    renderName = showSDocUnsafe . ppr

trim :: String -> String
trim = reverse . dropWhile (== '\n') . reverse

readProcessGhc :: [String] -> IO String
readProcessGhc args = do
  (code, out, err) <- readCreateProcessWithExitCode (proc "ghc" args) ""
  case code of
    ExitSuccess -> pure out
    _ -> ioError (userError ("ghc failed: " ++ err))

assertSemigroupSubset :: FilePath -> String -> IO ()
assertSemigroupSubset root libdir = runGhc (Just libdir) $ do
  flags <- getSessionDynFlags
  _ <- setSessionDynFlags (updOptLevel 0 flags)
    { importPaths = [root </> "test-prepared-stg", root </> "lib"] ++ importPaths flags
    , backend = noBackend
    , ghcLink = NoLink
    }
  target <- guessTarget (root </> "test-prepared-stg" </> "RecoveredBody.hs") Nothing Nothing
  setTargets [target]
  _ <- load LoadAllTargets
  summary <- getModSummary (mkModuleName "RecoveredBody")
  parsed <- parseModule summary
  typed <- typecheckModule parsed
  desugared <- desugarModule typed
  hsc <- getSession
  (guts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
  home <- maybe (fail "loaded module has no finalized home interface") pure
    (lookupHpt (hsc_HPT hsc) (ms_mod_name summary))
  prepared <- liftIO $ prepareModule hsc (ms_location summary) mempty
      (FinalizedModule home guts)
  let context = ProjectionContext
        { projectionProfile = Text.pack "w5-b2-recovered-same-owner"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionCurrentOriginals = mempty
        , projectionEntry = SymbolIdentity
            (Text.pack "main") (Text.pack "RecoveredBody") (Text.pack "value")
            (Text.pack "foldableCaller") Nothing
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTimeAuthority = Nothing
        , projectionJsonAuthority = Nothing
        , projectionTextUnit = Nothing
        }
      references = preparedTargetReferences context [prepared]
      semigroupReferences = filter isSemigroupOwner references
      monoidProductReferences = filter isMonoidProduct semigroupReferences
  semigroupId <- case monoidProductReferences of
    [value] -> pure value
    found -> liftIO $ ioError (userError
      ("expected one defining semigroup reference, got " ++ show (length found)
        ++ ": " ++ intercalate ", " (map renderId found)))
  cache <- liftIO newFatIfaceCache
  ownerCache <- liftIO newOwnerInterfaceCache
  bodyCache <- liftIO newPreparedBodyCache
  lookupResult <- liftIO $ recoverExactBody hsc cache semigroupId
  (owner, body) <- case lookupResult of
    ExactBody owner group -> pure (owner, group)
    other -> liftIO $ ioError (userError
      ("semigroup reference was not recovered exactly: " ++ showLookup' other))
  recovered <- liftIO $ prepareRecoveredBodies hsc ownerCache bodyCache owner body
  recoveredModule <- case recovered of
    Right value -> pure value
    Left failure -> liftIO $ ioError (userError
      ("same-owner recovered subset did not prepare: " ++ show failure))
  let recoveredReferences = preparedTargetReferences context [prepared, recoveredModule]
      productOneReferences = filter isMonoidProductOne recoveredReferences
  productOneId <- case productOneReferences of
    [value] -> pure value
    found -> liftIO $ ioError (userError
      ("expected one recovered $fMonoidProduct1 reference, got "
        ++ show (length found) ++ ": " ++ intercalate ", " (map renderId found)))
  productOneLookup <- liftIO $ recoverExactBody hsc cache productOneId
  (productOneOwner, productOneBody) <- case productOneLookup of
    ExactBody owner' group -> pure (owner', group)
    other -> liftIO $ ioError (userError
      ("$fMonoidProduct1 was not recovered exactly: " ++ showLookup' other))
  liftIO $ assert (productOneOwner == owner)
    "$fMonoidProduct1 defining owner changed during recovery"
  productOnePrepared <- liftIO $ prepareRecoveredBodies hsc ownerCache bodyCache productOneOwner
    productOneBody
  productOneModule <- case productOnePrepared of
    Right value -> pure value
    Left failure -> liftIO $ ioError (userError
      ("$fMonoidProduct1 recovered subset did not prepare: " ++ show failure))
  let allRecovered = [prepared, recoveredModule, productOneModule]
      allReferences = preparedTargetReferences context allRecovered
  liftIO $ assert (any isStimes allReferences)
    ("stimesMonoid1 dependency disappeared from references: "
      ++ intercalate ", " (map renderId allReferences))
  liftIO $ evictOwnerInterfaceMatching ownerCache (== owner)
  evicted <- liftIO $ lookupOwnerInterface ownerCache owner
  liftIO $ case evicted of
    Nothing -> pure ()
    Just _ -> ioError (userError "request-boundary eviction retained the owner context")
  freshBodyCache <- liftIO newPreparedBodyCache
  afterEviction <- liftIO $ prepareRecoveredBodies hsc ownerCache freshBodyCache owner
    productOneBody
  liftIO $ case afterEviction of
    Right value -> assert (pmModule value == owner)
      "reloaded owner context changed the recovered defining owner"
    Left failure -> ioError (userError
      ("recovery after owner eviction failed: " ++ show failure))
  let missingOwner = pmModule prepared
      assertMissingInterface = do
        sourceHome <- prepareRecoveredBodies hsc ownerCache bodyCache missingOwner []
        case sourceHome of
          Left RecoveredModuleInterfaceFailure{} -> pure ()
          Left failure -> ioError (userError
            ("missing source-home interface reported wrong failure: " ++ show failure))
          Right _ -> ioError (userError
            "missing source-home interface was unexpectedly readable")
        failedContext <- lookupOwnerInterface ownerCache missingOwner
        case failedContext of
          Nothing -> pure ()
          Just _ -> ioError (userError "failed interface read was cached")
  liftIO $ assertMissingInterface >> assertMissingInterface
  where
    isSemigroupOwner identifier = case nameModule_maybe (varName identifier) of
      Just owner -> moduleNameString (moduleName owner)
        == "GHC.Internal.Data.Semigroup.Internal"
      Nothing -> False
    isStimes identifier = occNameString (nameOccName (varName identifier))
      == "stimesMonoid1"
    isMonoidProduct identifier = occNameString (nameOccName (varName identifier))
      == "$fMonoidProduct"
    isMonoidProductOne identifier = occNameString (nameOccName (varName identifier))
      == "$fMonoidProduct1"
    renderId identifier = showSDocUnsafe (ppr (idName identifier))
    showLookup' (ExactBody owner _) = "defining body in " ++ renderModule' owner
    showLookup' (MissingExactBody name reason) = "missing " ++ renderName' name ++ ": " ++ show reason
    showLookup' (BodyInterfaceFailure owner reason) = "interface failure in " ++ renderModule' owner ++ ": " ++ reason
    showLookup' (BodyTypeMismatch owner name requested candidate reason) =
      "type mismatch in " ++ renderModule' owner ++ " for " ++ renderName' name
        ++ ": " ++ requested ++ " vs " ++ candidate
        ++ "; " ++ reason
    showLookup' (UnsupportedBodyCapability name) = "unsupported body " ++ renderName' name
    renderModule' = showSDocUnsafe . ppr
    renderName' = showSDocUnsafe . ppr

-- GHC.Types:krep$* is an ordinary boxed strict-field constructor body whose
-- STG representation leaves the final PrimRep annotation undefined.  The
-- prepared facts walk must derive its layout from the actual constructor
-- arguments while recovering the real dependency closure for showDouble.
assertRecoveredKindRep :: FilePath -> IO ()
assertRecoveredKindRep root = do
  prepared <- runPipelineSelected PreparedStg
    (root </> "test" </> "Suite.hs") [root </> "lib"]
  let pipeline = pprPipelineResult prepared
      home = pprModules prepared
      entry = SymbolIdentity (Text.pack "main") (Text.pack "Suite")
        (Text.pack "value") (Text.pack "showDouble") Nothing
      context = ProjectionContext
        { projectionProfile = Text.pack "w5-recovered-krep"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionCurrentOriginals = mempty
        , projectionEntry = entry
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTimeAuthority = Nothing
        , projectionJsonAuthority = Nothing
        , projectionTextUnit = Nothing
        }
  cache <- newFatIfaceCache
  ownerCache <- newOwnerInterfaceCache
  bodyCache <- newPreparedBodyCache
  closure <- recoverPreparedClosure (prHscEnv pipeline) cache ownerCache bodyCache context home
  let modules = closureModules closure
      references = preparedTargetReferences context modules
  identities <- case preparedTopIdentities modules of
    Left failure -> ioError (userError
      ("recovered showDouble identities failed: " ++ show failure))
    Right values -> pure values
  _ <- evaluate (length references)
  assert (any isKrepTop identities)
    ("recovered showDouble closure lost GHC.Types:krep$*: " ++ show (closureFailures closure))
  (owner, krep) <- case
      [(pmModule modul, binder) | modul <- modules
      , (Stg.StgTopLifted binding, _) <- pmBindings modul
      , (binder, Stg.StgRhsCon{}) <- stgPairs binding
      , moduleNameString (moduleName (pmModule modul)) == "GHC.Types"
      , occurrence binder == "krep$*"] of
    [value] -> pure value
    _ -> fail "completed recovery did not preserve the genuine krep$* constructor"
  krepEntry <- case filter isKrepTop identities of
    [identity] -> pure identity
    _ -> fail "completed recovery did not retain one exact krep$* identity"
  let krepContext = context { projectionEntry = krepEntry }
  raw <- lookupFatIfaceExact (prHscEnv pipeline) cache (varName krep)
  group <- case raw of
    FatIfaceFound body -> pure body
    _ -> fail "genuine krep$* fat body was unavailable"
  partialCache <- newPreparedBodyCache
  partial <- prepareRecoveredBodies (prHscEnv pipeline) ownerCache partialCache owner group
    >>= either (fail . show) pure
  partialBinder <- case
      [binder | (Stg.StgTopLifted binding, _) <- pmBindings partial
      , (binder, Stg.StgRhsClosure _ _ update parameters _ _) <- stgPairs binding
      , occurrence binder == "krep$*", update /= Stg.ReEntrant, null parameters] of
    [binder] -> pure binder
    _ -> fail "partial krep$* did not exercise the genuine strict-sibling thunk"
  case importedIdLFInfo <$> preparedExpectedEntry partial partialBinder of
    Just LFCon{} -> pure ()
    _ -> fail "partial krep$* lost its exact canonical constructor expectation"
  assert (any ((== "krep$*1") . occurrence)
    (preparedTargetReferences krepContext [partial]))
    "partial constructor preparation hid its missing strict sibling"
  case projectPreparedTarget krepContext [partial] of
    Left (RecoveredEntryContractMismatch symbol Nothing True (Just (Signature [] (Returns [LiftedRefRep]))) False)
      | symbol == krepEntry -> pure ()
    result -> fail ("unresolved constructor did not refuse its exact final entry contract: " ++ show result)
  partialSelection <- either (fail . show) pure (prepareProjection krepContext [partial])
  partialCandidate <- either (fail . show) pure
    (projectSelectedCandidateWithHostBindings [] partialSelection)
  assert (any ((== Text.pack "krep$*1") . symbolOccurrence . globalIdentity)
      (candidateGlobals partialCandidate))
    "provisional candidate hid the strict sibling needed by package-root discovery"
  case finalizePreparedCandidate partialCandidate of
    Left (RecoveredEntryContractMismatch symbol Nothing True
        (Just (Signature [] (Returns [LiftedRefRep]))) False)
      | symbol == krepEntry -> pure ()
    Left failure -> fail ("provisional candidate did not retain its exact entry refusal: " ++ show failure)
    Right _ -> fail "provisional candidate admitted an unresolved constructor entry"
  complete <- either (fail . ("completed constructor projection failed: " ++) . show) pure
    (projectPreparedTarget krepContext modules)
  assert (any (isEmittedConstructor krepEntry) (concatMap groupItems (programBindings complete)))
    "completed recovery did not emit the selected constructor"
  completeSelection <- either (fail . show) pure (prepareProjection krepContext modules)
  completeCandidate <- either (fail . show) pure
    (projectSelectedCandidateWithHostBindings [] completeSelection)
  (finalized, _) <- either (fail . ("completed candidate finalization failed: " ++) . show) pure
    (finalizePreparedCandidate completeCandidate)
  assert (any (isEmittedConstructor krepEntry) (concatMap groupItems (programBindings finalized)))
    "completed candidate did not finalize the selected constructor"
  case projectPreparedTarget (krepContext
      { projectionRetainedGenerations = singletonRetained krepEntry }) [partial] of
    Left (MissingPreparedEntry symbol) | symbol == krepEntry -> pure ()
    result -> fail ("retained constructor did not omit its body before final validation: " ++ show result)
  projected <- evaluate (projectPreparedTarget context modules)
  case projected of
    Left _ -> pure ()
    Right program -> do
      _ <- evaluate (length (programBindings program))
      pure ()
  where
    isKrepTop symbol = symbolModule symbol == Text.pack "GHC.Types"
      && symbolOccurrence symbol == Text.pack "krep$*"
    occurrence = occNameString . nameOccName . varName
    stgPairs (Stg.StgNonRec binder rhs) = [(binder, rhs)]
    stgPairs (Stg.StgRec pairs) = pairs
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops
    isEmittedConstructor wanted (TopBinding symbol (HeapBinding _ Constructor{})) = symbol == wanted
    isEmittedConstructor _ _ = False
    singletonRetained identity = Map.singleton identity 1

-- Representation-polymorphic error workers must never let an incompatible
-- fat-interface body reach pre-CorePrep. This fixture records that patError has
-- no real unfolding, proves the raw fat candidate is incompatible, and then
-- requires typed recovery rejection under the defining owner.
assertPatErrorBody :: FilePath -> IO ()
assertPatErrorBody root = do
  prepared <- runPipelineSelected PreparedStg
    (root </> "test" </> "Suite.hs") [root </> "lib"]
  let pipeline = pprPipelineResult prepared
      home = pprModules prepared
      entry = SymbolIdentity (Text.pack "main") (Text.pack "Suite")
        (Text.pack "value") (Text.pack "qq_patch_invert_involution") Nothing
      context = ProjectionContext
        { projectionProfile = Text.pack "w5-pat-error-body"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionCurrentOriginals = mempty
        , projectionEntry = entry
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTimeAuthority = Nothing
        , projectionJsonAuthority = Nothing
        , projectionTextUnit = Nothing
        }
      patErrors = filter isPatError (preparedTargetReferences context home)
  patError <- case patErrors of
    [value] -> pure value
    found -> ioError (userError
      ("expected one recovered patError reference, got "
        ++ show (length found) ++ ": "
        ++ intercalate ", " (map renderId found)))
  cache <- newFatIfaceCache
  case maybeUnfoldingTemplate (realIdUnfolding patError) of
    Just _ -> ioError (userError
      "patError unexpectedly has a real unfolding; expected fat-interface recovery")
    Nothing -> pure ()
  fatLookup <- lookupFatIfaceExact (prHscEnv pipeline) cache (varName patError)
  fatGroup <- case fatLookup of
    FatIfaceFound group -> pure group
    FatIfaceMissing reason -> ioError (userError
      ("patError fat interface has no candidate: " ++ show reason))
    FatIfaceLoadFailure owner reason -> ioError (userError
      ("patError fat interface failed to load " ++ renderModule owner
        ++ ": " ++ reason))
  let fatPairs = concatMap bindPairs fatGroup
      selected = [ (binder, body)
                 | (binder, body) <- fatPairs
                 , varName binder == varName patError ]
  (fatBinder, fatBody) <- case selected of
    [pair] -> pure pair
    found -> ioError (userError
      ("patError fat group selected-binder count was " ++ show (length found)))
  let binderMismatch = not (eqType (idType patError) (idType fatBinder))
      rhsMismatch = not (eqType (idType fatBinder) (CoreUtils.exprType fatBody))
  assert (not binderMismatch)
    "patError fat-interface loader did not reuse the requested wired-in binder"
  assert rhsMismatch
    "patError fat-interface binder/RHS mismatch regression was not exercised"
  result <- recoverExactBody (prHscEnv pipeline) cache patError
  case result of
    BodyTypeMismatch owner name requested candidate _ -> do
      assert (isControlExceptionBase owner && name == varName patError)
        "typed patError mismatch named the wrong defining Id"
      assert (not (null requested) && not (null candidate))
        "typed patError mismatch omitted requested/candidate types"
    ExactBody owner _ -> ioError (userError
      ("patError fat candidate mismatch was accepted as exact body in " ++ renderModule owner))
    other -> ioError (userError
      ("patError recovery returned an untyped outcome: " ++ showLookup' other))
  where
    isPatError identifier =
      occNameString (nameOccName (varName identifier)) == "patError"
        && maybe False isControlExceptionBase (nameModule_maybe (varName identifier))
    isControlExceptionBase owner =
      moduleNameString (moduleName owner) == "GHC.Internal.Control.Exception.Base"
    bindPairs (NonRec binder body) = [(binder, body)]
    bindPairs (Rec pairs) = pairs
    renderId identifier = showSDocUnsafe (ppr (idName identifier))
    renderModule = showSDocUnsafe . ppr
    showLookup' (ExactBody owner _) =
      "defining body in " ++ renderModule owner
    showLookup' (MissingExactBody name reason) =
      "missing " ++ showSDocUnsafe (ppr name) ++ ": " ++ show reason
    showLookup' (BodyInterfaceFailure owner reason) =
      "interface failure in " ++ renderModule owner ++ ": " ++ reason
    showLookup' (BodyTypeMismatch owner name requested candidate reason) =
      "type mismatch in " ++ renderModule owner ++ " for "
        ++ showSDocUnsafe (ppr name) ++ ": " ++ requested ++ " vs "
        ++ candidate ++ "; " ++ reason
    showLookup' (UnsupportedBodyCapability name) =
      "unsupported body " ++ showSDocUnsafe (ppr name)

-- Bottoming primops carry NoSuccess independently of the demanded result
-- type.  Keep both the ordinary exception throw and GHC's divide-by-zero
-- sentinel in a real prepared-STG fixture so projection cannot silently
-- recover the old Returns contract from an Int result alone.
assertRaiseContracts :: FilePath -> IO ()
assertRaiseContracts root = do
  prepared <- runPipelineSelected PreparedStg
    (root </> "test-prepared-stg" </> "RaiseContract.hs")
    [root </> "test-prepared-stg"]
  let entry = SymbolIdentity (Text.pack "main") (Text.pack "RaiseContract")
        (Text.pack "value") (Text.pack "raisePrimitive") Nothing
      context = ProjectionContext
        { projectionProfile = Text.pack "w5-result-contract-raise"
        , projectionToolchain = Text.pack "ghc-9.12.2"
        , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
            (Text.pack "sysv64") []
        , projectionRetainedGenerations = mempty
        , projectionCurrentOriginals = mempty
        , projectionEntry = entry
        , projectionAuxiliaryRoots = []
        , projectionFormattingAuthority = Nothing
        , projectionTimeAuthority = Nothing
        , projectionJsonAuthority = Nothing
        , projectionTextUnit = Nothing
        }
  program <- case projectPreparedTarget context (pprModules prepared) of
    Left failure -> ioError (userError
      ("raise-contract projection failed: " ++ show failure))
    Right value -> pure value
  let signatureAt (SignatureId value) =
        programSignatures program !! fromIntegral value
      signatureResult signature = signatureResults (signatureAt signature)
      operationResult (OperationId value) =
        let OperationDecl _ signature =
              programOperations program !! fromIntegral value
        in signatureResult signature
      topRhs =
        [ heapBindingRhs binding
        | group <- programBindings program
        , TopBinding symbol binding <- groupItems group
        , symbolOccurrence symbol == Text.pack "raisePrimitive"
        ]
  case topRhs of
    [rhs] -> do
      let entryResult = case rhs of
            Function signature _ _ _ -> signatureResults (signatureAt signature)
            Thunk signature _ _ _ -> signatureResults (signatureAt signature)
            other -> error ("raisePrimitive has non-executable RHS: " ++ show other)
      assert (entryResult == NoSuccess)
        ("zero-argument bottoming thunk entry was not NoSuccess: " ++ show entryResult)
      -- The contract is that nothing can follow the raise: the body demands a
      -- NoSuccess operation, either directly or as the scrutinee of a case
      -- with no alternatives (the shape projection may give a demanded raise).
      let nonReturningOperation expression = case expression of
            Operation operation _ -> Just operation
            Case scrutinee _ _ _ [] -> nonReturningOperation scrutinee
            _ -> Nothing
          body = case rhs of
            Function _ _ _ expression -> Just expression
            Thunk _ _ _ expression -> Just expression
            _ -> Nothing
      case body >>= nonReturningOperation of
        Just operation -> assert
          (operationResult operation == NoSuccess)
          "zero-argument bottoming thunk did not retain NoSuccess at its operation"
        Nothing -> ioError (userError
          ("raisePrimitive does not end in a non-returning operation: " ++ show rhs))
    found -> ioError (userError
      ("expected one raisePrimitive top, got " ++ show (length found)))
  let signatures = programSignatures program
      raised =
        [ (name, signatureResults (signatures !! fromIntegral (unSignatureId signature)))
        | OperationDecl (PrimOpIdentity name) signature <- programOperations program
        , name == Text.pack "raise#" || name == Text.pack "raiseDivZero#"
        ]
  case [result | (name, result) <- raised, name == Text.pack "raise#"] of
    [NoSuccess] -> pure ()
    found -> ioError (userError
      ("raise-contract raise# did not preserve NoSuccess: " ++ show found
        ++ "; all operations: " ++ show (programOperations program)))
  where
    unSignatureId (SignatureId value) = value
    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items

assertBottomingApplications :: FilePath -> IO ()
assertBottomingApplications root = do
  partial <- projectEntry "bottomingPartial"
  assertPartialBottoming partial
  called <- projectEntry "bottomingCalled"
  assertSaturatedBottoming called "bottomingCalled" "$wbottomingUnary" [IntRep 64]
  tupleCalled <- projectEntry "bottomingTupleCalled"
  assertSaturatedBottoming tupleCalled "bottomingTupleCalled" "bottomingTuple"
    [IntRep 64, FloatRep 64]
  voidCalled <- projectEntry "bottomingVoidCalled"
  assertSaturatedBottoming voidCalled "bottomingVoidCalled" "bottomingVoid" [VoidRep]
  where
    projectEntry occurrence = do
      prepared <- runPipelineSelected PreparedStg
        (root </> "test-prepared-stg" </> "RaiseContract.hs")
        [root </> "test-prepared-stg"]
      let context = ProjectionContext
            { projectionProfile = Text.pack "w5-result-contract-arity"
            , projectionToolchain = Text.pack "ghc-9.12.2"
            , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
                (Text.pack "sysv64") []
            , projectionRetainedGenerations = mempty
            , projectionCurrentOriginals = mempty
            , projectionEntry = SymbolIdentity (Text.pack "main")
                (Text.pack "RaiseContract") (Text.pack "value")
                (Text.pack occurrence) Nothing
            , projectionAuxiliaryRoots = []
            , projectionFormattingAuthority = Nothing
            , projectionTimeAuthority = Nothing
            , projectionJsonAuthority = Nothing
            , projectionTextUnit = Nothing
            }
          modules = if occurrence == "bottomingPartial"
            then map preservePartialCall (pprModules prepared)
            else pprModules prepared
      case projectPreparedTarget context modules of
        Left failure -> ioError (userError
          ("bottoming " ++ occurrence ++ " projection failed: " ++ show failure))
        Right program -> pure program

    -- CorePrep eta-expands the fixture's PAP into a one-argument `sat`
    -- closure. Restore the same worker application with one supplied argument
    -- so projection is tested at the unsaturated call boundary.
    preservePartialCall prepared = prepared
      { preparedBindings = bindings
      }
      where
        bindings = map restore (pmBindings prepared)
        restore (Stg.StgTopLifted (Stg.StgNonRec binder
                 (Stg.StgRhsClosure captures ccs update [_]
                   (Stg.StgCase _ _ _ [Stg.GenStgAlt _ _
                     (Stg.StgApp worker [first, _])]) _)), annotations)
          | occNameString (nameOccName (varName binder)) == "sat"
          , occNameString (nameOccName (varName worker)) == "$wbottomingBinary" =
              (Stg.StgTopLifted (Stg.StgNonRec binder
                (Stg.StgRhsClosure captures ccs update []
                  (Stg.StgApp worker [first]) (varType binder))), annotations)
        restore (Stg.StgTopLifted (Stg.StgNonRec binder rhs), _)
          | occNameString (nameOccName (varName binder)) == "sat" =
              error ("unexpected prepared sat: " ++ showSDocUnsafe (ppr rhs))
        restore binding = binding

    assertPartialBottoming program = do
      let consumerCalls = allCalls (topBody program "bottomingPartial")
      assert (any isConsumerCall consumerCalls)
        ("bottomingPartial did not pass the PAP closure to partialConsumer: "
          ++ show consumerCalls)
      let entry = signatureAt program (topSignature program "$wbottomingBinary")
      assert (signatureArguments entry == [IntRep 64, IntRep 64]
          && signatureResults entry == NoSuccess)
        ("$wbottomingBinary entry did not retain its two-argument bottoming contract: "
          ++ show entry)
      let partialCalls =
            [ (callee, signatureAt program signature, arguments)
            | (callee, signature, arguments) <- allCalls (topBody program "sat")
            ]
      assert (any isPartialCall partialCalls)
        ("bottomingPartial did not retain a partial Call node: " ++ show partialCalls)
      where
        isConsumerCall (callee, _, arguments) =
          callee == Ref (Local (topId program "partialConsumer"))
            && arguments == [Ref (Local (topId program "sat"))]
        isPartialCall (callee, signature, arguments) =
          callee == Ref (Local (topId program "$wbottomingBinary"))
            && length arguments == 1
            && length arguments < length (signatureArguments
                 (signatureAt program (topSignature program "$wbottomingBinary")))
            && signatureArguments signature == [IntRep 64]
            && signatureResults signature == Returns [LiftedRefRep]

    assertSaturatedBottoming program occurrence calleeName expectedArguments = do
      assertTopResultContract program occurrence NoSuccess
      let calleeEntry = signatureAt program (topSignature program calleeName)
      assert (signatureArguments calleeEntry == expectedArguments
          && signatureResults calleeEntry == NoSuccess)
        (calleeName ++ " entry did not retain the expected bottoming arity: "
          ++ show calleeEntry)
      let calls =
            [ (callee, signatureAt program signature, arguments)
            | (callee, signature, arguments) <- allCalls (topBody program occurrence)
            ]
          matching =
            [ (callee, signature, arguments)
            | (callee, signature, arguments) <- calls
            , callee == Ref (Local (topId program calleeName))
            , signatureArguments signature == expectedArguments
            , signatureResults signature == NoSuccess
            ]
      case matching of
        [(_, _, arguments)] -> assert (length arguments == length expectedArguments)
          (occurrence ++ " call argument count disagrees with its signature")
        [] -> ioError (userError
          (occurrence ++ " did not retain a projected saturated Call with expected signature; calls: "
            ++ show calls))
        found -> ioError (userError
          (occurrence ++ " retained multiple matching saturated Calls: " ++ show found))

    topBody program occurrence = case
      [heapBindingRhs binding
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == Text.pack occurrence
      ] of
      [Function _ _ _ body] -> body
      [Thunk _ _ _ body] -> body
      [rhs] -> error (occurrence ++ " has non-executable RHS: " ++ show rhs)
      found -> error ("expected one " ++ occurrence ++ " top, got " ++ show (length found))

    topId program occurrence = case
      [heapBindingId binding
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == Text.pack occurrence
      ] of
      [identifier] -> identifier
      found -> error ("expected one " ++ occurrence ++ " top Id, got " ++ show found
        ++ "; tops: " ++ show (topNames program))

    topSignature program occurrence = case
      [rhsSignature (heapBindingRhs binding)
      | group <- programBindings program
      , TopBinding symbol binding <- groupItems group
      , symbolOccurrence symbol == Text.pack occurrence
      ] of
      [signature] -> signature
      found -> error ("expected one " ++ occurrence ++ " top signature, got " ++ show found
        ++ "; tops: " ++ show (topNames program))

    topNames program =
      [symbolOccurrence symbol
      | group <- programBindings program
      , TopBinding symbol _ <- groupItems group
      ]

    assertTopResultContract program occurrence expected =
      case [signature
           | group <- programBindings program
           , TopBinding symbol binding <- groupItems group
           , symbolOccurrence symbol == Text.pack occurrence
           , signature <- [rhsSignature (heapBindingRhs binding)]
           ] of
        [signature] -> assert (signatureResult program signature == expected)
          (occurrence ++ " entry contract was " ++ show (signatureAt program signature)
            ++ ", expected " ++ show expected)
        found -> ioError (userError
          ("expected one executable " ++ occurrence ++ " top, got " ++ show (length found)))

    rhsSignature (Function signature _ _ _) = signature
    rhsSignature (Thunk signature _ _ _) = signature
    rhsSignature rhs = error ("non-executable RHS has no signature: " ++ show rhs)

    allCalls expression = case expression of
      Call callee signature arguments -> (callee, signature, arguments)
        : []
      Case scrutinee _ _ _ alternatives ->
        allCalls scrutinee <> concatMap (allCalls . alternativeBody) alternatives
      Let group body -> allHeap group <> allCalls body
      LetJoins group body -> allJoin group <> allCalls body
      _ -> []
      where
        alternativeBody (Alternative _ _ body) = body
        allHeap (NonRecursive binding) = allCalls (heapBody binding)
        allHeap (Recursive bindings) = concatMap (allCalls . heapBody) bindings
        allJoin (NonRecursive binding) = allCalls (joinBody binding)
        allJoin (Recursive bindings) = concatMap (allCalls . joinBody) bindings
        heapBody (HeapBinding _ (Function _ _ _ body)) = body
        heapBody (HeapBinding _ (Thunk _ _ _ body)) = body
        heapBody _ = Return []
        joinBody (JoinBinding _ _ _ body) = body

    signatureAt program (SignatureId value) =
      programSignatures program !! fromIntegral value
    signatureResult program signature = signatureResults (signatureAt program signature)

    groupItems (NonRecursive item) = [item]
    groupItems (Recursive items) = items
