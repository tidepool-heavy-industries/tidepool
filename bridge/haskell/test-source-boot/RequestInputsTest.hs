{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ForeignFunctionInterface #-}

module RequestInputsTest (ownedArenaRangeReads, artifactByteOwnership, requestInputHistories, requestInputBoundaries, retainedCompilationPublication, fixtureIssuerCountingHistories, ownedScopeInputReuse, validationTimingControl, freshOutputSealSetLaws) where

import Control.Exception (AsyncException(ThreadKilled), IOException, SomeException, bracket, bracketOnError, onException, fromException, throwIO, try)
import Control.Concurrent (forkIO, killThread, newEmptyMVar, putMVar, takeMVar, threadDelay)
import Control.Monad (foldM, forM, forM_, unless, void, when)
import qualified Data.ByteString as BS
import qualified Data.ByteString.Internal as BSI
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.List (isInfixOf, isPrefixOf, nub, permutations, stripPrefix, tails)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Text as T
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Directory (copyFile, createDirectory, createDirectoryIfMissing, doesDirectoryExist, doesFileExist, getPermissions, listDirectory, makeAbsolute, removeFile, setPermissions, Permissions(..))
import System.FilePath ((</>), takeDirectory)
import System.IO (hClose, hFlush, hGetLine, hPutStrLn)
import Foreign.C.String (CString, withCString)
import Foreign.C.Types (CInt(..), CUInt(..))
import System.Posix.Types (Fd(..))
import System.Posix.IO (fdToHandle, closeFd)
import System.Posix.Process (getProcessID)
import System.Process (CreateProcess(..), StdStream(CreatePipe), callProcess, proc, waitForProcess, withCreateProcess)
import System.Exit (ExitCode(ExitSuccess))
import GHC.Conc (ThreadStatus(..), threadStatus)
import System.Timeout (timeout)
import Test.QuickCheck
import Test.QuickCheck.Random (mkQCGen)
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
  ( ExactScope, ExactCompilation(..), SourceSelectedOriginals(..), scopeManifestPath, scopeRequestSha256
  , scopeInterfaces, scopeModuleInterfaceProofs, readExactScope, revalidateExactScope
  , ExactScopeValidationReason(..), revalidateExactScopesAtWithOutputs
  , writeCheckedExactCompilation, writeRetainedExactCompilation
  , writeRetainedExactCompilationWithOutputsAndPublication
  , FreshOutputSeals, emptyFreshOutputSeals, freshOutputSealsFromWrites, appendFreshOutputSeals
  , newExactInputOwner, readExactScopeWithOwner, scopeInterfaceBytes )
import Tidepool.FatIface (readExactInterface)
import Tidepool.GhcPipeline
  ( PipelineSelection(..), CompilePurpose(..), PreparedPipelineResult(..), PipelineResult(..)
  , runPipelineSessionSelected, preparedExactCompilation, preparedFreshDependencies, withSourceImportIntents )
import Tidepool.PackageWitness (PackageImportRoot(..), PackageImportEvidence(..), decodeCapturedPackageImports)
import Tidepool.ArtifactBytes
import Tidepool.RequestInputs
import Tidepool.Session (SessionScope(..), emptySessionScope, SessionModule(..), SessionModuleKind(..), Generation(..))
import Tidepool.Test.GenuineCandidate (writeGenuineMetadataScope, writeGenuineCandidateManifestFor, writeGenuineAuthoredDeclarationScope, writeGenuineExecutionScope)
import Tidepool.ModuleCandidates (readModuleCandidates, candidateModule, candidateGroups, candidateExecutionSource)
import Tidepool.Test.CandidateCodec (CandidateCodecCase(..), writeCandidateCodecFixture)
import Tidepool.Test.FixturePacket (PacketProducer(..), newPacketDirectory, runPacketProducer)
import Tidepool.Timing (readSummaryTimingEnabled, withValidationTiming)
import Codec.CBOR.Term (Term(..), encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import SourceBootCases (admitCheckedScope, counterValues)
import CodecFixtureSupport (readCodecTerm)
import SourceBootFixtureSupport (withScratch, withTiming, digest, capturePreparedFixture, captureDiagnostics)

foreign import ccall unsafe "memfd_create" createArena :: CString -> CUInt -> IO CInt
foreign import ccall unsafe "fcntl" sealArena :: CInt -> CInt -> CInt -> IO CInt

-- Exercise the production cold reader against real sealed Linux arenas. The
-- logical artifact path is absent throughout; it never becomes a read source.
ownedArenaRangeReads :: IO ()
ownedArenaRangeReads = withScratch $ \work -> do
  let bytes = BS.pack [10,20,30]
      backing = BS.pack [90,91] <> bytes <> BS.pack [92,93]
      acquire seal = do
        raw <- withCString "tidepool-artifact-byte-test" (\name -> createArena name 3)
        when (raw < 0) (fail "memfd_create failed")
        handle <- bracketOnError (pure (Fd raw)) closeFd fdToHandle
        (do BS.hPut handle backing
            hFlush handle
            when seal $ do
              result <- sealArena raw 1033 15
              unless (result == 0) (fail "arena sealing failed")) `onException` hClose handle
        pid <- getProcessID
        pure (handle,"/proc/" ++ show pid ++ "/fd/" ++ show raw)
      release (handle,_) = hClose handle
      reference endpoint path sha count extent offset = do
        transport <- either fail pure (ownedArenaRange endpoint extent offset)
        pure (OriginalInputReference path sha count [work </> "missing-producer-origin"] transport)
      refused action = do
        result <- try action :: IO (Either IOException RequestOriginalInputs)
        unless (either (const True) (const False) result) (fail "invalid owned arena input was accepted")
  bracket (acquire True) release $ \(_,endpoint) -> do
    selected <- reference endpoint (work </> "missing-logical-artifact") (digest bytes) 3 7 2
    cold <- continueRequestInputs emptyCapturedOriginalContent [selected]
    actual <- capturedRequestInput cold (originalInputPath selected) (digest bytes)
    unless (actual == bytes && requestInputBytes cold == 3) (fail "cold arena range capture differs")
    retained <- either fail pure (selectedOriginalContent [selected] cold)
    badSha <- reference endpoint (work </> "bad-sha") (digest (BS.singleton 0)) 3 7 2
    badLength <- reference endpoint (work </> "bad-length") (digest bytes) 2 7 2
    badExtent <- reference endpoint (work </> "bad-extent") (digest bytes) 3 8 2
    badRange <- reference endpoint (work </> "bad-range") (digest bytes) 3 7 6
    forM_ [badSha,badLength,badExtent,badRange] $ \bad -> refused (continueRequestInputs emptyCapturedOriginalContent [bad])
    refused (continueRequestInputs retained [badRange])
    second <- reference endpoint (work </> "another-logical-artifact") (digest bytes) 3 7 2
    shared <- continueRequestInputs emptyCapturedOriginalContent [selected,second]
    firstBody <- capturedRequestInputToken shared (originalInputPath selected) (digest bytes)
    secondBody <- capturedRequestInputToken shared (originalInputPath second) (digest bytes)
    let (firstOwner,_,_) = BSI.toForeignPtr (artifactBytes firstBody)
        (secondOwner,_,_) = BSI.toForeignPtr (artifactBytes secondBody)
    unless (firstOwner == secondOwner && requestInputBytes shared == 6)
      (fail "equal cold arena ranges copied bodies or lost per-path allowance")
    withFixtureEnvironment "TIDEPOOL_REQUEST_CAPTURE_BYTES" "3" $
      refused (continueRequestInputs retained [selected,second])
    replacement <- continueRequestInputs emptyCapturedOriginalContent [selected]
    replacementBytes <- capturedRequestInput replacement (originalInputPath selected) (digest bytes)
    unless (replacementBytes == bytes) (fail "fresh worker owner could not rehydrate a live arena")
  bracket (acquire False) release $ \(_,endpoint) -> do
    selected <- reference endpoint (work </> "unsealed") (digest bytes) 3 7 2
    before <- listDirectory "/proc/self/fd"
    forM_ [1..20 :: Int] $ \_ -> refused (continueRequestInputs emptyCapturedOriginalContent [selected])
    after <- listDirectory "/proc/self/fd"
    unless (length before == length after) (fail "failed arena reads retained file descriptors")

-- The byte primitive has content identity only. Admission and budgets remain
-- the responsibility of RequestOriginalInputs, including empty bodies.
artifactByteOwnership :: IO ()
artifactByteOwnership = do
  let check bytes =
        let body = captureArtifactBytes bytes
        in artifactBytes body == bytes
          && artifactLength body == BS.length bytes
          && artifactSha256 body == digest bytes
          && BS.length (artifactDigestBytes body) == 32
          && checkArtifactSeal (digest bytes) body == Right ()
          && checkArtifactSeal "wrong" body /= Right ()
          && body == captureArtifactBytes bytes
  result <- quickCheckWithResult stdArgs {maxSuccess=180, replay=Just (mkQCGen 771208,0)}
    (\bytes -> check (BS.pack bytes))
  unless (isSuccess result) (fail "artifact byte identity property failed")
  unless (check BS.empty) (fail "empty encoded byte body lost content identity")
  let source = BS.pack [0..255]
      body = captureArtifactBytes source
      same = artifactBytes body
      (sourceOwner,_,_) = BSI.toForeignPtr source
      (bodyOwner,_,_) = BSI.toForeignPtr same
  unless (sourceOwner == bodyOwner) (fail "artifact byte capture copied its strict body")

ownedScopeInputReuse :: IO ()
ownedScopeInputReuse = withTiming $ withScratch $ \work -> do
  forM_ ["CanonicalSource.hs","CanonicalDependency.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "CanonicalSource.hs") [work] Nothing
  fixture <- capturePreparedFixture work original
  owner <- newExactInputOwner
  firstPath <- writeGenuineExecutionScope ["CanonicalSource","CanonicalDependency"] ["CanonicalSource"] work fixture
  (_,cold) <- captureDiagnostics (readExactScopeWithOwner owner firstPath >>= either fail pure)
  secondPath <- writeGenuineMetadataScope work ["CanonicalDependency"] fixture
  (_,contracted) <- captureDiagnostics (readExactScopeWithOwner owner secondPath >>= either fail pure)
  thirdPath <- writeGenuineExecutionScope ["CanonicalSource","CanonicalDependency"] ["CanonicalSource"] work fixture
  removeFile firstPath
  removeFile secondPath
  (third,warm) <- captureDiagnostics (readExactScopeWithOwner owner thirdPath >>= either fail pure)
  revalidateExactScope (prHscEnv (pprPipelineResult original)) third >>= either fail pure
  let total label = sum . counterValues label
  unless (total "original_inputs.certificate.misses" cold == 2
      && total "original_inputs.certificate.hits" contracted == 1
      && total "original_inputs.certificate.hits" warm == 2
      && total "original_inputs.certificate.misses" warm == 0
      && total "original_inputs.packages.hits" warm == 2
      && total "original_inputs.census.hits" warm == 2
      && total "original_inputs.graph.hits" warm > 0)
    (fail ("owned scope A/B/A failed to reuse its immutable facts: " ++ cold ++ contracted ++ warm))
  rotated <- newExactInputOwner
  (_,rehydrated) <- captureDiagnostics (readExactScopeWithOwner rotated thirdPath >>= either fail pure)
  unless (total "original_inputs.certificate.misses" rehydrated == 2)
    (fail "fresh physical owner inherited a prior worker's decoder")
  withFixtureEnvironment "TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES" "0" $ do
    bounded <- newExactInputOwner
    _ <- readExactScopeWithOwner bounded thirdPath >>= either fail pure
    (_,evicted) <- captureDiagnostics (readExactScopeWithOwner bounded thirdPath >>= either fail pure)
    unless (counterValues "original_inputs.retained_encoded_bytes" evicted == [0]
        && total "original_inputs.certificate.misses" evicted == 2)
      (fail "zero inactive allowance retained content or decoded facts")
  -- Permit either genuine image alone, but not both. The receiving scope owns
  -- both images even while the inactive pool evicts one of them.
  term <- readCodecTerm thirdPath
  imageSizes <- case term of
    TList fields -> case last fields of
      TList [TString "continue-originals",TList images] -> forM images $ \image -> case image of
        TList [_,_,_,_,TList parts] -> do
          sizes <- forM parts $ \part -> case part of
            TList [_,_,TString sha,size,_] -> case size of
              TInt amount -> pure ((sha,toInteger amount),toInteger amount)
              TInteger amount -> pure ((sha,amount),amount)
              _ -> fail "owned image has another byte-count representation"
            _ -> fail "owned image has another part representation"
          pure (sum (Map.elems (Map.fromList sizes)))
        _ -> fail "owned image has another envelope"
      _ -> fail "genuine scope lacks continued original images"
    _ -> fail "genuine scope has another envelope"
  unless (length imageSizes == 2 && all (> 0) imageSizes)
    (fail "positive eviction fixture lacks two separately retainable images")

  let positiveLimit = maximum imageSizes
  unless (positiveLimit < sum imageSizes)
    (fail "positive eviction fixture lacks two separately retainable images")
  withFixtureEnvironment "TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES" (show positiveLimit) $ do
    bounded <- newExactInputOwner
    (live,evicted) <- captureDiagnostics (readExactScopeWithOwner bounded thirdPath >>= either fail pure)
    unless (total "original_inputs.retained_encoded_bytes" evicted > 0
        && total "original_inputs.evicted_encoded_bytes" evicted > 0)
      (fail "positive allowance did not retain and evict genuine images")
    putStrLn ("owned input positive allowance=" ++ show positiveLimit
      ++ " retained=" ++ show (total "original_inputs.retained_encoded_bytes" evicted)
      ++ " evicted=" ++ show (total "original_inputs.evicted_encoded_bytes" evicted))
    receiving <- writeGenuineMetadataScope work ["CanonicalDependency"] fixture
    _ <- readExactScopeWithOwner bounded receiving >>= either fail pure
    forM_ (scopeInterfaces live) $ \(iface,_,_) -> do
      bytes <- scopeInterfaceBytes live iface
      bracket (BS.readFile (exactPath iface)) (BS.writeFile (exactPath iface)) $ \_ -> do
        BS.writeFile (exactPath iface) (BS.singleton 0)
        held <- scopeInterfaceBytes live iface
        unless (held == bytes) (fail "inactive eviction dropped live original custody")
        revalidateExactScope (prHscEnv (pprPipelineResult original)) live >>= \result ->
          either fail pure result
    revalidateExactScope (prHscEnv (pprPipelineResult original)) live >>= either fail pure
  -- GHC opens FIFO descriptors nonblocking, so an already-open RDWR peer
  -- prevents an early EOF. The reader's blocked state then establishes that
  -- admission has started its incomplete-manifest read before cancellation.
  python <- lookupEnv "TIDEPOOL_TEST_PYTHON" >>= maybe (fail "missing declared fixture Python") pure
  fifo <- makeAbsolute (work </> "pending-exact-scope.cbor")
  callProcess python ["-c","import os,sys; os.mkfifo(sys.argv[1])",fifo]
  let script = "import os,sys; fd=os.open(sys.argv[1],os.O_RDWR); os.write(fd,b'\\x00'); print('ready',flush=True); sys.stdin.readline(); os.close(fd)"
      peer = (proc python ["-c",script,fifo]) {std_in=CreatePipe,std_out=CreatePipe}
  withCreateProcess peer $ \input output _ processHandle -> case (input,output) of
    (Just commands,Just responses) -> do
      ready <- timeout 5000000 (hGetLine responses)
      unless (ready == Just "ready") (fail "input cancellation peer did not open its FIFO")
      readerDone <- newEmptyMVar
      bracket (forkIO ((try (readExactScopeWithOwner owner fifo) :: IO (Either SomeException (Either String ExactScope)))
          >>= putMVar readerDone)) killThread $ \reader -> do
        let await = do
              status <- threadStatus reader
              case status of
                ThreadBlocked reason -> pure reason
                ThreadRunning -> threadDelay 1000 >> await
                _ -> fail ("exact input admission did not block on its partial manifest: " ++ show status)
        blocked <- timeout 5000000 await
        case blocked of
          Just reason -> putStrLn ("owned input cancelled admission blocked=" ++ show reason)
          Nothing -> fail "exact input admission did not reach its pending read"
        killThread reader
        settled <- timeout 5000000 (takeMVar readerDone)
        unless (case settled of
          Just (Left problem) -> fromException problem == Just ThreadKilled
          _ -> False) (fail "exact input admission swallowed cancellation or failed to settle")
      hPutStrLn commands "release"
      hFlush commands
      completed <- timeout 5000000 (waitForProcess processHandle)
      unless (completed == Just ExitSuccess) (fail "cancelled input admission retained its fixture peer")
    _ -> fail "input cancellation peer lacks its declared pipes"
  (_,afterCancellation) <- captureDiagnostics (readExactScopeWithOwner owner thirdPath >>= either fail pure)
  unless (total "original_inputs.certificate.hits" afterCancellation == 2)
    (fail "cancelled admission discarded the previous complete owner state")
  replacement <- newExactInputOwner
  (_,afterReplacement) <- captureDiagnostics (readExactScopeWithOwner replacement thirdPath >>= either fail pure)
  unless (total "original_inputs.certificate.misses" afterReplacement == 2)
    (fail "replacement owner inherited cancelled physical-worker facts")
  putStrLn ("owned exact inputs: A/B/A reuse, deleted manifests, rotation, positive/live and zero-budget eviction, cancelled admission and replacement passed\n" ++ warm)

validationTimingControl :: IO ()
validationTimingControl = do
  (minimalResult,minimalLog) <- bracket
    ((,) <$> lookupEnv "TIDEPOOL_TIMING_SUMMARY" <*> lookupEnv "TIDEPOOL_TIMING")
    (\(summary,detail) -> maybe (unsetEnv "TIDEPOOL_TIMING_SUMMARY") (setEnv "TIDEPOOL_TIMING_SUMMARY") summary
      >> maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING") detail)
    $ \_ -> do
      setEnv "TIDEPOOL_TIMING_SUMMARY" "1"
      unsetEnv "TIDEPOOL_TIMING"
      enabled <- readSummaryTimingEnabled
      captureDiagnostics (withValidationTiming enabled "exact_scope" "explicit_scope_validation"
        (pure [("scope_count",1)]) (pure (Right ())))
  unless (minimalResult == Right () && length [() | line <- lines minimalLog
      , "tidepool-validation " `isPrefixOf` line] == 1
      && not ("tidepool-timing-detail " `isInfixOf` minimalLog))
    (fail "minimal timing summary omitted its validation row or enabled detailed events")
  withTiming $ do
    let run :: IO (Either String ()) -> IO (Either String (),String)
        run action = captureDiagnostics
          (withValidationTiming True "exact_scope" "checked_receipt_publication"
            (pure [("scope_count",1),("input_count",2),("observed_file_count",3),("observed_bytes",17)]) action)
    (accepted,acceptedLog) <- run (pure (Right ()))
    (refused,refusedLog) <- run (pure (Left "controlled refusal"))
    (thrown,exceptionLog) <- captureDiagnostics
      (try (withValidationTiming True "exact_scope" "checked_receipt_publication"
        (pure []) (throwIO ThreadKilled)) :: IO (Either AsyncException (Either String ())))
    unless (accepted == Right () && refused == Left "controlled refusal"
        && either (const True) (const False) thrown)
      (fail "validation timing changed a validation result or swallowed its exception")
    let rows logText = filter ("tidepool-validation " `isPrefixOf`) (lines logText)
        allRows = concatMap rows [acceptedLog,refusedLog,exceptionLog]
        has outcome row = ("\"outcome\":\"" ++ outcome ++ "\"") `isInfixOf` row
    unless (length allRows == 3
        && length (filter (has "success") allRows) == 1
        && length (filter (has "refused") allRows) == 1
        && length (filter (has "exception") allRows) == 1
        && all ("\"clock_domain\":\"ghc_monotonic_ns\"" `isInfixOf`) allRows
        && all ("\"start_ns\":" `isInfixOf`) allRows
        && all ("\"end_ns\":" `isInfixOf`) allRows
        && all (\row -> "\"allocated_bytes\":" `isInfixOf` row
            || "\"rts_scope\":\"unavailable\"" `isInfixOf` row) allRows
        && all ("\"invocation_id\":" `isInfixOf`) allRows
        && "\"observed_file_count\":3" `isInfixOf` acceptedLog)
      (fail ("validation timing summary lost a bounded field or terminal outcome: " ++ show allRows))

-- This fault injector invokes the actual libtest binary. It never derives
-- success from rendered test output; receipt mutations use independently known
-- request/output bytes and paths instead of reproducing the completion decoder.
data IssuerStep = IssueNormally | SelectZeroCases | ProducerError | ReplayCompletion
  | WrongProducer | WrongRequest | WrongPacket | WrongOutput | ChangeOutput
  deriving (Eq, Show, Enum, Bounded)

instance Arbitrary IssuerStep where
  arbitrary = elements [minBound .. maxBound]
  shrink IssueNormally = []
  shrink SelectZeroCases = [IssueNormally]
  shrink _ = [IssueNormally, SelectZeroCases]

fixtureIssuerCountingHistories :: IO ()
fixtureIssuerCountingHistories = withTiming $ withScratch $ \work -> do
  forM_ ["OptionalAnchor.hs", "OptionalSupport.hs", "OptionalWarmer.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty GeneralCompile
    Nothing (work </> "OptionalWarmer.hs") [work] Nothing
  fixture <- capturePreparedFixture work original
  writeGenuineCandidateManifestFor ["OptionalAnchor"] work fixture
  term <- readCodecTerm (work </> "module-candidates.cbor")
  graph <- case term of
    TList [_,_,_,_,_,TList [TList (TList [_,TString path]:_),_],_] -> pure (T.unpack path)
    _ -> fail "genuine fixture lacks an independently validated graph companion"
  graphBytes <- BS.readFile graph
  let authored = work </> "Tidepool" </> "Session" </> "Lib" </> "G1.hs"
      originalAction = do
        writeGenuineCandidateManifestFor ["OptionalAnchor"] work fixture
        selected <- readModuleCandidates (work </> "module-candidates.cbor") >>= either fail pure
        unless (map candidateModule selected == ["OptionalAnchor"])
          (fail "genuine completion changed requested candidate owners")
      actions =
        [("original", originalAction)
        ,("authored", do
            path <- writeGenuineAuthoredDeclarationScope
              (SessionModule LibMod (Generation 1)) [work] authored work
            scope <- readExactScope path >>= either fail pure
            unless (not (Map.null (scopeModuleInterfaceProofs scope)))
              (fail "authored completion omitted original canonical evidence"))
        ,("codec", do
            path <- writeCandidateCodecFixture work EmptyCandidateInventory
            selected <- readModuleCandidates path >>= either fail pure
            unless (map candidateModule selected == ["Fixture"]
              && all (null . candidateGroups) selected
              && all ((== Nothing) . candidateExecutionSource) selected)
              (fail "empty structural codec changed its group inventory or execution parcel"))]
  createDirectoryIfMissing True (takeDirectory authored)
  writeFile authored "module Tidepool.Session.Lib.G1 where\nfixtureValue :: Int\nfixtureValue = 41\n"
  -- Save a valid old packet receipt. Replaying it into a fresh packet must
  -- refuse even when its original output files remain valid and readable.
  oldPacket <- newPacketDirectory work "prior-codec"
  BS.writeFile (oldPacket </> "request.cbor") (toStrictByteString (encodeTerm
    (TList [TString "TPCODECFIXTURE1", TString "candidate_empty", TList []])))
  _ <- runPacketProducer CodecProducer oldPacket
  copyFile (oldPacket </> "completion.cbor") (work </> "prior-completion.cbor")
  duplicate <- try (void (runPacketProducer CodecProducer oldPacket)) :: IO (Either IOException ())
  case duplicate of Left _ -> pure (); Right _ -> fail "completed packet was issued twice"
  issuer <- lookupEnv "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER" >>= maybe (fail "missing fixture issuer") pure
  python <- lookupEnv "TIDEPOOL_TEST_PYTHON" >>= maybe (fail "missing declared fixture Python") pure
  let wrapper = work </> "faulted-fixture-issuer"
  writeFile wrapper ("#!" ++ python ++ "\n" ++ unlines
    ["import os,sys,subprocess,hashlib,pathlib,time"
    ,"args=sys.argv[1:]"
    ,"issuer=" ++ show issuer
    ,"if '--list' in args: os.execv(issuer,[issuer]+args)"
    ,"packet=pathlib.Path(os.environ['TIDEPOOL_CANDIDATE_FIXTURE_PACKET'])"
    ,"mode=os.environ.get('TIDEPOOL_FIXTURE_FAULT','IssueNormally')"
    ,"receipt=packet/'completion.cbor'"
    ,"if mode=='ProducerError': sys.exit(23)"
    ,"if mode=='SelectZeroCases': os.execv(issuer,[issuer,'--exact','fixture_counting_intentionally_missing','--ignored','--nocapture'])"
    ,"if mode=='ReplayCompletion':"
    ,"    receipt.write_bytes(pathlib.Path(" ++ show (work </> "prior-completion.cbor") ++ ").read_bytes()); sys.exit(0)"
    ,"status=subprocess.call([issuer]+args)"
    ,"if status: sys.exit(status)"
    ,"if mode=='CancelAfterIssuance':"
    ,"    marker=pathlib.Path(" ++ show (work </> "cancel-ready") ++ "); temporary=marker.with_suffix('.ready')"
    ,"    temporary.write_text(str(os.getpid())); temporary.replace(marker); time.sleep(60); sys.exit(0)"
    ,"data=receipt.read_bytes()"
    ,"if mode=='WrongProducer':"
    ,"    for kind in [b'original-products',b'authored-declaration',b'codec']: data=data.replace(kind,kind[:-1]+b'x')"
    ,"elif mode=='WrongRequest': data=data.replace(hashlib.sha256((packet/'request.cbor').read_bytes()).digest(),bytes(32))"
    ,"elif mode=='WrongPacket': data=data.replace(str(packet).encode(),(str(packet)[:-1]+'!').encode())"
    ,"elif mode=='ChangeSharedOutput': (packet.parent/'module-candidates.cbor').write_bytes(b'stale shared output')"
    ,"elif mode=='ChangeSharedCompanion': pathlib.Path(" ++ show graph ++ ").write_bytes(b'changed companion')"
    ,"elif mode=='MissingSharedCompanion': pathlib.Path(" ++ show graph ++ ").unlink()"
    ,"elif mode in ['WrongOutput','ChangeOutput']:"
    ,"    outputs=list(packet.glob('module-candidates.cbor'))+list(packet.glob('**/exact-declaration-scope.cbor'))"
    ,"    for output in outputs:"
    ,"        if mode=='ChangeOutput': output.write_bytes(output.read_bytes()+b'x')"
    ,"        else:"
    ,"            wrong=output.with_name(output.name[:-6]+'x'+output.name[-5:]); wrong.write_bytes(output.read_bytes()); data=data.replace(str(output).encode(),str(wrong).encode())"
    ,"receipt.write_bytes(data)"
    ])
  permissions <- getPermissions wrapper
  setPermissions wrapper permissions {executable=True}
  withFixtureEnvironment "TIDEPOOL_CANDIDATE_FIXTURE_ISSUER" wrapper $ do
    let check step action = withFixtureEnvironment "TIDEPOOL_FIXTURE_FAULT" (show step) $ do
          before <- BS.readFile (work </> "module-candidates.cbor")
          outcome <- try action :: IO (Either IOException ())
          unless (either (const (step /= IssueNormally)) (const (step == IssueNormally)) outcome)
            (fail ("fixture completion acceptance mismatch at " ++ show step
              ++ ": " ++ either (take 256 . show) (const "unexpected successful delivery") outcome))
          when (step /= IssueNormally) $ do
            after <- BS.readFile (work </> "module-candidates.cbor")
            selected <- readModuleCandidates (work </> "module-candidates.cbor") >>= either fail pure
            unless (after == before && map candidateModule selected == ["OptionalAnchor"])
              (fail "refused issuer changed retained candidate output")
    check SelectZeroCases (writeGenuineCandidateManifestFor ["NeverCapturedByFixture"] work fixture)
    -- Fixed matrix proves positive, zero-selection, error and bound-identity
    -- controls through each real producer and its actual Haskell consumer.
    forM_ actions $ \(name, action) -> do
      forM_ [minBound .. maxBound] $ \step -> check step action
      let marker = work </> "cancel-ready"
      exists <- doesFileExist marker
      when exists (removeFile marker)
      withFixtureEnvironment "TIDEPOOL_FIXTURE_FAULT" "CancelAfterIssuance" $ do
        done <- newEmptyMVar
        worker <- forkIO (try action >>= putMVar done)
        ready <- timeout 10000000 (let await = doesFileExist marker >>= \seen ->
                                        unless seen (threadDelay 1000 >> await) in await)
        killThread worker
        outcome <- takeMVar done :: IO (Either SomeException ())
        unless (ready == Just () && either
          (\problem -> fromException problem == Just ThreadKilled) (const False) outcome)
          (fail ("fixture cancellation did not propagate at " ++ name))
        pid <- readFile marker
        gone <- timeout 5000000 (let await = doesDirectoryExist ("/proc" </> pid) >>= \alive ->
                                       when alive (threadDelay 1000 >> await) in await)
        unless (gone == Just ()) (fail ("cancelled fixture subprocess survived cleanup: " ++ pid))
      putStrLn ("fixture completion controls: " ++ name ++ " passed")
    withFixtureEnvironment "TIDEPOOL_FIXTURE_FAULT" "ChangeSharedOutput" $ do
      writeGenuineCandidateManifestFor ["OptionalAnchor"] work fixture
      selected <- readModuleCandidates (work </> "module-candidates.cbor") >>= either fail pure
      unless (map candidateModule selected == ["OptionalAnchor"])
        (fail "candidate delivery reopened the changed shared manifest instead of its completed snapshot")
    forM_ ["ChangeSharedCompanion", "MissingSharedCompanion"] $ \mode -> do
      withFixtureEnvironment "TIDEPOOL_FIXTURE_FAULT" mode $ do
        outcome <- try originalAction :: IO (Either IOException ())
        case outcome of
          Left _ -> pure ()
          Right _ -> fail ("independent candidate graph validation accepted " ++ mode)
      BS.writeFile graph graphBytes
    withFixtureEnvironment "TIDEPOOL_FIXTURE_FAULT" "IssueNormally" $
      forM_ [OriginalProductsProducer, AuthoredDeclarationProducer, CodecProducer] $ \producer -> do
        packet <- newPacketDirectory work "malformed-request"
        BS.writeFile (packet </> "request.cbor") (BS.singleton 0)
        outcome <- try (void (runPacketProducer producer packet)) :: IO (Either IOException ())
        exists <- doesFileExist (packet </> "completion.cbor")
        unless (either (const True) (const False) outcome && not exists)
          (fail ("failed producer issued completion: " ++ show producer))
    -- Histories mix issuance, fresh zero-selection and replay while old genuine
    -- candidate bytes stay available. The oracle is simply actual issuance iff
    -- IssueNormally; shrinking preserves the sequence of requested operations.
    result <- quickCheckWithResult stdArgs {maxSuccess=30,maxSize=8,replay=Just (mkQCGen 20261009,0)} $
      forAllShrink (resize 8 (listOf arbitrary)) shrink $ \steps -> ioProperty $ do
        forM_ steps $ \step -> check step (snd (last actions))
        pure (tabulate "issuer history operations" (map show steps) (counterexample (show steps) True))
    unless (isSuccess result) (fail "fixture completion histories failed")
  putStrLn "fixture completion: 27 producer controls, stale global request refusal, completed snapshot delivery, 2 companion refusals, 3 actual producer errors, 3 cancellations, 30 generated histories passed"

withFixtureEnvironment :: String -> String -> IO a -> IO a
withFixtureEnvironment name value action = bracket (lookupEnv name)
  (maybe (unsetEnv name) (setEnv name)) (\_ -> setEnv name value >> action)

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
      observed = nub [scopeManifestPath retained,scopeManifestPath checked]
        ++ map dependencySourcePath (dependencySources evidence)
        ++ map dependencySourcePath (dependencySources (selectedOriginalEvidence selected))
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
      validationRows diagnostics = [line | line <- lines diagnostics
        , "tidepool-validation " `isPrefixOf` line]
      validationLine diagnostics = case validationRows diagnostics of
        [line] -> line
        _ -> ""
      jsonNumber key line = case
          [value | suffix <- tails line, ("\"" ++ key ++ "\":") `isPrefixOf` suffix
            , (value,_) <- reads (drop (length key + 3) suffix)] of
        [value] -> value
        _ -> error ("validation summary lacks numeric " ++ key ++ " field: " ++ line)
      observedCounterRows diagnostics = [line | line <- lines diagnostics
        , "tidepool-count name=hash_bytes.observed_file." `isPrefixOf` line]
      counterNumber line = case [value | field <- words line
          , Just raw <- [stripPrefix "count=" field], (value,_) <- reads raw] of
        [value] -> value
        _ -> error ("invalid observed-file counter: " ++ line)
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
                && length (validationRows diagnostics) == 1
                && "\"owner\":\"exact_scope\"" `isInfixOf` validationLine diagnostics
                && "\"reason\":\"retained_receipt_publication\"" `isInfixOf` validationLine diagnostics
                && "\"outcome\":\"success\"" `isInfixOf` validationLine diagnostics
                && jsonNumber "observed_file_count" (validationLine diagnostics)
                    == fromIntegral (length (observedCounterRows diagnostics))
                && jsonNumber "observed_bytes" (validationLine diagnostics)
                    == sum (map counterNumber (observedCounterRows diagnostics))
                && null (counterValues ("hash_bytes.observed_file." ++ exactSha256 (fst shared)) diagnostics)
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
  unless (length (proofRows separateDiagnostics) == 2)
    (fail "separate validation control did not produce two independent terminal proofs")
  measurement "separate" separateDiagnostics
  (_,combinedDiagnostics) <- captureDiagnostics (publish environment)
  measurement "combined" combinedDiagnostics
  let freshOutput = work </> "fresh-retained-output.cbor"
      publicationMarker = work </> "retained-publication-marker"
      freshOutputBytes = BS.pack [12,34,56,78]
      outputSeal = either error id (freshOutputSealsFromWrites [(freshOutput,freshOutputBytes)])
      publishWithOutput = writeRetainedExactCompilationWithOutputsAndPublication
        environment retained compilation evidence outputSeal
        (BS.writeFile publicationMarker (BS.pack [9,8,7]))
  BS.writeFile freshOutput freshOutputBytes
  beforeOutputPublish <- receipts
  (outputAccepted,outputDiagnostics) <- captureDiagnostics
    (try publishWithOutput :: IO (Either IOException ()))
  afterOutputPublish <- receipts
  markerExists <- doesFileExist publicationMarker
  unless (case outputAccepted of Right () -> markerExists && length afterOutputPublish == length beforeOutputPublish + 1; _ -> False)
    (fail "terminal output proof did not publish exactly after observing a matching fresh output")
  unless (length (counterValues ("hash_bytes.observed_file." ++ digest freshOutputBytes) outputDiagnostics) == 1)
    (fail "terminal output proof omitted or repeated its fresh output observation")
  removeFile publicationMarker
  BS.writeFile freshOutput (BS.pack [0])
  beforeOutputRefusal <- receipts
  (outputRefused,refusalDiagnostics) <- captureDiagnostics
    (try publishWithOutput :: IO (Either IOException ()))
  afterOutputRefusal <- receipts
  markerAfterRefusal <- doesFileExist publicationMarker
  unless (case outputRefused of Left _ -> not markerAfterRefusal && afterOutputRefusal == beforeOutputRefusal; _ -> False)
    (fail "mutated fresh output published a receipt or staged product")
  unless (length (counterValues ("hash_bytes.observed_file." ++ digest (BS.pack [0])) refusalDiagnostics) == 1)
    (fail "refused terminal output proof did not retain its observed mismatch evidence")
  BS.writeFile freshOutput freshOutputBytes
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
  sharedBytes <- BS.readFile sharedPath
  beforeCapturedDrift <- receipts
  BS.writeFile sharedPath (sharedBytes <> BS.singleton 0)
  capturedDrift <- try (publish environment) :: IO (Either IOException ())
  afterCapturedDrift <- receipts
  BS.writeFile sharedPath sharedBytes
  unless (case capturedDrift of Right () -> length afterCapturedDrift == length beforeCapturedDrift + 1; _ -> False)
    (fail "terminal publication depended on a captured original materialization path")
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
  continuation <- quickCheckWithResult stdArgs {maxSuccess=100,maxSize=64} $
    \(NonEmpty generated) originDrift aliasDrift ->
      classify originDrift "protected origin drift"
      $ classify aliasDrift "receiving alias drift"
      $ ioProperty (continued (BS.pack [if bit then 1 else 0 | bit <- take 64 (generated :: [Bool])]) originDrift aliasDrift)
  unless (isSuccess continuation) (fail "continued original input history property failed")
  where
    continued values originDrift aliasDrift = withScratch $ \directory -> do
      let bytes = values
          historical = directory </> "previous-offer"
          oldManifest = directory </> "previous-manifest"
          original = directory </> "protected-origin"
          receiving = directory </> "receiving-alias"
          changed = bytes <> BS.singleton 123
          reference path = OriginalInputReference path (digest bytes) (BS.length bytes) [original]
            (either error id (ownedArenaRange path (toInteger (BS.length bytes)) 0))
      BS.writeFile historical bytes
      BS.writeFile oldManifest (BS.singleton 7)
      BS.writeFile original bytes
      (_,old) <- captureRequestInputs Nothing $ \reader -> do
        _ <- reader oldManifest 1
        reader historical 64
      content <- either fail pure (selectedOriginalContent [reference historical] old)
      removeFile historical
      removeFile oldManifest
      BS.writeFile receiving (if aliasDrift then changed else bytes)
      BS.writeFile original (if originDrift then changed else bytes)
      selected <- continueRequestInputs content [reference receiving]
      actual <- capturedRequestInput selected receiving (digest bytes)
      observed <- revalidateRequestInputs selected
      unless (actual == bytes && requestInputBytes selected == toInteger (BS.length bytes))
        (fail "continuation lost content or inherited historical accounting")
      unless (either (const False) (const True) observed == not (originDrift || aliasDrift))
        (fail "continuation publication disagreed with current selected-path model")
      (_,fresh) <- captureRequestInputs Nothing (\reader -> reader receiving 65)
      freshBytes <- capturedRequestInput fresh receiving (digest (if aliasDrift then changed else bytes))
      unless (freshBytes == if aliasDrift then changed else bytes)
        (fail "fresh acquisition consumed a retained original")
      cold <- try (continueRequestInputs emptyCapturedOriginalContent [reference receiving])
        :: IO (Either IOException RequestOriginalInputs)
      unless (either (const False) (const True) cold == not aliasDrift)
        (fail "rotation fallback used an unsealed materialization")
      BS.writeFile receiving bytes
      BS.writeFile original bytes
      revalidateRequestInputs selected >>= either fail pure
      rotated <- continueRequestInputs emptyCapturedOriginalContent [reference receiving]
      revalidateRequestInputs rotated >>= either fail pure
      pure True
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
  let reference destination = OriginalInputReference destination (digest bytes) 3 []
        (either error id (ownedArenaRange destination 3 0))
  (shared,counts) <- withTiming $ captureDiagnostics
    (continueRequestInputs emptyCapturedOriginalContent [reference path,reference other])
  left <- capturedRequestInput shared path (digest bytes)
  right <- capturedRequestInput shared other (digest bytes)
  unless (BSI.toForeignPtr left == BSI.toForeignPtr right
      && requestInputBytes shared == 6
      && sum (counterValues "original_inputs.content_misses" counts) == 1
      && sum (counterValues "original_inputs.content_hits" counts) == 1)
    (fail "cold continuation duplicated equal content or discarded receiving-path accounting")
  revalidateRequestInputs shared >>= either fail pure
  bracket (BS.readFile other) (BS.writeFile other) $ \_ -> do
    BS.writeFile other (BS.singleton 9)
    held <- capturedRequestInput shared other (digest bytes)
    refused <- revalidateRequestInputs shared
    unless (held == bytes && either (const True) (const False) refused)
      (fail "shared backing storage erased a receiving path's terminal obligation")
  revalidateRequestInputs shared >>= either fail pure
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
    unless (case retainRequestEncodedBytes [captureArtifactBytes bytes] bounded of Nothing -> True; _ -> False)
      (fail "new encoded graph bytes bypassed the request aggregate budget")
    let graphBytes = BS.pack [4,5]
    graphOwner <- maybe (fail "bounded encoded graph was refused") pure
      (retainRequestEncodedBytes [captureArtifactBytes graphBytes] bounded)
    unless (requestInputBytes graphOwner == 5
        && fmap requestInputBytes (retainRequestEncodedBytes [captureArtifactBytes graphBytes] graphOwner) == Just 5)
      (fail "encoded graph generations lost accounting or charged the same graph again")
    -- A donor allowance cannot enlarge the receiver. Transfer shares bytes,
    -- including duplicate donors, and observes no current producer path.
    setEnv config "64"
    (_,donor) <- captureRequestInputs Nothing (\reader -> reader other 3)
    setEnv config "5"
    content <- either fail pure (selectedOriginalContent [reference other] donor)
    continued <- continueRequestInputs content [reference path]
    unless (requestInputBytes continued == 3) (fail "continued bytes inherited the donor allowance")
    continuationOverflow <- try (continueRequestInputs content [reference path,reference copy])
      :: IO (Either IOException RequestOriginalInputs)
    unless (either (const True) (const False) continuationOverflow)
      (fail "content hits bypassed the fresh receiving aggregate budget")
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

freshOutputSealSetLaws :: IO ()
freshOutputSealSetLaws = withScratch $ \work -> do
  let firstPath = work </> "first-output.bin"
      secondPath = work </> "second-output.bin"
      firstBytes = BS.pack [1,2,3]
      secondBytes = BS.pack [4,5,6]
      conflictingBytes = BS.pack [9,8,7]
      seal writes = freshOutputSealsFromWrites writes
      merged seals = foldM appendFreshOutputSeals emptyFreshOutputSeals seals
  first <- either fail pure (seal [(firstPath,firstBytes)])
  second <- either fail pure (seal [(secondPath,secondBytes)])
  duplicateFirst <- either fail pure (seal [(firstPath,firstBytes),(firstPath,firstBytes)])
  conflict <- either fail pure (seal [(firstPath,conflictingBytes)])
  forM_ (permutations [(firstPath,firstBytes),(secondPath,secondBytes),(firstPath,firstBytes)]) $ \ordered ->
    case seal ordered of
      Left reason -> fail ("same-content output grouping depended on input order: " ++ reason)
      Right _ -> pure ()
  forM_ (permutations [first,duplicateFirst,second]) $ \ordered -> do
    joined <- either fail pure (merged ordered)
    case appendFreshOutputSeals joined conflict of
      Left _ -> pure ()
      Right _ -> fail "merged output set lost same-path conflict detection"
  case seal [(firstPath,firstBytes),(firstPath,conflictingBytes)] of
    Left _ -> pure ()
    Right _ -> fail "seal factory accepted conflicting writes to one path"
  case appendFreshOutputSeals first conflict of
    Left _ -> pure ()
    Right _ -> fail "seal union accepted conflicting content for one path"
  _ <- either fail pure (appendFreshOutputSeals emptyFreshOutputSeals first)
  _ <- either fail pure (appendFreshOutputSeals second emptyFreshOutputSeals)
  pure ()
