module ProgressBoundaryTest (progressBoundaryChecks) where

import Control.Exception (bracket)
import Control.Monad (forM_, unless)
import Data.List (isInfixOf)
import Data.Maybe (isJust)
import Data.Text qualified as T
import GHC.Types.Name (getOccString)
import GHC.Unit.Module (moduleName, moduleNameString)
import System.Directory (copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.EffectSchema (YieldSite(..), SiteType(..))
import Tidepool.GhcPipeline (PipelineSelection(..), PreparedPipelineResult(..), runPipelineSelected)
import Tidepool.PreparedSites (SiteRejection(..))
import Tidepool.PreparedStg (PreparedModule(..))

progressBoundaryChecks :: FilePath -> IO ()
progressBoundaryChecks effects = bracket scratch removeDirectoryRecursive $ \work -> do
  let target = work </> "ProgressBoundary.hs"
  copyFile "test-source-boot/fixtures/ProgressBoundary.hs" target
  first <- runPipelineSelected (PreparedProducts Nothing) target [work, "lib", effects]
  issued <- case [site | owner <- pprModules first, site <- pmYieldSites owner,
        "ProgressBoundary.publish" `T.isInfixOf` ysOrigin site] of
    site : _ -> pure (ysSite site)
    [] -> fail "copied-site regression has no genuine nominal publisher site"
  source <- T.pack <$> readFile target
  writeFile target (T.unpack (T.replace "1 {- copied-site -}" (T.pack (show issued)) source))
  result <- runPipelineSelected (PreparedProducts Nothing) target [work, "lib", effects]
  unless (any ((== issued) . ysSite) (concatMap pmYieldSites (pprModules result))) $
    fail "copied-site regression no longer names an actual concrete site"
  prepared <- case filter ((== "ProgressBoundary") . moduleNameString . moduleName . pmModule) (pprModules result) of
    [value] -> pure value
    _ -> fail "progress fixture has no original prepared module"
  let rejections = [(getOccString (srBinder rejection), srMessage rejection) | rejection <- pmSiteRejections prepared]
      sites = pmYieldSites prepared
      protected = ["rawPublish", "rawObserve", "rawWatch", "bareRaw", "partialRaw", "tickedRaw", "castedRaw",
        "copiedPublisher", "copiedObserver", "copiedWatch", "copiedMany", "copiedSource", "copiedIntPublisher", "copiedRawPublisher"]
  forM_ protected $ \owner -> unless
    (any (\(binder, reason) -> binder == owner && "compiler-issued typed helper evidence" `isInfixOf` reason) rejections) $
      fail ("authored raw/Sited progress reference escaped rejection: " ++ owner)
  unless (any (\(binder, reason) -> binder == "openObserver" && "polymorphic" `isInfixOf` reason) rejections) $
    fail "open progress helper escaped monomorphic evidence requirement"
  unless (length sites == 6 && all (\site -> length (ysInputs site) == 1
      && length (ysInputTypeWitnesses site) == 1 && all isJust (ysInputTypeWitnesses site)) sites) $
    fail "supported direct/watch/source progress helper lost canonical input authority"
  unless (length [() | site <- sites, input <- ysInputs site, "ProgressNote" `T.isInfixOf` stType input] == 5) $
    fail "progress helper metadata erased nominal newtype identity"
  -- Approved library implementations forward the hidden site; they must not be
  -- rejected merely because their private bodies construct the raw effect.
  forM_ (pprModules result) $ \owner ->
    forM_ (pmSiteRejections owner) $ \rejection ->
      unless (getOccString (srBinder rejection) `notElem`
          ["reportRequestProgressSited", "pollProgressSited", "awaitProgressAfterSited", "awaitAnyProgressSited", "installSource", "attachSource"]) $
        fail "trusted progress forwarding implementation was rejected"
  putStrLn "progress boundary: concrete direct/watch/source evidence; raw, copied Sited and open references refused"
 where
  scratch = do
    root <- getTemporaryDirectory
    (path, handle) <- openTempFile root "tidepool-progress-boundary"
    hClose handle
    -- openTempFile reserves an exclusive name; turn it into the scratch directory.
    removeFile path
    createDirectory path
    pure path
