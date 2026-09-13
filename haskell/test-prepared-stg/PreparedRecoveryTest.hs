{-# LANGUAGE GADTs #-}

module Main (main) where

import Control.Monad (unless)
import Control.Monad.IO.Class (liftIO)
import Data.Text qualified as Text
import GHC
import GHC.Driver.Main (hscTidy)
import GHC.Driver.Session (updOptLevel)
import GHC.Builtin.Types (intTy)
import GHC.Core (Bind(..), Expr(..))
import GHC.Types.Id (mkVanillaGlobal)
import GHC.Types.Name (nameOccName)
import GHC.Types.Name (mkSystemName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Name.Occurrence (mkVarOcc)
import GHC.Types.Unique (mkUnique)
import GHC.Types.Var (varName)
import GHC.Stg.Syntax qualified as Stg
import System.Directory (getCurrentDirectory)
import System.Exit (ExitCode(..))
import System.FilePath ((</>))
import System.Process (proc, readCreateProcessWithExitCode)
import Tidepool.ExecutionProjection
  ( ProjectionContext(..), ProjectionError(..), preparedTopIdentities
  , projectPreparedTarget )
import Tidepool.ExecutionSchema
  ( Architecture(..), Endianness(..), SymbolIdentity(..), TargetDescriptor(..) )
import Tidepool.ExecutionSchema (GlobalDecl(..), WireProgram(..))
import Tidepool.PreparedRecovery
  ( RecoveryFailure(..), RecoveredClosure(..), insertGroup
  , recoverPreparedClosure )
import Tidepool.PreparedStg
  ( PreparedCoverage(..), PreparedModule(..), prepareModule, unelaboratedModule )

assert :: Bool -> String -> IO ()
assert ok message = unless ok (ioError (userError message))

main :: IO ()
main = do
  assertOverlapMerge
  assertSubsetPreservesFullGroup
  root <- getCurrentDirectory
  let source = root </> "test-prepared-stg" </> "RecoveryCaller.hs"
      includes = [root </> "test-prepared-stg"]
  libdir <- trim <$> readProcessGhc ["--print-libdir"]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    _ <- setSessionDynFlags (updOptLevel 0 flags)
      { importPaths = includes
      , backend = noBackend
      , ghcLink = NoLink
      }
    target <- guessTarget source Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    hsc <- getSession
    home <- prepareNamed hsc "RecoveryHome"
    caller <- prepareNamed hsc "RecoveryCaller"
    let modules = [home, caller]
        entry = callerEntry modules
        context = ProjectionContext
          { projectionProfile = Text.pack "w5-b3-prepared-recovery"
          , projectionToolchain = Text.pack "ghc-9.12.2"
          , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64
              (Text.pack "sysv64") []
          , projectionRetainedGenerations = mempty
          , projectionEntry = entry
          }
    closure <- liftIO $ recoverPreparedClosure hsc context modules
    liftIO $ assert (any recoveredFst (closureModules closure))
      "closure did not retain the newly prepared defining module for fst"
    liftIO $ assert (any namedResidual (closureFailures closure))
      ("closure omitted the named residual: " ++ show (closureFailures closure))
    liftIO $ assert (all (\original -> originalModuleRetained original (closureModules closure)) modules)
      "recovery dropped an original prepared module"
    liftIO $ incompleteSubsetContract home context
    liftIO $ putStrLn "prepared recovery closure: ok"
  where
    assertOverlapMerge = do
      let a = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991001) (mkVarOcc "a")) intTy
          b = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991002) (mkVarOcc "b")) intTy
          c = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991003) (mkVarOcc "c")) intTy
          d = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991004) (mkVarOcc "d")) intTy
          e = mkVanillaGlobal (mkSystemName (mkUnique 'b' 991005) (mkVarOcc "e")) intTy
          previous = [ Rec [(a, Var b), (b, Var a)]
                     , Rec [(c, Var d), (d, Var c)] ]
          incoming = Rec [(a, Var c), (c, Var a), (e, Var e)]
          merged = insertGroup incoming previous
      case merged of
        [Rec pairs] -> assert (all (`elem` map (occNameString . nameOccName . varName . fst) pairs)
          ["a", "b", "c", "d", "e"])
          ("overlapping merge dropped a sibling: " ++ show (map (occNameString . nameOccName . varName . fst) pairs))
        other -> ioError (userError ("overlapping groups were not merged: " ++ show (length other)))

    assertSubsetPreservesFullGroup = do
      let a = mkVanillaGlobal (mkSystemName (mkUnique 'b' 992001) (mkVarOcc "a")) intTy
          b = mkVanillaGlobal (mkSystemName (mkUnique 'b' 992002) (mkVarOcc "b")) intTy
          c = mkVanillaGlobal (mkSystemName (mkUnique 'b' 992003) (mkVarOcc "c")) intTy
          existing = [Rec [(a, Var b), (b, Var a)]]
          incoming = NonRec a (Var c)
      case insertGroup incoming existing of
        [Rec pairs] -> do
          assert (map (occNameString . nameOccName . varName . fst) pairs == ["a", "b"])
            "subset lookup dropped a sibling from the full Rec group"
          case [rhs | (binder, rhs) <- pairs, varName binder == varName a] of
            [Var reference] -> assert (varName reference == varName b)
              "subset lookup replaced the authoritative full-group body"
            _ -> ioError (userError "subset lookup did not retain an a body")
        other -> ioError (userError
          ("subset lookup changed the full Rec group shape: " ++ show (length other)))


    prepareNamed hsc name = do
      summary <- getModSummary (mkModuleName name)
      parsed <- parseModule summary
      typed <- typecheckModule parsed
      desugared <- desugarModule typed
      (guts, _) <- liftIO $ hscTidy hsc (coreModule desugared)
      liftIO $ prepareModule hsc summary (unelaboratedModule guts)

    trim = reverse . dropWhile (== '\n') . reverse

    readProcessGhc args = do
      (code, out, err) <- readCreateProcessWithExitCode (proc "ghc" args) ""
      case code of
        ExitSuccess -> pure out
        _ -> ioError (userError ("ghc failed: " ++ err))

    callerEntry modules = case
      [ identity
      | identity <- either (error . show) id (preparedTopIdentities modules)
      , symbolOccurrence identity == Text.pack "caller"
      ] of
      [identity] -> identity
      found -> error ("expected one caller entry, got " ++ show found)

    recoveredFst prepared =
      moduleNameString (moduleName (pmModule prepared)) /= "RecoveryHome"
      && moduleNameString (moduleName (pmModule prepared)) /= "RecoveryCaller"
      && any topIsFst (pmBindings prepared)

    topIsFst (binding, _) = any
      ((== "fst") . occNameString . nameOccName . varName)
      (topBinders binding)

    namedResidual (UnsupportedExternalCapability name) =
      occNameString (nameOccName name) == "()"
    namedResidual _ = False

    originalModuleRetained original recovered =
      any ((== pmModule original) . pmModule) recovered

    incompleteSubsetContract home context = do
      homeOtherEntry <- case
        [ identity
        | identity <- either (error . show) id (preparedTopIdentities [home])
        , symbolOccurrence identity == Text.pack "homeOther"
        ] of
        [identity] -> pure identity
        found -> ioError (userError ("expected one homeOther entry, got " ++ show found))
      let subset = home
            { pmCoverage = ExactBodySubset
            , pmBindings = filter hasHomeOther (pmBindings home)
            }
          subsetContext = context { projectionEntry = homeOtherEntry }
      case projectPreparedTarget subsetContext [subset] of
        Left failure -> ioError (userError
          ("incomplete subset rejected its genuine external global: " ++ show failure))
        Right program -> assert
          (any ((== Text.pack "homeValue") . symbolOccurrence . globalIdentity)
            (programGlobals program))
          "incomplete subset dropped its unresolved homeValue global"
      let complete = subset { pmCoverage = CompleteSourceModule }
      case projectPreparedTarget subsetContext [complete] of
        Left (MissingPreparedTop _) -> pure ()
        Left failure -> ioError (userError
          ("complete source reported the wrong missing-top failure: " ++ show failure))
        Right _ -> ioError (userError
          "complete source with a missing top unexpectedly projected")
      where
        hasHomeOther (Stg.StgTopLifted binding, _) =
          any ((== "homeOther") . occNameString . nameOccName . varName)
            (bindingBinders binding)
        hasHomeOther _ = False

    topBinders (Stg.StgTopStringLit binder _) = [binder]
    topBinders (Stg.StgTopLifted binding) = bindingBinders binding

    bindingBinders (Stg.StgNonRec binder _) = [binder]
    bindingBinders (Stg.StgRec pairs) = map fst pairs
