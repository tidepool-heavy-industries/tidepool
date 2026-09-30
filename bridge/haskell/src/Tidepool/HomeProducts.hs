-- | Revalidate immutable home products in one fresh compiler transaction.
-- Boot declarations are source inputs, not executable products. Their HMIs
-- are rebuilt for this transaction and selected only by SOURCE importers.
module Tidepool.HomeProducts
  ( hydrateCandidateHomeProducts ) where

import Control.Exception
  ( SomeException, SomeAsyncException, displayException, fromException, throwIO, try )
import Control.Monad (forM, unless)
import Control.Monad.IO.Class (liftIO)
import Data.Map.Strict qualified as Map
import Data.IORef (readIORef)
import GHC
  ( Ghc, ModSummary(..), getSession, parseModule, setSession, typecheckModule
  , tm_internals_, unLoc, ms_mod_name, SafeHaskellMode(Sf_None) )
import GHC.Driver.Env
  ( HscEnv(..), hscUpdateFlags, hscUpdateHPT )
import GHC.Driver.Monad (reflectGhc, reifyGhc)
import GHC.Iface.Make (mkIfaceTc)
import GHC.Iface.Recomp (MaybeValidated(..), checkOldIface)
import GHC.Iface.Tidy (mkBootModDetailsTc)
import GHC.Driver.Session (GeneralFlag(Opt_Pp), gopt, xopt)
import GHC.LanguageExtensions.Type qualified as LangExt
import GHC.Tc.Types (tcg_dependent_files)
import GHC.Types.SourceFile (HscSource(..))
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), addToHpt, emptyHomeModInfoLinkable )
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Module (moduleName)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact, freshExactState, hydrateExactScope )
import Tidepool.RetainedUnfoldings (scopeRetainedHscEnv)

-- Every ordinary summary and every boot summary is from the current
-- downsweep. The caller has already checked source/package witnesses and
-- closed the candidate set over all selected home imports, including SOURCE.
-- No target summary belongs to this set.
hydrateCandidateHomeProducts
  :: HscEnv -> [(ExactIfaceArtifact, ModIface)] -> [ModSummary] -> [ModSummary]
  -> Ghc (Either String HscEnv)
hydrateCandidateHomeProducts initial interfaces summaries boots = reifyGhc $ \session -> do
  result <- try (reflectGhc hydrate session)
  case result of
    Left failure | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
    Left (failure :: SomeException) -> do
      -- Typechecking may have populated mutable EPS/finder state before the
      -- refusal. Ordinary source fallback starts from empty mutable tables.
      fresh <- freshExactState initial
      reflectGhc (setSession fresh) session
      pure (Left (displayException failure))
    Right environment -> pure (Right environment)
  where
    hydrate = do
      ordinary <- liftIO (hydrateExactScope initial interfaces)
      setSession ordinary
      bootEntries <- forM boots $ \summary -> do
        unless (ms_hsc_src summary == HsBootFile) $
          liftIO (ioError (userError "cached home boot input is not a boot summary"))
        unless (not (gopt Opt_Pp (ms_hspp_opts summary)) && not (any
            (`xopt` ms_hspp_opts summary)
            [LangExt.Cpp, LangExt.TemplateHaskell, LangExt.QuasiQuotes])) $
          liftIO (ioError (userError "cached home boot input has untracked compile-time inputs"))
        parsed <- parseModule summary
        typed <- typecheckModule parsed
        current <- getSession
        let tcg = fst (tm_internals_ typed)
            scoped = scopeRetainedHscEnv (ms_mod summary)
              (hscUpdateFlags (const (ms_hspp_opts summary)) current)
        dependentFiles <- liftIO (readIORef (tcg_dependent_files tcg))
        unless (null dependentFiles) $
          liftIO (ioError (userError "cached home boot input read untracked dependent files"))
        details <- liftIO (mkBootModDetailsTc (hsc_logger scoped) tcg)
        iface <- liftIO (mkIfaceTc scoped Sf_None details summary Nothing tcg)
        pure (ms_mod_name summary, HomeModInfo iface details emptyHomeModInfoLinkable)
      current <- getSession
      let bootTable = Map.fromList bootEntries
          scopedFor summary = hscUpdateHPT (\hpt -> foldr
            (\(_, imported) table -> case Map.lookup (unLoc imported) bootTable of
                Just hmi -> addToHpt table (unLoc imported) hmi
                Nothing -> table)
            hpt (ms_srcimps summary)) current
          ordinaryByName = Map.fromList
            [(moduleName (mi_module iface), iface) | (_, iface) <- interfaces]
      unless (length bootEntries == Map.size bootTable) $
        liftIO (ioError (userError "duplicate cached home boot owner"))
      accepted <- liftIO $ forM summaries $ \summary -> case Map.lookup
          (ms_mod_name summary) ordinaryByName of
        Nothing -> pure False
        Just iface -> do
          let allBootsPresent = all (\(_, imported) -> Map.member (unLoc imported) bootTable)
                (ms_srcimps summary)
          if not allBootsPresent then pure False else do
            decision <- checkOldIface (scopeRetainedHscEnv (ms_mod summary) (scopedFor summary))
              summary (Just iface)
            pure $ case decision of
              UpToDateItem _ -> True
              OutOfDateItem _ _ -> False
      unless (and accepted) $
        liftIO (ioError (userError "cached home product failed fresh SOURCE/interface validation"))
      setSession current
      pure current
