module RequestInputsTest (requestInputHistories, requestInputBoundaries, retainedCompilationPublication) where

import Control.Exception (AsyncException(ThreadKilled), IOException, SomeException, bracket, fromException, throwIO, try)
import Control.Concurrent (forkIO, killThread, newEmptyMVar, putMVar, takeMVar)
import Control.Monad (foldM, forM, forM_, unless, when)
import qualified Data.ByteString as BS
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.List (isInfixOf, isPrefixOf, nub, stripPrefix)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Directory (copyFile, createDirectory, doesDirectoryExist, doesFileExist, listDirectory, removeFile)
import System.FilePath ((</>), takeDirectory)
import System.Timeout (timeout)
import Test.QuickCheck
import GHC.Driver.Env (HscEnv(..))
import GHC.Unit.Finder (initFinderCache, addModuleToFinder)
import GHC.Unit.Finder.Types (FinderCache(..))
import GHC.Unit.Module (mkModule, mkModuleName)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (stringToUnit, GenWithIsBoot(..))
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import Tidepool.Binders (analyzeCellWithFlags, cellPlanPrologue)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyResolution(..) )
import Tidepool.ExactHydration (ExactIfaceArtifact(..))
import Tidepool.ExactScope
  ( ExactCompilation(..), SourceSelectedOriginals(..), scopeManifestPath, scopeRequestSha256
  , scopeInterfaces, scopeModuleInterfaceProofs, canonicalCertificatePath
  , canonicalCoreArtifact, canonicalCorePath, readExactScope, revalidateExactScope
  , writeCheckedExactCompilation, writeRetainedExactCompilation )
import Tidepool.FatIface (readExactInterface)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), CompilePurpose(..), PreparedPipelineResult(..), PipelineResult(..)
  , runPipelineSessionSelected, preparedExactCompilation, preparedFreshDependencies, withSourceImportIntents )
import Tidepool.PackageWitness (PackageImportRoot(..), PackageImportEvidence(..), decodeCapturedPackageImports)
import Tidepool.RequestInputs
import Tidepool.Session (SessionScope(..), emptySessionScope)
import Tidepool.Test.GenuineCandidate (writeGenuineMetadataScope)
import SourceBootCases (admitCheckedScope, counterValues)
import SourceBootFixtureSupport (withScratch, withTiming, digest, capturePreparedFixture, captureDiagnostics)

data PublicationStep
  = RestorePublicationInputs
  | RestorePublicationInput (NonNegative Int)
  | ChangePublicationInput (NonNegative Int)
  | RemovePublicationInput (NonNegative Int)
  deriving Show

instance Arbitrary PublicationStep where
  arbitrary = frequency
    [(2,pure RestorePublicationInputs), (3,RestorePublicationInput <$> arbitrary)
    ,(3,ChangePublicationInput <$> arbitrary), (2,RemovePublicationInput <$> arbitrary)]
  shrink RestorePublicationInputs = []
  shrink (RestorePublicationInput index) = RestorePublicationInput <$> shrink index
  shrink (ChangePublicationInput index) = ChangePublicationInput <$> shrink index
  shrink (RemovePublicationInput index) = RemovePublicationInput <$> shrink index

-- One real compiler cohort supplies both scopes and current source selection.
-- Histories mutate only private issued paths; the model compares raw payloads
-- and absent lookup candidates, independently of the production capture maps.
retainedCompilationPublication :: IO ()
retainedCompilationPublication = withTiming $ withScratch $ \work -> do
  forM_ ["CanonicalSource.hs","CanonicalDependency.hs","CanonicalConsumer.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "CanonicalSource.hs") [work] Nothing
  fixture <- capturePreparedFixture work original
  scopePath <- writeGenuineMetadataScope work ["CanonicalSource","CanonicalDependency"] fixture
  retained <- readExactScope scopePath >>= either fail pure
  let emptyRoot = work </> "empty-root"
      source = work </> "CanonicalConsumer.hs"
      includes = [emptyRoot,work]
  createDirectory emptyRoot
  admitted <- admitCheckedScope retained includes []
  plan <- analyzeCellWithFlags (hsc_dflags (prHscEnv (pprPipelineResult original))) ""
    "import CanonicalSource as Source (Answer)\nimport CanonicalSource (Answer)\n(1 :: Answer)"
    >>= either (fail . show) pure
  prepared <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty
    (ExactScopeCompile (withSourceImportIntents (cellPlanPrologue plan) GeneralCompile) admitted)
    (Just emptySessionScope {ssRoot=work,ssExactScope=Just (scopeManifestPath admitted)})
    source includes Nothing
  compilation <- maybe (fail "publication fixture has no exact completion") pure
    (preparedExactCompilation prepared)
  selected <- maybe (fail "publication fixture has no current source selection") pure
    (compilationSourceSelection compilation)
  let environment = prHscEnv (pprPipelineResult prepared)
      evidence = preparedFreshDependencies prepared
      checked = compilationScope compilation
      paths scope = scopeManifestPath scope
        : concat [[exactPath iface,packages] | (iface,packages,_) <- scopeInterfaces scope]
        ++ concat [[canonicalCertificatePath proof]
             ++ maybe [] (pure . canonicalCorePath) (canonicalCoreArtifact proof)
            | proof <- Map.elems (scopeModuleInterfaceProofs scope)]
      observed = nub (paths retained ++ paths checked
        ++ map dependencySourcePath (dependencySources evidence)
        ++ map dependencySourcePath (dependencySources (selectedOriginalEvidence selected)))
      absent = nub [path | resolution <- dependencyResolutions (selectedOriginalEvidence selected)
        , path <- case dependencyResolutionSelected resolution of
            Nothing -> dependencyResolutionCandidates resolution
            Just chosen -> takeWhile (/= chosen) (dependencyResolutionCandidates resolution)]
      parent = takeDirectory source </> ".exact-compilations"
      receipts = doesDirectoryExist parent >>= \exists -> if exists then listDirectory parent else pure []
      publish env = writeRetainedExactCompilation env retained compilation evidence
  unless (not (null absent) && scopeRequestSha256 retained /= scopeRequestSha256 checked)
    (fail "publication fixture lacks negative candidates or distinct scope manifests")
  forM_ absent $ \path -> doesFileExist path >>= \exists ->
    when exists (fail "publication negative candidate was already present")
  originals <- Map.fromList <$> ((++)
    <$> mapM (\path -> (path,) . Just <$> BS.readFile path) observed
    <*> pure [(path,Nothing) | path <- absent])
  unless (all (isPrefixOf (work ++ "/")) (Map.keys originals))
    (fail "publication mutation escaped its private fixture")
  sourceBytes <- BS.readFile source
  shared <- case [(iface,packages) | (iface,packages,_) <- scopeInterfaces retained
      , any (\(other,_,_) -> exactPath other == exactPath iface) (scopeInterfaces checked)] of
    value:_ -> pure value
    _ -> fail "publication fixture lacks an actual shared interface path"
  verdicts <- newIORef (0::Int,0::Int)
  let remove path = doesFileExist path >>= \exists -> when exists (removeFile path)
      write path = maybe (remove path) (BS.writeFile path)
      restore = mapM_ (uncurry write) (Map.toAscList originals)
      changed value = Just (maybe (BS.singleton 0) (<> BS.singleton 0) value)
      proofRows diagnostics = [line | line <- lines diagnostics
        , "tidepool-timing-detail parent=exact_scope phase=revalidate " `isPrefixOf` line]
      measurement variant diagnostics = putStrLn ("publication-proof variant=" ++ variant
        ++ " proofs=" ++ show (length (proofRows diagnostics))
        ++ " wall_ns=" ++ show (sum [read value :: Integer | line <- proofRows diagnostics
              , word <- words line, Just value <- [stripPrefix "wall_ns=" word]])
        ++ " observed_files=" ++ show (length [() | line <- lines diagnostics
              , "tidepool-count name=hash_bytes.observed_file." `isPrefixOf` line]))
      check env current = do
        before <- receipts
        (result,diagnostics) <- captureDiagnostics
          (try (publish env) :: IO (Either IOException ()))
        after <- receipts
        let expected = current == originals
            added = filter (`notElem` before) after
        case result of
          Left _ -> do
            unless (not expected && null added) (fail "refused publication exposed a receipt or rejected unchanged inputs")
            modifyIORef' verdicts (\(accepted,refused) -> (accepted,refused+1))
          Right () -> do
            unless (expected && length added == 1) (fail "publication accepted drift or failed to issue one receipt")
            let output = parent </> head added
            snapshot <- BS.readFile (output </> "source.hs")
            receipt <- BS.readFile (output </> "receipt.cbor")
            unless (snapshot == sourceBytes && not (BS.null receipt)) (fail "publication lost its captured source or receipt")
            unless (length (proofRows diagnostics) == 1
                && length (counterValues ("hash_bytes.observed_file." ++ exactSha256 (fst shared)) diagnostics) == 1
                && length (counterValues ("hash_bytes.observed_file." ++ scopeRequestSha256 retained) diagnostics) == 1
                && length (counterValues ("hash_bytes.observed_file." ++ scopeRequestSha256 checked) diagnostics) == 1)
              (fail "publication repeated a shared path observation or omitted one of its scopes")
            modifyIORef' verdicts (\(accepted,refused) -> (accepted+1,refused))
        pure ()
      input (NonNegative index) = Map.toAscList originals !! (index `mod` Map.size originals)
      step current operation = do
        next <- case operation of
          RestorePublicationInputs -> restore >> pure originals
          RestorePublicationInput index -> do
            let (path,value) = input index
            write path value
            pure (Map.insert path value current)
          ChangePublicationInput index -> do
            let (path,value) = input index
            write path (changed value)
            pure (Map.insert path (changed value) current)
          RemovePublicationInput index -> do
            let (path,_) = input index
            remove path
            pure (Map.insert path Nothing current)
        check environment next
        pure next
  -- The prior two-proof sequence is an independent full-validation control on
  -- the same issued inputs, environment and publication mechanism.
  (_,separateDiagnostics) <- captureDiagnostics $ do
    revalidateExactScope environment retained >>= either fail pure
    writeCheckedExactCompilation environment compilation evidence
  unless (length (proofRows separateDiagnostics) == 2
      && length (counterValues ("hash_bytes.observed_file." ++ exactSha256 (fst shared)) separateDiagnostics) == 2)
    (fail "separate validation control did not observe the shared original twice")
  measurement "separate" separateDiagnostics
  (_,combinedDiagnostics) <- captureDiagnostics (publish environment)
  measurement "combined" combinedDiagnostics
  check environment originals
  -- Every issued file and negative candidate must reach the publication owner.
  forM_ (Map.toAscList originals) $ \(path,value) -> bracket (pure ()) (const restore) $ \_ -> do
    write path (changed value)
    check environment (Map.insert path (changed value) originals)
    write path value
    check environment originals
    remove path
    check environment (Map.insert path Nothing originals)
    write path value
  result <- quickCheckWithResult stdArgs {maxSuccess=80,maxSize=20} $ \operations ->
    let selectedSteps = take 20 (operations :: [PublicationStep])
    in classify (any (\case RestorePublicationInputs -> True; _ -> False) selectedSteps) "whole-input restoration"
      $ classify (length selectedSteps > 3) "interacting publication history"
      $ ioProperty $ bracket (restore >> pure ()) (const restore) $ \_ ->
          foldM step originals selectedSteps >> pure True
  unless (isSuccess result) (fail "retained compilation publication histories failed")
  -- A pending fresh Finder observation establishes the cancellation boundary.
  entered <- newEmptyMVar
  blocked <- newEmptyMVar
  finished <- newEmptyMVar
  let finder = hsc_FC environment
      cancelledEnvironment = environment {hsc_FC=finder {lookupFinderCache = \owner -> do
        putMVar entered ()
        takeMVar blocked
        lookupFinderCache finder owner}}
  beforeCancellation <- receipts
  settled <- bracket
    (forkIO $ (try (publish cancelledEnvironment) :: IO (Either SomeException ())) >>= putMVar finished)
    killThread $ \thread -> do
      reached <- timeout 5000000 (takeMVar entered)
      when (case reached of Nothing -> True; _ -> False) (fail "publication never reached its fresh Finder check")
      killThread thread
      timeout 5000000 (takeMVar finished)
  unless (case settled of Just (Left failure) -> fromException failure == Just ThreadKilled; _ -> False)
    (fail "publication swallowed cancellation or failed to settle")
  afterCancellation <- receipts
  unless (beforeCancellation == afterCancellation) (fail "cancelled publication exposed a receipt")
  let sharedPath = exactPath (fst shared)
      sharedBytes = originals Map.! sharedPath
  write sharedPath (changed sharedBytes)
  check environment (Map.insert sharedPath (changed sharedBytes) originals)
  restore
  check environment originals
  -- Equal installed bytes at another selected path must still refuse.
  roots <- concat <$> forM (scopeInterfaces retained) (\(iface,path,_) ->
    BS.readFile path >>= either fail (pure . packageInterfaces) . decodeCapturedPackageImports iface)
  root <- case roots of value:_ -> pure value; _ -> fail "publication fixture has no installed package root"
  let owner = mkModule (stringToUnit (packageUnit root)) (mkModuleName (packageModule root))
      alternate = work </> "alternate-package.hi"
  (_,location) <- readExactInterface environment owner >>= either (fail . show) pure
  copyFile (packagePath root) alternate
  isolatedFinder <- initFinderCache
  addModuleToFinder isolatedFinder (GWIB owner NotBoot) (location {ml_hi_file=alternate})
  beforeSelection <- receipts
  wrongSelection <- try (publish environment {hsc_FC=isolatedFinder}) :: IO (Either IOException ())
  afterSelection <- receipts
  unless (case wrongSelection of
      Left failure -> beforeSelection == afterSelection
        && "package selection or interface bytes differ from the certified import root" `isInfixOf` show failure
      _ -> False)
    (fail "publication accepted an equal-byte alternate package selection")
  check environment originals
  (accepted,refused) <- readIORef verdicts
  putStrLn ("retained publication inputs=" ++ show (Map.size originals) ++ " accepted=" ++ show accepted
    ++ " refused=" ++ show refused ++ "; cancellation and current package selection controls")

-- The model owns values, independently of the implementation's custody map.
-- Every step writes a producer path, then either captures a new original or
-- consumes an earlier one. Mutations and restoration test both snapshot use and
-- the terminal current-path proof; independent requests see the current bytes.
requestInputHistories :: IO ()
requestInputHistories = do
  result <- quickCheckWithResult stdArgs {maxSuccess=80, maxSize=40} $ \steps ->
    let selected = take 40 (steps :: [(NonNegative Int, [Bool])])
        (extended,changed,restored) = historyFlags selected
    in classify (null selected) "empty history"
      $ classify extended "extend retained owner"
      $ classify changed "producer drift"
      $ classify restored "restore captured input"
      $ ioProperty (history selected)
  unless (isSuccess result) (fail "request input custody history property failed")
  where
    history steps = withScratch $ \directory -> do
      (_,empty) <- captureRequestInputs Nothing (const (pure ()))
      (_,model,owner) <- foldM (step directory) (Map.empty,Map.empty,empty) steps
      observed <- revalidateRequestInputs owner
      let current = fst3Result steps
      -- The final proof is independently determined by the disk model, rather
      -- than by the owner itself or by its readback result.
      pure (either (const False) (const True) observed == (current == model))
    fst3Result = foldl (\current (NonNegative key,bits) -> Map.insert (key `mod` 5) (payload bits) current) Map.empty
    step directory (current,model,owner) (NonNegative key,bits) = do
      let index = key `mod` 5
          path = directory </> show index
          bytes = payload bits
      BS.writeFile path bytes
      (captured,next) <- captureRequestInputs (Just owner) (\reader -> reader path 64)
      let expected = Map.findWithDefault bytes index model
          admitted = Map.insertWith (\_ old -> old) index bytes model
          updated = Map.insert index bytes current
      unless (captured == expected) (fail "scope extension consumed a changed admitted original")
      unless (requestInputBytes next == sum (map (toInteger . BS.length) (Map.elems admitted)))
        (fail "scope extension charged retained input bytes again or lost custody accounting")
      retained <- capturedRequestInput next path (digest expected)
      unless (retained == expected) (fail "captured request input lost its admitted byte identity")
      (_,independent) <- captureRequestInputs Nothing (\reader -> reader path 64)
      fresh <- capturedRequestInput independent path (digest bytes)
      unless (fresh == bytes) (fail "independent request inherited another request's snapshot")
      case mergeRequestInputs next [independent,independent] of
        Left _ | bytes /= expected -> pure ()
        Right transferred | bytes == expected -> do
          removeFile path
          shared <- capturedRequestInput transferred path (digest expected)
          unless (shared == expected && requestInputBytes transferred == requestInputBytes next)
            (fail "duplicate capture transfer reread producer bytes or changed aggregate accounting")
          BS.writeFile path bytes
        _ -> fail "capture transfer disagreed with independent path ownership model"
      pure (updated,admitted,next)
    historyFlags steps =
      let (_,_,extended,changed,restored) =
            foldl observe (Map.empty,Map.empty,False,False,False) steps
      in (extended,changed,restored)
      where
        observe (first,current,extended,changed,restored) (NonNegative key,bits) =
          let index = key `mod` 5
              bytes = payload bits
              original = Map.lookup index first
          in (Map.insertWith (\_ old -> old) index bytes first,Map.insert index bytes current
            ,extended || maybe False (const True) original
            ,changed || maybe False (/= bytes) original
            ,restored || (original == Just bytes && Map.lookup index current /= original))
    payload bits = BS.pack [if bit then 1 else 0 | bit <- take 64 bits]

requestInputBoundaries :: IO ()
requestInputBoundaries = withScratch $ \directory -> do
  let path = directory </> "original"
      other = directory </> "other"
      bytes = BS.pack [1,2,3]
  BS.writeFile path bytes
  BS.writeFile other bytes
  (escaped,owner) <- captureRequestInputs Nothing $ \reader -> do
    _ <- reader path 3
    pure reader
  BS.writeFile path (BS.singleton 9)
  stable <- capturedRequestInput owner path (digest bytes)
  changed <- revalidateRequestInputs owner
  unless (stable == bytes && either (const True) (const False) changed)
    (fail "producer replacement changed consumption or passed terminal validation")
  BS.writeFile path bytes
  restored <- revalidateRequestInputs owner
  unless (restored == Right ()) (fail "restored producer bytes remained stale in terminal validation")
  afterSeal <- try (escaped path 3) :: IO (Either IOException BS.ByteString)
  unless (either (const True) (const False) afterSeal) (fail "admission reader escaped its lifetime")
  tooSmall <- try (captureRequestInputs (Just owner) (\reader -> reader path 2))
    :: IO (Either IOException (BS.ByteString,RequestOriginalInputs))
  unless (either (const True) (const False) tooSmall) (fail "retained bytes bypassed a stricter bound")
  wrongSeal <- try (capturedRequestInput owner path (replicate 64 '0')) :: IO (Either IOException BS.ByteString)
  missing <- try (capturedRequestInput owner other (digest bytes)) :: IO (Either IOException BS.ByteString)
  unless (all (either (const True) (const False)) [wrongSeal,missing])
    (fail "snapshot lookup reconstructed missing or conflicting authority from disk")
  let copy = directory </> "durable-copy"
  BS.writeFile copy bytes
  aliased <- either fail pure (aliasRequestInputs [(copy,path,digest bytes)] owner)
  (_,aliasedAgain) <- captureRequestInputs (Just aliased) (\reader -> reader copy 3)
  copied <- capturedRequestInput aliasedAgain copy (digest bytes)
  unless (copied == bytes && requestInputBytes aliasedAgain == requestInputBytes owner)
    (fail "durable original alias retained or charged another payload")
  unless (case aliasRequestInputs [(copy,path,replicate 64 '0')] owner of Left _ -> True; _ -> False)
    (fail "durable original alias accepted a conflicting seal")
  (_,independentCopy) <- captureRequestInputs Nothing (\reader -> reader copy 3)
  transferredAlias <- either fail pure (mergeRequestInputs aliased [aliasedAgain,independentCopy])
  unless (requestInputBytes transferredAlias == requestInputBytes owner)
    (fail "alias transfer charged a captured copy twice")
  removeFile copy
  missingAlias <- revalidateRequestInputs transferredAlias
  unless (either (const True) (const False) missingAlias)
    (fail "terminal publication ignored a missing durable support alias")
  stableAlias <- capturedRequestInput transferredAlias copy (digest bytes)
  unless (stableAlias == bytes) (fail "deleted durable alias changed captured consumption")
  BS.writeFile copy bytes
  revalidateRequestInputs transferredAlias >>= either fail pure
  let config = "TIDEPOOL_REQUEST_CAPTURE_BYTES"
      restore Nothing = unsetEnv config
      restore (Just value) = setEnv config value
  bracket (lookupEnv config) restore $ \_ -> do
    setEnv config "5"
    (_,bounded) <- captureRequestInputs Nothing (\reader -> reader path 3)
    exceeds <- try (captureRequestInputs (Just bounded) (\reader -> reader other 3))
      :: IO (Either IOException (BS.ByteString,RequestOriginalInputs))
    unless (either (const True) (const False) exceeds) (fail "aggregate capture budget was applied per file")
    unless (requestInputBytes bounded == 3) (fail "encoded input accounting differs from retained bytes")
    unless (case retainRequestEncodedBytes [bytes] bounded of Nothing -> True; _ -> False)
      (fail "new encoded graph bytes bypassed the request aggregate budget")
    let graphBytes = BS.pack [4,5]
    graphOwner <- maybe (fail "bounded encoded graph was refused") pure
      (retainRequestEncodedBytes [graphBytes] bounded)
    unless (requestInputBytes graphOwner == 5
        && fmap requestInputBytes (retainRequestEncodedBytes [graphBytes] graphOwner) == Just 5)
      (fail "encoded graph generations lost accounting or charged the same graph again")
    -- A donor allowance cannot enlarge the receiver. Transfer shares bytes,
    -- including duplicate donors, and observes no current producer path.
    setEnv config "64"
    (_,donor) <- captureRequestInputs Nothing (\reader -> reader other 3)
    setEnv config "5"
    (_,receiving) <- captureRequestInputs Nothing (const (pure ()))
    removeFile other
    transferred <- either fail pure (mergeRequestInputs receiving [donor,donor])
    transferredBytes <- capturedRequestInput transferred other (digest bytes)
    unless (transferredBytes == bytes && requestInputBytes transferred == 3)
      (fail "capture transfer reread deleted producer bytes or charged a duplicate owner")
    unless (case mergeRequestInputs bounded [donor] of Left _ -> True; _ -> False)
      (fail "capture transfer inherited donor allowance instead of receiving budget")
    BS.writeFile other (BS.singleton 7)
    (_,conflicting) <- captureRequestInputs Nothing (\reader -> reader other 3)
    unless (case mergeRequestInputs transferred [conflicting] of Left _ -> True; _ -> False)
      (fail "capture transfer replaced an admitted path with conflicting owner bytes")
    BS.writeFile other bytes
    setEnv config "0"
    invalid <- try (captureRequestInputs Nothing (const (pure ())))
      :: IO (Either IOException ((),RequestOriginalInputs))
    unless (either (const True) (const False) invalid) (fail "invalid capture budget silently selected a default")
  cancelled <- try (captureRequestInputs Nothing (\_ -> throwIO ThreadKilled))
    :: IO (Either AsyncException ((),RequestOriginalInputs))
  unless (case cancelled of Left ThreadKilled -> True; _ -> False)
    (fail "capture admission converted asynchronous cancellation")
