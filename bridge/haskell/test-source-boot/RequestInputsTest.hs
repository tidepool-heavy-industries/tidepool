{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ForeignFunctionInterface #-}
{-# LANGUAGE PatternSynonyms #-}

module RequestInputsTest (ownedArenaRangeReads, artifactByteOwnership, requestInputHistories, requestInputBoundaries, retainedCompilationPublication, fixtureIssuerCountingHistories, ownedScopeInputReuse, capturedCanonicalPromotionRefusesReadableMissingInputs, validationTimingControl, compilerTimingRoundtrip, compilerTaskAttemptHistories, freshOutputSealSetLaws, compilerServiceBootBoundaries, worldCellAdmissionParse, worldRejectionDiagnosticBoundaries) where

import Tidepool.RequestCompletion (finishCompletion)

import Control.Exception (AsyncException(ThreadKilled), IOException, SomeException, bracket, bracketOnError, onException, fromException, throwIO, try, finally, mask)
import Control.Concurrent (forkIO, killThread, newEmptyMVar, putMVar, takeMVar, readMVar, threadDelay)
import Control.Monad ((>=>), foldM, forM, forM_, unless, void, when)
import qualified Data.ByteString.Char8 as BS8
import qualified Data.ByteString as BS
import qualified Crypto.Hash.SHA256 as SHA
import qualified Crypto.Hash as Crypto
import Numeric (readHex)
import qualified Data.ByteString.Lazy as BL
import qualified Data.ByteString.Internal as BSI
import Data.IORef (IORef, newIORef, modifyIORef', readIORef, writeIORef)
import Data.Bits (xor)
import Data.List (sortOn, isInfixOf, isPrefixOf, nub, permutations, stripPrefix, tails)
import Data.Maybe (isNothing)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.Directory (canonicalizePath, copyFile, createDirectory, createDirectoryIfMissing, doesDirectoryExist, doesFileExist, getPermissions, getTemporaryDirectory, listDirectory, makeAbsolute, removeFile, removeDirectory, setPermissions, Permissions(..))
import System.Posix.Files (createSymbolicLink)
import System.FilePath ((</>), takeDirectory, makeRelative, takeExtension)
import System.IO (hClose, hFlush, hGetLine, hPutStrLn, openTempFile, Handle, stderr, stdout, utf8, BufferMode(NoBuffering), hGetEncoding, hGetBuffering, hSetEncoding, hSetBuffering, hSetBinaryMode)
import GHC.IO.Handle (hDuplicate, hDuplicateTo)
import Foreign.C.String (CString, withCString)
import Foreign.C.Types (CInt(..), CUInt(..))
import System.Posix.Types (Fd(..))
import System.Posix.IO (fdToHandle, closeFd)
import System.Posix.Process (getProcessID)
import System.Process (CreateProcess(..), StdStream(CreatePipe), callProcess, proc, waitForProcess, withCreateProcess)
import System.Exit (ExitCode(ExitSuccess))
import GHC.Conc (ThreadStatus(..), threadStatus)
import GHC.IO.Buffer (Buffer(..), CharBuffer, readCharBuf)
import GHC.IO.Encoding.Types (BufferCodec(BufferCodec), TextEncoding(TextEncoding))
import System.Timeout (timeout)
import Test.QuickCheck
import Test.QuickCheck.Random (mkQCGen)
import GHC.Driver.Env (HscEnv(..))
import GHC (getSession, setSession, setContext, InteractiveImport(..), simpleImportDecl, exprType, TcRnExprMode(TM_Inst))
import GHC.Data.ShortText qualified as PackageText
import GHC.Unit.Env (ue_setUnits)
import GHC.Unit.State (UnitState(..))
import GHC.Unit.Info (unitImportDirs, unitLibraryDirs, unitLibraryDynDirs, unitIncludeDirs)
import GHC.Types.Unique.Map (mapUniqMap)
import GHC.Driver.Env (hsc_units)
import Control.Monad.IO.Class (liftIO)
import GHC.Unit.Finder (initFinderCache, addModuleToFinder)
import GHC.Unit.Finder.Types (FinderCache(..))
import GHC.Unit.Module (mkModule, mkModuleName)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (stringToUnit, GenWithIsBoot(..))
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import Tidepool.Binders (analyzeCellWithFlags, cellPlanPrologue, analyzeOrderedCellWithFlags, BoundBinder(..)
  , CellSourcePlan(..), CellAnalysisItem(..), TurnKind(..), StmtBinders(..), cellInferenceSegments
  , prepareTypedSegmentSource, preparedTypedSegmentPlan, preparedTypedSegmentSource, preparedTypedSegmentOperations)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyResolution(..) )
import Tidepool.ExactHydration (ExactIfaceArtifact(..), hydrateExactScope)
import Tidepool.DeclarationJoin (DeclarationExport(..), ExportIdentity(..), interfaceExports, encodeDeclarationExports, normalizeDeclarationExports)
import Tidepool.ExactScope
  ( ExactScope, ExactCompilation(..), SourceSelectedOriginals(..), scopeManifestPath, scopeRequestSha256
  , scopeInterfaces, scopeModuleInterfaceProofs, scopeProducerSha256, scopeProducts, ExactProduct(..)
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalCoreArtifact
  , CanonicalInterfaceProof, bindCanonicalProofInputs, captureCanonicalProofs, CanonicalInterfaceUse(..)
  , canonicalCorePath, canonicalCoreSha256, readExactScope, revalidateExactScope
  , ExactScopeValidationReason(..), revalidateExactScopesAtWithOutputs
  , writeCheckedExactCompilation, writeRetainedExactCompilation
  , ExactCompilationPublication(..), writeRetainedExactCompilationWithOutputsAndPublication
  , FreshOutputSeals, emptyFreshOutputSeals, freshOutputSealsFromWrites, appendFreshOutputSeals
  , newExactInputOwner, readExactScopeWithOwner, scopeInterfaceBytes, readScopedInterfaces
  , exactInputRetentionLimits, commitWorldInputRetention
  , worldInputsLexicalSurface, worldLexicalImports, worldLexicalDeclaration
  , WorldObservationProfile(..), worldLexicalObservation, worldObservationProfile
  , worldObservationFragment, worldObservationImports, worldObservationRequestTypes, worldObservationInputType
  , worldInputsSources, worldSourceDomainSha256
  , readWorldSuccessorWithOwner, worldSuccessorFacts, worldSuccessorChangedAliases
  , readWorldInputsWithOwner, readWorldStringMap, WorldInputFacts, worldInputsIdentity, worldInputsIdentityBytes, worldInputsBindings, worldInputsBindingScope, worldInputsRetainedCharge, worldInputsPrimaryIsEmpty, worldHasSelectedSource, readWorldSourceInputs, WorldSourceClosureEntry(..), readWorldTypeInputs, worldTypeEntryArtifact, worldTypeEntryInterface, worldTypeEntryPackages, hydrateWorldDeclarationInputs, verifiedWorldDeclarationInterface, verifiedWorldDeclarationExports, WorldInputsFailure(..), WorldInputsFailureCause(..) )
import Tidepool.FatIface (readExactInterface)
import Tidepool.CompilerRequest (withBoundCompilerRequest, runBoundWorldCellPlan, boundWorldCellPreparation
  , prepareWorldCell, commitPreparedWorldDependencies)
import Tidepool.ExtractRequest
  ( ExactInputsRequest(..), ExactInputsAcquisition(..), EnsureExactInputsOutcome(..)
  , WorldCellRecipe(..), WorldCellPlanRequest(..), WorldCellPrepareRequest(..), PlannedCellSlot(..)
  , RequestField(..), WorkerRequest, workerArgv, workerRequestFromArgv )
import Tidepool.GhcPipeline
  ( PipelineSelection(..), CompilePurpose(..), pprPipelineResult, prHscEnv, runPipelineSessionSelected
  , scopedEnsureWorld, scopedCompileWorldCell, scopedPrepareWorldCellStage
  , heldWorldCellStagePath, heldWorldCellStageTypedSource
  , heldWorldCellQuestion, heldWorldCellPlan, heldWorldCellSlots
  , WorldCellRefusal(..), emitWorldAcquisitionRejection
  , pprReuseContext, pprModules, pprProductInterfaces, preparedSegmentProducts
  , preparedExactCompilation, preparedFreshDependencies, withSourceImportIntents
  , withResidentCompilerScopes, scopedRunGhc )
import Tidepool.PackageWitness (PackageImportRoot(..), PackageImportEvidence(..), decodeCapturedPackageImports)
import Tidepool.PackageWitness (readCompilerPackageBoot, requireObservationPackageEnvironment, captureSupportedPackageProfileWith)
import Tidepool.ArtifactBytes
import Tidepool.RequestInputs
import Tidepool.OwnedInputTransport (OriginalInputKind(..), inputImageDigest, inputKindTag)
import Tidepool.OwnedInputTransport qualified as Transport
import Tidepool.Session (SessionScope(..), emptySessionScope, SessionModule(..), SessionModuleKind(..), Generation(..)
  , capturedSessionInterface, capturedSessionInterfaceEvidence)
import Tidepool.Test.GenuineCandidate (writeGenuineMetadataScope, writeGenuineCandidateManifestFor, writeGenuineAuthoredDeclarationScope, writeGenuineExecutionScope, writeGenuineExecutionScopeWithContext, writeGenuinePublishedScopeWithContext)
import Tidepool.ModuleCandidates (readModuleCandidates, candidateModule, candidateGroups, candidateExecutionSource)
import Tidepool.Test.CandidateCodec (CandidateCodecCase(..), writeCandidateCodecFixture)
import Tidepool.Test.FixturePacket (PacketProducer(..), newPacketDirectory, runPacketProducer, issueCodecFixturePacket)
import Tidepool.Timing (readSummaryTimingEnabled, withValidationTiming, TaskAttempt(..), newTimingRequestIdentity)
import Tidepool.CompilerExecution (compilerExecutionGrant, withCompilerExecutor, runCompilerTasksWithAttempt)
import Tidepool.CompilerProducts (prepareCompilerProjectionContext, prepareOriginalProductsWithCacheAndReuse, retainTypeInterfaceCaptures)
import Tidepool.PreparedStg (newPreparedBodyCache, pmModule, pmIdentityResolver)
import Tidepool.DiagJson (InputRejection(..))
import Tidepool.SessionArtifacts (prepareTypedSegmentSessionBindings, typedSegmentSessionEnvironment
  , typedSegmentSessionGlobals, typedSegmentSessionInterfaces, resolveTypedSegmentBindings
  , resolvedTypedItemOrdinal, resolvedTypedItemBinders, resolvedTypedItemInterface)
import GHC.Unit.Module (moduleName, moduleNameString)
import Codec.CBOR.Term (Term(..), encodeTerm, decodeTerm)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import SourceBootCases (admitCheckedScope, counterValues)
import CodecFixtureSupport
  ( readCodecTerm, readScopeCodecFixture, scopeCodecField, scopeCodecAcquisition
  , ScopeCodecField(..), ScopeCodecAcquisition(..), replaceScopeCodecAcquisition, scopeCodecTerm )
import SourceBootFixtureSupport (withScratch, withTiming, digest, capturePreparedFixture, captureDiagnostics, requireUserError)

foreign import ccall unsafe "memfd_create" createArena :: CString -> CUInt -> IO CInt
foreign import ccall unsafe "fcntl" sealArena :: CInt -> CInt -> CInt -> IO CInt

withSealedInputBytes :: BS.ByteString -> (FilePath -> IO a) -> IO a
withSealedInputBytes bytes action = bracket (acquireSealedInputBytes bytes) (hClose . fst) (action . snd)

acquireSealedInputBytes :: BS.ByteString -> IO (Handle,FilePath)
acquireSealedInputBytes = acquireInputBytes True

acquireInputBytes :: Bool -> BS.ByteString -> IO (Handle,FilePath)
acquireInputBytes seal bytes = do
  raw <- withCString "tidepool-artifact-byte-test" (\name -> createArena name 3)
  when (raw < 0) (fail "memfd_create failed")
  handle <- bracketOnError (pure (Fd raw)) closeFd fdToHandle
  (do BS.hPut handle bytes
      hFlush handle
      when seal $ do
        result <- sealArena raw 1033 15
        unless (result == 0) (fail "owned fixture arena sealing failed")
      pid <- getProcessID
      pure (handle,"/proc/" ++ show pid ++ "/fd/" ++ show raw))
    `onException` hClose handle

-- Exercise the production cold reader against real sealed Linux arenas. The
-- logical artifact path is absent throughout; it never becomes a read source.
ownedArenaRangeReads :: IO ()
ownedArenaRangeReads = withScratch $ \work -> do
  let bytes = BS.pack [10,20,30]
      backing = BS.pack [90,91] <> bytes <> BS.pack [92,93]
      acquire seal = acquireInputBytes seal backing
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
    -- These receiving aliases deliberately contain valid readable bytes. A
    -- missing continued part must refuse rather than acquire any trap file.
    forM_ ["interface.hi", "packages", "certificate", "core", "native", "census", "execution-graph"] $ \name -> do
      let trap = work </> name
      BS.writeFile trap bytes
      attempted <- try (withCapturedRequestInputTokens cold $ \reader -> reader trap 3)
        :: IO (Either IOException (ArtifactBytes,RequestOriginalInputs))
      unless (either (const True) (const False) attempted)
        (fail ("continued input used a readable trap file: " ++ name))
      unchanged <- BS.readFile trap
      unless (unchanged == bytes && requestInputBytes cold == 3)
        (fail "refused continued input changed its prior owner or trap")
    (strictBody,_) <- withCapturedRequestInputTokens cold $ \reader ->
      reader (originalInputPath selected) 3
    unless (artifactBytes strictBody == bytes) (fail "strict continued reader lost its sealed part")
    overBound <- try (withCapturedRequestInputTokens cold $ \reader ->
      reader (originalInputPath selected) 2)
      :: IO (Either IOException (ArtifactBytes,RequestOriginalInputs))
    unless (either (const True) (const False) overBound)
      (fail "strict continued reader ignored its consumer bound")
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

-- The genuine issuer owns semantic selection and certificates. This test
-- adapts only its file transport to a live sealed arena in the consuming process;
-- the production parser validates the complete receiving projection again.
ownedFixtureScope :: IORef [Handle] -> FilePath -> IO FilePath
ownedFixtureScope handles path = do
  (scope,fixture) <- readScopeCodecFixture path
  unless (scopeCodecAcquisition fixture == Just FreshCodecInputs)
    (fail "owned transport fixture requires the independent file-only issuer")
  let producer = scopeProducerSha256 scope
      ifaceParts = Map.fromListWith (++)
        [((exactUnit iface,exactModule iface),[(InputInterface,exactPath iface,exactSha256 iface),
          (InputPackages,packages,packageSha)]) | (iface,packages,packageSha) <- scopeInterfaces scope]
      certificateParts = Map.map (\proof ->
        [(InputCertificate,canonicalCertificatePath proof,canonicalCertificateSha256 proof)]
          ++ [(InputCore,canonicalCorePath core,canonicalCoreSha256 core)
            | core <- maybe [] pure (canonicalCoreArtifact proof)]) (scopeModuleInterfaceProofs scope)
  nativeParts <- case scopeCodecField ScopeCodecNativeProducts fixture of
    TList rows -> Map.fromListWith (++) <$> forM rows (\row -> case row of
      TList [TString unit,TString name,_,_,_,_,_,TList [TString censusPath,TString censusSha],_,_] -> do
        let key = (T.unpack unit,T.unpack name)
        product' <- maybe (fail "issued native census lacks its admitted product") pure
          (Map.lookup key (Map.fromList [((originalUnit value,originalModule value),value)
            | value <- scopeProducts scope]))
        pure (key,[(InputNative,originalProductPath product',originalProductSha256 product'),
          (InputCensus,T.unpack censusPath,T.unpack censusSha)])
      _ -> fail "issued native selection has another row shape")
    _ -> fail "issued native selection has another inventory shape"
  graphParts <- case scopeCodecField ScopeCodecExecution fixture of
    TNull -> pure Map.empty
    TList [TList descriptors,TList references] -> do
      let byDigest = Map.fromList [(sha,T.unpack graphPath) | TList [TString sha,TString graphPath] <- descriptors]
      Map.fromListWith (++) <$> forM references (\reference -> case reference of
        TList [TString unit,TString name,_,_,_,TString sha] -> case Map.lookup sha byDigest of
          Just graphPath -> pure ((T.unpack unit,T.unpack name),[(InputGraph,graphPath,T.unpack sha)])
          Nothing -> fail "issued graph selection lacks its descriptor"
        _ -> fail "issued graph selection has another reference shape")
    _ -> fail "issued graph selection has another parcel shape"
  let owners = Map.unionsWith (++) [ifaceParts,certificateParts,nativeParts,graphParts]
  bodies <- forM (Set.toAscList (Set.fromList (concat (Map.elems owners)))) $ \(_,input,sha) -> do
    bytes <- BS.readFile input
    unless (digest bytes == sha && not (BS.null bytes)) (fail ("issued fixture bytes differ from their original seal: " ++ input ++ " expected=" ++ sha ++ " actual=" ++ digest bytes))
    pure (input,(sha,bytes))
  let allBodies = Map.fromList bodies
      (extent,ranges) = foldl (\(offset,parts) (input,(_,bytes)) ->
        (offset + BS.length bytes,Map.insert input (offset,BS.length bytes) parts))
        (0,Map.empty) (Map.toAscList allBodies)
  (handle,endpoint) <- acquireSealedInputBytes (BS.concat [bytes | (_,(_,bytes)) <- Map.toAscList allBodies])
  modifyIORef' handles (handle:)
  transport <- either fail pure (ownedArenaRange endpoint (toInteger extent) 0)
  let text = TString . T.pack
      part owner = forM (sortOn (\(kind,_,sha) -> (kind,sha)) owner) $ \(kind,input,sha) -> do
        let (offset,size) = ranges Map.! input
        selectedRange <- either fail pure (ownedArenaRange endpoint (toInteger extent) (toInteger offset))
        pure (kind,OriginalInputReference input sha size [] selectedRange,offset)
  images <- forM (Map.toAscList owners) $ \(key@(unit,name),parts) -> do
    selected <- part parts
    let references = [(kind,reference) | (kind,reference,_) <- selected]
        emit (kind,reference,offset) = TList [text (inputKindTag kind),text (originalInputPath reference),
          text (originalInputSha256 reference),TInt (originalInputLength reference),TList [],TList [TInt 0,TInt offset]]
    pure (TList [text producer,text unit,text name,text (inputImageDigest producer key references),TList (map emit selected)])
  -- Exercise the real cold range reader before publishing the adapted frame.
  probe <- continueRequestInputs emptyCapturedOriginalContent
    [OriginalInputReference endpoint (digest (BS.concat [bytes | (_,(_,bytes)) <- Map.toAscList allBodies])) extent [] transport]
  unless (requestInputBytes probe == toInteger extent) (fail "sealed fixture arena lost its exact extent")
  let acquisition = ContinueCodecOriginals (TList [TList [text endpoint,TInt extent]]) (TList images)
  BS.writeFile path (toStrictByteString (encodeTerm
    (scopeCodecTerm (replaceScopeCodecAcquisition acquisition fixture))))
  _ <- readExactScope path >>= either fail pure
  pure path

-- Descriptor-completeness control: mutate only an already admitted packet.
-- This refuses before the strict reader; reached-reader sensitivity is covered
-- separately by incomplete captured canonical promotion below.
-- Its genuine artifact files remain
-- readable, and the remaining image digest is valid, so missing-part refusal
-- cannot be explained by an unreadable trap or a stale image checksum.
missingOwnedPartsRefuse :: FilePath -> IO ()
missingOwnedPartsRefuse path = do
  (_,fixture) <- readScopeCodecFixture path
  (arenas,terms) <- case scopeCodecAcquisition fixture of
    Just (ContinueCodecOriginals arenas (TList images)) -> pure (arenas,images)
    _ -> fail "missing-part control needs an admitted continued packet"
  images <- case deserialiseFromBytes Transport.decodeInputAcquisition
      (BL.fromStrict (toStrictByteString (encodeTerm (scopeCodecField ScopeCodecAcquisition fixture)))) of
    Right (remaining,Transport.ContinueOwnedOriginals images) | BL.null remaining -> pure images
    _ -> fail "admitted continued packet lost its input images"
  unless (length images == length terms) (fail "continued image oracle lost a packet row")
  forM_ [InputInterface,InputPackages,InputCertificate,InputCore,InputNative,InputCensus,InputGraph] $ \kind -> do
    (key,reference) <- case [(key,reference) | Transport.OriginalInputImage _ key _ parts <- images
        , (actual,reference) <- parts, actual == kind] of
      selected : _ -> pure selected
      [] -> fail ("genuine executable packet lacks control kind " ++ show kind)
    trap <- BS.readFile (originalInputPath reference)
    unless (digest trap == originalInputSha256 reference && BS.length trap == originalInputLength reference)
      (fail "missing-part trap differs from its valid original")
    changed <- forM (zip images terms) $ \(Transport.OriginalInputImage producer owner _ parts,term) ->
      if owner /= key then pure term else case term of
        TList [p,u,n,_,TList encodedParts] -> do
          let retained = filter ((/=kind) . fst) parts
              keep part = case part of
                TList (TString tag : _) -> tag /= T.pack (inputKindTag kind)
                _ -> False
          pure (TList [p,u,n,TString (T.pack (inputImageDigest producer owner retained)),
            TList (filter keep encodedParts)])
        _ -> fail "admitted image has another shape"
    let mutated = path ++ ".missing-" ++ inputKindTag kind
    BS.writeFile mutated (toStrictByteString (encodeTerm (scopeCodecTerm
      (replaceScopeCodecAcquisition (ContinueCodecOriginals arenas (TList changed)) fixture))))
    refused <- readExactScope mutated
    unless (either (const True) (const False) refused)
      (fail ("continued packet acquired a missing part from its readable trap: " ++ show kind))
    unchanged <- BS.readFile (originalInputPath reference)
    unless (unchanged == trap) (fail "missing-part refusal changed its original trap")

missingCapturedProofPartsRefuse :: ExactScope -> IO ()
missingCapturedProofPartsRefuse scope = do
  (key,proof,core) <- case [(key,proof,core) | (key,proof) <- Map.toAscList (scopeModuleInterfaceProofs scope)
      , Just core <- [canonicalCoreArtifact proof]] of
    selected : _ -> pure selected
    [] -> fail "genuine executable scope lacks canonical Core for promotion controls"
  (iface,packages,_) <- case [row | row@(iface,_,_) <- scopeInterfaces scope
      , (exactUnit iface,exactModule iface) == key] of
    [row] -> pure row
    _ -> fail "canonical promotion control lacks one exact interface row"
  let required = [exactPath iface,packages,canonicalCertificatePath proof,canonicalCorePath core]
      native = [originalProductPath product | product <- scopeProducts scope
        , (originalUnit product,originalModule product) == key]
      acquire paths = snd <$> captureRequestInputs Nothing (\reader ->
        mapM_ (\path -> reader path (64 * 1024 * 1024)) paths)
  full <- acquire (required ++ native)
  _ <- captureCanonicalProofs Nothing [(bindCanonicalProofInputs full proof,ExecutableInterfaceUse)]
  forM_ required $ \missing -> do
    trap <- BS.readFile missing
    partial <- acquire (filter (/=missing) (required ++ native))
    rejected <- try (captureCanonicalProofs Nothing
      [(bindCanonicalProofInputs partial proof,ExecutableInterfaceUse)])
      :: IO (Either IOException [CanonicalInterfaceProof])
    unless (either (const True) (const False) rejected)
      (fail ("captured canonical promotion read a missing part from disk: " ++ missing))
    unchanged <- BS.readFile missing
    unless (unchanged == trap) (fail "refused canonical promotion changed its readable trap")

-- This reached-reader control needs an actual admitted canonical proof, not a
-- reconstructed native scope or a replacement owned acquisition envelope.
capturedCanonicalPromotionRefusesReadableMissingInputs :: IO ()
capturedCanonicalPromotionRefusesReadableMissingInputs = withScratch $ \work -> do
  forM_ ["CanonicalSource.hs","CanonicalDependency.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
    Nothing (work </> "CanonicalSource.hs") [work] Nothing
  fixture <- capturePreparedFixture work original
  path <- writeGenuineExecutionScope ["CanonicalSource","CanonicalDependency"] ["CanonicalSource"] work fixture
  scope <- readExactScope path >>= either fail pure
  missingCapturedProofPartsRefuse scope

-- Raw transfer controls consume actual acquired rows and all three Materializer
-- roots from one genuine issuer completion. Only physical arena endpoints are
-- adapted into this test's existing sealed-byte owner; semantic facts are kept.
semanticWorldBindingControls :: IORef [Handle] -> FilePath -> FilePath -> BS.ByteString -> IO ()
semanticWorldBindingControls = semanticWorldInputControls Nothing

semanticWorldInputControls :: Maybe [String] -> IORef [Handle] -> FilePath -> FilePath -> BS.ByteString -> IO ()
semanticWorldInputControls published handles work scopePath contextParcel = do
  scope <- readExactScope scopePath >>= either fail pure
  producer <- rawHex (scopeProducerSha256 scope)
  let encode = toStrictByteString . encodeTerm
      decode bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
        Right (remaining,value) | BL.null remaining -> pure value
        _ -> fail "world fixture needs one complete CBOR value"
      decodePairs (TList entries) = forM entries $ \entry -> case entry of
        TList [TBytes cid,TBytes body] -> pure (cid,body)
        _ -> fail "world fixture needs exact CID/body pairs"
      decodePairs _ = fail "world fixture omitted its block array"
      parcel bytes = decode bytes >>= \term -> case term of
        TList [TBytes root,TBytes association,TList rows,acquisition,pairs,TBytes lexical] -> do
          blocks <- decodePairs pairs
          pure (root,association,rows,acquisition,blocks,lexical)
        _ -> fail "world fixture needs actual Context/association roots, rows, owned bytes and blocks"
      fixture operation arguments = do
        path <- issueCodecFixturePacket work (operation ++ ".cbor") (encode
          (TList [TString "TPCODECFIXTURE1",TString (T.pack operation),TList arguments]))
        BS.readFile path
      sealed (TList [TString "TPCWORLDFIXTUREBYTES1",TList arenas,images]) = do
        live <- forM arenas $ \entry -> case entry of
          TList [TString path,extent] -> do
            amount <- unsigned extent
            bytes <- BS.readFile (T.unpack path)
            unless (toInteger (BS.length bytes) == amount) (fail "world fixture arena extent drifted")
            mask $ \_ -> do
              (handle,endpoint) <- acquireSealedInputBytes bytes
              modifyIORef' handles (handle:)
              pure (TList [TString (T.pack endpoint),extent])
          _ -> fail "world fixture needs exact durable arena DATA"
        pure (TList [TString "continue-originals",TList live,images])
      sealed _ = fail "world fixture bytes were not explicitly DATA before resealing"
      links (TTagged 42 (TBytes bytes)) | not (BS.null bytes),BS.head bytes == 0 = [BS.tail bytes]
      links (TList values) = concatMap links values
      links (TMap values) = concatMap (\(key,value) -> links key ++ links value) values
      links _ = []
      reachable roots pairs = do
        let available = Map.fromList pairs
            visit seen [] = pure seen
            visit seen (cid:rest) | Set.member cid seen = visit seen rest
            visit seen (cid:rest) = case Map.lookup cid available of
              Nothing -> fail "genuine world fixture omitted a reachable block"
              Just body -> decode body >>= \term -> visit (Set.insert cid seen) (links term ++ rest)
        selected <- visit Set.empty roots
        pure (Map.toAscList (Map.restrictKeys available selected))
  (primary,association,base,durable,contextBlocks,lexical) <- parcel contextParcel
  case published of
    Nothing -> unless (length base == 2) (fail "genuine primary control needs both acquired owners")
    Just _ -> unless (not (null base)) (fail "genuine Published control needs acquired primary rows")
  acquisition <- sealed durable
  case published of
    Nothing -> worldContextCoverage primary contextBlocks
    Just names -> worldPublishedCoverage names primary contextBlocks
  (emptyPrimary,emptyAssociation,emptyBase,emptyDurable,emptyBlocks,emptyLexical) <- fixture "world-context-blocks" [TBytes producer] >>= parcel
  unless (lexical == emptyLexical) (fail "empty lexical fixture changed across genuine context issuances")
  unless (null emptyBase) (fail "empty issuer unexpectedly published primary rows")
  emptyAcquisition <- sealed emptyDurable
  emptyBody <- maybe (fail "empty Context root is absent") pure (lookup emptyPrimary emptyBlocks)
  emptyTerm <- decode emptyBody
  emptyBinding <- case emptyTerm of
    TMap fields -> case lookup (TString "roles") fields of
      Just (TTagged 42 (TBytes bytes)) | not (BS.null bytes),BS.head bytes == 0 -> pure (BS.tail bytes)
      _ -> fail "empty Materializer context omitted its empty map"
    _ -> fail "empty Materializer Context descriptor changed"
  blockPairs <- reachable [primary,association,emptyBinding,lexical] (contextBlocks ++ emptyBlocks)
  stringBytes <- fixture "world-blocks" []
  (hamtRoot,hamtBlocks) <- decode stringBytes >>= \term -> case term of
    TList [TBytes root,pairs] -> (,) root <$> decodePairs pairs
    _ -> fail "String HAMT fixture changed"
  worldHamtControls hamtRoot hamtBlocks
  let sources = "TPCSRC01" <> BS.replicate 8 0
      sourcePath = work </> "world-seal-sources.bin"
      profile = BS.replicate 32 97
      text = TString . T.pack
      blocks pairs = TList [TList [TBytes cid,TBytes body] | (cid,body) <- pairs]
      visibility root = encode (TList [TString "TPCWBIND4",TInt 0,TBytes root])
      restorationWithLexical version root associations occurrences surface = encode (TList
        [TString "TPCWREST",TInt version,TBytes root,TBytes (SHA.hash sources),TBytes producer
        ,TBytes profile,TInt 0,TBytes associations,TBytes occurrences,TBytes surface])
      restoration version root associations occurrences = restorationWithLexical version root associations occurrences lexical
      product = restoration 4 primary association emptyBinding
      packet version receipt root pairs rows visible inputs occurrences = TList
        [TString "TPCWORLD",TInt version,TBytes receipt,text sourcePath,TBytes producer,TBytes profile
        ,TList [TList rows,TList visible],inputs,TBytes (visibility occurrences),TBytes root,blocks pairs]
      originalPacket = packet 4 product primary blockPairs base [] acquisition emptyBinding
      readPacket label term = do
        let path = work </> (label ++ ".world.cbor")
        BS.writeFile path (encode term)
        owner <- newExactInputOwner
        readWorldInputsWithOwner owner path
      requireCorrupt expected label term = do
        result <- try (readPacket label term) :: IO (Either WorldInputsFailure WorldInputFacts)
        case result of
          Left (WorldInputsFailure WorldInputsCorrupt reason)
            | maybe True (\cause -> reason == cause || show cause `isInfixOf` reason) expected -> pure ()
          Left failure -> throwIO failure
          Right _ -> fail ("world association reader accepted mutation: " ++ label)
      replaceFirst value (_:rest) = value:rest
      replaceFirst _ [] = error "world association control requires a genuine row"
      retag (TList [TList [u,n,path,sha,requires,packages,packageSha,_],image]) =
        TList [TList [u,n,path,sha,requires,packages,packageSha,TList [TString "join"]],image]
      retag _ = error "world role control requires a genuine eight-field row"
      changeRequires (TList [TList [u,n,path,sha,TList requires,packages,packageSha,evidence],image]) =
        TList [TList [u,n,path,sha,TList (TList [TString "other-unit",TString "Other.Module"]:requires),packages,packageSha,evidence],image]
      changeRequires _ = error "world requirement control requires a genuine row"
  BS.writeFile sourcePath sources
  original <- readPacket "world-v4-valid" originalPacket
  unless (worldInputsIdentityBytes original == SHA.hash product
      && Map.null (worldInputsBindings original) && worldInputsBindingScope original == 0
      && not (worldInputsPrimaryIsEmpty original))
    (fail "cold v4 reader lost actual primary selection or occurrence identity")
  selectedOwners <- forM base $ \entry -> case entry of
    TList [TList [TString unit,TString name,_,_,_,_,_,_],_] -> pure (T.unpack unit,T.unpack name)
    _ -> fail "genuine primary control lost its actual row owner"
  unless (all (worldHasSelectedSource original) selectedOwners)
    (fail "genuine Context source owners lost their exact acquired association")
  unless (null (worldLexicalImports (worldInputsLexicalSurface original))
      && isNothing (worldLexicalDeclaration (worldInputsLexicalSurface original)))
    (fail "empty published lexical selection was inferred from other acquired interfaces")
  let orderedImports = ["qualified Data.List as L","Data.Map ()","Data.Set hiding (map)"]
      importsBody = encode (TList [TString "TPCWLEX2",TList (map text orderedImports),TNull,TNull])
  importsRoot <- worldFixtureCID importsBody
  importsPairs <- reachable [primary,association,emptyBinding,importsRoot]
    ((importsRoot,importsBody):blockPairs)
  imported <- readPacket "world-v4-ordered-lexical" (packet 4
    (restorationWithLexical 4 primary association emptyBinding importsRoot)
    primary importsPairs base [] acquisition emptyBinding)
  unless (worldLexicalImports (worldInputsLexicalSurface imported) == orderedImports
      && isNothing (worldLexicalDeclaration (worldInputsLexicalSurface imported))
      && worldInputsIdentityBytes imported /= worldInputsIdentityBytes original)
    (fail "world product lost ordered import qualification, explicit empty selection or lexical identity")
  let observationFragment = "module WorldObservation where\nobservation = let value = value in value\n"
      observationTerm profile native = TList [profile,text observationFragment,TList (map text orderedImports),native]
      observationPacket value = do
        let body = encode (TList [TString "TPCWLEX2",TList [],TNull,value])
        root <- worldFixtureCID body
        pairs <- reachable [primary,association,emptyBinding,root] ((root,body):blockPairs)
        pure (packet 4 (restorationWithLexical 4 primary association emptyBinding root)
          primary pairs base [] acquisition emptyBinding)
  forM_ [(0,WorldWorkbenchObservation),(1,WorldHostCarrierObservation)] $ \(profile,expected) -> do
    offered <- observationPacket (observationTerm (TInt profile) TNull)
    observed <- readPacket ("world-observation-profile-" ++ show profile) offered
    case worldLexicalObservation (worldInputsLexicalSurface observed) of
      Just scaffold | worldObservationProfile scaffold == expected
        , worldObservationFragment scaffold == observationFragment
        , worldObservationImports scaffold == orderedImports
        , isNothing (worldObservationRequestTypes scaffold)
        , isNothing (worldObservationInputType scaffold) -> pure ()
      _ -> fail "World observation decoder lost its issued profile/source/type correlation"
  forM_ [("profile",observationTerm (TInt 2) TNull)
        ,("annotations",observationTerm (TInt 0) (TList [TNull]))
        ,("redundant-annotations",observationTerm (TInt 0) (TList [TNull,TNull]))
        ,("helper-recipe",observationTerm (TInt 0) (TList [TList [TString "unknown",TNull],TNull]))] $ \(label,value) -> do
    offered <- observationPacket value
    requireCorrupt Nothing ("world-observation-invalid-" ++ label) offered
  -- Use the same genuine cold facts as an independent full-read oracle. The
  -- successor carries only its changed lexical block, then returns to the old
  -- root without retaining either descriptor. Compatible alternate captures of
  -- the same immutable predecessor remain valid reconstruction inputs.
  let noInputs = TList [TString "continue-originals",TList [],TList []]
      successor receipt changed = TList [TString "TPCWSUCC4",TBytes receipt,blocks changed,TList [],TList [],noInputs]
      readSuccessor label before term = do
        let path = work </> (label ++ ".successor.cbor")
        BS.writeFile path (encode term)
        owner <- newExactInputOwner
        readWorldSuccessorWithOwner owner before path
      importedProduct = restorationWithLexical 4 primary association emptyBinding importsRoot
      requireSuccessorCorrupt label before term = do
        outcome <- try (readSuccessor label before term)
        case outcome of
          Left (WorldInputsFailure WorldInputsCorrupt _) -> pure ()
          Left failure -> throwIO failure
          Right _ -> fail ("successor accepted malformed immutable facts: " ++ label)
  next <- readSuccessor "lexical-delta" original (successor importedProduct [(importsRoot,importsBody)])
  unless (worldInputsIdentityBytes (worldSuccessorFacts next) == worldInputsIdentityBytes imported
      && worldInputsRetainedCharge (worldSuccessorFacts next) == worldInputsRetainedCharge imported
      && worldLexicalImports (worldInputsLexicalSurface (worldSuccessorFacts next)) == orderedImports
      && Set.null (worldSuccessorChangedAliases next))
    (fail "successor lexical facts/charge differ from independent full cold admission")
  reverted <- readSuccessor "lexical-revert" (worldSuccessorFacts next) (successor product [(lexical,maybe (error "genuine lexical block absent") id (lookup lexical blockPairs))])
  unless (worldInputsRetainedCharge (worldSuccessorFacts reverted) == worldInputsRetainedCharge original
      && worldInputsIdentityBytes (worldSuccessorFacts reverted) == worldInputsIdentityBytes original)
    (fail "successor retained retired lexical blocks or predecessor envelopes")
  compatible <- readPacket "compatible-predecessor" originalPacket
  equivalent <- readSuccessor "compatible-frontier" compatible (successor importedProduct [(importsRoot,importsBody)])
  unless (worldInputsIdentityBytes (worldSuccessorFacts equivalent) == worldInputsIdentityBytes imported)
    (fail "compatible independently authenticated frontier changed the next immutable product")
  requireSuccessorCorrupt "missing-changed-block" original (successor importedProduct [])
  requireSuccessorCorrupt "corrupt-changed-block" original
    (successor importedProduct [(importsRoot,importsBody <> BS.singleton 0)])
  requireSuccessorCorrupt "extra-unreachable-block" original
    (successor product [(importsRoot,importsBody)])
  requireCorrupt (Just "compiler world block child is absent") "world-missing-lexical-root"
    (packet 4 product primary (filter ((/= lexical) . fst) blockPairs) base [] acquisition emptyBinding)
  let unselectedBody = encode (TList [TString "TPCWLEX2",TList [],TList
        [TString "main",TString "NeverSelected",TBytes (BS.replicate 32 0),TBytes (BS.singleton 0x80)],TNull])
  unselectedRoot <- worldFixtureCID unselectedBody
  unselectedPairs <- reachable [primary,association,emptyBinding,unselectedRoot]
    ((unselectedRoot,unselectedBody):blockPairs)
  requireCorrupt (Just "world declaration facade has no primary selected role") "world-unselected-declaration-facade"
    (packet 4 (restorationWithLexical 4 primary association emptyBinding unselectedRoot)
      primary unselectedPairs base [] acquisition emptyBinding)
  forM_ (take 1 selectedOwners) $ \(unit,name) -> do
    let changed = encode (TList [TString "TPCWLEX2",TList [],TList
          [text unit,text name,TBytes (BS.replicate 32 0),TBytes (BS.singleton 0x80)],TNull])
    changedRoot <- worldFixtureCID changed
    changedPairs <- reachable [primary,association,emptyBinding,changedRoot] ((changedRoot,changed):blockPairs)
    requireCorrupt (Just "world declaration facade has another selected interface artifact") "world-crossed-declaration-interface"
      (packet 4 (restorationWithLexical 4 primary association emptyBinding changedRoot)
        primary changedPairs base [] acquisition emptyBinding)
  withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \compiler -> scopedRunGhc compiler $ do
    environment <- getSession
    owner <- liftIO newExactInputOwner
    (admitted,types) <- liftIO (readWorldSourceInputs owner environment original selectedOwners) >>= either (liftIO . fail) pure
    unless (Map.keysSet admitted == Set.fromList selectedOwners
        && Map.keysSet types == Map.keysSet admitted
        && all (\(key,entry) -> (exactUnit (worldSourceEntryArtifact entry),exactModule (worldSourceEntryArtifact entry)) == key)
             (Map.toList admitted))
      (liftIO (fail "demanded primary source input lost its genuine canonical/interface/package owners"))
    missing <- liftIO (readWorldSourceInputs owner environment original [("main","NeverSelectedInContext")])
    case missing of
      Left _ -> pure ()
      Right _ -> liftIO (fail "demanded primary source input invented an unselected association")
  -- One genuine compiled cohort supplies retained home types and the exact
  -- interface oracle for the untrusted lexical export vector.
  exported <- withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \compiler -> scopedRunGhc compiler $ do
    environment <- getSession
    owner <- liftIO newExactInputOwner
    types <- liftIO (readWorldTypeInputs owner environment original selectedOwners) >>= either (liftIO . fail) pure
    (_,warm) <- liftIO (captureDiagnostics (readWorldTypeInputs owner environment original selectedOwners >>= either fail pure))
    liftIO $ unless (sum [sum (counterValues ("exact_iface_decode_reuse." ++ snd key) warm)
          | key <- selectedOwners] == toInteger (length selectedOwners))
      (fail "World type closure allocated an independent decoder instead of reusing its owner")
    let leaves = [(exactUnit artifact,exactModule artifact)
          | (artifact,_,_) <- scopeInterfaces scope, null (exactRequirements artifact)
          , (exactUnit artifact,exactModule artifact) `elem` selectedOwners]
    liftIO $ unless (not (null leaves)) (fail "partial demand control needs an actual leaf type owner")
    forM_ leaves $ \key -> do
      partial <- liftIO (readWorldTypeInputs owner environment original [key]) >>= either (liftIO . fail) pure
      liftIO $ unless (Map.keysSet partial == Set.singleton key)
        (fail "one leaf type demand widened to unrelated selected primary rows")
    empty <- liftIO (readWorldTypeInputs owner environment original []) >>= either (liftIO . fail) pure
    liftIO $ unless (Map.null empty) (fail "empty type demand widened to available primary rows")
    hydrated <- liftIO (hydrateExactScope environment
      [(worldTypeEntryArtifact entry,worldTypeEntryInterface entry) | entry <- Map.elems types])
    setSession hydrated
    when (("main","CanonicalSource") `elem` selectedOwners) $ do
      setContext [IIDecl (simpleImportDecl (mkModuleName "CanonicalSource"))]
      _ <- exprType TM_Inst "(\\x -> x) :: CanonicalSource.Retained -> CanonicalSource.Retained"
      pure ()
    setSession environment
    freshFinder <- liftIO initFinderCache
    let unavailable unit = unit {unitImportDirs=[PackageText.pack (work </> "absent-package-root")]}
        units = (hsc_units environment) {unitInfoMap=mapUniqMap unavailable (unitInfoMap (hsc_units environment))}
        changed = environment {hsc_unit_env=ue_setUnits units (hsc_unit_env environment),hsc_FC=freshFinder}
    liftIO $ unless (any (not . null . packageInterfaces . worldTypeEntryPackages) (Map.elems types))
      (fail "World package mismatch control needs a genuine recorded package root")
    mismatch <- liftIO (readWorldTypeInputs owner changed original selectedOwners)
    case mismatch of
      Left _ -> pure ()
      Right _ -> liftIO (fail "World type closure accepted a different current package selection")
    liftIO $ forM (Map.toAscList types) $ \(key,entry) -> do
      exports <- interfaceExports hydrated (worldTypeEntryInterface entry)
      pure (key,exports)
  forM_ (take 1 exported) $ \(key@(unit,name),exports) -> do
    context <- worldFixtureBlock blockPairs primary
    roles <- worldFixtureFieldLink "roles" context >>= worldFixtureHamtRows blockPairs
    interfaces <- fmap concat $ forM roles $ \(owner,reference) -> do
      actualUnit <- worldFixtureField "unit" owner
      actualModule <- worldFixtureField "module" owner
      if (actualUnit,actualModule) /= (text unit,text name) then pure [] else do
        role <- worldFixtureLink reference >>= worldFixtureBlock blockPairs
        identity <- worldFixtureField "interface" role
        case identity of
          TList values -> pure [BS.pack [fromIntegral value | TInt value <- values]]
          _ -> fail "actual compiler role omitted its interface artifact identity"
    interface <- case interfaces of
      [value] | BS.length value == 32 -> pure value
      _ -> fail "lexical control needs one actual selected interface artifact"
    let surface values = encode (TList [TString "TPCWLEX2",TList [],TList
          [text unit,text name,TBytes interface,TBytes (encodeDeclarationExports values)],TNull])
        changedExports = case exports of
          first:rest -> first {exportHead=(exportHead first) {exportModule="Foreign.Declaration"}}:rest
          [] -> error "lexical export refusal needs a genuine nonempty interface"
    unless (not (null exports)) (fail "lexical export oracle must be nonempty")
    forM_ [("exact",reverse exports,True),("omitted",[],False),("foreign",changedExports,False)
          ,("duplicate",exports ++ take 1 exports,False)] $ \(label,offered,accept) -> do
      let body = surface offered
      root <- worldFixtureCID body
      pairs <- reachable [primary,association,emptyBinding,root] ((root,body):blockPairs)
      facts <- readPacket ("world-lexical-exports-" ++ label)
        (packet 4 (restorationWithLexical 4 primary association emptyBinding root)
          primary pairs base [] acquisition emptyBinding)
      withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \compiler -> scopedRunGhc compiler $ do
        environment <- getSession
        owner <- liftIO newExactInputOwner
        result <- liftIO (hydrateWorldDeclarationInputs owner environment facts)
        liftIO $ case result of
          Right (_,Just verified) | accept
            , (exactUnit (verifiedWorldDeclarationInterface verified),exactModule (verifiedWorldDeclarationInterface verified)) == key
            , verifiedWorldDeclarationExports verified == normalizeDeclarationExports exports -> pure ()
          Left _ | not accept -> pure ()
          _ -> fail ("World lexical export authentication disagreed with actual interface: " ++ label)
  -- Reordering physical rows/blocks does not change the four logical roots.
  reordered <- readPacket "world-v4-reordered" (packet 4 product primary (reverse blockPairs) (reverse base) [] acquisition emptyBinding)
  unless (worldInputsIdentityBytes reordered == worldInputsIdentityBytes original)
    (fail "physical cold order changed the fixed logical world identity")
  emptyPairs <- reachable [emptyPrimary,emptyAssociation,emptyBinding,lexical] emptyBlocks
  let emptyProduct = restoration 4 emptyPrimary emptyAssociation emptyBinding
      emptyPacket = packet 4 emptyProduct emptyPrimary emptyPairs [] [] emptyAcquisition emptyBinding
  emptyWorld <- readPacket "world-v4-empty" emptyPacket
  unless (worldInputsPrimaryIsEmpty emptyWorld) (fail "actual empty primary issuer became selected")
  -- The genuine acquisition includes more available custody than this empty
  -- Context selects. Closed endpoints prove passive images are never read,
  -- and equal charges prove their bodies are not retained under the world.
  let passiveAcquisition = case acquisition of
        TList [tag,TList arenas,images] -> TList [tag,TList
          [TList [TString (T.pack ("/proc/2147483647/fd/" ++ show index)),extent]
          | (index,TList [_,extent]) <- zip [0 :: Int ..] arenas],images]
        _ -> error "genuine world acquisition omitted its owned arenas"
  passive <- readPacket "world-v4-empty-passive-custody"
    (packet 4 emptyProduct emptyPrimary emptyPairs [] [] passiveAcquisition emptyBinding)
  unless (worldInputsIdentityBytes passive == worldInputsIdentityBytes emptyWorld
      && worldInputsRetainedCharge passive == worldInputsRetainedCharge emptyWorld
      && worldInputsPrimaryIsEmpty passive && Map.null (worldInputsBindings passive))
    (fail "passive available images changed empty selected world facts or custody")
  requireCorrupt (Just "world interface row lacks one exact owned input image") "world-selected-image-missing"
    (packet 4 product primary blockPairs base [] emptyAcquisition emptyBinding)
  withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \compiler -> scopedRunGhc compiler $ do
    environment <- getSession
    owner <- liftIO newExactInputOwner
    result <- liftIO (readWorldTypeInputs owner environment passive selectedOwners)
    case result of
      Left _ -> pure ()
      Right _ -> liftIO (fail "World type closure widened from selected primary into broader custody")
  expectedPrimary <- worldPrimaryChargeOracle primary association blockPairs
  let TList [_,TList _,TList images] = acquisition
  parts <- concat <$> forM images (\image -> case image of
    TList [_,_,_,_,TList values] -> forM values $ \part -> case part of
      TList [_,TString path,_,length,_,_] -> (,) path <$> unsigned length
      _ -> fail "world charge oracle needs exact image part lengths"
    _ -> fail "world charge oracle needs actual images")
  emptyNode <- maybe (fail "empty binding node is absent") pure (lookup emptyBinding blockPairs)
  lexicalBody <- maybe (fail "actual lexical surface block is absent") pure (lookup lexical blockPairs)
  let uniqueParts = Map.fromList parts
      (primaryBytes,primaryEntries) = expectedPrimary
      -- The receiver retains row parts, source bytes and the authenticated
      -- graph, independently of the disposable transport envelope.
      rawBytes = sum (Map.elems uniqueParts) + toInteger (BS.length sources)
      rowMetadata (TList [TList [unit,name,path,sha,TList requirements,packages,packageSha,TList (role:proof)],image]) =
        let extra = case proof of
              [certificate,certificateSha,TNull,TNull] -> [certificate,certificateSha]
              [certificate,certificateSha,core,coreSha] -> [certificate,certificateSha,core,coreSha]
              [] -> []
              _ -> error "row charge oracle needs one closed role"
            paths = [path,sha,packages,packageSha] ++ extra
            textBytes (TString value) = BS.length (TE.encodeUtf8 value)
            textBytes _ = error "row charge oracle needs path/seal text"
            order (TList [TString u,TString n]) = (u,n)
            order _ = error "row charge oracle needs exact requirements"
            semantic = TList [unit,name,TBytes producer,image,role,TList (sortOn order requirements)]
        in toInteger (sum (map textBytes paths) + BS.length (encode semantic))
      rowMetadata _ = error "row charge oracle needs genuine transport rows"
      rowBytes = sum (map rowMetadata base)
      frontierBytes = sum [toInteger (BS.length cid + BS.length body)
        + 38 * toInteger (Set.size (Set.fromList (links term)))
        | (cid,body) <- blockPairs,Right (remaining,term) <- [deserialiseFromBytes decodeTerm (BL.fromStrict body)]
        ,BL.null remaining]
      -- The retained demand scope reserves the encoded binding basis again.
      expectedCharge = (rawBytes + 2 * toInteger (BS.length emptyNode) + primaryBytes
          + toInteger (BS.length lexicalBody) + rowBytes + frontierBytes
        ,Map.size uniqueParts + 2 + primaryEntries + length base + length blockPairs)
  unless (worldInputsRetainedCharge original == expectedCharge)
    (fail ("four-root known backing/record charge differs from independent DATA census: "
      ++ show (worldInputsRetainedCharge original,expectedCharge)))
  forM_ ([("drop",tail base),("retag",replaceFirst (retag (head base)) base)
         ,("requires",replaceFirst (changeRequires (head base)) base)]
         ++ [("valid-swap",replaceFirst (last base) base) | length base > 1]) $ \(label,changed) ->
    requireCorrupt Nothing ("world-base-" ++ label) (packet 4 product primary blockPairs changed [] acquisition emptyBinding)
  requireCorrupt (Just "compiler world visible input census differs from occurrence facts") "world-unreferenced-visible"
    (packet 4 product primary blockPairs base base acquisition emptyBinding)
  requireCorrupt (Just "compiler world base input census differs from primary associations") "world-unreferenced-base"
    (packet 4 emptyProduct emptyPrimary emptyPairs base [] acquisition emptyBinding)
  requireCorrupt Nothing "old-descriptor-v2" (packet 2 product primary blockPairs base [] acquisition emptyBinding)
  requireCorrupt Nothing "old-descriptor-v3" (packet 3 product primary blockPairs base [] acquisition emptyBinding)
  requireCorrupt Nothing "old-restoration-v2" (packet 4 (restoration 2 primary association emptyBinding) primary blockPairs base [] acquisition emptyBinding)
  requireCorrupt Nothing "old-restoration-v3" (packet 4 (encode (TList
    [TString "TPCWREST",TInt 3,TBytes primary,TBytes (SHA.hash sources),TBytes producer
    ,TBytes profile,TInt 0,TBytes association,TBytes emptyBinding])) primary blockPairs base [] acquisition emptyBinding)
  let oldBindings = encode (TList [TString "TPCWBIND3",TInt 0,TBytes emptyBinding])
      oldBindingPacket = case originalPacket of
        TList fields -> TList (take 8 fields ++ [TBytes oldBindings] ++ drop 9 fields)
        _ -> error "world binding control lost its actual carrier"
  requireCorrupt (Just "unsupported compiler world bindings") "old-occurrence-tag3" oldBindingPacket
  -- Mutate actual root blocks, then rebind the portable product. CID validation
  -- must reject body/digest/codec changes even under a self-consistent SHA ID.
  rootBody <- maybe (fail "Context root is absent") pure (lookup primary blockPairs)
  let replaceBlock old cid body pairs = [(if key == old then cid else key,if key == old then body else bytes) | (key,bytes) <- pairs]
      malformed = [("body",primary,rootBody <> BS.singleton 0,"compiler world block CID differs from its body")
        ,("digest",BS.init primary <> BS.singleton (BS.last primary `xor` 1),rootBody,"compiler world block CID differs from its body")
        ,("codec",BS.take 1 primary <> BS.singleton 0x55 <> BS.drop 2 primary,rootBody,"unsupported compiler world root CID")
        ,("hash-family",BS.pack [1,0x71,0x12,32] <> SHA.hash rootBody,rootBody,"unsupported compiler world root CID")
        ,("version",BS.singleton 0 <> BS.drop 1 primary,rootBody,"unsupported compiler world root CID")
        ,("noncanonical-varint",BS.pack [0x81,0] <> BS.drop 1 primary,rootBody,"unsupported compiler world root CID")
        ,("trailing",primary <> BS.singleton 0,rootBody,"unsupported compiler world root CID")
        ,("truncated",BS.init primary,rootBody,"unsupported compiler world root CID")]
  forM_ malformed $ \(label,cid,body,reason) -> requireCorrupt (Just reason) ("world-block-" ++ label)
    (packet 4 (restoration 4 cid association emptyBinding) cid (replaceBlock primary cid body blockPairs) base [] acquisition emptyBinding)
  rootTerm <- decode rootBody
  fields <- case rootTerm of
    TMap fields -> pure fields
    _ -> fail "actual Context descriptor was not its owning map"
  let replaceField name replacement = TMap [(key,if key == TString name then replacement else old) | (key,old) <- fields]
      wrongProducer = BS.unpack (BS.cons (BS.head producer `xor` 1) (BS.tail producer))
      contextMutations =
        [("version",replaceField "version" (TInt 5),"unsupported compiler world descriptor version")
        ,("producer",replaceField "producer" (TList (map (TInt . fromIntegral) wrongProducer)),"compiler world context producer differs")
        ,("missing-field",TMap (filter ((/= TString "roles") . fst) fields),"invalid compiler world descriptor fields")
        ,("missing-decisions",TMap (filter ((/= TString "input_decisions") . fst) fields),"invalid compiler world descriptor fields")
        ,("unknown-field",TMap [(if key == TString "roles" then TString "wrong" else key,value) | (key,value) <- fields],"unsupported compiler world descriptor field")]
  forM_ contextMutations $ \(label,term,reason) -> do
    let body = encode term
    cid <- worldFixtureCID body
    exact <- reachable [cid,association,emptyBinding,lexical] (replaceBlock primary cid body blockPairs)
    requireCorrupt (Just reason) ("world-context-" ++ label)
      (packet 4 (restoration 4 cid association emptyBinding) cid exact base [] acquisition emptyBinding)
  forM_ published $ \_ -> do
    let mutatePublished label target changed expected = do
          (roots,updated) <- rewriteWorldFixtureBlock target changed [primary,association,emptyBinding,lexical] blockPairs
          case roots of
            [context,associations,occurrences,lexicalRoot] | lexicalRoot == lexical -> requireCorrupt expected label
              (packet 4 (restoration 4 context associations occurrences) context updated base [] acquisition occurrences)
            _ -> fail "Published mutation lost its exact four roots"
    publications <- worldPublishedFacts primary blockPairs
    forM_ publications $ \(roleCID,role,authorityCID,authority) -> do
      let remove name (TMap fields) = TMap (filter ((/= TString name) . fst) fields)
          remove _ _ = error "genuine Published mutation needs descriptor map"
          set name replacement (TMap fields) = TMap [(key,if key == TString name then replacement else value) | (key,value) <- fields]
          set _ _ _ = error "genuine Published mutation needs descriptor map"
          oldAuthority = set "version" (TInt 1) (remove "roles" authority)
      mutatePublished "world-published-role7" roleCID (remove "native_authority" role)
        (Just "unsupported compiler world compiler role")
      mutatePublished "world-published-authority1" authorityCID oldAuthority
        (Just "invalid compiler world descriptor fields")
      mutatePublished "world-published-authority1-map5" authorityCID (set "version" (TInt 1) authority)
        (Just "unsupported compiler world descriptor version")
      forM_ ["roles","offers","native_availability","input_decisions"] $ \name -> do
        mutatePublished ("world-published-missing-" ++ name) authorityCID (remove (T.pack name) authority)
          (Just "invalid compiler world descriptor fields")
        root <- worldFixtureFieldLink name authority
        requireCorrupt (Just "compiler world block child is absent") ("world-published-absent-" ++ name)
          (packet 4 product primary (filter ((/= root) . fst) blockPairs) base [] acquisition emptyBinding)
      requireCorrupt (Just "compiler world block child is absent") "world-published-absent-authority"
        (packet 4 product primary (filter ((/= authorityCID) . fst) blockPairs) base [] acquisition emptyBinding)
    independent <- readPacket "world-published-second-acquisition" originalPacket
    unless (worldInputsIdentityBytes independent == worldInputsIdentityBytes original
        && worldInputsRetainedCharge independent == worldInputsRetainedCharge original)
      (fail "independent Published acquisition lost exact logical facts or known charge")
  unless (not (any ((== hamtRoot) . fst) blockPairs))
    (fail "extra/missing-root controls require an independent actual String HAMT")
  extraRoot <- maybe (fail "String HAMT fixture omitted root") pure (lookup hamtRoot hamtBlocks)
  requireCorrupt (Just "duplicate compiler world block CID") "world-duplicate-block"
    (packet 4 product primary (head blockPairs:blockPairs) base [] acquisition emptyBinding)
  requireCorrupt (Just "compiler world frontier contains unreachable blocks") "world-extra-block"
    (packet 4 product primary ((hamtRoot,extraRoot):blockPairs) base [] acquisition emptyBinding)
  child <- case links rootTerm of
    value:_ | value /= primary -> pure value
    _ -> fail "genuine Context fixture needs an actual child"
  requireCorrupt (Just "compiler world block child is absent") "world-missing-context-child"
    (packet 4 product primary (filter ((/= child) . fst) blockPairs) base [] acquisition emptyBinding)
  -- Each raw root independently participates in frontier validation.
  forM_ [("primary-association",hamtRoot,emptyBinding),("occurrence",association,hamtRoot)] $ \(label,assoc,occurrence) -> do
    let receipt = restoration 4 primary assoc occurrence
    requireCorrupt (Just "compiler world block child is absent") ("world-missing-" ++ label ++ "-root")
      (packet 4 receipt primary blockPairs base [] acquisition occurrence)
    combined <- reachable [primary,assoc,occurrence,lexical] (blockPairs ++ hamtBlocks)
    hamtBody <- maybe (fail "String HAMT root omitted") pure (lookup hamtRoot hamtBlocks)
    actualChild <- decode hamtBody >>= \term -> case links term of
      value:_ -> pure value
      _ -> fail "128-entry actual HAMT must provide a child before omission control"
    requireCorrupt (Just "compiler world block child is absent") ("world-missing-" ++ label ++ "-child")
      (packet 4 receipt primary (filter ((/= actualChild) . fst) combined) base [] acquisition occurrence)
  -- Rebind one actual association fact and its exact HAMT link. Equal row image
  -- bytes cannot hide another interface binding's scope.
  associationBody <- maybe (fail "primary association root omitted") pure (lookup association blockPairs)
  associationTerm <- decode associationBody
  (factCID,factBody,changedFact) <- firstAssociationFact decode blockPairs associationTerm
  changedCID <- worldFixtureCID (encode changedFact)
  let rewriteLink (TTagged 42 (TBytes bytes)) | bytes == BS.cons 0 factCID = TTagged 42 (TBytes (BS.cons 0 changedCID))
      rewriteLink (TList values) = TList (map rewriteLink values)
      rewriteLink other = other
      changedAssociationBody = encode (rewriteLink associationTerm)
  changedAssociation <- worldFixtureCID changedAssociationBody
  let changedPairs = replaceBlock association changedAssociation changedAssociationBody
        (replaceBlock factCID changedCID (encode changedFact) blockPairs)
  requireCorrupt (Just "compiler world selected role differs from acquired binding scopes") "world-wrong-scope-equal-image"
    (packet 4 (restoration 4 primary changedAssociation emptyBinding) primary changedPairs base [] acquisition emptyBinding)
  unless (not (BS.null factBody)) (fail "association mutation lacked actual input")
  let paths = Set.toAscList (Set.fromList [path | TList [_,_,_,_,TList values] <- images,TList [_,TString path,_,_,_,_] <- values])
      aliases = Map.fromList [(path,T.pack (work </> ("absent-world-alias-" ++ show i))) | (i,path) <- zip [0 :: Int ..] paths]
      relocate (TString value) = TString (Map.findWithDefault value value aliases)
      relocate (TList values) = TList (map relocate values)
      relocate other = other
  aliased <- readPacket "world-v4-alias-stable"
    (packet 4 product primary blockPairs (map relocate base) [] (relocate acquisition) emptyBinding)
  unless (worldInputsIdentityBytes aliased == worldInputsIdentityBytes original)
    (fail "physical receiving aliases changed logical world identity")
  forM_ (Map.elems aliases) $ \path -> do
    exists <- doesFileExist (T.unpack path)
    unless (not exists) (fail "captured package control requires absent physical aliases")
  withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \compiler -> scopedRunGhc compiler $ do
    environment <- getSession
    owner <- liftIO newExactInputOwner
    captured <- liftIO (readWorldSourceInputs owner environment aliased selectedOwners)
    case captured of
      Left reason -> liftIO (fail ("captured package/owning-interface authentication failed for absent receiving aliases: " ++ reason))
      Right (closure,_) -> unless
        (Map.keysSet closure == Set.fromList selectedOwners
          && all (\(key,entry) -> (exactUnit (worldSourceEntryArtifact entry),exactModule (worldSourceEntryArtifact entry)) == key)
               (Map.toList closure))
        (liftIO (fail "captured package authentication lost a demanded owner after alias relocation"))
  where
    unsigned (TInt value) | value >= 0 = pure (toInteger value)
    unsigned (TInteger value) | value >= 0 = pure value
    unsigned _ = fail "world fixture extent/length must be unsigned"
    rawHex [] = pure BS.empty
    rawHex (a:b:rest) = case readHex [a,b] of
      [(byte,"")] -> BS.cons byte <$> rawHex rest
      _ -> fail "genuine producer has malformed SHA"
    rawHex _ = fail "genuine producer has an odd SHA length"
    firstAssociationFact decode pairs term = do
      let pointers (TTagged 42 (TBytes bytes)) | not (BS.null bytes),BS.head bytes == 0 = [BS.tail bytes]
          pointers (TList values) = concatMap pointers values
          pointers _ = []
      candidates <- forM (pointers term) $ \cid -> case lookup cid pairs of
        Just body -> do
          value <- decode body
          pure (cid,body,value)
        Nothing -> fail "primary association control lacks referenced fact"
      case [(cid,body,TList [magic,TMap [(key,if key == TString "selection" then changeScope value else value) | (key,value) <- interface],original,semantic])
           | (cid,body,TList [magic@(TString "TPCWPRIMARY1"),TMap interface,original,semantic]) <- candidates
           , original /= TNull] of
        value:_ -> pure value
        [] -> fail "scope mutation needs an actual Original/canonical acquired association"
    changeScope (TList (TInt byte:rest)) = TList (TInt (byte `xor` 1):rest)
    changeScope _ = error "actual binding scope is not its fixed 32-byte array"

-- These helpers inspect genuine producer DATA for oracle preconditions. They
-- do not construct roles, publication owners or service-world authority.
worldFixtureTerm :: BS.ByteString -> IO Term
worldFixtureTerm bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
  Right (remaining,value) | BL.null remaining -> pure value
  _ -> fail "world fixture needs one complete CBOR term"

worldFixtureBlock :: [(BS.ByteString,BS.ByteString)] -> BS.ByteString -> IO Term
worldFixtureBlock pairs cid = maybe (fail "world fixture omitted its actual block") worldFixtureTerm (lookup cid pairs)

worldFixtureLink :: Term -> IO BS.ByteString
worldFixtureLink (TTagged 42 (TBytes bytes)) | not (BS.null bytes),BS.head bytes == 0 = pure (BS.tail bytes)
worldFixtureLink _ = fail "world fixture needs an actual tagged CID"

worldFixtureField :: String -> Term -> IO Term
worldFixtureField name (TMap fields) = maybe (fail ("world fixture omitted field " ++ name)) pure (lookup (TString (T.pack name)) fields)
worldFixtureField _ _ = fail "world fixture needs its descriptor map"

worldFixtureFieldLink :: String -> Term -> IO BS.ByteString
worldFixtureFieldLink name term = worldFixtureField name term >>= worldFixtureLink

worldFixtureHamtRows :: [(BS.ByteString,BS.ByteString)] -> BS.ByteString -> IO [(Term,Term)]
worldFixtureHamtRows pairs root = worldFixtureBlock pairs root >>= \term -> case term of
  TList [TBytes _,TList pointers] -> concat <$> forM pointers (\pointer -> case pointer of
    TList bucket -> forM bucket $ \entry -> case entry of
      TList [TBytes key,value] -> do
        decoded <- worldFixtureTerm key
        case decoded of
          TList [fact] -> pure (fact,value)
          _ -> fail "world fixture needs its exact SnapshotKey wrapper"
      _ -> fail "world fixture needs exact HAMT key/value pairs"
    _ -> worldFixtureLink pointer >>= worldFixtureHamtRows pairs)
  _ -> fail "world fixture needs its actual HAMT node"

worldPublishedFacts :: BS.ByteString -> [(BS.ByteString,BS.ByteString)]
  -> IO [(BS.ByteString,Term,BS.ByteString,Term)]
worldPublishedFacts root pairs = do
  context <- worldFixtureBlock pairs root
  roles <- worldFixtureFieldLink "roles" context >>= worldFixtureHamtRows pairs
  concat <$> forM roles (\(_,reference) -> do
    cid <- worldFixtureLink reference
    role <- worldFixtureBlock pairs cid
    kind <- worldFixtureField "kind" role
    case kind of
      TString "published_source_original" -> do
        authorityCID <- worldFixtureFieldLink "native_authority" role
        authority <- worldFixtureBlock pairs authorityCID
        pure [(cid,role,authorityCID,authority)]
      _ -> pure [])

worldPublishedCoverage :: [String] -> BS.ByteString -> [(BS.ByteString,BS.ByteString)] -> IO ()
worldPublishedCoverage expected root pairs = do
  context <- worldFixtureBlock pairs root
  rows <- worldFixtureFieldLink "roles" context >>= worldFixtureHamtRows pairs
  published <- fmap concat $ forM rows $ \(key,reference) -> do
    cid <- worldFixtureLink reference
    role <- worldFixtureBlock pairs cid
    kind <- worldFixtureField "kind" role
    case (kind,key) of
      (TString "published_source_original",TMap fields) -> case lookup (TString "module") fields of
        Just (TString name) -> pure [T.unpack name]
        _ -> fail "actual Published role lacks its module key"
      _ -> pure []
  unless (Set.fromList published == Set.fromList expected && length published == length expected)
    (fail "genuine completion did not issue exactly the requested Published roots")
  publications <- worldPublishedFacts root pairs
  unless (length publications > 1)
    (fail "genuine Published fixture must exercise multiple actual publication owners")
  let uniqueAuthorities = Set.size (Set.fromList [cid | (_,_,cid,_) <- publications])
  putStrLn ("genuine Published authority references=" ++ show (length publications)
    ++ " unique=" ++ show uniqueAuthorities)
  forM_ publications $ \(_,_,_,authority) -> do
    version <- worldFixtureField "version" authority
    unless (version == TInt 2) (fail "genuine publication must carry authority descriptor2")
    maps <- forM ["roles","offers","native_availability","input_decisions"] $ \name ->
      worldFixtureFieldLink name authority >>= worldFixtureHamtRows pairs
    unless (not (null (head maps)) && not (null (maps !! 1)))
      (fail "genuine Published birth must exercise nonempty typed roles and native offers")
  putStrLn ("genuine Published authority coverage roots=" ++ show published ++ " descriptor2/four required maps")

-- Rehash negative fixture DATA along actual parent links. Unchanged bodies
-- retain their producer CIDs; only this mutation path gets new test addresses.
rewriteWorldFixtureBlock :: BS.ByteString -> Term -> [BS.ByteString] -> [(BS.ByteString,BS.ByteString)]
  -> IO ([BS.ByteString],[(BS.ByteString,BS.ByteString)])
rewriteWorldFixtureBlock target replacement roots pairs = do
  memo <- newIORef Map.empty
  let encode = toStrictByteString . encodeTerm
      visit cid = do
        done <- readIORef memo
        case Map.lookup cid done of
          Just (actual,_) -> pure actual
          Nothing -> do
            body <- maybe (fail "world mutation omitted an actual reachable body") pure (lookup cid pairs)
            original <- worldFixtureTerm body
            changed <- rewrite (if cid == target then replacement else original)
            let bytes = encode changed
            actual <- if bytes == body then pure cid else worldFixtureCID bytes
            modifyIORef' memo (Map.insert cid (actual,bytes))
            pure actual
      rewrite tagged@(TTagged 42 _) = do
        child <- worldFixtureLink tagged
        actual <- visit child
        pure (TTagged 42 (TBytes (BS.cons 0 actual)))
      rewrite (TList values) = TList <$> mapM rewrite values
      rewrite (TMap fields) = TMap <$> mapM (\(key,value) -> (,) <$> rewrite key <*> rewrite value) fields
      rewrite value = pure value
  actualRoots <- mapM visit roots
  actualPairs <- Map.elems <$> readIORef memo
  exact <- worldFixtureReachableBlocks actualRoots actualPairs
  pure (actualRoots,exact)

-- Independent recomputation from the genuine parcel's raw CBOR terms. This
-- neither invokes the typed receiver fold nor issues any selection authority.
worldPrimaryChargeOracle :: BS.ByteString -> BS.ByteString -> [(BS.ByteString,BS.ByteString)] -> IO (Integer,Int)
worldPrimaryChargeOracle contextRoot associationRoot pairs = do
  seenAuthorities <- newIORef Set.empty
  let blocks = Map.fromList pairs
      body cid = maybe (fail "charge oracle lacks actual block") pure (Map.lookup cid blocks)
      decode bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
        Right (remaining,value) | BL.null remaining -> pure value
        _ -> fail "charge oracle needs one actual CBOR term"
      value cid = body cid >>= decode
      size cid = toInteger . BS.length <$> body cid
      plus (a,b) (c,d) = (a+c,b+d)
      field name (TMap fields) = maybe (fail ("charge oracle missing field " ++ name)) pure (lookup (TString (T.pack name)) fields)
      field _ _ = fail "charge oracle expected descriptor map"
      link (TTagged 42 (TBytes encoded)) | not (BS.null encoded),BS.head encoded == 0 = pure (BS.tail encoded)
      link _ = fail "charge oracle expected actual CID link"
      linked name descriptor = field name descriptor >>= link
      array (TList values) = pure values
      array _ = fail "charge oracle expected actual array"
      encode = toStrictByteString . encodeTerm
      snapshotKey bytes = decode bytes >>= \term -> case term of
        TList [key] -> pure (encode key)
        _ -> fail "charge oracle expected the existing one-field SnapshotKey"
      hamt root = do
        let walk seen [] entries = do
              bytes <- sum <$> mapM size (Set.toAscList seen)
              pure (entries,(bytes,toInteger (Set.size seen + length entries)))
            walk seen (cid:pending) entries | Set.member cid seen = walk seen pending entries
            walk seen (cid:pending) entries = do
              node <- value cid
              pointers <- case node of
                TList [TBytes _,TList pointers] -> pure pointers
                _ -> fail "charge oracle expected actual HAMT node"
              (children,more) <- foldM (\(children,entries) pointer -> case pointer of
                TTagged 42 _ -> do
                  child <- link pointer
                  pure (child:children,entries)
                TList bucket -> do
                  rows <- forM bucket $ \row -> case row of
                    TList [TBytes key,value] -> pure (key,value)
                    _ -> fail "charge oracle expected actual HAMT bucket entry"
                  pure (children,rows ++ entries)
                _ -> fail "charge oracle expected actual HAMT pointer") ([],[]) pointers
              walk (Set.insert cid seen) (children ++ pending) (more ++ entries)
        walk Set.empty [root] []
      selection root = do
        descriptor <- value root
        descriptorBytes <- size root
        maps <- forM ["scopes","roots","namespace","custody_roots"] $ \name -> linked name descriptor >>= hamt
        let scopeRows = fst (head maps)
        proofs <- forM scopeRows $ \(_,reference) -> do
          cid <- link reference
          proof <- value cid
          nodes <- field "nodes" proof >>= array
          dependencies <- field "dependencies" proof >>= array
          bytes <- size cid
          edges <- forM dependencies $ \row -> case row of
            TList [source,target,_] -> pure (encode source,[encode target])
            _ -> fail "charge oracle expected actual scope dependency triple"
          pure (edges,(bytes,toInteger (length nodes + length dependencies + 1)))
        roots <- mapM (snapshotKey . fst) (fst (maps !! 1) ++ fst (maps !! 3))
        let edges = Map.fromListWith (++) (concatMap fst proofs)
            closure seen [] = seen
            closure seen (node:rest) | Set.member node seen = closure seen rest
            closure seen (node:rest) = closure (Set.insert node seen) (Map.findWithDefault [] node edges ++ rest)
            selected = closure Set.empty roots
        pure (foldl plus (descriptorBytes,1+toInteger (Set.size selected)) (map snd maps ++ map snd proofs))
      roleFact reference = do
        cid <- link reference
        role <- value cid
        bytes <- size cid
        kind <- field "kind" role
        extra <- case kind of
          TString "published_source_original" -> do
            selected <- linked "selection" role >>= selection
            authorityCID <- linked "native_authority" role
            seen <- readIORef seenAuthorities
            authorityCharge <- if Set.member authorityCID seen then pure (0,1) else do
              modifyIORef' seenAuthorities (Set.insert authorityCID)
              authority <- value authorityCID
              authorityBytes <- size authorityCID
              authorityMaps <- forM ["roles","offers","native_availability","input_decisions"] $ \name -> linked name authority >>= hamt
              bornRoles <- mapM (roleFact . snd) (fst (head authorityMaps))
              pure (foldl plus (authorityBytes,2) (map snd authorityMaps ++ bornRoles))
            pure (plus selected authorityCharge)
          _ -> pure (0,0)
        pure (plus (bytes,1) extra)
  context <- value contextRoot
  contextBytes <- size contextRoot
  maps <- forM ["roles","lexical","offers","native_availability","input_decisions"] $ \name -> linked name context >>= hamt
  selected <- linked "selection" context >>= selection
  roleFacts <- mapM (roleFact . snd) (fst (head maps))
  lexicalFacts <- forM (fst (maps !! 1)) $ \(_,reference) -> do
    cid <- link reference
    fact <- value cid
    imports <- field "imports" fact >>= array
    bytes <- size cid
    pure (bytes,toInteger (1+length imports))
  (associations,associationCharge) <- hamt associationRoot
  references <- Set.fromList <$> mapM (link . snd) associations
  associationBytes <- sum <$> mapM size (Set.toAscList references)
  let (bytes,entries) = foldl plus (contextBytes,1)
        (map snd maps ++ [selected,associationCharge
        ,(associationBytes,toInteger (Set.size references + length (fst (head maps))))] ++ roleFacts ++ lexicalFacts)
  unless (entries <= toInteger (maxBound :: Int)) (fail "charge oracle exceeds host entry range")
  pure (bytes,fromInteger entries)

-- Check the positive fixture's actual contents independently of the typed
-- production fold. Empty typed maps cannot prove their key/value decoders.
worldContextCoverage :: BS.ByteString -> [(BS.ByteString,BS.ByteString)] -> IO ()
worldContextCoverage root blocks = do
  context <- readBlock root
  selection <- fieldLink "selection" context >>= readBlock
  moduleRows <- fieldLink "roles" context >>= hamtRows
  digestRows <- fieldLink "offers" context >>= hamtRows
  scopes <- fieldLink "scopes" selection >>= hamtRows
  bindings <- fieldLink "namespace" selection >>= hamtRows
  roots <- fieldLink "roots" selection >>= hamtRows
  custody <- fieldLink "custody_roots" selection >>= hamtRows
  unless (not (null moduleRows) && not (null digestRows) && not (null scopes)
      && not (null bindings) && not (null (roots ++ custody)))
    (fail "genuine context must exercise module/digest/binding-node keys and binding values")
  putStrLn ("genuine Context HAMT coverage module=" ++ show (length moduleRows)
    ++ " digest=" ++ show (length digestRows) ++ " scopes=" ++ show (length scopes)
    ++ " bindings=" ++ show (length bindings) ++ " binding-nodes=" ++ show (length (roots ++ custody)))
  where
    readBlock cid = case lookup cid blocks of
      Nothing -> fail "genuine context coverage omitted a reachable block"
      Just bytes -> case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
        Right (remaining,value) | BL.null remaining -> pure value
        _ -> fail "genuine context coverage needs complete CBOR blocks"
    fieldLink name (TMap fields) = case lookup (TString name) fields of
      Just value -> link value
      Nothing -> fail "genuine context coverage omitted a descriptor field"
    fieldLink _ _ = fail "genuine context coverage needs a descriptor map"
    link (TTagged 42 (TBytes bytes)) | not (BS.null bytes) && BS.head bytes == 0 = pure (BS.tail bytes)
    link _ = fail "genuine context coverage needs a tagged CID"
    hamtRows cid = readBlock cid >>= \node -> case node of
      TList [TBytes _,TList pointers] -> concat <$> mapM rows pointers
      _ -> fail "genuine context coverage needs a HAMT node"
    rows (TList values) = pure values
    rows tagged = link tagged >>= hamtRows

-- The positive parcel comes from the actual Rust Materializer, and each
-- negative changes those owned node bytes before issuing a new content address.
-- These are DATA controls for the production HAMT fold, never rooted worlds.
worldHamtControls :: BS.ByteString -> [(BS.ByteString,BS.ByteString)] -> IO ()
worldHamtControls root blocks = do
  actual <- readWorldStringMap root blocks
  let expected = Map.fromList [("fact-" ++ show n,"value-" ++ show n) | n <- [0 :: Int ..127]]
  unless (actual == expected) (fail "HAMT reader lost independently materialized String facts")
  let rootBody = case lookup root blocks of Just bytes -> bytes; _ -> error "genuine HAMT root missing"
      encode = toStrictByteString . encodeTerm
      term bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
        Right (remaining,value) | BL.null remaining -> pure value
        _ -> fail "genuine HAMT node is not one CBOR value"
      replace old cid body = [(if key == old then cid else key,if key == old then body else bytes)
        | (key,bytes) <- blocks]
      refused label expectedReason cid pairs = do
        exact <- worldFixtureReachableBlocks [cid] pairs
        result <- try (readWorldStringMap cid exact) :: IO (Either WorldInputsFailure (Map.Map String String))
        case result of
          Left (WorldInputsFailure WorldInputsCorrupt reason)
            | reason == expectedReason || (show expectedReason `isInfixOf` reason) -> pure ()
          Left failure -> fail (label ++ " refused for another reason: " ++ show failure)
          Right _ -> fail (label ++ " accepted a malformed HAMT")
      mutate label reason value = do
        let body = encode value
        cid <- worldFixtureCID body
        refused label reason cid (replace root cid body)
  original <- term rootBody
  (bitmap,pointers) <- case original of
    TList [TBytes bits,TList values] | not (null values) -> pure (bits,values)
    _ -> fail "actual Materializer root is not a nonempty V3 HAMT node"
  let leafIndexes = [i | (i,TList (_:_)) <- zip [0 :: Int ..] pointers]
  leafIndex <- case leafIndexes of i:_ -> pure i; _ -> fail "HAMT fixture needs an actual inline bucket"
  bucket <- case pointers !! leafIndex of TList values -> pure values; _ -> error "checked HAMT bucket disappeared"
  entry <- case bucket of value:_ -> pure value; _ -> error "checked HAMT entry disappeared"
  (key,value) <- case entry of TList [TBytes key,initial] -> pure (key,initial); _ -> fail "actual HAMT leaf is not a byte-key pair"
  let node values = TList [TBytes bitmap,TList values]
      changeBucket entries = node (take leafIndex pointers ++ TList entries : drop (leafIndex+1) pointers)
  mutate "node arity" "invalid compiler world tuple" (TList [TBytes bitmap,TList pointers,TNull])
  mutate "bitmap leading zero" "invalid compiler world HAMT bitmap" (TList [TBytes (BS.cons 0 bitmap),TList pointers])
  mutate "bitmap width" "invalid compiler world HAMT bitmap" (TList [TBytes (BS.pack [1,0,0,0,0]),TList pointers])
  mutate "bitmap count" "compiler world HAMT pointers differ from bitmap" (TList [TBytes BS.empty,TList pointers])
  mutate "empty bucket" "invalid compiler world HAMT bucket width" (changeBucket [])
  mutate "large bucket" "invalid compiler world HAMT bucket width" (changeBucket (replicate 4 entry))
  mutate "duplicate bucket key" "duplicate or unsorted compiler world HAMT bucket keys" (changeBucket [entry,entry])
  mutate "entry arity" "invalid compiler world tuple" (changeBucket [TList [TBytes key]])
  mutate "key type" "expected string" (changeBucket [TList [TBytes (encode (TList [TInt 7])),value]])
  mutate "value type" "expected string" (changeBucket [TList [TBytes key,TInt 7]])
  let rootSlot encoded = fromIntegral (BS.head (SHA.hash encoded)) `div` 8 :: Int
      alternate = head [encode (TList [TString (T.pack ("changed-key-" ++ show n))])
        | n <- [0 :: Int ..], rootSlot (encode (TList [TString (T.pack ("changed-key-" ++ show n))])) /= rootSlot key]
  mutate "key route" "compiler world HAMT key has another hash route" (changeBucket [TList [TBytes alternate,value]])
  child <- case [BS.tail encoded | TTagged 42 (TBytes encoded) <- pointers
        , not (BS.null encoded),BS.head encoded == 0] of
    cid:_ | any ((== cid) . fst) blocks -> pure cid
    _ -> fail "HAMT fixture needs a genuine provided child"
  refused "missing HAMT child" "compiler world block child is absent" root (filter ((/= child) . fst) blocks)
  let emptyChildBody = encode (TList [TBytes BS.empty,TList []])
  emptyChild <- worldFixtureCID emptyChildBody
  let replaceLink (TTagged 42 (TBytes encoded)) | encoded == BS.cons 0 child = TTagged 42 (TBytes (BS.cons 0 emptyChild))
      replaceLink other = other
      changedRootBody = encode (node (map replaceLink pointers))
  changedRoot <- worldFixtureCID changedRootBody
  let childChanged = [(if key == child then emptyChild else key,if key == child then emptyChildBody else body)
        | (key,body) <- replace root changedRoot changedRootBody]
  refused "empty child" "compiler world HAMT pointers differ from bitmap" changedRoot childChanged

-- Mutations retain exactly their new root closure. Missing references remain
-- missing so the production verifier can observe them; obsolete valid blocks
-- cannot mask a schema refusal with a different extra-frontier failure.
worldFixtureReachableBlocks :: [BS.ByteString] -> [(BS.ByteString,BS.ByteString)] -> IO [(BS.ByteString,BS.ByteString)]
worldFixtureReachableBlocks roots pairs = do
  let available = Map.fromList pairs
      links (TTagged 42 (TBytes bytes)) | not (BS.null bytes),BS.head bytes == 0 = [BS.tail bytes]
      links (TList values) = concatMap links values
      links (TMap fields) = concatMap (\(key,value) -> links key ++ links value) fields
      links _ = []
      visit seen [] = pure seen
      visit seen (cid:rest) | Set.member cid seen = visit seen rest
      visit seen (cid:rest) = case Map.lookup cid available of
        Nothing -> visit (Set.insert cid seen) rest
        Just body -> case deserialiseFromBytes decodeTerm (BL.fromStrict body) of
          Right (remaining,value) | BL.null remaining -> visit (Set.insert cid seen) (links value ++ rest)
          _ -> fail "mutated fixture must remain one CBOR term before typed refusal"
  selected <- visit Set.empty roots
  pure (Map.toAscList (Map.restrictKeys available selected))

-- Mutation data only: positive content addresses always come from Materializer.
worldFixtureCID :: BS.ByteString -> IO BS.ByteString
worldFixtureCID body = do
  digestBytes <- hexBytes (show (Crypto.hash body :: Crypto.Digest Crypto.Blake2b_256))
  pure (BS.pack [1,0x71,0xa0,0xe4,2,32] <> digestBytes)
  where
    hexBytes [] = pure BS.empty
    hexBytes (a:b:rest) = case readHex [a,b] of
      [(byte,"")] -> BS.cons byte <$> hexBytes rest
      _ -> fail "invalid test digest"
    hexBytes _ = fail "invalid test digest length"

ownedScopeInputReuse :: IO ()
ownedScopeInputReuse = withTiming $ withScratch $ \work ->
  bracket (newIORef []) (\owners -> readIORef owners >>= mapM_ hClose) $ \handles -> do
    forM_ ["CanonicalSource.hs","CanonicalDependency.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
    original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing (work </> "CanonicalSource.hs") [work] Nothing
    fixture <- capturePreparedFixture work original
    filePath <- writeGenuineMetadataScope work ["CanonicalDependency"] fixture
    fileOwner <- newExactInputOwner
    _ <- readExactScopeWithOwner fileOwner filePath >>= either fail pure
    (_,fileAgain) <- captureDiagnostics (readExactScopeWithOwner fileOwner filePath >>= either fail pure)
    unless (sum (counterValues "original_inputs.certificate.misses" fileAgain) == 1
        && counterValues "original_inputs.retained_encoded_bytes" fileAgain == [0])
      (fail "file-only fixture claimed owned continuation reuse")
    owner <- newExactInputOwner
    (fileScope,contextParcel) <- writeGenuineExecutionScopeWithContext
      ["CanonicalSource","CanonicalDependency"] ["CanonicalSource"] work fixture
    firstPath <- ownedFixtureScope handles fileScope
    semanticWorldBindingControls handles work firstPath contextParcel
    let publishedWork = work </> "published-world"
    createDirectoryIfMissing True publishedWork
    forM_ ["PublishedConsumer.hs","PublishedSeed.hs","PublishedSibling.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (publishedWork </> name)
    publishedOriginal <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing (publishedWork </> "PublishedConsumer.hs") [publishedWork] Nothing
    publishedFixture <- capturePreparedFixture publishedWork publishedOriginal
    (publishedScope,publishedParcel) <- writeGenuinePublishedScopeWithContext "PublishedConsumer" ["PublishedSeed","PublishedSibling"] publishedWork publishedFixture
    publishedOwned <- ownedFixtureScope handles publishedScope
    semanticWorldInputControls (Just ["PublishedSeed","PublishedSibling"]) handles publishedWork publishedOwned publishedParcel
    missingOwnedPartsRefuse firstPath
    capturedProofScope <- readExactScope firstPath >>= either fail pure
    missingCapturedProofPartsRefuse capturedProofScope
    (_,cold) <- captureDiagnostics (readExactScopeWithOwner owner firstPath >>= either fail pure)
    secondPath <- writeGenuineMetadataScope work ["CanonicalDependency"] fixture >>= ownedFixtureScope handles
    (_,contracted) <- captureDiagnostics (readExactScopeWithOwner owner secondPath >>= either fail pure)
    thirdPath <- writeGenuineExecutionScope ["CanonicalSource","CanonicalDependency"] ["CanonicalSource"] work fixture >>= ownedFixtureScope handles
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
      live <- readExactScopeWithOwner bounded thirdPath >>= either fail pure
      let interfaces = [iface | (iface,_,_) <- scopeInterfaces live]
          decode = readScopedInterfaces (prHscEnv (pprPipelineResult original)) live interfaces >>= either fail pure
      unless (length interfaces == 2 && Set.size (Set.fromList (map exactSha256 interfaces)) == 2)
        (fail "zero-budget decoder control needs two genuine distinct interface owners")
      _ <- decode
      (_,decodedAgain) <- captureDiagnostics decode
      unless (sum [total ("exact_iface_read_calls." ++ exactModule iface) decodedAgain | iface <- interfaces]
              == toInteger (length interfaces)
          && sum [total ("exact_iface_decode_reuse." ++ exactModule iface) decodedAgain | iface <- interfaces] == 0)
        (fail "late live-scope decoding repopulated zero-budget inactive interface retention")
      (_,evicted) <- captureDiagnostics (readExactScopeWithOwner bounded thirdPath >>= either fail pure)
      unless (counterValues "original_inputs.retained_encoded_bytes" evicted == [0]
          && total "original_inputs.certificate.misses" evicted == 2)
        (fail "zero inactive allowance retained content or decoded facts")
    -- Permit either genuine image alone, but not both. The receiving scope owns
    -- both images even while the inactive pool evicts one of them.
    term <- readCodecTerm thirdPath
    imageSizes <- case term of
      TList fields -> case last fields of
        TList [TString "continue-originals",_,TList images] -> forM images $ \image -> case image of
          TList [_,_,_,_,TList parts] -> do
            sizes <- forM parts $ \part -> case part of
              TList [_,_,TString sha,size,_,_] -> case size of
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
      -- The production world index reserves against this same owner. A full
      -- byte OR entry reservation must evict genuine optional image/decoder
      -- state while preserving the live receiving scope's captured bytes.
      forM_ [False,True] $ \entriesOnly -> do
        shared <- newExactInputOwner
        sharedLive <- readExactScopeWithOwner shared thirdPath >>= either fail pure
        let (byteLimit,entryLimit) = exactInputRetentionLimits shared
            (reservedBytes,reservedEntries) = if entriesOnly then (0,entryLimit) else (byteLimit,0)
            interfaces = [iface | (iface,_,_) <- scopeInterfaces sharedLive]
            decode = readScopedInterfaces (prHscEnv (pprPipelineResult original)) sharedLive interfaces >>= either fail pure
        before <- mapM (scopeInterfaceBytes sharedLive) interfaces
        _ <- decode
        published <- newIORef False
        commitWorldInputRetention shared reservedBytes reservedEntries (writeIORef published True)
        installed <- readIORef published
        unless installed (fail "shared retention did not publish its world reservation")
        _ <- decode
        (_,decoded) <- captureDiagnostics decode
        unless (sum [total ("exact_iface_read_calls." ++ exactModule iface) decoded | iface <- interfaces] == 2
            && sum [total ("exact_iface_decode_reuse." ++ exactModule iface) decoded | iface <- interfaces] == 0)
          (fail "world reservation left optional image decoding in another pool")
        (_,reserved) <- captureDiagnostics (readExactScopeWithOwner shared thirdPath >>= either fail pure)
        unless (counterValues "original_inputs.retained_encoded_bytes" reserved == [0]
            && total "original_inputs.certificate.misses" reserved == 2)
          (fail "legacy acquisition ignored the shared world reservation")
        after <- mapM (scopeInterfaceBytes sharedLive) interfaces
        unless (before == after) (fail "world reservation revoked live raw input custody")
        commitWorldInputRetention shared 0 0 (pure ())
        _ <- readExactScopeWithOwner shared thirdPath >>= either fail pure
        (_,released) <- captureDiagnostics (readExactScopeWithOwner shared thirdPath >>= either fail pure)
        unless (total "original_inputs.retained_encoded_bytes" released > 0
            && total "original_inputs.certificate.hits" released > 0)
          (fail "released world reservation failed to restore the same optional pool")
      bounded <- newExactInputOwner
      (live,evicted) <- captureDiagnostics (readExactScopeWithOwner bounded thirdPath >>= either fail pure)
      let interfaces = [iface | (iface,_,_) <- scopeInterfaces live]
          decode = readScopedInterfaces (prHscEnv (pprPipelineResult original)) live interfaces >>= either fail pure
      unless (length interfaces == 2 && Set.size (Set.fromList (map exactSha256 interfaces)) == 2)
        (fail "positive-budget decoder control needs two genuine distinct interface owners")
      _ <- decode
      (_,decodedAgain) <- captureDiagnostics decode
      unless (sum [total ("exact_iface_decode_reuse." ++ exactModule iface) decodedAgain | iface <- interfaces] > 0
          && sum [total ("exact_iface_read_calls." ++ exactModule iface) decodedAgain | iface <- interfaces] > 0)
        (fail "positive inactive allowance did not separate retained and live-only interface decoding")
      unless (total "original_inputs.retained_encoded_bytes" evicted > 0
          && total "original_inputs.evicted_encoded_bytes" evicted > 0)
        (fail "positive allowance did not retain and evict genuine images")
      putStrLn ("owned input positive allowance=" ++ show positiveLimit
        ++ " retained=" ++ show (total "original_inputs.retained_encoded_bytes" evicted)
        ++ " evicted=" ++ show (total "original_inputs.evicted_encoded_bytes" evicted))
      receiving <- writeGenuineMetadataScope work ["CanonicalDependency"] fixture >>= ownedFixtureScope handles
      _ <- readExactScopeWithOwner bounded receiving >>= either fail pure
      -- Change only image order: the same issued complete scope now retains
      -- Source first. Restoring the same full images Dependency first must
      -- evict Source's optional decode while the old complete scope stays usable.
      (_,codec) <- readScopeCodecFixture thirdPath
      (arenas,images) <- case scopeCodecAcquisition codec of
        Just (ContinueCodecOriginals arenas (TList images)) -> pure (arenas,images)
        _ -> fail "publication control needs genuine owned images"
      let sourceFirst image = case image of
            TList [_,_,TString name,_,_] -> name /= "CanonicalSource"
            _ -> True
          failedPath = work </> "failed-scope-publication.cbor"
      BS.writeFile failedPath (toStrictByteString (encodeTerm (scopeCodecTerm
        (replaceScopeCodecAcquisition (ContinueCodecOriginals arenas
          (TList (sortOn sourceFirst images))) codec))))
      forM_ [False,True] $ \asynchronous -> do
        let kind = if asynchronous then "async" else "io"
        atomicOwner <- newExactInputOwner
        oldLive <- readExactScopeWithOwner atomicOwner failedPath >>= either fail pure
        _ <- readExactScopeWithOwner atomicOwner thirdPath >>= either fail pure
        let oldInterfaces = [iface | (iface,_,_) <- scopeInterfaces oldLive]
            decodeOld = readScopedInterfaces (prHscEnv (pprPipelineResult original)) oldLive oldInterfaces >>= either fail pure
        _ <- decodeOld
        (_,beforeFailed) <- captureDiagnostics decodeOld
        unless (total "exact_iface_read_calls.CanonicalSource" beforeFailed == 1
            && total "exact_iface_decode_reuse.CanonicalSource" beforeFailed == 0
            && total "exact_iface_decode_reuse.CanonicalDependency" beforeFailed == 1)
          (fail "publication control did not establish Dependency-only inactive decoder retention")
        attempted <- failRetainedCountWrite asynchronous (readExactScopeWithOwner atomicOwner failedPath)
        unless (case (kind,attempted) of
            ("io",Right (Left reason)) -> "injected retained-count write failure" `isInfixOf` reason
            ("async",Left problem) -> fromException problem == Just ThreadKilled
            _ -> False)
          (fail ("scope publication did not reach its injected " ++ kind ++ " failure"))
        (_,afterFailed) <- captureDiagnostics (decodeOld >> decodeOld)
        unless (total "exact_iface_read_calls.CanonicalSource" afterFailed == 2
            && total "exact_iface_decode_reuse.CanonicalSource" afterFailed == 0
            && total "exact_iface_decode_reuse.CanonicalDependency" afterFailed == 2)
          (fail ("failed " ++ kind ++ " acquisition split raw custody from decoder retention: " ++ afterFailed))
        -- Probe only Dependency after the decoder observations: aggregate
        -- full-scope hit counts cannot distinguish retained Dependency/Source.
        (_,stillPrevious) <- captureDiagnostics (readExactScopeWithOwner atomicOwner receiving >>= either fail pure)
        unless (total "original_inputs.certificate.hits" stillPrevious == 1
            && total "original_inputs.certificate.misses" stillPrevious == 0)
          (fail "failed acquisition discarded previous raw input facts")
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

-- Fail a real timing write at the old decoder/raw publication split. A one-shot
-- encoder delegates UTF-8 unchanged and recognizes only consumed characters;
-- chunk boundaries and retries cannot silently miss or duplicate the injection.
failRetainedCountWrite :: Bool -> IO a -> IO (Either SomeException a)
failRetainedCountWrite asynchronous action = do
  fired <- newIORef False
  (result,_) <- captureDiagnostics $
    if asynchronous then do
      reached <- newEmptyMVar
      blocked <- newEmptyMVar
      done <- newEmptyMVar
      let retire reader = do
            settled <- timeout 5000000 (killThread reader >> readMVar done)
            case settled of
              Nothing -> fail "publication fault observer did not join its acquisition thread"
              Just _ -> pure ()
      bracket (forkIO (try (withRetainedCountFault fired
          (putMVar reached () >> takeMVar blocked) action) >>= putMVar done)) retire $ \reader -> do
        observed <- timeout 5000000 (takeMVar reached)
        unless (observed == Just ()) (fail "publication cancellation did not reach its timing write")
        killThread reader
        timeout 5000000 (readMVar done) >>= maybe
          (fail "publication cancellation did not settle") pure
    else try (withRetainedCountFault fired
      (throwIO (userError "injected retained-count write failure")) action)
  didFire <- readIORef fired
  unless didFire (fail "publication failure control did not reach its timing write")
  pure result

withRetainedCountFault :: IORef Bool -> IO () -> IO a -> IO a
withRetainedCountFault fired fault action = mask $ \restore -> do
  oldEncoding <- hGetEncoding stderr
  oldBuffering <- hGetBuffering stderr
  armed <- newIORef True
  suffix <- newIORef ""
  let marker = "tidepool-count name=original_inputs.retained_encoded_bytes "
      wrapped (TextEncoding name makeDecoder makeEncoder) =
        TextEncoding name makeDecoder $ do
          BufferCodec encode recover close getState setState <- makeEncoder
          let inspect input output = do
                result@(_, consumed, _) <- encode input output
                active <- readIORef armed
                when active $ do
                  -- UTF-8 resets a fully consumed character buffer to 0/0.
                  let end = if bufL consumed == 0 && bufR consumed == 0
                        then bufR input else bufL consumed
                  chars <- consumedChars input end
                  previous <- readIORef suffix
                  let observed = previous ++ chars
                  if marker `isInfixOf` observed
                    then do
                      writeIORef armed False
                      writeIORef fired True
                      fault
                    else writeIORef suffix (drop (max 0 (length observed - length marker + 1)) observed)
                pure result
          pure (BufferCodec inspect recover close getState setState)
      restoreSettings = do
        writeIORef armed False
        hFlush stderr `finally` (do
          case oldEncoding of
            Just encoding -> hSetEncoding stderr encoding
            Nothing -> hSetBinaryMode stderr True
          hSetBuffering stderr oldBuffering)
      install = do
        hFlush stderr
        hSetBuffering stderr NoBuffering
        hSetEncoding stderr (wrapped utf8)
  install `onException` restoreSettings
  restore action `finally` restoreSettings

consumedChars :: CharBuffer -> Int -> IO String
consumedChars buffer end
  | end < bufL buffer || end > bufR buffer =
      fail "UTF-8 adapter observed an invalid consumed input range"
  | otherwise = go (bufL buffer)
  where
    go index
      | index == end = pure []
      | index > end = fail "UTF-8 adapter stepped beyond consumed characters"
      | otherwise = do
          (character,next) <- readCharBuf (bufRaw buffer) index
          (character :) <$> go next

-- One genuine preparation exercises the admitted-operation boundary. The
-- held plan is parsed exactly once while binding the prepare request; neither
-- the plan operation nor the consuming validation reparses the same recipe.
-- Rooted boot supplies the production package profile through its actual
-- service association rather than an observation-only fixture scope.
worldCellAdmissionParse :: IO ()
worldCellAdmissionParse = withTiming $ withScratch $ \work ->
  bracket (newIORef []) (\owners -> readIORef owners >>= mapM_ hClose) $ \handles -> do
    forM_ ["CanonicalSource.hs","CanonicalDependency.hs"] $ \name ->
      copyFile ("test-source-boot/fixtures" </> name) (work </> name)
    original <- runPipelineSessionSelected (PreparedProducts Nothing) Set.empty CertifyHomeProductsCompile
      Nothing (work </> "CanonicalSource.hs") [work] Nothing
    fixture <- capturePreparedFixture work original
    (scopePath,contextParcel) <- writeGenuineExecutionScopeWithContext
      ["CanonicalSource","CanonicalDependency"] ["CanonicalSource"] work fixture
    scope <- readExactScope scopePath >>= either fail pure
    let producerHex = scopeProducerSha256 scope
    producer <- rawWorldHex producerHex
    profile <- rawWorldHex rootedProfileIdentity
    (packetPath,_primary) <- writeWorldCellAdmissionPacket handles work producer profile contextParcel
    owner <- newExactInputOwner
    facts <- readWorldInputsWithOwner owner packetPath
    let identity = worldInputsIdentity facts
    worldDirectory <- canonicalizePath work
    ifaceTrace <- lookupEnv "TIDEPOOL_IFACE_DEBUG"
    withRootedCompilerEnvironment worldDirectory $ do
      (outcome,diagnostics) <- withQuietStdout $ captureDiagnostics $
        withResidentCompilerScopes [] $ \runScope -> do
          admission <- runScope (pure ()) $ \compiler -> do
            ready <- scopedEnsureWorld compiler (ExactInputsRequest identity (CompleteExactInputs packetPath))
            reference <- case ready of
              ExactInputsReady _ realization -> pure realization
              other -> fail ("real preparation could not acquire its captured world: " ++ show other)
            -- The component template uses session/dialect.rs:EVAL_PRAGMAS.
            -- Its explicit Prelude import supplies this small template's
            -- lexical base under the shipped NoImplicitPrelude dialect.
            let template = unlines
                  ["{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, KindSignatures, RankNTypes, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, StandaloneDeriving, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot, OverloadedLabels #-}"
                  ,if ifaceTrace == Just "1" then "{-# OPTIONS_GHC -ddump-if-trace #-}" else ""
                  ,"{{CELL_PRAGMAS}}","module CellCheck where"
                  ,"import Prelude","import Control.Monad.Freer (Eff)","{{CELL_IMPORTS}}"
                  ,"__tidepoolInEffectRow :: Eff '[] value -> Eff '[] value"
                  ,"__tidepoolInEffectRow = id"
                  ,"{{CELL_DECLS}}"
                  ,"__tidepool_cell_check :: Eff '[] ()"
                  ,"__tidepool_cell_check = do { {{CELL_BODY}} ; pure () }"]
                recipe = WorldCellRecipe "let answer = (1 :: Int)" template []
                planOut = work </> "cell-plan.cbor"
                planRequest = WorldCellPlanRequest identity reference recipe Nothing planOut
            planArgs <- decodeWorldArgs [WorldCellPlan planRequest]
            planBytes <- (withBoundCompilerRequest (const (pure ())) compiler planArgs >=> finishCompletion) $ \request ->
              runBoundWorldCellPlan request
            slots <- scopedRunGhc compiler $ do
              environment <- getSession
              plan <- liftIO (analyzeOrderedCellWithFlags (hsc_dflags environment)
                (worldCellTemplate recipe) (worldCellSource recipe)) >>= either (liftIO . fail . show) pure
              let items = cellPlanItems plan
              liftIO $ unless (length items == 1
                  && sbKind (cellAnalysisVerdict (head items)) == KBind
                  && length (cellInferenceSegments plan) == 1)
                (fail ("admission fixture is not one executable bind item: items="
                  ++ show (length items)
                  ++ " kinds=" ++ show (map (sbKind . cellAnalysisVerdict) items)
                  ++ " sources=" ++ show (map cellAnalysisSource items)
                  ++ " segments=" ++ show (length (cellInferenceSegments plan))))
              liftIO (pure [(0, PlannedBind 1)])
            let directory = work </> "cell-admission"
                prepareRequest = WorldCellPrepareRequest identity reference recipe Nothing planBytes
                  (BS.replicate 32 7) 17 slots directory (directory </> "prepared.cbor")
            createDirectoryIfMissing True directory
            prepareArgs <- decodeWorldArgs [WorldCellPrepare prepareRequest]
            validated <- (withBoundCompilerRequest (const (pure ())) compiler prepareArgs >=> finishCompletion) $ \request -> do
              validation <- boundWorldCellPreparation request
              forM_ validation $ \admitted -> do
                unissued <- try (void (scopedCompileWorldCell compiler admitted CheckedEnvironment Set.empty))
                  :: IO (Either InputRejection ())
                unless (either (\failure -> case failure of CheckedPurposeMismatch -> True; _ -> False)
                    (const False) unissued)
                  (fail "World compiled before its owner issued the current stage")
                stage <- scopedPrepareWorldCellStage compiler admitted >>= maybe
                  (fail "admitted World lost its first rendered stage") pure
                again <- scopedPrepareWorldCellStage compiler admitted >>= maybe
                  (fail "repeated World stage lookup consumed the current stage") pure
                issued <- maybe (fail "executable World stage omitted its typed source") pure
                  (heldWorldCellStageTypedSource stage)
                repeated <- maybe (fail "repeated World stage lost its typed source") pure
                  (heldWorldCellStageTypedSource again)
                unless (heldWorldCellStagePath stage == heldWorldCellStagePath again
                    && preparedTypedSegmentSource issued == preparedTypedSegmentSource repeated
                    && preparedTypedSegmentPlan issued == preparedTypedSegmentPlan repeated)
                  (fail "repeated World stage lookup changed its issued rendering")
                let stagePath = heldWorldCellStagePath stage
                issuedBytes <- BS.readFile stagePath
                changed <- (do
                    let modifiedBytes = issuedBytes <> "\nunauthorizedStageValue = ()\n"
                    BS.writeFile stagePath modifiedBytes
                    cached <- scopedPrepareWorldCellStage compiler admitted >>= maybe
                      (fail "cached World rendering disappeared after a file change") pure
                    observedBytes <- BS.readFile stagePath
                    unless (heldWorldCellStagePath cached == stagePath && observedBytes == modifiedBytes)
                      (fail "repeated World stage lookup regenerated its issued file")
                    try (void (scopedCompileWorldCell compiler admitted
                      (PreparedSegmentProducts (preparedTypedSegmentPlan issued) Nothing) Set.empty)))
                  `finally` BS.writeFile stagePath issuedBytes
                  :: IO (Either WorldCellRefusal ())
                unless (either (\failure -> case failure of WorldCellCorrupt -> True; _ -> False)
                    (const False) changed)
                  (fail "World accepted a source file changed after stage issuance")
                completions <- prepareWorldCell request >>= either
                  (fail . ("real world preparation refused: " ++) . show) pure
                completion <- case completions of
                  [value] -> pure value
                  _ -> fail "real world preparation did not emit exactly one compiler completion"
                output <- BS.readFile (worldCellPrepareOutput prepareRequest)
                certificate <- BS.readFile (directory </> "stage-0" </> "certified-products.cbor")
                case deserialiseFromBytes decodeTerm (BL.fromStrict certificate) of
                  Right (remaining,TList [TString "TPCERT",TInt 10,TList
                    [TString "segment-originals",_,_,_,_,TList
                      [TString "world-unavailable",TBytes base,TBytes domain,TString "unsupported-source-recipe"],_]])
                    | BL.null remaining,base == worldInputsIdentityBytes facts
                    , domain == worldSourceDomainSha256 (worldInputsSources facts) -> pure ()
                  _ -> fail "World native certificate lost its actual base/source-domain recipe provenance"
                itemSha <- case deserialiseFromBytes decodeTerm (BL.fromStrict output) of
                  Right (remaining,TList [TString "TPCWPREP5",_,_,_
                    ,TList [TString "prepared",TList [TList [TInt 0,TInt 1,TBytes sha]]]
                    ,_,TList common,TList [_]
                    ,TList [TList [identity@(TList [TString "exec",TString _]),TList [TInt 0]
                      ,TList refs,TList []]]]) | BL.null remaining -> do
                        unless (map (\entry -> case entry of
                            TList [TString name,TBytes _] -> T.unpack name
                            _ -> "") refs == map ("stage-0/" ++)
                              ["world-source.cbor","module-products.cbor","certified-products.cbor"])
                          (fail "World stage omitted its exact closed output bundle")
                        forM_ refs $ \entry -> do
                          unless (entry `elem` common) (fail "World stage output leaves the common manifest")
                          case entry of
                            TList [TString name,TBytes expected] -> do
                              bytes <- BS.readFile (directory </> T.unpack name)
                              unless (SHA.hash bytes == expected) (fail "World stage output seal changed")
                            _ -> fail "World stage omitted its file reference"
                        source <- BS.readFile (directory </> "stage-0" </> "world-source.cbor")
                        case deserialiseFromBytes decodeTerm (BL.fromStrict source) of
                          Right (tailBytes,TList (TString "TPCWSOURCE3":_:_:_:actualIdentity:_))
                            | BL.null tailBytes,actualIdentity == identity -> pure ()
                          _ -> fail "World stage source receipt changed its issued identity"
                        pure sha
                  _ -> fail "real world preparation omitted its exact item and typed segment manifest"
                let itemDirectory = directory </> "item-0"
                item <- BS.readFile (itemDirectory </> "world-item.cbor")
                unless (SHA.hash item == itemSha)
                  (fail "real world item differs from its terminal output manifest")
                case deserialiseFromBytes decodeTerm (BL.fromStrict item) of
                  Right (remaining,TList fields@(TString "TPCWITEM2":_)) | BL.null remaining,length fields == 13 ->
                    case last fields of
                      TList files -> forM_ files $ \entry -> case entry of
                        TList [TString name,TBytes sha] -> do
                          bytes <- BS.readFile (itemDirectory </> T.unpack name)
                          unless (SHA.hash bytes == sha) (fail "emitted world file differs from its item proof")
                        _ -> fail "real world item omitted an exact file seal"
                      _ -> fail "real world item omitted its captured files"
                  _ -> fail "real world preparation omitted its actual item proof"
                crossedCommit <- (withBoundCompilerRequest (const (pure ())) compiler planArgs >=> finishCompletion) $ \other ->
                  try (commitPreparedWorldDependencies other completion) :: IO (Either InputRejection ())
                unless (either (\failure -> case failure of CheckedPurposeMismatch -> True; _ -> False)
                    (const False) crossedCommit)
                  (fail "foreign request committed another preparation's pending dependencies")
                commitPreparedWorldDependencies request completion
                replay <- try (commitPreparedWorldDependencies request completion) :: IO (Either InputRejection ())
                unless (either (\failure -> case failure of CheckedPurposeMismatch -> True; _ -> False)
                    (const False) replay)
                  (fail "world dependency commitment replayed after consumption")
                -- The completed cursor cannot compile a replay, even while
                -- the request's own phase and executor remain open.
                replayCompile <- try (void (scopedCompileWorldCell compiler admitted CheckedEnvironment Set.empty))
                  :: IO (Either InputRejection ())
                unless (either (\failure -> case failure of CheckedPurposeMismatch -> True; _ -> False)
                    (const False) replayCompile)
                  (fail "a completed World stage cursor compiled a replay")
              pure validation
            case validated of
              Left refusal -> fail ("admitted real preparation refused its plan: " ++ show refusal)
              Right admitted
                | worldCellPreparePlan (heldWorldCellQuestion admitted) /= planBytes ->
                    fail "admission changed the prepared plan bytes"
                | length (cellPlanItems (heldWorldCellPlan admitted)) /= 1 ->
                    fail "admission lost its actual parsed execution item"
                | heldWorldCellSlots admitted /= slots ->
                    fail "admitted preparation lost its matched slot collection"
                | otherwise -> do
                    -- These small native controls exercise the same admitted
                    -- producer and its global stage ordinals. The shipped
                    -- runtime-template tests remain the end-to-end oracle.
                    forM_
                      [("imports","import Data.Maybe (Maybe(..))",[(0,PlannedPrologue 11)],["prologue"])
                      ,("declaration",unlines ["data OwnedStage = OwnedStage Int"
                          ,"class OwnedStageClass value where ownedStageMethod :: value -> Int"
                          ,"instance OwnedStageClass OwnedStage where ownedStageMethod (OwnedStage value) = value"]
                        ,[(0,PlannedDeclaration 21)],["declaration"])
                      ,("mixed",unlines ["let baseStage = (7 :: Int)"
                          ,"data MixedStage = MixedStage Int"
                          ,"readMixedStage :: MixedStage -> Int"
                          ,"readMixedStage (MixedStage value) = value + baseStage"
                          ,"let answerStage = MixedStage baseStage"
                          ,"readMixedStage answerStage"]
                        ,[(0,PlannedBind 31),(1,PlannedDeclaration 32),(2,PlannedBind 33)
                          ,(3,PlannedExpression 34 "ownedStageObservation")]
                        ,["exec","declaration","exec"])
                      ,("publication",unlines ["let publishedBaseStage = (7 :: Int)"
                          ,"readPublishedStage :: Int -> Int"
                          ,"readPublishedStage value = value + publishedBaseStage"]
                        ,[(0,PlannedBind 41),(1,PlannedDeclaration 42)]
                        ,["exec","declaration"])
                      ,("default-publication",unlines ["let defaultBaseStage = (7 :: Int)"
                          ,"class PublishedDefault value where"
                          ,"  readPublishedDefault :: value -> Int"
                          ,"  readPublishedDefault _ = defaultBaseStage"]
                        ,[(0,PlannedBind 51),(1,PlannedDeclaration 52)]
                        ,["exec","declaration"])
                      ,("transitive-publication",unlines ["let transitiveBaseStage = (7 :: Int)"
                          ,"readFirstStage :: Int -> Int"
                          ,"readFirstStage value = value + transitiveBaseStage"
                          ,"let unrelatedStage = ()"
                          ,"readLaterStage :: Int -> Int"
                          ,"readLaterStage value = readFirstStage value"]
                        ,[(0,PlannedBind 61),(1,PlannedDeclaration 62)
                          ,(2,PlannedBind 63),(3,PlannedDeclaration 64)]
                        ,["exec","declaration","exec","declaration"])
                      ,("source-family",unlines ["import qualified CacheEntry"
                          ,"import qualified CacheEven"
                          ,"readBootStage :: CacheEven.Payload Bool -> Bool"
                          ,"readBootStage value = value && CacheEntry.result"
                          ,"let bootAnswerStage = readBootStage True"]
                        ,[(0,PlannedPrologue 71),(1,PlannedDeclaration 72),(2,PlannedBind 73)]
                        ,["prologue","declaration","exec"])] $ \(label,source,caseSlots,kinds) -> do
                      let ownRecipe = recipe {worldCellSource=source}
                          ownDirectory = work </> ("world-stages-" ++ label)
                          ownPlan = planRequest {worldCellPlanRecipe=ownRecipe
                            ,worldCellPlanOutput=ownDirectory </> "plan.cbor"}
                      createDirectoryIfMissing True ownDirectory
                      ownPlanArgs <- decodeWorldArgs [WorldCellPlan ownPlan]
                      ownPlanBytes <- (withBoundCompilerRequest (const (pure ())) compiler ownPlanArgs >=> finishCompletion)
                        runBoundWorldCellPlan
                      let ownPrepare = WorldCellPrepareRequest identity reference ownRecipe Nothing ownPlanBytes
                            (BS.replicate 32 9) 17 caseSlots ownDirectory (ownDirectory </> "prepared.cbor")
                      ownArgs <- decodeWorldArgs [WorldCellPrepare ownPrepare]
                      (withBoundCompilerRequest (const (pure ())) compiler ownArgs >=> finishCompletion) $ \request -> do
                        completions <- prepareWorldCell request >>= either
                          (fail . (("ordered World " ++ label ++ " refused: ") ++) . show) pure
                        unless (length completions == length kinds)
                          (fail "ordered World lost an actual stage completion")
                        bytes <- BS.readFile (worldCellPrepareOutput ownPrepare)
                        case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
                          Right (remaining,TList [TString "TPCWPREP5",_,_,_
                            ,TList [TString "prepared",TList completed],_,_,TList typed,TList stages])
                              | BL.null remaining -> do
                                let stageKind entry = case entry of
                                      TList [TList (TString kind:_),TList _,TList _,TList _] -> kind
                                      _ -> "invalid"
                                    stageOrdinals entry = case entry of
                                      TList [_,TList ordinals,_,TList _] -> ordinals
                                      _ -> []
                                    stageReferences entry = case entry of
                                      TList [_,_,_,TList references] -> references
                                      _ -> []
                                    referencesAt ordinal = case drop ordinal stages of
                                      entry:_ -> stageReferences entry
                                      _ -> []
                                    inputRefs = concatMap stageReferences stages
                                    capturedByDeclaration entry = case entry of
                                      TList [TList [TInt 1,TInt 0,TInt 1],_,_,_,_,_,_] -> True
                                      _ -> False
                                unless (map stageKind stages == kinds
                                    && concatMap stageOrdinals stages == map (TInt . fromIntegral . fst) caseSlots
                                    && length completed == length caseSlots
                                    && length typed == length (filter (== "exec") kinds))
                                  (fail "ordered World changed stage kind, global ordinal, or executable-plan coverage")
                                unless (if label `elem` ["mixed","publication","default-publication","transitive-publication"]
                                    then any capturedByDeclaration (referencesAt 1)
                                    else null inputRefs)
                                  (fail "World execution inputs lost their actual native closure or original capture stage")
                                when (label == "mixed") $ do
                                  let direct entry = case entry of
                                        TList [TList [TInt 1,TInt 0,TInt 2],_,_,_,_,_,_] -> True
                                        _ -> False
                                  unless (any direct (referencesAt 2)
                                      && not (any capturedByDeclaration (referencesAt 2)))
                                    (fail "World stage failed to prefer its genuine current capture")
                                when (label == "transitive-publication") $
                                  unless (not (null (referencesAt 1))
                                      && referencesAt 3 == referencesAt 1
                                      && null (referencesAt 0) && null (referencesAt 2))
                                    (fail "later publication did not reuse the exact earlier selected capture")
                          _ -> fail "ordered World omitted its terminal stage table"
                        forM_ caseSlots $ \(ordinal,slot) -> case slot of
                          PlannedBind _ -> pure ()
                          PlannedExpression _ _ -> pure ()
                          _ -> do
                            bytes <- BS.readFile (ownDirectory </> ("item-" ++ show ordinal) </> "world-item.cbor")
                            case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
                              Right (remaining,TList (TString "TPCWORIGINAL1":fields))
                                | BL.null remaining,length fields == 7 -> pure ()
                              _ -> fail "original stage emitted an executable item surrogate"
                        mapM_ (commitPreparedWorldDependencies request) completions
                    pure admitted
          runScope (pure ()) $ \compiler -> do
            let question = heldWorldCellQuestion admission
            ready <- scopedEnsureWorld compiler (ExactInputsRequest identity
              (ResidentExactInputs (worldCellPrepareRealization question)))
            case ready of
              ExactInputsReady _ _ -> pure ()
              other -> fail ("scope substitution control lost the actual retained world: " ++ show other)
            crossed <- try (void (scopedCompileWorldCell compiler admission CheckedEnvironment Set.empty))
              :: IO (Either InputRejection ())
            unless (either (\failure -> case failure of CheckedPurposeMismatch -> True; _ -> False)
                (const False) crossed)
              (fail "another compiler scope consumed the original scope's admission")
      outcome `seq` pure ()
      let admissionParses = counterValues "world.cell.admission_parses" diagnostics
      hPutStrLn stderr ("world.cell.admission_parses count=" ++ show (sum admissionParses))
      unless (admissionParses == replicate 8 1)
        (fail ("real preparation did not parse its recipe exactly once: "
          ++ show admissionParses))

-- The Context fixture's logical roots and captured bytes stay genuine. Only
-- the transport restoration's profile field is rebound to the rooted service
-- profile, and its owned acquisition is resealed as production DATA endpoints.
writeWorldCellAdmissionPacket
  :: IORef [Handle] -> FilePath -> BS.ByteString -> BS.ByteString -> BS.ByteString
  -> IO (FilePath,BS.ByteString)
writeWorldCellAdmissionPacket handles work producer profile parcelBytes = do
  (primary,association,base,durable,contextBlocks,lexical) <- worldParcel parcelBytes
  acquisition <- sealWorldParcel handles (TList durable)
  (emptyPrimary,_,emptyBase,_,emptyBlocks,_) <- fixture "world-context-blocks" [TBytes producer] >>= worldParcel
  unless (null emptyBase) (fail "empty world context published primary rows")
  emptyBody <- maybe (fail "empty Context root is absent") pure (lookup emptyPrimary emptyBlocks)
  emptyTerm <- decode emptyBody
  emptyBinding <- case emptyTerm of
    TMap fields -> case lookup (TString "roles") fields of
      Just (TTagged 42 (TBytes bytes)) | not (BS.null bytes),BS.head bytes == 0 -> pure (BS.tail bytes)
      _ -> fail "empty Materializer context omitted its empty map"
    _ -> fail "empty Materializer Context descriptor changed"
  pairs <- reachable [primary,association,emptyBinding,lexical] (contextBlocks ++ emptyBlocks)
  captured <- forM ["CacheEven.hs","CacheEven.hs-boot","CacheOdd.hs","CacheEntry.hs"] $ \name -> do
    body <- BS.readFile ("test-source-boot/fixtures" </> name)
    pure (BS8.pack name,body)
  libraryRoot <- lookupEnv "TIDEPOOL_PRELUDE_DIR" >>= maybe
    (fail "missing declared renderer source resource") pure
  library <- captureSourceFiles libraryRoot libraryRoot
  let capturedFiles = sortOn fst (captured ++ library)
      word32 value = BS.pack [fromIntegral (value `div` (256 ^ power)) | power <- [0..3 :: Int]]
      frame bytes = word32 (BS.length bytes) <> bytes
      sources = "TPCSRC01" <> frame "world-stage-source" <> word32 1 <> word32 (length capturedFiles)
        <> foldMap (\(name,body) -> frame name <> frame body) capturedFiles
      sourcePath = work </> "world-admission-sources.bin"
      text = TString . T.pack
      blocks values = TList [TList [TBytes cid,TBytes body] | (cid,body) <- values]
      visibility root = encode (TList [TString "TPCWBIND4",TInt 0,TBytes root])
      restoration = encode (TList [TString "TPCWREST",TInt 4,TBytes primary,TBytes (SHA.hash sources)
        ,TBytes producer,TBytes profile,TInt 0,TBytes association,TBytes emptyBinding,TBytes lexical])
      packet = TList [TString "TPCWORLD",TInt 4,TBytes restoration,text sourcePath,TBytes producer
        ,TBytes profile,TList [TList base,TList []],acquisition,TBytes (visibility emptyBinding)
        ,TBytes primary,blocks pairs]
  BS.writeFile sourcePath sources
  let path = work </> "world-admission.cbor"
  BS.writeFile path (encode packet)
  pure (path,primary)
  where
    captureSourceFiles root directory = do
      children <- listDirectory directory
      fmap concat $ forM children $ \name -> do
        let path = directory </> name
        nested <- doesDirectoryExist path
        if nested then captureSourceFiles root path
        else if takeExtension path == ".hs" then do
          bytes <- BS.readFile path
          pure [(BS8.pack (makeRelative root path),bytes)]
        else pure []
    fixture operation arguments = do
      path <- issueCodecFixturePacket work (operation ++ ".cbor") (encode
        (TList [TString "TPCODECFIXTURE1",TString (T.pack operation),TList arguments]))
      BS.readFile path
    worldParcel bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
      Right (remaining,TList [TBytes primary,TBytes association,TList rows,TList durable,TList pairs,TBytes lexical])
        | BL.null remaining -> do
            blocks <- forM pairs $ \entry -> case entry of
              TList [TBytes cid,TBytes body] -> pure (cid,body)
              _ -> fail "world fixture omitted its block array"
            pure (primary,association,rows,durable,blocks,lexical)
      _ -> fail "world fixture needs actual Context/association roots, rows, owned bytes and blocks"
    sealWorldParcel owners (TList [TString "TPCWORLDFIXTUREBYTES1",TList arenas,TList images]) = do
      live <- forM arenas $ \entry -> case entry of
        TList [TString path,extent] -> do
          amount <- unsigned extent
          bytes <- BS.readFile (T.unpack path)
          unless (toInteger (BS.length bytes) == amount) (fail "world fixture arena extent drifted")
          mask $ \_ -> do
            (handle,endpoint) <- acquireSealedInputBytes bytes
            modifyIORef' owners (handle:)
            pure (TList [TString (T.pack endpoint),extent])
        _ -> fail "world fixture needs exact durable arena DATA"
      pure (TList [TString "continue-originals",TList live,TList images])
    sealWorldParcel _ _ = fail "world fixture bytes were not explicitly DATA before resealing"
    reachable roots pairs = do
      let available = Map.fromList pairs
          visit seen [] = pure seen
          visit seen (cid:rest) | Set.member cid seen = visit seen rest
          visit seen (cid:rest) = case Map.lookup cid available of
            Nothing -> fail "genuine world fixture omitted a reachable block"
            Just body -> decode body >>= \term -> visit (Set.insert cid seen) (links term ++ rest)
      selected <- visit Set.empty roots
      pure (Map.toAscList (Map.restrictKeys available selected))
    links (TTagged 42 (TBytes bytes)) | not (BS.null bytes),BS.head bytes == 0 = [BS.tail bytes]
    links (TList values) = concatMap links values
    links (TMap values) = concatMap (\(key,value) -> links key ++ links value) values
    links _ = []
    unsigned (TInt value) | value >= 0 = pure (toInteger value)
    unsigned (TInteger value) | value >= 0 = pure value
    unsigned _ = fail "world fixture extent must be unsigned"

decodeWorldArgs :: [RequestField] -> IO WorkerRequest
decodeWorldArgs fields = case workerRequestFromArgv (workerArgv fields) of
  Right (Just request) -> pure request
  other -> fail ("invalid held world admission fixture: " ++ show other)

encode :: Term -> BS.ByteString
encode = toStrictByteString . encodeTerm

decode :: BS.ByteString -> IO Term
decode bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
  Right (remaining,term) | BL.null remaining -> pure term
  _ -> fail "world fixture term has trailing or malformed bytes"

rawWorldHex :: String -> IO BS.ByteString
rawWorldHex [] = pure BS.empty
rawWorldHex (a:b:rest) = case readHex [a,b] of
  [(byte,"")] -> BS.cons byte <$> rawWorldHex rest
  _ -> fail "world producer has malformed SHA"
rawWorldHex _ = fail "world producer has an odd SHA length"

-- The production package profile matched to this checkout's declared GHC
-- libdir. The preparation test bootstraps that same profile through the
-- resident compiler service rather than fabricating boot metadata.
rootedProfilePath :: String
rootedProfilePath = "/nix/store/bfa5qsshnhgxihzxf2klvzxx0d2pass9-compiler-package-profile-v1"

rootedProfileSha256 :: String
rootedProfileSha256 = "6722c5a9ea70c599d76a874dc6c4c014d1264abd700537293f9f347ab6d745ce"

rootedProfileIdentity :: String
rootedProfileIdentity = "15663ac19c31058a9203f6404f72cad491923a1f34209bc68cff3bbbf3111880"

withRootedCompilerEnvironment :: FilePath -> IO a -> IO a
withRootedCompilerEnvironment worldDirectory action = bracket
  (traverse (\name -> (name,) <$> lookupEnv name) names)
  (mapM_ (\(name,value) -> maybe (unsetEnv name) (setEnv name) value)) $ \_ -> do
    unsetEnv "LD_LIBRARY_PATH"
    unsetEnv "LD_PRELOAD"
    unsetEnv "LD_AUDIT"
    setEnv "TIDEPOOL_COMPILER_PACKAGE_PROFILE" rootedProfilePath
    setEnv "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256" rootedProfileSha256
    setEnv "TIDEPOOL_COMPILER_SERVICE_EPOCH" (replicate 64 '0')
    setEnv "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID" (replicate 64 '0')
    setEnv "TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID" rootedProfileIdentity
    setEnv "TIDEPOOL_COMPILER_WORKER_INSTANCE" (replicate 64 '0')
    setEnv "TIDEPOOL_COMPILER_WORLD_DIRECTORY" worldDirectory
    action
  where
    names =
      [ "TIDEPOOL_COMPILER_PACKAGE_PROFILE"
      , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256"
      , "TIDEPOOL_COMPILER_SERVICE_EPOCH"
      , "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID"
      , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID"
      , "TIDEPOOL_COMPILER_WORKER_INSTANCE"
      , "TIDEPOOL_COMPILER_WORLD_DIRECTORY"
      , "LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT"
      ]

-- Rooted boot acknowledges its association on stdout. Keep that binary frame
-- out of the test runner's protocol while the real association is exercised.
withQuietStdout :: IO a -> IO a
withQuietStdout action = mask $ \restore -> do
  hFlush stdout
  temporary <- getTemporaryDirectory
  (path,output) <- openTempFile temporary "rooted-boot-ack"
  let cleanup = hClose output `finally` removeFile path
  bracket (hDuplicate stdout) hClose $ \saved -> do
    (do hDuplicateTo output stdout
        restore action `finally` (hFlush stdout >> hDuplicateTo saved stdout))
      `finally` cleanup


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
        && thrown == Left ThreadKilled)
      (fail "validation timing changed a validation result or swallowed its exception")
    forM_ [Right (),Left "controlled refusal"] $ \expected -> do
      (observed,_) <- captureDiagnostics
        (withValidationTiming True "exact_scope" "checked_receipt_publication"
          (ioError (userError "optional count observation failed")) (pure expected))
      unless (observed == expected)
        (fail "optional count observation failure changed the validation result")
    (cancelledCounts,_) <- captureDiagnostics
      (try (withValidationTiming True "exact_scope" "checked_receipt_publication"
        (throwIO ThreadKilled) (pure (Right ()))) :: IO (Either AsyncException (Either String ())))
    unless (cancelledCounts == Left ThreadKilled)
      (fail "optional count observation swallowed cancellation after successful validation")
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

-- Feed actual owner-emitted diagnostics into the declared consumer. This
-- fixture deliberately executes two cycles in one physical request history;
-- parser controls derive from those events rather than copied wire examples.
worldRejectionDiagnosticBoundaries :: IO ()
worldRejectionDiagnosticBoundaries = do
  let first = "first actual GHC error: could not load selected module\n"
      detail = first ++ concat (replicate 20000 "界😀\n\"\\") ++ "UNRETAINED_FINAL_SENTINEL"
  (_,diagnostics) <- captureDiagnostics
    (emitWorldAcquisitionRejection "world cell demand" "<sealed scaffold>" detail)
  unless (firstLine `isInfixOf` diagnostics
      && "[diagnostic truncated]" `isInfixOf` diagnostics
      && not ("UNRETAINED_FINAL_SENTINEL" `isInfixOf` diagnostics)
      && not ('\xfffd' `elem` diagnostics)
      && BS.length (TE.encodeUtf8 (T.pack diagnostics)) < 25 * 1024)
    (fail "bounded World diagnostics lost their first error, truncation marker or UTF-8 boundary")
  where firstLine = "first actual GHC error: could not load selected module"

compilerTimingRoundtrip :: IO ()
compilerTimingRoundtrip = withTiming $ withScratch $ \work -> do
  forM_ ["CanonicalSource.hs","CanonicalDependency.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  grant <- either fail pure (compilerExecutionGrant 2)
  (_,diagnostics) <- captureDiagnostics $ (withCompilerExecutor (const (pure ())) grant >=> finishCompletion) $ \executor -> do
    let finishFallback owner prepared = do
          cache <- newPreparedBodyCache
          context <- prepareCompilerProjectionContext prepared Map.empty
            (mkModule (stringToUnit "main") (mkModuleName owner)) "__original_projection" [] Nothing
          void $ prepareOriginalProductsWithCacheAndReuse (Just (pprReuseContext prepared))
            cache Nothing executor (prHscEnv (pprPipelineResult prepared)) Nothing
            (pprProductInterfaces prepared) context Set.empty (pprModules prepared)
    plain <- runPipelineSessionSelected
      (WithCompilerExecution grant executor (PreparedProducts Nothing)) Set.empty CertifyHomeProductsCompile
      Nothing (work </> "CanonicalSource.hs") [work] Nothing
    finishFallback "CanonicalSource" plain
    effects <- lookupEnv "TIDEPOOL_TEST_EFFECTS_DIR" >>= maybe (fail "missing declared effects") pure
    prelude <- lookupEnv "TIDEPOOL_PRELUDE_DIR" >>= maybe (fail "missing declared prelude") pure
    template <- readFile "test-source-boot/fixtures/TelemetryTypedTemplate.hs"
    let body = unlines
          ["let (telemetryValue,telemetryOther) = ((42 :: Int),(43 :: Int))"
          ,"_ <- pure ()"
          ,"let afterDiscard = telemetryValue + telemetryOther"]
    ordered <- analyzeOrderedCellWithFlags (hsc_dflags (prHscEnv (pprPipelineResult plain)))
      template body >>= either (fail . show) pure
    source <- either fail pure (prepareTypedSegmentSource template ordered (digest (BS8.pack body))
      [(0,1,Nothing),(1,2,Nothing),(2,3,Nothing)])
    completedBatch <- newIORef Nothing
    let path = work </> "TelemetryTyped.hs"
        plan = preparedTypedSegmentPlan source
        prepare environment admissions segment = do
          batch <- prepareTypedSegmentSessionBindings environment admissions segment (work </> "typed-stage")
          writeIORef completedBatch (Just batch)
          pure (typedSegmentSessionEnvironment batch, typedSegmentSessionGlobals batch,
            typedSegmentSessionInterfaces batch)
    writeFile path (preparedTypedSegmentSource source)
    typed <- runPipelineSessionSelected
      (WithCompilerExecution grant executor
        (WithTypedSegmentPreparation prepare (PreparedSegmentProducts plan Nothing))) Set.empty
      (TypedSegmentCompile plan (preparedTypedSegmentOperations source) GeneralCompile)
      Nothing path [work,effects,prelude] Nothing
    batch <- readIORef completedBatch >>= maybe (fail "typed producer omitted its actual capture batch") pure
    actualTarget <- case [value | value <- pprModules (preparedSegmentProducts typed)
        , moduleNameString (moduleName (pmModule value)) == "TelemetryTyped"] of
      [value] -> pure value
      _ -> fail "typed capture finalization lacks the unique actual target"
    resolved <- either throwIO pure (resolveTypedSegmentBindings (pmIdentityResolver actualTarget) batch)
    unless (map resolvedTypedItemOrdinal resolved == [0,1,2]
        && map (map bbName . resolvedTypedItemBinders) resolved
          == [["telemetryValue","telemetryOther"],[],["afterDiscard"]])
      (fail "typed capture finalization lost pattern/discard/execution-item correspondence")
    forM_ resolved $ \item -> case (resolvedTypedItemBinders item,resolvedTypedItemInterface item) of
      ([],Nothing) -> pure ()
      (binders,Just interface) -> do
        let (owner,_) = capturedSessionInterface interface
        unless (all ((== moduleNameString (moduleName owner)) . bbModule) binders
            && case capturedSessionInterfaceEvidence interface of Just _ -> True; Nothing -> False)
          (fail "typed capture finalization crossed its actual issued interface")
      _ -> fail "typed capture finalization split capture presence from its issued interface"
    finishFallback "TelemetryTyped" (preparedSegmentProducts typed)
  let logPath = work </> "producer.log"
  writeFile logPath diagnostics
  python <- lookupEnv "TIDEPOOL_TEST_PYTHON" >>= maybe (fail "missing declared Python") pure
  reporter <- lookupEnv "TIDEPOOL_TEST_REUSE_REPORT" >>= maybe (fail "missing declared reuse reporter") pure
  checks <- lookupEnv "TIDEPOOL_TEST_REUSE_REPORT_TESTS" >>= maybe (fail "missing declared reuse checks") pure
  callProcess python [checks,"--producer-log",logPath,"--reporter",reporter]

compilerTaskAttemptHistories :: IO ()
compilerTaskAttemptHistories = do
  grant <- either fail pure (compilerExecutionGrant 3)
  result <- quickCheckWithResult stdArgs {maxSuccess=100,maxSize=8,replay=Just (mkQCGen 710117,0)} $
    forAllShrink (resize 8 (listOf (chooseInt (0,5)))) shrink $ \batches -> ioProperty $ do
      (withCompilerExecutor (const (pure ())) grant >=> finishCompletion) $ \executor -> do
        attempts <- fmap concat $ forM batches $ \count ->
          runCompilerTasksWithAttempt executor
            (\(TaskAttempt issuer ordinal) input -> pure (issuer,ordinal,input))
            (\_ _ -> pure ()) (replicate count "same-owned-input")
        later <- (withCompilerExecutor (const (pure ())) grant >=> finishCompletion) $ \other -> runCompilerTasksWithAttempt other
          (\(TaskAttempt issuer ordinal) _ -> pure (issuer,ordinal))
          (\_ _ -> pure ()) [()]
        issued <- runCompilerTasksWithAttempt executor
          (\_ _ -> newTimingRequestIdentity) (\_ _ -> pure ()) (replicate 30 ())
        let ordinals = [ordinal | (_,ordinal,_) <- attempts]
            issuers = nub [issuer | (issuer,_,_) <- attempts]
            otherIssuer = fst (head later)
        pure $ counterexample (show (batches,attempts,later)) $
          length (nub issued) == length issued
          && ordinals == [0 .. sum batches - 1]
          && length issuers <= 1
          && otherIssuer `notElem` issuers
          && all (\(_,_,input) -> input == "same-owned-input") attempts
  unless (isSuccess result) (fail "compiler task attempt histories reused an execution identity")

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
    TList [_,_,_,_,_,TList [TList (TList [_,TString path]:_),_],_,_] -> pure (T.unpack path)
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

-- These enter the production pre-GHC admission owner with a deliberately
-- unreadable profile. Partial boot metadata must refuse before any profile or
-- compiler input is opened; it cannot silently downgrade to observation.
compilerServiceBootBoundaries :: IO ()
compilerServiceBootBoundaries = bracket
  (traverse (\name -> (name,) <$> lookupEnv name) names)
  (mapM_ (\(name,value) -> maybe (unsetEnv name) (setEnv name) value)) $ \_ -> do
    mapM_ unsetEnv names
    void readCompilerPackageBoot
    requireObservationPackageEnvironment
    forM_ [selected | name <- names, selected <- [[name],filter (/= name) names]] $ \selected -> do
      mapM_ unsetEnv names
      forM_ selected $ \name -> setEnv name (if name == head names then "/unavailable/compiler-profile" else replicate 64 '0')
      requireUserError "partial compiler service boot"
        "compiler service boot fields must be supplied together" readCompilerPackageBoot
      requireUserError "rooted metadata cannot become an observation"
        "package profile observation cannot inherit compiler service boot fields"
        requireObservationPackageEnvironment
    mapM_ unsetEnv names
    -- Observe the real producer, with adversarial raw UnitInfo metadata. This
    -- proves the qualifier receives the consumed spelling; it does not issue
    -- a rooted service profile from this test-only metadata alteration.
    withScratch $ \work -> do
      createDirectoryIfMissing True (work </> "mutable" </> "subdir")
      createDirectory (work </> "modules")
      createSymbolicLink (work </> "mutable" </> "subdir") (work </> "alias")
      createSymbolicLink (work </> "modules") (work </> "mutable" </> "modules")
      let raw = work </> "alias" </> ".." </> "modules"
      target <- canonicalizePath raw
      unless (raw /= target) (fail "package profile alias control did not change spelling")
      libdir <- lookupEnv "TIDEPOOL_GHC_LIBDIR" >>= maybe (fail "missing declared GHC libdir") pure
      withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \scope ->
        scopedRunGhc scope $ do
          env <- getSession
          let replace unit = unit
                { unitImportDirs=[PackageText.pack raw],unitLibraryDirs=[PackageText.pack raw]
                , unitLibraryDynDirs=[PackageText.pack raw],unitIncludeDirs=[PackageText.pack raw] }
              units = (hsc_units env) {unitInfoMap=mapUniqMap replace (unitInfoMap (hsc_units env))}
              observed = env {hsc_unit_env=ue_setUnits units (hsc_unit_env env)}
          bytes <- liftIO (captureSupportedPackageProfileWith libdir observed)
          liftIO $ unless (BS.take 8 bytes == "TPGHC002" && BS8.pack raw `BS.isInfixOf` bytes
            && not (BS8.pack target `BS.isInfixOf` bytes))
            (fail "supported package profile erased the raw GHC path before qualification")
  where
    names =
      [ "TIDEPOOL_COMPILER_PACKAGE_PROFILE"
      , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256"
      , "TIDEPOOL_COMPILER_SERVICE_EPOCH"
      , "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID"
      , "TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID"
      , "TIDEPOOL_COMPILER_WORKER_INSTANCE"
      ]

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
        (PublishCertificate publicationMarker (BS.pack [9,8,7]))
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
  beforeWriteFailure <- receipts
  bracket (createDirectory publicationMarker) (const (removeDirectory publicationMarker)) $ \_ -> do
    writeFailed <- try publishWithOutput :: IO (Either IOException ())
    afterWriteFailure <- receipts
    unless (case writeFailed of Left _ -> afterWriteFailure == beforeWriteFailure; Right _ -> False)
      (fail "failed certificate write issued a compilation receipt")
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
    continued values originDrift aliasDrift = withScratch $ \directory ->
      withSealedInputBytes values $ \endpoint -> do
        let bytes = values
            historical = directory </> "previous-offer"
            oldManifest = directory </> "previous-manifest"
            original = directory </> "protected-origin"
            receiving = directory </> "receiving-alias"
            changed = bytes <> BS.singleton 123
            reference path = OriginalInputReference path (digest bytes) (BS.length bytes) [original]
              (either error id (ownedArenaRange endpoint (toInteger (BS.length bytes)) 0))
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
        rotatedInput <- either (fail . show) pure cold
        rotatedBytes <- capturedRequestInput rotatedInput receiving (digest bytes)
        rotatedProof <- revalidateRequestInputs rotatedInput
        unless (rotatedBytes == bytes
            && either (const False) (const True) rotatedProof == not (originDrift || aliasDrift))
          (fail "rotation changed owned bytes or ignored the separate current-path proof")
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
requestInputBoundaries = withScratch $ \directory ->
  withSealedInputBytes (BS.pack [1,2,3]) $ \endpoint -> do
    let path = directory </> "original"
        other = directory </> "other"
        bytes = BS.pack [1,2,3]
    BS.writeFile path bytes
    BS.writeFile other bytes
    let reference destination = OriginalInputReference destination (digest bytes) 3 []
          (either error id (ownedArenaRange endpoint 3 0))
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
    removeFile path
    removeFile copy
    let purpose = directory </> "protected-purpose-interface"
    relocated <- either fail pure (aliasRequestInputs [(purpose,path,digest bytes)] owner)
    (body,continued) <- withCapturedRequestInputTokens relocated (\reader -> reader purpose 3)
    originalBody <- capturedRequestInputToken owner path (digest bytes)
    unless (BSI.toForeignPtr (artifactBytes body) == BSI.toForeignPtr (artifactBytes originalBody)
        && requestInputBytes continued == requestInputBytes owner)
      (fail "new purpose path reopened a deleted source or copied its captured payload")
    BS.writeFile other (BS.singleton 9)
    (_,conflicting) <- captureRequestInputs (Just owner) (\reader -> reader other 3)
    unless (case aliasRequestInputs [(other,path,digest bytes)] conflicting of Left _ -> True; _ -> False)
      (fail "purpose alias replaced another captured path")
    BS.writeFile path bytes
    BS.writeFile copy bytes
    BS.writeFile other bytes
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
  -- TYPE custody and output ownership have distinct identities: moving an
  -- unchanged type interface preserves its issued descriptor, while an output
  -- path still refuses a changed body above. Neither comparison grants native
  -- implementation authority.
  let body = captureArtifactBytes firstBytes
      artifact = ExactIfaceArtifact "main" "CapturedType" firstPath (artifactSha256 body)
        [("main","RequiredType")]
      previous = Map.singleton ("main","CapturedType") (artifact,body)
      relocated = artifact {exactPath=secondPath}
      additions value = Map.singleton ("main","CapturedType") value
      requireConflict value = case retainTypeInterfaceCaptures previous (additions value) of
        Left _ -> pure ()
        Right _ -> fail "retained TYPE custody accepted a changed owner, body, seal or requirement"
  kept <- either fail pure (retainTypeInterfaceCaptures previous (additions (relocated,body)))
  unless (kept == previous) (fail "TYPE relocation replaced the already-issued descriptor")
  let different = captureArtifactBytes conflictingBytes
  requireConflict (relocated {exactSha256=artifactSha256 different},different)
  requireConflict (relocated {exactSha256=artifactSha256 different},body)
  requireConflict (relocated {exactRequirements=[]},body)
  requireConflict (relocated {exactModule="ForeignType"},body)
  fresh <- either fail pure (retainTypeInterfaceCaptures Map.empty (additions (artifact,body)))
  unless (fresh == previous) (fail "fresh TYPE custody lost its actual issued capture")
