module ProgressBoundaryTest (progressBoundaryChecks, watchReplyEvidenceChecks, watchReplyWarmAuthorityChecks) where

import Control.Exception (bracket, try, SomeException)
import Data.IntMap.Strict qualified as IntMap
import Control.Monad (forM, forM_, unless)
import Data.ByteString qualified as BS
import Data.List (isInfixOf)
import Data.Maybe (isJust)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC.Types.Name (getOccString)
import GHC.Unit.Module (moduleName, moduleNameString)
import System.Directory (createDirectoryIfMissing, copyFile, createDirectory, getTemporaryDirectory, removeDirectoryRecursive, removeFile)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.EffectSchema (YieldSite(..), SiteType(..))
import Tidepool.GhcPipeline
  ( prHscEnv, PipelineSelection(..), PreparedPipelineResult, pprPipelineResult, pprModules
  , pprProductInterfaces, CompilePurpose(..), runPipelineSelected, withResidentPipelineSelected )
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.ExecutionEncode (encodeWireProgram, encodeProjectedGroup)
import Tidepool.PreparedSites (SiteRejection(..))
import Tidepool.PreparedStg (pmModule, pmBindings, pmYieldSites, pmSiteRejections)

progressBoundaryChecks :: FilePath -> IO ()
progressBoundaryChecks effects = bracket scratch removeDirectoryRecursive $ \work -> do
  let target = work </> "ProgressBoundary.hs"
  copyFile "test-source-boot/fixtures/ProgressBoundary.hs" target
  result <- runPipelineSelected (PreparedProducts Nothing) target [work, "lib", effects]
  forM_ [
      "bad :: RequestSite '[Int] Bool -> RequestSite '[Int] Int\nbad = coerce",
      "bad :: RequestSite '[Int] Bool -> RequestSite '[Bool] Bool\nbad = coerce",
      "bad :: Int -> RequestSite '[Int] Bool\nbad = coerce"
    ] $ \declaration -> do
    let mismatch = work </> "CarrierMismatch.hs"
    writeFile mismatch (unlines ["{-# LANGUAGE DataKinds #-}", "module CarrierMismatch where",
      "import Data.Coerce (coerce)", "import Tidepool.Internal.RequestSite (RequestSite)", declaration])
    rejected <- try (runPipelineSelected PreparedStg mismatch [work, "lib", effects])
      :: IO (Either SomeException PreparedPipelineResult)
    unless (case rejected of Left _ -> True; Right _ -> False) $
      fail "opaque nominal RequestSite accepted a forged reply, input vector, or integer"
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
        (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty Map.empty
        (SymbolIdentity "main" "ProgressBoundary" "value" "safeSibling" Nothing)
        [] Nothing Nothing Nothing Nothing
      products = projectOriginalHomeModuleProducts
        (prHscEnv (pprPipelineResult result)) (pprProductInterfaces result)
        context mempty (pprModules result)
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
watchReplyEvidenceChecks = watchReplyEvidenceWith Nothing

-- The defining Watch module is unchanged between the two genuine compiles.
-- Only the target's import of the KnownEffect instance provider changes.
watchReplyWarmAuthorityChecks :: FilePath -> FilePath -> IO ()
watchReplyWarmAuthorityChecks = watchReplyEvidenceWith
  (Just "test-source-boot/fixtures/WatchReplyEvidenceWarmup.hs")

watchReplyEvidenceWith :: Maybe FilePath -> FilePath -> FilePath -> IO ()
watchReplyEvidenceWith warmup effects work = do
  createDirectoryIfMissing True work
  let target = work </> "WatchReplyEvidence.hs"
      includes = [work, "lib", effects]
  withResidentPipelineSelected includes $ \compile -> do
    forM_ warmup $ \source -> do
      copyFile source target
      prepared <- compile (PreparedProducts Nothing) Set.empty GeneralCompile Nothing target includes Nothing
      let initial = work </> "warm-general"
      createDirectoryIfMissing True initial
      copyFile target (initial </> "WatchReplyEvidence.hs")
      _ <- retainWatchReplyEvidence initial prepared
      unless (any ((== "Tidepool.Agent.Watch.Internal") . moduleNameString . moduleName . pmModule)
          (pprModules prepared)) $
        fail "general warmup did not prepare the genuine Watch defining module"
    copyFile "test-source-boot/fixtures/WatchReplyEvidence.hs" target
    result <- compile (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile Nothing target includes Nothing
    rows <- retainWatchReplyEvidence work result
    forM_ rows $ \(occurrence, targetRows, originalRows) -> do
      unless (any (== [True]) targetRows) $
        fail ("watch target lacks its closed Int reply row: " ++ T.unpack occurrence)
      unless (any (== [True]) originalRows) $
        fail ("watch native original lacks its closed Int reply row: " ++ T.unpack occurrence)
  putStrLn ("watch reply evidence retained at " ++ work)

retainWatchReplyEvidence :: FilePath -> PreparedPipelineResult -> IO [(T.Text, [[Bool]], [[Bool]])]
retainWatchReplyEvidence work result = do
    let environment = prHscEnv (pprPipelineResult result)
        context entry = ProjectionContext "ghc-9.12-prepared-stg" "ghc-9.12.2"
          (TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []) Map.empty Map.empty
          (SymbolIdentity "main" "WatchReplyEvidence" "value" entry Nothing)
          [] Nothing Nothing Nothing Nothing
        relevant owner = moduleNameString (moduleName owner) `elem`
          ["Tidepool.Agent.Watch.Internal", "WatchReplyEvidence"]
        products = projectOriginalHomeModuleProducts environment (pprProductInterfaces result)
          (context "registerSingle") mempty (pprModules result)
        outcomes = [(owner, outcome) | (owner, outcome) <- preparedModuleProductOutcomes products, relevant owner]

    writeFile (work </> "reply-evidence.txt") (unlines
      ["original outcomes: " ++ show [(moduleNameString (moduleName owner),
          either (Left . show) (Right . map (\group -> (projectedOriginalOrdinal group,
            projectedBinders group, projectedConstructorReplies (projectedBody group)))) outcome)
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
    forM ["RegisterWatchWith", "RegisterWatchGroupsWith"] $ \occurrence -> do
      let targetRows = [replyRows occurrence (programConstructors wire) (programTypes wire)
            (programConstructorReplies wire) | wire <- targets]
          originalRows = [replyRows occurrence (projectedConstructors body) (projectedTypes body)
            (projectedConstructorReplies body) | body <- originalBodies]
      putStrLn ("watch reply evidence " ++ T.unpack occurrence ++ ": targets=" ++ show targetRows
        ++ " originals=" ++ show originalRows)
      pure (occurrence, targetRows, originalRows)
 where
  replyRows occurrence constructors types replies =
    [case do
       body <- child (TypeNodeId index) TypeBody
       declaration <- child body TypeHead
       node <- nodeAt declaration
       case node of TypeDeclaration family _ _ _ -> Just (symbolOccurrence family == "Int"); _ -> Nothing
     of Just True -> True; _ -> False
    | (ConstructorId constructor, StaticReply (TypeNodeId index)) <- replies
    , symbolOccurrence (constructorIdentity (constructors !! fromIntegral constructor)) == occurrence
    ]
   where
    nodeAt (TypeNodeId raw) = IntMap.lookup (fromIntegral raw) (typeGraphNodes types)
    child (TypeNodeId raw) role = case [target | (actual, target) <-
        IntMap.findWithDefault [] (fromIntegral raw) (typeGraphEdges types), actual == role] of
      [target] -> Just target
      _ -> Nothing
