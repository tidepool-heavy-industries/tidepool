module ProgressBoundaryTest (progressBoundaryChecks, watchReplyEvidenceChecks) where

import Control.Exception (bracket)
import Control.Monad (forM, forM_, unless)
import Data.ByteString qualified as BS
import Data.List (isInfixOf)
import Data.Maybe (isJust)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import Data.Text.IO qualified as TIO
import GHC.Types.Name (getOccString)
import GHC.Unit.Module (moduleName, moduleNameString)
import System.Directory (createDirectoryIfMissing, copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.EffectSchema (YieldSite(..), SiteType(..))
import Tidepool.GhcPipeline (PipelineResult(..), PipelineSelection(..), PreparedPipelineResult(..), CompilePurpose(..), runPipelineSelected, withResidentPipelineSelected)
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.ExecutionEncode (encodeWireProgram, encodeProjectedGroup)
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
  source <- TIO.readFile target
  TIO.writeFile target (T.replace "1 {- copied-site -}" (T.pack (show issued)) source)
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
  let context = ProjectionContext "test" "matched"
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
        (SymbolIdentity "main" "ProgressBoundary" "value" "safeSibling" Nothing)
        [] Nothing Nothing Nothing Nothing
      products = projectOriginalHomeModuleProducts
        (prHscEnv (pprPipelineResult result)) (pprProductInterfaces result)
        context (pprModules result)
      originalRefusals =
        [refusal | (owner, refusals) <- preparedModuleProductOmissions products, refusal <- refusals
        , owner == pmModule prepared]
      omitted = Set.fromList (concatMap omittedOriginalBinders originalRefusals)
  groups <- case [outcome | (owner, outcome) <- preparedModuleProductOutcomes products
                , owner == pmModule prepared] of
    [Right values] -> pure values
    _ -> fail "unrelated raw group discarded the safe original product"
  safeOrdinal <- case
    [fromIntegral ordinal | (ordinal, (binding, _)) <- zip [0 :: Int ..] (pmBindings prepared)
    , any ((== "safeSibling") . getOccString) (topBinders binding)] of
    [ordinal] -> pure ordinal
    _ -> fail "safe original sibling lost its defining group"
  unless (any (\group -> projectedOriginalOrdinal group == safeOrdinal
      && any ((== "safeSibling") . symbolOccurrence) (projectedBinders group)) groups) $
    fail "safe original sibling changed its ordinal or lost native custody"
  unless (any (any ((== "publish") . symbolOccurrence) . projectedBinders) groups) $
    fail "compiler-issued concrete progress publisher lost its original native group"
  forM_ ["rawAlias", "rawAliasChain"] $ \occurrence -> unless
    (any (\refusal -> any ((== T.pack occurrence) . symbolOccurrence) (omittedOriginalBinders refusal)
      && case omittedOriginalReason refusal of DependsOnUnavailable _ -> True; _ -> False) originalRefusals) $
    fail ("original product retained a dependent of an omitted raw group: " ++ occurrence)
  unless (all (\group -> all ((`Set.notMember` omitted) . globalIdentity)
      (projectedGlobals (projectedBody group))) groups) $
    fail "original product's safe subset requires an omitted native group"
  forM_ (protected ++ ["rawAlias", "rawAliasChain", "openObserver"]) $ \occurrence ->
    case prepareProjection (context { projectionEntry = SymbolIdentity
        "main" "ProgressBoundary" "value" (T.pack occurrence) Nothing }) (pprModules result) of
      Left RejectedTypedSite{} -> pure ()
      Left other -> fail ("authored raw projection changed refusal: " ++ show other)
      Right _ -> fail ("authored raw projection acquired execution: " ++ occurrence)
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

-- The explicit destination retains successful GHC projections for inspection.
-- It contains compiler output, never a hand-built authority packet.
watchReplyEvidenceChecks :: FilePath -> FilePath -> IO ()
watchReplyEvidenceChecks effects work = do
  createDirectoryIfMissing True work
  let target = work </> "WatchReplyEvidence.hs"
      includes = [work, "lib", effects]
  copyFile "test-source-boot/fixtures/WatchReplyEvidence.hs" target
  withResidentPipelineSelected includes $ \compile -> do
    result <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile Nothing target includes Nothing
    let environment = prHscEnv (pprPipelineResult result)
        context entry = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
          (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty
          (SymbolIdentity "main" "WatchReplyEvidence" "value" entry Nothing)
          [] Nothing Nothing Nothing Nothing
        relevant owner = moduleNameString (moduleName owner) `elem`
          ["Tidepool.Agent.Watch.Internal", "WatchReplyEvidence"]
        products = projectOriginalHomeModuleProducts environment (pprProductInterfaces result)
          (context "registerSingle") (pprModules result)
        outcomes = [(owner, outcome) | (owner, outcome) <- preparedModuleProductOutcomes products, relevant owner]
        census = [(moduleNameString (moduleName (pmModule prepared)), pmCoverage prepared,
          Set.toAscList (pmEffectRequestTypeIds prepared))
          | prepared <- pprModules result, relevant (pmModule prepared)]
    writeFile (work </> "reply-evidence.txt") (unlines
      ["prepared census: " ++ show census
      , "original outcomes: " ++ show [(moduleNameString (moduleName owner),
          either (Left . show) (Right . map (\group -> (projectedOriginalOrdinal group,
            projectedBinders group, projectedVerbSites (projectedBody group)))) outcome)
          | (owner, outcome) <- outcomes]
      , "original omissions: " ++ show [(moduleNameString (moduleName owner), omissions)
          | (owner, omissions) <- preparedModuleProductOmissions products]])
    forM_ outcomes $ \(owner, outcome) -> case outcome of
      Left failure -> fail ("watch original projection failed: " ++ show failure)
      Right groups -> forM_ groups $ \group -> BS.writeFile
        (work </> (moduleNameString (moduleName owner) ++ "-" ++ show (projectedOriginalOrdinal group) ++ ".cbor"))
        (encodeProjectedGroup group)
    targets <- forM ["registerSingle", "registerGrouped"] $ \entry -> do
      wire <- either (fail . show) pure (projectPreparedTarget (context entry) (pprModules result))
      BS.writeFile (work </> (T.unpack entry ++ ".cbor")) (encodeWireProgram wire)
      pure wire
    let originalBodies = [projectedBody group | (owner, Right groups) <- outcomes
          , moduleNameString (moduleName owner) == "Tidepool.Agent.Watch.Internal", group <- groups]
    forM_ ["RegisterWatchWith", "RegisterWatchGroupsWith"] $ \occurrence -> do
      let targetRows = [replyRows occurrence (programConstructors wire) (programTypes wire)
            (programSites wire) (programVerbSites wire) | wire <- targets]
          originalRows = [replyRows occurrence (projectedConstructors body) (projectedTypes body)
            (projectedSites body) (projectedVerbSites body) | body <- originalBodies]
      putStrLn ("watch reply evidence " ++ T.unpack occurrence ++ ": targets=" ++ show targetRows
        ++ " originals=" ++ show originalRows ++ " census=" ++ show census)
      unless (any (== [True]) targetRows) $
        fail ("watch target lacks its closed Int reply row: " ++ T.unpack occurrence)
      unless (any (== [True]) originalRows) $
        fail ("watch native original lacks its closed Int reply row: " ++ T.unpack occurrence)
  putStrLn ("watch reply evidence retained at " ++ work)
 where
  replyRows occurrence constructors types sites verbs =
    [case types !! fromIntegral index of
       TypeData family _ _ -> symbolOccurrence family == "Int"
       _ -> False
    | (ConstructorId constructor, site) <- verbs
    , symbolOccurrence (constructorIdentity (constructors !! fromIntegral constructor)) == occurrence
    , row <- sites, siteId row == site
    , let TypeNodeId index = siteWire row]
