module Tidepool.GhcPipeline (runPipeline, PipelineResult(..), dumpCore) where

import GHC
import GHC.Driver.Main (hscDesugar)
import GHC.Core.Opt.Pipeline (core2core)
import GHC.Core.Ppr (pprCoreBindings)
import GHC.Driver.Session (updOptLevel, gopt_set, gopt_unset, GeneralFlag(..))
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Core (CoreBind)
import GHC.Utils.Outputable (renderWithContext, defaultSDocContext)
import System.Process (readProcess)
import System.Environment (lookupEnv)
import System.FilePath (takeBaseName)
import Control.Monad.IO.Class (liftIO)
import Control.Monad (forM, when)
import Data.Char (toUpper)

data PipelineResult = PipelineResult
  { prBinds  :: [CoreBind]
  , prTyCons :: [TyCon]
  , prHscEnv :: HscEnv
  }

runPipeline :: FilePath -> [FilePath] -> IO PipelineResult
runPipeline path includes = do
  libdir <- getLibdir
  runGhc (Just libdir) $ do
    dflags <- getSessionDynFlags
    let dflags' = gopt_set (gopt_set (gopt_unset (gopt_unset (updOptLevel 2 $ dflags
          { backend = noBackend
          , ghcLink = NoLink
          , importPaths = importPaths dflags ++ includes
          }) Opt_FullLaziness) Opt_CprAnal)
          Opt_ExposeAllUnfoldings) Opt_ExposeOverloadedUnfoldings
    setSessionDynFlags dflags'
    target <- guessTarget path Nothing Nothing
    setTargets [target]
    _ <- load LoadAllTargets
    modGraph <- getModuleGraph
    let summaries = mgModSummaries modGraph
    when (null summaries) $
      liftIO $ ioError (userError "runPipeline: empty module graph")
    -- Process all modules: parse, typecheck, desugar, optimize each
    results <- forM summaries $ \modSum -> do
      parsed <- parseModule modSum
      typechecked <- typecheckModule parsed
      hscEnv <- getSession
      let tcGblEnv = fst (tm_internals_ typechecked)
      desugared <- liftIO $ hscDesugar hscEnv modSum tcGblEnv
      simplified <- liftIO $ core2core hscEnv desugared
      return simplified
    -- Merge: dependency module bindings first, target module last
    let targetModName = capitalize (takeBaseName path)
        isTargetMod g = moduleNameString (moduleName (mg_module g)) == targetModName
        (targetGuts, depGuts) = case filter isTargetMod results of
          (tgt:_) -> (tgt, [g | g <- results, mg_module g /= mg_module tgt])
          []       -> (head results, tail results)
        allBinds = concatMap mg_binds depGuts ++ mg_binds targetGuts
        allTyCons = concatMap mg_tcs depGuts ++ mg_tcs targetGuts
    hscEnv <- getSession
    return PipelineResult
      { prBinds  = allBinds
      , prTyCons = allTyCons
      , prHscEnv = hscEnv
      }

capitalize :: String -> String
capitalize [] = []
capitalize (c:cs) = toUpper c : cs

dumpCore :: [CoreBind] -> String
dumpCore binds = renderWithContext defaultSDocContext (pprCoreBindings binds)

getLibdir :: IO FilePath
getLibdir = do
  envDir <- lookupEnv "TIDEPOOL_GHC_LIBDIR"
  case envDir of
    Just dir -> pure dir
    Nothing  -> trim <$> readProcess "ghc" ["--print-libdir"] ""
  where trim = reverse . dropWhile (== '\n') . reverse
