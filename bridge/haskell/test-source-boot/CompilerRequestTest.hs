{-# LANGUAGE ScopedTypeVariables #-}
module CompilerRequestTest (workerRequestAdmissionBeforeEffects, nativeReceiptIdentityChecks, nativePackageOutputChecks, packageTargetRootDriftRefusal, originalUseClosureChecks, nativeOutputProvenanceChecks, completedRequestHistories, completedRequestCancellation, completedRequestRelease, completedRequestOwnerBoundaries, completedRequestScopeRelease, checkedRequestOwnerBoundaries, compilerScopeCleanupFailure, checkedPublicModuleHistories, worldAcquisitionDiagnostics, worldRequestSlotBoundaries, worldRequestBoundaries, worldMaterializationBoundaries) where

import Tidepool.RequestCompletion
  ( finishCompletion, captureCompletion, completionResult, completionFailures
  , BodyOutcome(..), CleanupStage(..), cleanupStage, cleanupException, withCompletionRelease )

import Control.Exception (AsyncException(ThreadKilled), Exception, SomeException, SomeAsyncException, IOException, fromException, throw, throwIO, try)
import Control.Concurrent (forkFinally, killThread, newEmptyMVar, putMVar, readMVar)
import Data.IORef (newIORef, readIORef, writeIORef, modifyIORef')
import Data.List (isInfixOf, sort)
import Data.Bits ((.|.), shiftL, shiftR)
import qualified Data.ByteString.Char8 as BSC
import System.Environment (getEnvironment, lookupEnv)
import System.Exit (ExitCode(..))
import System.Process (CreateProcess(..), StdStream(..), proc, readCreateProcessWithExitCode, waitForProcess, withCreateProcess)
import System.Posix.Files (createNamedPipe, ownerReadMode, ownerWriteMode, unionFileModes)
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import System.Timeout (timeout)
import System.IO (stderr, hClose, hFlush, hSetBinaryMode)
import Control.Monad ((>=>), forM, forM_, unless, void, when)
import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Write (toStrictByteString)
import Codec.CBOR.Term (Term(..), decodeTerm)
import qualified Data.ByteString.Lazy as BL
import Control.Monad.IO.Class (liftIO)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Data.ByteString as BS
import GHC (findModule, getModuleInfo, modInfoExports, mkModuleName, getSessionDynFlags)
import GHC.Driver.Backend (backendGeneratesCode)
import GHC.Driver.Session (backend, gopt, GeneralFlag(Opt_ByteCode), parseDynamicFilePragma)
import GHC.Types.SrcLoc (noLoc)
import GHC.Types.Name (getOccString)
import Tidepool.WorkerDiagnostics (sourceFailureDiagnostics)
import Tidepool.Binders (BoundBinder(..))
import Tidepool.Introspection (withInspectionWork, InspectionResult(..))
import GHC.Tc.Types (tcg_mod)
import GHC.Unit.Module (moduleName, moduleNameString)
import SourceBootFixtureSupport (hasIntResultLiteral, withScratch, captureDiagnostics
  , withRootedCompilerEnvironment, withQuietStdout)
import System.Directory (copyFile, createDirectory, doesDirectoryExist, doesFileExist, getFileSize, getPermissions, setPermissions, listDirectory, removeFile, Permissions(..))
import System.FilePath ((</>), takeFileName, takeDirectory)
import Test.QuickCheck
import Tidepool.CompilerProducts
  ( prepareCompilerProjectionContext, prepareOriginalProducts, newPreparedOriginalInterfaceArtifacts
  , retainPreparedPackageTargets, writeCertifiedSegmentProducts, publishStagedOriginalProducts
  , writeCertifiedSegmentItemProductsWithInputs )
import Tidepool.CompilerRequest
import Tidepool.ExtractRequest
import Tidepool.GhcPipeline
import Tidepool.ExecutionProjection (packageTargetGroups, projectedPackageModule, projectedPackageInterfaceBody)
import qualified Tidepool.ExecutionSchema as Wire
import Tidepool.ExecutionSchema (WireProgram(..), SymbolIdentity(..), Group(..), TopBinding(..))
import Tidepool.DependencyEvidence (DependencyEvidence(..), DependencyModule(..))
import Tidepool.PackageWitness (requireObservationPackageEnvironment)
import Tidepool.ExactScope (decodeWorldSources)
import Tidepool.ExactHydration (CheckedTemplateImports(..), generatedCheckingTemplateRecipe)
import Tidepool.DiagJson (InputRejection(..))
import Tidepool.CertifiedProducts
  ( captureSelectedPackageWitnesses, encodeReceiptIdentity
  , validatePackageTargetRoots, validateOriginalUseClosure, OriginalUseClosureFailure(..), retainedInputClosureFromWitnesses, CertifiedGroupFor(..), mergeCertifiedGroupObservations
  , WitnessOwnerFor(..) )
import Tidepool.ArtifactBytes (artifactSha256, artifactBytes, captureArtifactBytes)
import qualified Crypto.Hash.SHA256 as SHA256

-- The Rust receipt consumer expects null/text, independently of the execution
-- schema's tagged optional parent. The joined World test exercises the actual
-- native-demand exchange; this control covers both receipt parent forms.
nativeReceiptIdentityChecks :: IO ()
nativeReceiptIdentityChecks = forM_ [Nothing, Just (Text.pack "Record")] $ \parent -> do
  let symbol = SymbolIdentity (Text.pack "unit") (Text.pack "Module")
        (Text.pack "value") (Text.pack "field") parent
      expected = TList [TString (Text.pack "unit"), TString (Text.pack "Module")
        , TString (Text.pack "value"), TString (Text.pack "field")
        , maybe TNull TString parent]
  case deserialiseFromBytes decodeTerm (BL.fromStrict (toStrictByteString (encodeReceiptIdentity symbol))) of
    Right (remaining, actual) | BL.null remaining && actual == expected -> pure ()
    other -> fail ("compiler receipt symbol grammar changed: " ++ show other)

packageTargetRootDriftRefusal :: IO ()
packageTargetRootDriftRefusal = do
  let expected = Map.fromList [(Text.pack "item", [0, 1])]
      mutated = Map.fromList [(Text.pack "item", [0, 2])]
  case validatePackageTargetRoots expected expected of
    Right () -> pure ()
    Left reason -> fail ("unchanged target roots refused: " ++ reason)
  case validatePackageTargetRoots expected mutated of
    Left reason
      | "target package roots changed" `isInfixOf` reason -> pure ()
    other -> fail ("target root mutation was not refused: " ++ show other)

-- Symbolic references test the pure observation graph; they cannot construct a
-- production World reference, certification context, or executable target.
originalUseClosureChecks :: IO ()
originalUseClosureChecks = do
  let symbol name = SymbolIdentity (Text.pack "main") (Text.pack "Observed")
        (Text.pack "value") (Text.pack name) Nothing
      root = symbol "root"
      middle = symbol "middle"
      auxiliary = symbol "auxiliary"
      original = symbol "original"
      alias = symbol "alias"
      zero = symbol "zeroPackageLeaves"
      coordinate ordinal = (Text.pack "main",Text.pack "Observed",Nothing,ordinal)
      witness identity owner = (identity,Wire.LiftedRefRep,Nothing,False,owner)
      source ordinal = SourceOwner (Text.pack "main") (Text.pack "Observed") Nothing ordinal
      binders = Map.fromList [(root,coordinate 0),(middle,coordinate 1),(auxiliary,coordinate 2)]
      groups = Map.fromList
        [(coordinate 0,CertifiedGroup [root] [witness middle (source 1)])
        ,(coordinate 1,CertifiedGroup [middle] [witness root (source 0),witness original (OriginalBindingOwner (1 :: Int))
          ,witness alias (OriginalBindingOwner 1)])
        ,(coordinate 2,CertifiedGroup [auxiliary] [witness zero (OriginalBindingOwner 2)])]
      globals = [witness root (source 0)]
      locals = Set.singleton auxiliary
      expected = Set.fromList [(1,original),(1,alias),(2,zero)]
      check = validateOriginalUseClosure binders groups locals globals
      require label value = unless value (fail label)
  require "cycle/alias/auxiliary HOME/zero-leaf closure refused" (check expected == Right ())
  require "missing Original use accepted" (check (Set.delete (1,alias) expected)
    == Left (OriginalUseKeysDiffer 1 0 [(1,alias)] []))
  require "extra Original use accepted" (check (Set.insert (3,original) expected)
    == Left (OriginalUseKeysDiffer 0 1 [] [(3,original)]))
  require "crossed Original reference accepted" (check (Set.insert (3,zero) (Set.delete (2,zero) expected))
    == Left (OriginalUseKeysDiffer 1 1 [(2,zero)] [(3,zero)]))
  require "zero-package-leaf Original use was dropped" (check (Set.delete (2,zero) expected)
    == Left (OriginalUseKeysDiffer 1 0 [(2,zero)] []))
  require "missing independent local binding accepted"
    (validateOriginalUseClosure Map.empty groups locals globals expected
      == Left (OriginalUseMissingLocalBinding auxiliary))
  require "missing transitive HOME group accepted"
    (validateOriginalUseClosure binders (Map.delete (coordinate 1) groups) locals globals expected
      == Left (OriginalUseMissingGroup (coordinate 1)))
  require "source edge accepted another binder's exact ordinal"
    (validateOriginalUseClosure binders groups Set.empty [witness root (source 1)] Set.empty
      == Left (OriginalUseSourceOwnerMismatch root (coordinate 1)))
  let foreignCoordinate = (Text.pack "main",Text.pack "Observed",Just (Text.pack "foreign-version"),0)
  require "source edge accepted a foreign version"
    (validateOriginalUseClosure binders groups Set.empty
      [witness root (SourceOwner (Text.pack "main") (Text.pack "Observed") (Just (Text.pack "foreign-version")) 0)] Set.empty
      == Left (OriginalUseMissingGroup foreignCoordinate))
  let retained = symbol "retainedInput"
      appendGlobal global group = group {certifiedGroupGlobals=certifiedGroupGlobals group ++ [global]}
      captureGroups = Map.adjust (appendGlobal (witness retained (RetainedOwner 41))) (coordinate 1)
        (Map.adjust (appendGlobal (witness retained (RetainedOwner 42))) (coordinate 2) groups)
  require "native closure lost exact retained generations through a cycle"
    (retainedInputClosureFromWitnesses captureGroups
      (globals ++ [witness auxiliary (source 2)]) == Right (Set.fromList [(retained,41),(retained,42)]))
  require "retained input traversal accepted a foreign source coordinate"
    (case retainedInputClosureFromWitnesses captureGroups [witness root (source 1)] of
      Left _ -> True; Right _ -> False)
  let historicalSource = SourceOwner (Text.pack "main") (Text.pack "Observed") (Just (Text.pack "foreign-version")) 0
      historicalGroups = Map.insert foreignCoordinate
        (CertifiedGroup [root] [witness root historicalSource,witness zero (OriginalBindingOwner 9)]) groups
  require "recorded historical edge was rematched to current symbol selection"
    (validateOriginalUseClosure binders historicalGroups Set.empty [witness root historicalSource]
      (Set.singleton (9,zero)) == Right ())
  require "identical exact group observations conflicted"
    (mergeCertifiedGroupObservations groups groups == Right groups)
  forM_ [CertifiedGroup [middle] [witness middle (source 1)],
         CertifiedGroup [root] [witness middle (OriginalBindingOwner 7)],
         CertifiedGroup [root] [(middle,Wire.LiftedRefRep,Nothing,True,source 1)]] $ \changed ->
    require "conflicting exact group membership or full dependency observation was overwritten"
      (case mergeCertifiedGroupObservations groups (Map.singleton (coordinate 0) changed) of
        Left _ -> True; Right _ -> False)
  require "distinct historical coordinates were collapsed by symbol identity"
    (mergeCertifiedGroupObservations groups (Map.singleton foreignCoordinate (historicalGroups Map.! foreignCoordinate))
      == Right historicalGroups)
  -- Fixed-point set expansion is an independent oracle for all root subsets,
  -- including empty selection. Root order and repeated references cannot alter it.
  forM_ [Set.fromList [i | i <- [0..2], mask `div` (2^i) `mod` 2 == (1 :: Int)]
        | mask <- [0..7 :: Int]] $ \seeds -> do
    let edges = Map.fromList [(0,Set.singleton 1),(1,Set.singleton 0),(2,Set.empty)]
        expand reached = Set.union reached (Set.unions
          [Map.findWithDefault Set.empty i edges | i <- Set.toList reached])
        closure = until (\reached -> expand reached == reached) expand seeds
        oracle = Set.unions
          [if i == 1 then Set.fromList [(1,original),(1,alias)]
           else if i == 2 then Set.singleton (2,zero) else Set.empty
          | i <- Set.toList closure]
        observed = [witness ([root,middle,auxiliary] !! i) (source (fromIntegral i)) | i <- Set.toList seeds]
    require "Original-use reachability differs from independent fixed point"
      (validateOriginalUseClosure binders groups Set.empty (observed ++ reverse observed) oracle == Right ())

worldRequestSlotBoundaries :: IO ()
worldRequestSlotBoundaries = do
  let recipe = WorldCellRecipe "(this must not parse" "module CellCheck where" []
      cellPrepare = WorldCellPrepareRequest (replicate 64 'a') (replicate 64 '1' ++ ".7")
        recipe Nothing "untrusted-receipt" (BS.replicate 32 1) 17
        [] "/unavailable/cell-operation" "/unavailable/cell-prepared.cbor"
  let decodeSlots slots = workerRequestFromArgv (workerArgv
        [WorldCellPrepare cellPrepare {worldCellPrepareSlots=slots}])
  forM_ [[(0,PlannedDeclaration 1),(1,PlannedBind 1)]
    ,[(0,PlannedBind 1),(1,PlannedDeclaration 1)]
    ,[(0,PlannedExpression 1 "observation"),(1,PlannedPrologue 1)]] $ \slots ->
      case decodeSlots slots of
        Right (Just _) -> pure ()
        other -> fail ("held request conflated Lib1 and Val1 reservations: " ++ show other)
  forM_ [[(0,PlannedBind 1),(1,PlannedExpression 1 "observation")]
    ,[(0,PlannedPrologue 1),(1,PlannedDeclaration 1)]] $ \slots ->
      case decodeSlots slots of
        Left _ -> pure ()
        other -> fail ("held request admitted duplicate module reservations: " ++ show other)

worldRequestBoundaries :: IO ()
worldRequestBoundaries = do
  requireObservationPackageEnvironment
  let root = replicate 64 'a'
      foreignReference = replicate 64 '1' ++ ".7"
      resident = ExactInputsRequest root (ResidentExactInputs foreignReference)
      complete = ExactInputsRequest root (CompleteExactInputs "/unavailable/world-inputs.cbor")
      question = WorldQueryRequest root foreignReference (WorldPublicModuleQuery (WorldPublicInfo
        (StructuredInspection (StructuredPublicModule "Data.List") StructuredValueName "map"
          (InspectionProvenance 1 "display-only")))) "/unavailable/world-answer.bin"
      body = BS.pack [136,1,88,32] <> BS.replicate 32 170 <> BS.pack [0,105] <> "Data.List"
        <> BS.pack [132,1,99] <> "map" <> BS.pack [1,108] <> "display-only"
        <> BS.pack [130,104] <> "rejected" <> BS.pack [1,246,64]
      expected = "TPWQ0002" <> BS.pack [80,0,0,0] <> body
  args <- case workerRequestFromArgv (workerArgv [WorldQuery question]) of
    Right (Just request) -> pure request
    other -> fail ("invalid world query fixture: " ++ show other)
  (lateEnsure,lateQuery) <- withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \scope -> do
    forM_ [resident,complete] $ \request -> do
      outcome <- scopedEnsureWorld scope request
      unless (outcome == ExactInputsRejected UnsupportedOperation)
        (fail "observation boot acquired a world or read complete inputs")
    (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \request -> do
      actual <- runBoundWorldQuery request
      unless (actual == expected) (fail "missing scope hold lost its typed unavailable response")
      generic <- try (void (extractBoundDeclarationBinders request "/unavailable/NoTurn.hs" "NoTurn")) :: IO (Either SomeException ())
      case generic of
        Left failure | Just CheckedPurposeMismatch <- fromException failure -> pure ()
        Left failure -> throwIO failure
        Right _ -> fail "held-world request entered the generic GHC operation path"
      pure (void (scopedEnsureWorld scope resident),void (runBoundWorldQuery request))
  requirePhase CompilerTransactionReleased lateEnsure
  try lateQuery >>= requireCancellation
  let recipe = WorldCellRecipe "(this must not parse" "module CellCheck where" []
      cellPlan = WorldCellPlanRequest root foreignReference recipe Nothing "/unavailable/cell-plan.cbor"
      cellPrepare = WorldCellPrepareRequest root foreignReference recipe Nothing "untrusted-receipt" (BS.replicate 32 1) 17
        [] "/unavailable/cell-operation" "/unavailable/cell-prepared.cbor"
      decode fields = case workerRequestFromArgv (workerArgv fields) of
        Right (Just request) -> pure request
        other -> fail ("invalid held cell fixture: " ++ show other)
  planArgs <- decode [WorldCellPlan cellPlan]
  prepareArgs <- decode [WorldCellPrepare cellPrepare]
  (latePlan,latePrepare) <- withResidentCompilerScopes [] $ \runScope -> runScope (pure ()) $ \scope -> do
    latePlan <- (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest planArgs) >=> finishCompletion) $ \request -> do
      absent <- try (runBoundWorldCellPlan request) :: IO (Either WorldCellRefusal BS.ByteString)
      unless (either (== WorldCellUnavailable) (const False) absent)
        (fail "unheld plan parsed source or obtained a compiler world")
      pure (void (runBoundWorldCellPlan request))
    latePrepare <- (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest prepareArgs) >=> finishCompletion) $ \request -> do
      absent <- boundWorldCellPreparation request
      unless (either (== WorldCellUnavailable) (const False) absent)
        (fail "unheld prepare parsed source or allocated compiler generation state")
      compiled <- try (compileCheckedRequest request CheckedCompilationEnvironment Set.empty GeneralCompile Nothing "/unavailable/target.hs")
        :: IO (Either InputRejection CheckedCompilation)
      case compiled of
        Left CheckedPurposeMismatch -> pure ()
        Left failure -> throwIO failure
        Right _ -> fail "held World request entered the generic compiler path"
      withScratch $ \work -> do
        let path = work </> "PurposeConflict.hs"
            source = "module PurposeConflict where\nvalue = ()\n"
        writeFile path source
        recipe <- generatedCheckingTemplateRecipe (scopedParserFlags scope)
          (CheckedTemplateImports [] []) source source path "PurposeConflict" >>= either fail pure
        conflict <- try (compileCheckedRequest request CheckedCompilationEnvironment Set.empty
          (GeneratedScaffoldCompile recipe (GeneratedScaffoldCompile recipe GeneralCompile))
          Nothing "/unavailable/target.hs")
          :: IO (Either CompilePurposeConflict CheckedCompilation)
        unless (either (== DuplicateCompilePurposeSetting "GeneratedScaffoldCompile") (const False) conflict)
          (fail "conflicting compile purpose was not refused before held-world work")
      pure (void (boundWorldCellPreparation request))
    pure (latePlan,latePrepare)
  try latePlan >>= requireCancellation
  try latePrepare >>= requireCancellation

-- Exercise the production human diagnostic emitter without manufacturing a
-- rooted package/world. Actual Complete dispatch still requires the HOST probe.
worldAcquisitionDiagnostics :: IO ()
worldAcquisitionDiagnostics = do
  let reason = "read/decode WorldInputsCorrupt"
      manifest = "/captured/\x03bb/world.cbor"
      detail = "fixture decode failure: \x03bb"
      emittedBytes = BS.length . TextEncoding.encodeUtf8 . Text.pack
  (_,ordinary) <- captureDiagnostics (emitWorldAcquisitionRejection reason manifest detail)
  unless (all (`isInfixOf` ordinary) [reason,manifest,detail])
    (fail "world acquisition diagnostic lost its typed read cause/path/detail")
  -- A valid UTF-8 stream may cross the existing capture byte boundary.
  -- Decode its complete prefix, without treating a split final code point as
  -- malformed data or replacing it with a different character.
  let captureLimit = 4 * 1024 * 1024
      truncatedMarker = "\n[compiler diagnostics truncated at 4 MiB]\n"
  (_,truncated) <- captureDiagnostics $ BS.hPut stderr
    (BS.replicate (captureLimit - 1) 97 <> TextEncoding.encodeUtf8 (Text.pack "\x03bb"))
  unless (truncated == replicate (captureLimit - 1) 'a' ++ truncatedMarker)
    (fail "bounded compiler diagnostics rejected or changed a valid UTF-8 prefix")
  limited <- timeout 5000000 $ captureDiagnostics $
    emitWorldAcquisitionRejection reason (repeat '\x1f30a') (repeat '\n')
  case limited of
    Nothing -> fail "world acquisition diagnostic forced an unbounded lazy tail"
    Just (_,output) -> do
      unless (reason `isInfixOf` output && '\x1f30a' `elem` output
          && "[diagnostic truncated]" `isInfixOf` output)
        (fail "world acquisition diagnostic lost its bounded UTF-8 prefix or truncation marker")
      unless (emittedBytes output < 74 * 1024)
        (fail "world acquisition diagnostic exceeded its escaped UTF-8 allowance")
  (_,escaped) <- captureDiagnostics $
    emitWorldAcquisitionRejection reason (replicate 4096 '\0') (replicate 4096 '\n')
  unless (emittedBytes escaped < 74 * 1024 && "\\u0000" `isInfixOf` escaped)
    (fail "world acquisition diagnostic JSON expansion exceeded its bounded allowance")
  -- A lazy formatting IOException reaches the actual emitter's exception
  -- boundary. This is not a physical stderr/capture failure simulation.
  (refusal,_) <- captureDiagnostics $ do
    emitWorldAcquisitionRejection reason manifest
      (throw (userError "injected world diagnostic formatting failure"))
    pure (ExactInputsRejected AcquisitionRejected)
  unless (refusal == ExactInputsRejected AcquisitionRejected)
    (fail "diagnostic failure replaced the original acquisition rejection")
  cancelled <- try (captureDiagnostics (emitWorldAcquisitionRejection reason manifest
    (throw ThreadKilled))) :: IO (Either SomeException ((),String))
  case cancelled of
    Left failure | fromException failure == Just ThreadKilled -> pure ()
    Left failure -> throwIO failure
    Right _ -> fail "world acquisition diagnostic swallowed cancellation"

-- These are raw closed-source data/I/O controls, not rooted world issuers.
-- They enter the same materialization owner used before world publication.
worldMaterializationBoundaries :: IO ()
worldMaterializationBoundaries = do
  -- Root order is part of source selection. Equal relative paths in distinct
  -- roots keep distinct coordinates, including after the roots are reordered.
  forM_ [["first","second"],["second","first"]] $ \bodies -> withScratch $ \parent -> do
    sources <- sourceRootsFixture [[("Same.hs",body)] | body <- bodies]
    materialized <- materializeWorldSources parent sources Set.empty pure
    child <- onlyChild parent
    forM_ (zip [0 :: Int ..] bodies) $ \(ordinal,expected) -> do
      actual <- BS.readFile (parent </> child </> ("root-" ++ show ordinal) </> "Same.hs")
      unless (actual == expected)
        (fail "World source root reordering changed its coordinate or captured bytes")
    _ <- releaseWorldMaterialization materialized
    requireEmpty parent
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\nanswer = 1\n")]
    materialized <- materializeWorldSources parent sources Set.empty pure
    child <- onlyChild parent
    let directory = parent </> child
    -- Three direct realized owners share exactly one physical source tree.
    retainWorldMaterialization materialized
    retainWorldMaterialization materialized
    owners <- worldMaterializationOwners materialized
    unless (owners == Just 3) (fail "world fork did not retain three direct backing owners")
    forM_ [2,1] $ \remaining -> do
      removed <- releaseWorldMaterialization materialized
      alive <- doesDirectoryExist directory
      owners <- worldMaterializationOwners materialized
      unless (not removed && alive && owners == Just remaining)
        (fail "dropping a predecessor removed surviving successor backing")
      measured <- measureBacking directory
      unless (measured == worldMaterializationCharge materialized)
        (fail "shared source backing changed its once-issued physical charge")
    removed <- releaseWorldMaterialization materialized
    alive <- doesDirectoryExist directory
    owners <- worldMaterializationOwners materialized
    unless (removed && not alive && owners == Just 0)
      (fail "last realized world failed to release its physical backing")
    repeated <- try (releaseWorldMaterialization materialized) :: IO (Either SomeException Bool)
    unless (either (const True) (const False) repeated)
      (fail "released backing accepted a duplicate release")
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    first <- materializeWorldSources parent sources Set.empty pure
    firstChild <- onlyChild parent
    second <- materializeWorldSources parent sources Set.empty pure
    children <- listDirectory parent
    secondChild <- case filter (/= firstChild) children of
      [child] -> pure child
      _ -> fail "independent cold acquisitions did not keep distinct physical backing"
    firstOwners <- worldMaterializationOwners first
    secondOwners <- worldMaterializationOwners second
    firstCharge <- measureBacking (parent </> firstChild)
    secondCharge <- measureBacking (parent </> secondChild)
    unless (firstOwners == Just 1 && secondOwners == Just 1
        && firstCharge == worldMaterializationCharge first
        && secondCharge == worldMaterializationCharge second)
      (fail "equal source bytes substituted sharing for actual physical acquisitions")
    _ <- releaseWorldMaterialization first
    remaining <- doesDirectoryExist (parent </> secondChild)
    unless remaining (fail "independent acquisition lost backing when its equal peer retired")
    _ <- releaseWorldMaterialization second
    requireEmpty parent
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    materialized <- materializeWorldSources parent sources Set.empty pure
    child <- onlyChild parent
    let directory = parent </> child
        root = directory </> "root-0"
    permissions <- getPermissions root
    setPermissions root (permissions {readable=False,writable=False,searchable=False})
    failed <- try (releaseWorldMaterialization materialized) :: IO (Either SomeException Bool)
    setPermissions root permissions
    unless (either (const True) (const False) failed)
      (fail "unreadable backing unexpectedly reported successful cleanup")
    owners <- worldMaterializationOwners materialized
    unless (owners == Nothing) (fail "cleanup failure reported settled backing ownership")
    retained <- try (retainWorldMaterialization materialized) :: IO (Either SomeException ())
    unless (either (const True) (const False) retained)
      (fail "quarantined backing accepted a new successor")
    retried <- try (releaseWorldMaterialization materialized) :: IO (Either SomeException Bool)
    unless (either (const True) (const False) retried)
      (fail "quarantined backing manufactured a successful release")
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\nanswer = 1\n")
      ,("Nested/B.hs","module Nested.B where\n")]
    charged <- materializeWorldSources parent sources Set.empty (pure . worldMaterializationCharge)
    child <- onlyChild parent
    bytes <- BS.readFile (parent </> child </> "root-0" </> "A.hs")
    unless (bytes == "module A where\nanswer = 1\n")
      (fail "successful world materialization removed or changed its issued files")
    -- Independent cold-test filesystem oracle, never a production hit scan.
    measured <- measureBacking (parent </> child)
    unless (charged == measured) (fail ("world backing issuance charge differs: " ++ show (charged,measured)))
  withScratch $ \parent -> do
    -- A file/parent collision deterministically fails after the first copy.
    sources <- sourceFixture [("A","copied"),("A/B.hs","module A.B where\n")]
    entered <- newIORef False
    outcome <- try (materializeWorldSources parent sources Set.empty
      (const (writeIORef entered True))) :: IO (Either SomeException ())
    case outcome of
      Left failure | Just (_ :: IOException) <- fromException failure -> pure ()
      Left failure -> throwIO failure
      Right () -> fail "world materializer accepted a file/directory collision"
    published <- readIORef entered
    unless (not published) (fail "failed source copy entered world publication")
    requireEmpty parent
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    outcome <- try (materializeWorldSources parent sources Set.empty
      (const (throwIO policy))) :: IO (Either SomeException ())
    requirePolicy outcome
    requireEmpty parent
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    ready <- newEmptyMVar
    blocked <- newEmptyMVar
    done <- newEmptyMVar
    let action = materializeWorldSources parent sources Set.empty $ \_ -> do
          putMVar ready ()
          readMVar blocked
        retire thread = bounded "world materialization child retirement"
          (killThread thread >> readMVar done) >> pure ()
    completed <- withCompletionRelease ExecutorStop
      (forkFinally action (putMVar done)) retire $ \thread -> captureCompletion $ do
        bounded "world materialization publication barrier" (readMVar ready)
        bounded "world materialization cancellation" (killThread thread)
        result <- bounded "cancelled world materialization completion" (readMVar done)
        requireCancellation result
        requireEmpty parent
    finishCompletion completed
  withScratch $ \parent -> do
    sources <- sourceFixture [("A.hs","module A where\n")]
    recovery <- newIORef Nothing
    completed <- captureCompletion $ materializeWorldSources parent sources Set.empty $ \_ -> do
      child <- onlyChild parent
      let root = parent </> child </> "root-0"
      permissions <- getPermissions root
      writeIORef recovery (Just (root,permissions))
      setPermissions root (permissions {readable=False,writable=False,searchable=False})
      throwIO policy
    restore <- readIORef recovery
    forM_ restore $ \(root,permissions) -> setPermissions root permissions
    case completionResult completed of
      BodyFailed failure -> requirePolicy (Left failure)
      _ -> fail "failed world cleanup lost its original policy refusal"
    case completionFailures completed of
      [failure] | cleanupStage failure == CompilerScopeRestore
        , Just (_ :: IOException) <- fromException (cleanupException failure) -> pure ()
      _ -> fail "failed world cleanup lost its independent release evidence"
    _ <- onlyChild parent
    pure ()
  where
    policy = WorldModulePolicyFailure "fixture-source" WorldUncapturedDependency
    requirePolicy result = case result of
      Left failure | Just (WorldModulePolicyFailure path reason) <- fromException failure
        , path == "fixture-source", reason == WorldUncapturedDependency -> pure ()
      Left failure -> throwIO failure
      Right () -> fail "unissued world unexpectedly survived policy refusal"
    requireEmpty parent = do
      children <- listDirectory parent
      unless (null children) (fail ("unissued world directory survived failure: " ++ show children))
    onlyChild parent = do
      children <- listDirectory parent
      case children of
        [child] -> pure child
        _ -> fail ("expected one owned world directory, got " ++ show children)
    bounded label action = timeout 5000000 action >>= maybe (fail (label ++ " timed out")) pure
    measureBacking path = do
      directory <- doesDirectoryExist path
      if directory then do
        children <- listDirectory path
        charges <- mapM (measureBacking . (path </>)) children
        pure (sum (map fst charges),1 + sum (map snd charges))
      else do
        bytes <- getFileSize path
        pure (bytes,1)
    sourceFixture files = sourceRootsFixture [files]
    sourceRootsFixture roots = either fail pure (decodeWorldSources
      ("TPCSRC01" <> frame "fixture-generation" <> word32 (length roots)
        <> foldMap (\files -> word32 (length files)
          <> foldMap (\(path,body) -> frame path <> frame body) files) roots))
    frame bytes = word32 (BS.length bytes) <> bytes
    word32 value = BS.pack [fromIntegral (value `div` (256 ^ power)) | power <- [0..3 :: Int]]

-- Actual module checking enters the resident kernel through its scoped helper.
-- Refusals must identify the transitive dependency before
-- preprocessing/typechecking has an opportunity to execute its options.
checkedPublicModuleHistories :: IO ()
checkedPublicModuleHistories = do
  withCompilerFixture $ \scope work -> do
    scopedRunGhc scope $ do
      baseline <- getSessionDynFlags
      (effective,leftover,_) <- parseDynamicFilePragma baseline [noLoc "-fbyte-code"]
      liftIO $ unless (null leftover
          && not (backendGeneratesCode (backend baseline))
          && not (gopt Opt_ByteCode baseline)
          && not (backendGeneratesCode (backend effective))
          && gopt Opt_ByteCode effective)
        (fail "pinned GHC bytecode pragma no longer separates its generation flag from the checking backend")
    writeTarget work
    checked <- scopedCompile scope CheckedPublicModule Set.empty GeneralCompile Nothing
      (work </> "BoundPreparedA.hs") [work] Nothing
    unless (moduleNameString (moduleName (checkedPublicModuleIdentity checked)) == "BoundPreparedA"
        && Set.fromList (map dependencyModuleName (dependencyModules (checkedPublicModuleEvidence checked)))
          == Set.fromList ["BoundPreparedA","BoundPreparedB"])
      (fail "public module completion lost the actual target or dependency graph")
    -- Ordinary consumers still receive their target's checked environment.
    ordinary <- scopedCompile scope CheckedEnvironment Set.empty GeneralCompile Nothing
      (work </> "BoundPreparedA.hs") [work] Nothing
    unless (moduleNameString (moduleName (tcg_mod (crTargetTcGblEnv ordinary))) == "BoundPreparedA")
      (fail "ordinary checked consumer changed after public module completion")
  forM_ ["{-# LANGUAGE CPP #-}","{-# LANGUAGE TemplateHaskell #-}"] $ \pragma ->
    withCompilerFixture $ \scope work -> do
      writeTarget work
      prependDependency work pragma
      refused scope work (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldUntrackedOptions
  forM_ [("-ddump-parsed -ddump-to-file -dumpdir ",WorldSideOutputs)
        ,("-fwrite-ide-info -hiedir ",WorldSideOutputs)
        ,("-hidir ",WorldOutputPolicyChanged)
        ,("-tmpdir ",WorldOutputPolicyChanged)
        ,("-ddump-file-prefix ",WorldOutputPolicyChanged)] $ \(flags,reason) ->
    withCompilerFixture $ \scope work -> do
      writeTarget work
      let ambient = work </> "ambient-outputs"
      createDirectory ambient
      prependDependency work ("{-# OPTIONS_GHC " ++ flags ++ show ambient ++ " #-}")
      refused scope work (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" reason
      outputs <- listDirectory ambient
      unless (null outputs) (fail ("world module admission wrote untracked outputs: " ++ show outputs))
  forM_ ["-fwrite-interface","-fwrite-if-simplified-core","-shared"] $ \flag ->
    withCompilerFixture $ \scope work -> do
      writeTarget work
      prependDependency work ("{-# OPTIONS_GHC " ++ flag ++ " #-}")
      refused scope work (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldOutputPolicyChanged
  forM_ ["-fbyte-code","-fobject-code"] $ \backendFlag ->
    withCompilerFixture $ \scope work -> do
      writeTarget work
      let compiler = work </> "foreign-compiler.sh"
          marker = work </> "foreign-compiler-ran"
      writeFile compiler ("#!/bin/sh\necho ran > " ++ show marker ++ "\nexit 1\n")
      permissions <- getPermissions compiler
      setPermissions compiler (permissions {executable=True})
      writeFile (work </> "BoundPreparedB.hs") (unlines
        [ "{-# LANGUAGE CApiFFI #-}"
        , "{-# OPTIONS_GHC " ++ backendFlag ++ " -pgmc " ++ show compiler ++ " #-}"
        , "module BoundPreparedB where"
        , "import Foreign.C.Types (CInt(..))"
        , "foreign import capi unsafe \"uncaptured_world_header.h world_probe\" probe :: IO CInt"
        , "__result :: Int"
        , "__result = 97"
        ])
      refused scope work (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldCodeGeneration
      ran <- doesFileExist marker
      unless (not ran) (fail "world module admission executed a transitive foreign compiler")
  withCompilerFixture $ \scope work -> do
    writeTarget work
    let preprocessor = work </> "preprocessor.sh"
        marker = work </> "preprocessor-ran"
    writeFile preprocessor ("#!/bin/sh\necho ran > " ++ show marker ++ "\nexit 1\n")
    permissions <- getPermissions preprocessor
    setPermissions preprocessor (permissions {executable=True})
    prependDependency work ("{-# OPTIONS_GHC -F -pgmF " ++ show preprocessor ++ " #-}")
    refused scope work (work </> "BoundPreparedA.hs") "BoundPreparedB.hs" WorldUntrackedOptions
    ran <- doesFileExist marker
    unless (not ran) (fail "world module admission executed a transitive custom preprocessor")
  withCompilerFixture $ \scope work -> do
    let literate = work </> "BoundPreparedA.lhs"
    writeFile literate "> module BoundPreparedA where\n> answer :: Int\n> answer = 41\n"
    refused scope work literate "BoundPreparedA.lhs" WorldLiterateSource
  where
    writeTarget work = writeFile (work </> "BoundPreparedA.hs")
      "module BoundPreparedA where\nimport qualified BoundPreparedB\nanswer :: Int\nanswer = BoundPreparedB.__result\n"
    prependDependency work pragma = do
      let path = work </> "BoundPreparedB.hs"
      original <- readFile path
      -- Force the old text before reopening the same path for writing.
      length original `seq` writeFile path (pragma ++ "\n" ++ original)
    refused scope work path dependency reason = do
      outcome <- try (scopedCompile scope CheckedPublicModule Set.empty GeneralCompile Nothing path [work] Nothing)
        :: IO (Either SomeException CheckedPublicModuleResult)
      case outcome of
        Left failure
          | Just (WorldModulePolicyFailure actual actualReason) <- fromException failure
          , takeFileName actual == dependency, actualReason == reason -> pure ()
          | otherwise -> throwIO failure
        Right _ -> fail "unsupported world module source was checked"

requestFor :: FilePath -> IO WorkerRequest
requestFor work = case workerRequestFromArgv (workerArgv [Input (work </> "BoundPreparedA.hs"), Include work, Targets ["__result", "alternate"]]) of
  Right (Just args) -> pure args
  other -> fail ("invalid bound compiler fixture request: " ++ show other)

withCompilerFixture :: (CompilerScope -> FilePath -> IO a) -> IO a
withCompilerFixture action = withScratch $ \work -> do
  forM_ ["BoundPreparedA.hs", "BoundPreparedB.hs", "BoundPreparedError.hs"] $ \name ->
    copyFile ("test-source-boot/fixtures" </> name) (work </> name)
  withRootedCompilerEnvironment work $ withQuietStdout $
    withResidentCompilerScopes [work] $ \runScope -> runScope (pure ()) $ \scope -> action scope work

withRequestFixture :: (CompilerScope -> BoundCompilerRequest -> FilePath -> IO a) -> IO a
withRequestFixture action = withCompilerFixture $ \scope work -> do
  args <- requestFor work
  (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \request -> action scope request work

compileA :: BoundCompilerRequest -> FilePath -> IO PreparedCompilation
compileA request work = compilePreparedRequest request (PreparedCompilationProducts Nothing) Set.empty GeneralCompile Nothing
  (work </> "BoundPreparedA.hs")

-- Expected module/entry and integer bytes are authored fixture facts. This
-- oracle inspects the emitted execution schema, never the retained Core/cache.
projectA :: FilePath -> String -> PreparedCompilation -> IO ()
projectA work target completion = projectFixture work "BoundPreparedA" target
  (if target == "alternate" then 43 else 41) completion

projectFixture :: FilePath -> Text.Text -> String -> Int -> PreparedCompilation -> IO ()
projectFixture work owner target expected completion = do
  interfaces <- newPreparedCompilationInterfaceArtifacts completion work
  (artifacts,_) <- prepareArtifacts interfaces [target] [] Map.empty []
  case artifacts of
    [artifact] | wireResultLiteral owner (Text.pack target) expected (paProgram artifact) -> pure ()
    _ -> fail "completed preparation lost its independently specified owner or integer value"

wireResultLiteral :: Text.Text -> Text.Text -> Int -> WireProgram -> Bool
wireResultLiteral owner target expected program = case matching of
  [binding] | Wire.heapBindingId binding == programEntry program -> case Wire.heapBindingRhs binding of
    Wire.Constructor constructor [value] -> intConstructor constructor && intAtom value
    Wire.Thunk _ _ [] (Wire.Construct constructor [value]) -> intConstructor constructor && intAtom value
    _ -> False
  _ -> False
  where
    matching = [binding | group <- programBindings program
      , TopBinding identity binding <- case group of NonRecursive value -> [value]; Recursive values -> values
      , symbolModule identity == owner, symbolOccurrence identity == target]
    intAtom (Wire.Scalar (Wire.IntLiteral 64 bytes)) = bytes == BS.pack (replicate 7 0 ++ [fromIntegral expected])
    intAtom _ = False
    intConstructor (Wire.ConstructorId ordinal) = case drop (fromIntegral ordinal) (programConstructors program) of
      declaration:_ -> let identity = Wire.constructorSymbol (Wire.constructorIdentity declaration)
        in symbolModule identity == "GHC.Types" && symbolOccurrence identity == "I#"
      [] -> False

completedRequestHistories :: IO ()
completedRequestHistories = do
  runHistory [0,1,2,3,4]
  -- Each generated case and shrink recreates its owning universe, executor and
  -- first completion. A minimized history has no hidden earlier case prefix.
  result <- quickCheckWithResult stdArgs {maxSuccess=8,maxSize=8} $
    forAllShrink (resize 8 (listOf (chooseInt (0,4)))) shrink $ \history ->
      ioProperty (runHistory history >> pure True)
  unless (isSuccess result) (fail "completed compiler histories failed")

runHistory :: [Int] -> IO ()
runHistory history = withRequestFixture $ \scope request work -> do
  first <- compileA request work
  projectA work "__result" first
  let step :: Int -> IO ()
      step choice = case choice of
        0 -> do
          second <- compilePreparedRequest request (PreparedCompilationProducts Nothing) Set.empty GeneralCompile Nothing
            (work </> "BoundPreparedB.hs")
          projectFixture work "BoundPreparedB" "__result" 97 second
        1 -> do
          refused <- try (compileCheckedRequest request CheckedCompilationEnvironment Set.empty GeneralCompile Nothing
            (work </> "BoundPreparedError.hs")) :: IO (Either SomeException CheckedCompilation)
          case refused of
            Left failure | Just (_ :: SomeAsyncException) <- fromException failure -> throwIO failure
            Left failure | Just _ <- sourceFailureDiagnostics failure -> pure ()
            Left failure -> throwIO failure
            Right _ -> fail "independent invalid Haskell fixture unexpectedly compiled"
        2 -> scopedRunGhc scope $ do
          package <- findModule (mkModuleName "Data.Maybe") Nothing
          info <- getModuleInfo package
          unless (maybe False (any ((== "fromMaybe") . getOccString) . modInfoExports) info) $
            liftIO (fail "package-only helper did not demand the actual Data.Maybe interface")
        3 -> projectA work "__result" first
        _ -> projectA work "alternate" first
  mapM_ step history
  projectA work "__result" first

completedRequestCancellation :: IO ()
completedRequestCancellation = withRequestFixture $ \scope request work -> do
  first <- compileA request work
  originals <- newPreparedCompilationInterfaceArtifacts first work
  bindings <- prepareCompilationSessionBindings first ["retained"]
  checked <- compileCheckedRequest request CheckedCompilationEnvironment Set.empty GeneralCompile Nothing (work </> "BoundPreparedA.hs")
  cancelled <- try (scopedRunGhc scope (liftIO (throwIO ThreadKilled))) :: IO (Either SomeException ())
  requireCancellation cancelled
  unless (preparedCompilationBindingCount first > 0 && null (preparedCompilationWarnings first)) $
    fail "cancellation changed previously completed compiler observations"
  noOutputChanges work $ forM_ (preparedHandleOperations first originals bindings) $ requirePhase CompilerTransactionFailed
  withInspectionWork $ \inspection ->
    requirePhase CompilerTransactionFailed (inspectCheckedCompilation inspection checked [])
  -- The existing scope guard, rather than a second request lifecycle, refuses
  -- all later compiler effects too.
  requirePhase CompilerTransactionFailed (scopedRunGhc scope (pure ()))

data CompilerScopeControlFailure = GuardedBodyFailure | RecoveryCleanupFailure
  deriving (Eq, Show)
instance Exception CompilerScopeControlFailure

-- The actual resident scope owns recovery and admission. Fail only its existing
-- caller-supplied recovery cleanup, after an ordinary synchronous Ghc failure.
compilerScopeCleanupFailure :: IO ()
compilerScopeCleanupFailure = withResidentCompilerScopes [] $ \runScope -> do
  let guardedFailure compiler = scopedRunGhc compiler (liftIO (throwIO GuardedBodyFailure)) :: IO ()
      requirePrimary completed = case completionResult completed of
        BodyFailed failure | fromException failure == Just GuardedBodyFailure -> pure ()
        _ -> fail "compiler recovery lost the original guarded body failure"
      requireCleanup completed = case completionFailures completed of
        [failure] | cleanupStage failure == CompilerScopeRestore
          , fromException (cleanupException failure) == Just RecoveryCleanupFailure -> pure ()
        _ -> fail "compiler recovery lost or duplicated its independent cleanup failure"
  -- Successful recovery permits another operation and a later scope. This
  -- distinguishes cleanup failure from the guarded body failure itself.
  runScope (pure ()) $ \compiler -> do
    recovered <- captureCompletion (guardedFailure compiler)
    requirePrimary recovered
    unless (null (completionFailures recovered)) (fail "healthy recovery reported failed cleanup")
    scopedRunGhc compiler (pure ())
  runScope (pure ()) $ \compiler -> scopedRunGhc compiler (pure ())

  cleanupCalls <- newIORef (0 :: Int)
  let failedRecovery = modifyIORef' cleanupCalls (+1) >> throwIO RecoveryCleanupFailure
  completed <- captureCompletion $ runScope failedRecovery $ \compiler -> do
    failed <- captureCompletion (guardedFailure compiler)
    requirePrimary failed
    unless (null (completionFailures failed))
      (fail "guarded operation reported cleanup owned by final scope settlement")
    requirePhase CompilerTransactionFailed (scopedRunGhc compiler (pure ()))
    finishCompletion failed
  -- Crossing the scope's release must retain both causes and cannot clear the
  -- poisoned resident owner, even though no compiler phase remains open.
  requirePrimary completed
  requireCleanup completed
  attempts <- readIORef cleanupCalls
  unless (attempts == 1) (fail "synchronous compiler cleanup failure was retried or skipped")
  requirePhase CompilerTransactionPoisoned
    (runScope (pure ()) (\compiler -> scopedRunGhc compiler (pure ())))

completedRequestRelease :: IO ()
completedRequestRelease = withCompilerFixture $ \scope work -> do
  args <- requestFor work
  (first,interfaces,bindings) <- (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \request -> do
    first <- compileA request work
    projectA work "__result" first
    interfaces <- newPreparedCompilationInterfaceArtifacts first work
    bindings <- prepareCompilationSessionBindings first ["retained"]
    pure (first,interfaces,bindings)
  -- The compiler scope remains Ready; the issuing executor is already closed.
  noOutputChanges work $ forM_ (preparedHandleOperations first interfaces bindings) $ \operation -> do
    refused <- try operation
    requireCancellation refused
  unless (preparedCompilationBindingCount first > 0 && null (preparedCompilationWarnings first))
    (fail "closed preparation lost its immutable observations")
  (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \later -> do
    requireCancellation =<< try (commitPreparedWorldDependencies later first)
    noOutputChanges work $ forM_ (preparedHandleOperations first interfaces bindings) $ \operation ->
      requireCancellation =<< try operation

preparedHandleOperations :: PreparedCompilation -> PreparedCompilationInterfaces -> PreparedCompilationBindings -> [IO ()]
preparedHandleOperations completion interfaces bindings =
  [ void (newPreparedCompilationInterfaceArtifacts completion "/unavailable/closed-preparation")
  , void (prepareArtifacts interfaces [] [] Map.empty [])
  , void (prepareArtifacts interfaces ["__result"] [] Map.empty [])
  , void (sealPreparedActivationPreviewWitness interfaces)
  , validatePreparedCompilationInputs completion
  , void (writePreparedCompilationSidecars InlineYieldSites interfaces [])
  , void (writePreparedRequestProducts interfaces Nothing [])
  , writePreparedPortableOutputs interfaces
  , void (prepareCompilationSessionBindings completion [])
  , void (writeCompilationSessionBindings bindings 1 "/unavailable/closed-preparation")
  ]

completedRequestScopeRelease :: IO ()
completedRequestScopeRelease = withScratch $ \output -> do
  BS.writeFile (output </> "existing-product") "preserve closed-scope product"
  (first,interfaces,bindings,checked) <- withRequestFixture $ \_ request work -> do
    first <- compileA request work
    interfaces <- newPreparedCompilationInterfaceArtifacts first output
    bindings <- prepareCompilationSessionBindings first ["retained"]
    checked <- compileCheckedRequest request CheckedCompilationEnvironment Set.empty GeneralCompile Nothing (work </> "BoundPreparedA.hs")
    pure (first,interfaces,bindings,checked)
  noOutputChanges output $ forM_ (preparedHandleOperations first interfaces bindings) $
    requirePhase CompilerTransactionReleased
  unless (preparedCompilationBindingCount first > 0 && null (preparedCompilationWarnings first))
    (fail "released compiler scope lost immutable diagnostics")
  withInspectionWork $ \inspection -> do
    requirePhase CompilerTransactionReleased (inspectCheckedCompilation inspection checked [])
    requirePhase CompilerTransactionReleased (satisfiesCheckedActivationConstraint checked)
  unless (null (checkedCompilationWarnings checked)) (fail "released checked diagnostics changed")

completedRequestOwnerBoundaries :: IO ()
completedRequestOwnerBoundaries = withRequestFixture $ \scope request work -> do
  let directoryA = work </> "output-a"
      directoryB = work </> "output-b"
  createDirectory directoryA
  createDirectory directoryB
  BS.writeFile (directoryA </> "existing-product") "A already exists"
  BS.writeFile (directoryB </> "existing-product") "B already exists"
  first <- compileA request work
  interfacesA <- newPreparedCompilationInterfaceArtifacts first directoryA
  bindingsA <- prepareCompilationSessionBindings first ["retained"]
  (artifactsA,contextA) <- prepareArtifacts interfacesA ["__result"] [] Map.empty []
  second <- compilePreparedRequest request (PreparedCompilationProducts Nothing) Set.empty GeneralCompile Nothing
    (work </> "BoundPreparedB.hs")
  interfacesB <- newPreparedCompilationInterfaceArtifacts second directoryB
  (artifactsB,contextB) <- prepareArtifacts interfacesB ["__result"] [] Map.empty []
  unless (case (artifactsA,artifactsB) of
      ([a],[b]) -> wireResultLiteral "BoundPreparedA" "__result" 41 (paProgram a)
        && wireResultLiteral "BoundPreparedB" "__result" 97 (paProgram b)
      _ -> False) (fail "same-request owner controls did not compile distinct actual products")
  unless (maybe False (const True) contextA && maybe False (const True) contextB)
    (fail "owner mismatch controls lack genuine native contexts")
  noOutputChanges directoryA $ requirePurpose (writePreparedRequestProducts interfacesA contextB artifactsA)
  noOutputChanges directoryA $ requirePurpose (writePreparedRequestProducts interfacesA Nothing artifactsA)
  noOutputChanges directoryB $ requirePurpose (writePreparedRequestProducts interfacesB contextA artifactsB)
  noOutputChanges directoryA $ requirePurpose (writePreparedPortableOutputs interfacesA)
  noOutputChanges directoryA $ requirePurpose (sealPreparedActivationPreviewWitness interfacesA)
  -- A separate live request on the same compiler cannot supply A's context.
  args <- requestFor work
  foreignContext <- (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \other -> do
    third <- compileA other work
    interfaces <- newPreparedCompilationInterfaceArtifacts third directoryB
    snd <$> prepareArtifacts interfaces ["__result"] [] Map.empty []
  noOutputChanges directoryA $ requirePurpose (writePreparedRequestProducts interfacesA foreignContext artifactsA)
  noOutputChanges directoryA $ forM_ (preparedHandleOperations first interfacesA bindingsA) $ \operation -> do
    result <- newEmptyMVar
    _ <- forkFinally operation (putMVar result)
    outcome <- readMVar result
    requirePhase CompilerTransactionWrongThread (either throwIO pure outcome)
  -- Repeating and changing a valid projection retains the same compilation.
  (again,_) <- prepareArtifacts interfacesA ["__result"] [] Map.empty []
  unless (map paBytes again == map paBytes artifactsA) (fail "repeated projection changed completed code")
  projectA directoryA "alternate" first
  _ <- writePreparedRequestProducts interfacesA contextA artifactsA
  _ <- writePreparedCompilationSidecars InlineYieldSites interfacesA artifactsA
  published <- doesFileExist (directoryA </> "certified-products.cbor")
  unless published (fail "matching owner did not publish certified products")
  binders <- writeCompilationSessionBindings bindingsA 1 (work </> "session-output")
  case binders of
    [BoundBinder "retained" _ _ _ "Int" _ _] -> pure ()
    _ -> fail "session output lost the first compilation's prepared binding"

checkedRequestOwnerBoundaries :: IO ()
checkedRequestOwnerBoundaries = withCompilerFixture $ \scope work -> withInspectionWork $ \inspection -> do
  let pathA = work </> "CheckedOwnerA.hs"
      pathB = work </> "CheckedOwnerB.hs"
      -- TypeOf consumes the real generated probe, rather than reevaluating the
      -- diagnostic query string. Different probe types distinguish the owners.
      question = InspectTypeOf "probe"
      requireType expected completion = do
        answers <- inspectCheckedCompilation inspection completion [question]
        case answers of
          [InspectionType "probe" actual _] | actual == expected -> pure ()
          _ -> fail ("checked handle lost its issuing probe type: " ++ show answers)
  writeFile pathA "module CheckedOwnerA where\n__tidepool_inspect_0 = (41 :: Int)\n"
  writeFile pathB "module CheckedOwnerB where\n__tidepool_inspect_0 = False\n"
  args <- case workerRequestFromArgv (workerArgv
      [Input pathA,Include work,InspectType "probe",InspectOut (work </> "inspection.cbor")]) of
    Right (Just request) -> pure request
    other -> fail ("invalid checked owner fixture: " ++ show other)
  first <- (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \request -> do
    first <- compileCheckedRequest request CheckedCompilationEnvironment Set.empty GeneralCompile Nothing pathA
    second <- compileCheckedRequest request CheckedCompilationEnvironment Set.empty GeneralCompile Nothing pathB
    requireType "Bool" second
    requireType "Int" first
    requirePurpose (satisfiesCheckedActivationConstraint first)
    result <- newEmptyMVar
    _ <- forkFinally (void (inspectCheckedCompilation inspection first [])) (putMVar result)
    outcome <- readMVar result
    requirePhase CompilerTransactionWrongThread (either throwIO pure outcome)
    pure first
  requireCancellation =<< try (void (inspectCheckedCompilation inspection first []))
  requireCancellation =<< try (void (satisfiesCheckedActivationConstraint first))
  unless (null (checkedCompilationWarnings first)) (fail "closed checked handle changed diagnostics")
  (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \later -> do
    live <- compileCheckedRequest later CheckedCompilationEnvironment Set.empty GeneralCompile Nothing pathB
    requireType "Bool" live
    requireCancellation =<< try (void (inspectCheckedCompilation inspection first []))

requirePurpose :: IO a -> IO ()
requirePurpose operation = do
  outcome <- try (void operation) :: IO (Either InputRejection ())
  case outcome of
    Left CheckedPurposeMismatch -> pure ()
    Left failure -> throwIO failure
    Right () -> fail "operation accepted a foreign owner or wrong admitted intent"

-- Observe entries as well as bytes: an empty/new file and an overwrite both fail.
noOutputChanges :: FilePath -> IO a -> IO a
noOutputChanges directory operation = do
  before <- directorySnapshot directory
  result <- operation
  after <- directorySnapshot directory
  unless (before == after) (fail "refused operation changed existing output entries or contents")
  pure result

directorySnapshot :: FilePath -> IO [(FilePath,Maybe BS.ByteString)]
directorySnapshot root = walk ""
  where
    walk relative = do
      names <- sort <$> listDirectory (root </> relative)
      fmap concat $ forM names $ \name -> do
        let child = relative </> name
            path = root </> child
        directory <- doesDirectoryExist path
        if directory then ((child,Nothing):) <$> walk child
          else do
            bytes <- BS.readFile path
            pure [(child,Just bytes)]

requireCancellation :: Either SomeException () -> IO ()
requireCancellation result = case result of
  Left failure | Just ThreadKilled <- fromException failure -> pure ()
  Left failure -> throwIO failure
  Right () -> fail "closed or cancelled owner accepted effectful preparation"

requirePhase :: CompilerTransactionFailure -> IO a -> IO ()
requirePhase expected action = do
  result <- try (void action) :: IO (Either SomeException ())
  case result of
    Left failure | Just actual <- fromException failure, actual == expected -> pure ()
    Left failure -> throwIO failure
    Right () -> fail ("compiler preparation accepted invalid phase: " ++ show expected)


nativeOutputProvenanceChecks :: IO ()
nativeOutputProvenanceChecks = withScratch $ \work -> do
  requireObservationPackageEnvironment
  copyFile "test-source-boot/fixtures/BoundPreparedA.hs" (work </> "BoundPreparedA.hs")
  native <- requestFor work
  portable <- case workerRequestFromArgv (workerArgv
      [Input (work </> "BoundPreparedA.hs"),Include work,Target "__result",OutputDir work,PortableOutput]) of
    Right (Just request) -> pure request
    other -> fail ("invalid explicit portable fixture: " ++ show other)
  withResidentCompilerScopes [work] $ \runScope -> runScope (pure ()) $ \scope -> do
    refused <- try ((withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest native) >=> finishCompletion) $ \request ->
      compilePreparedRequest request PreparedCompilationStg Set.empty GeneralCompile Nothing (work </> "missing.hs"))
      :: IO (Either NativeProductConfigurationUnavailable PreparedCompilation)
    case refused of
      Left NativeProductConfigurationUnavailable -> pure ()
      Right _ -> fail "observation configuration issued a native compilation"
    (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest portable) >=> finishCompletion) $ \request -> do
      completion <- compilePreparedRequest request PreparedCompilationStg Set.empty GeneralCompile Nothing
        (work </> "BoundPreparedA.hs")
      interfaces <- newPreparedCompilationInterfaceArtifacts completion work
      output <- prepareArtifactsWithIntent interfaces ["__result"] [] Map.empty []
      case output of
        PortableOutputs [artifact] -> unless (wireResultLiteral "BoundPreparedA" "__result" 41 (paProgram artifact))
          (fail "explicit portable compilation emitted incorrect code")
        _ -> fail "explicit portable intent selected native products"
      noOutputChanges work $ requirePurpose (writePreparedRequestProducts interfaces Nothing [])
      bindings <- prepareCompilationSessionBindings completion ["portable"]
      noOutputChanges work $ requirePurpose (writeCompilationSessionBindings bindings 3 work)
      case output of
        PortableOutputs artifacts -> void (writePreparedCompilationSidecars SeparateYieldSites interfaces artifacts)
        _ -> fail "portable sidecar control changed output intent"
      writePreparedPortableOutputs interfaces
      sidecar <- doesFileExist (work </> "meta.cbor")
      unless sidecar (fail "portable output omitted shared metadata")
      marker <- BS.readFile (work </> "portable-output.mode")
      unless (marker == "TPPORTABLEOUTPUT1\n") (fail "portable writer omitted its output contract")

-- Finite installers and World stages consume this same native output owner.
-- A real list operation requires a separately prepared package implementation.
nativePackageOutputChecks :: IO ()
nativePackageOutputChecks = forM_ cases $ \(name,sourceLines,names,wantsCode) -> withScratch $ \work -> do
  let source = work </> name ++ ".hs"
  writeFile source (unlines sourceLines)
  args <- case workerRequestFromArgv (workerArgv [Input source,Include work,OutputDir work,Targets names]) of
    Right (Just request) -> pure request
    other -> fail ("invalid native package fixture: " ++ show other)
  withRootedCompilerEnvironment work $ withQuietStdout $
    withResidentCompilerScopes [work] $ \runScope -> runScope (pure ()) $ \scope ->
      (withBoundCompilerRequest (const (pure ())) scope (admitFixtureRequest args) >=> finishCompletion) $ \request -> do
        completion <- compilePreparedRequest request (PreparedCompilationProducts Nothing) Set.empty GeneralCompile Nothing source
        interfaces <- newPreparedCompilationInterfaceArtifacts completion work
        output <- prepareArtifactsWithIntent interfaces names [] Map.empty []
        case output of
          PortableOutputs _ -> fail "native package fixture selected portable output"
          NativeOutputs artifacts products -> do
            unless (length artifacts == length names) (fail "native package fixture lost its targets")
            duplicate <- try (void (prepareArtifactsWithIntent interfaces
              (names ++ take 1 names) [] Map.empty [])) :: IO (Either SomeException ())
            case duplicate of
              Left failure | "package projections repeat a target" `isInfixOf` show failure -> pure ()
              Left failure -> throwIO failure
              Right () -> fail "package issuer accepted duplicate actual target projections"
            forM_ artifacts $ \artifact -> unless
              (not (any ((== "reverse") . symbolOccurrence)
                [identity | group <- programBindings (paProgram artifact)
                  , TopBinding identity _ <- case group of NonRecursive top -> [top]; Recursive tops -> tops]))
              (fail "native target retained the package reverse implementation")
            void (writePreparedArtifacts work artifacts)
            void (writePreparedRequestProducts interfaces (Just products) artifacts)
            hasCode <- doesDirectoryExist (work </> "package-products")
            paths <- if hasCode then listDirectory (work </> "package-products") else pure []
            unless (not (null paths) == wantsCode)
              (fail "native writer confused TYPE support with executable package products")
            certificate <- BS.readFile (work </> "certified-products.cbor")
            forM_ paths $ \path -> do
              bytes <- BS.readFile (work </> "package-products" </> path)
              unless ("TPPACKAGEGROUP1" `BS.isInfixOf` bytes)
                (fail "native writer emitted an invalid package envelope")
              unless (TextEncoding.encodeUtf8 (Text.pack ("package-products/" ++ path)) `BS.isInfixOf` certificate)
                (fail "native certificate omitted an emitted package product")
            captured <- listDirectory (work </> "package-interfaces")
            unless (not (null captured)) (fail "native writer omitted captured package interfaces")
            (packageRows,packageLinks) <- case deserialiseFromBytes decodeTerm (BL.fromStrict certificate) of
              Right (rest,TList [TString "TPCERT",TInt 13,TList
                [TString "ordinary",_,_,TList rows,_,_,_,_,_,links]]) | BL.null rest -> pure (rows,links)
              _ -> fail "native writer emitted a malformed package witness table"
            ordinaryStageId <- case packageLinks of
              TList [TInt 4,TBytes stageId,TList occurrences,TList roots,TList _,TList originalUses,TList products] -> do
                byTarget <- forM roots $ \root -> case root of
                  TList [TString target,TList ids] -> pure (target,ids)
                  _ -> fail "native writer emitted malformed target package roots"
                let packageRoots = concatMap snd byTarget
                    occurrenceSets = [Set.fromList
                      [occurrence | TList [TString "local",TInt occurrence] <- ids]
                      | (_,ids) <- byTarget]
                unless (BS.length stageId == 32
                    && length products == length paths
                    && Set.fromList (map fst byTarget) == Set.fromList (map Text.pack names)
                    && length byTarget == length names
                    && null originalUses
                    && (not wantsCode || all (not . null . snd) byTarget)
                    && sum (map Set.size occurrenceSets) == Set.size (Set.unions occurrenceSets)
                    && all (validOccurrence (length occurrences)) packageRoots)
                  (fail "native writer emitted crossed or out-of-range package links")
                pure stageId
              _ -> fail "native writer emitted a malformed package-link manifest"
            when wantsCode $ case artifacts of
              artifact:_ -> do
                let segmentDirectory = work </> "segment"
                    itemDirectory = segmentDirectory </> "item-0"
                createDirectory segmentDirectory
                -- Exercise the lower product stage with its independently owned
                -- compiler result; request handles never expose their raw inputs.
                prepared <- scopedCompile scope (PreparedProducts Nothing) Set.empty GeneralCompile Nothing source [work] Nothing
                originalInterfaces <- newPreparedOriginalInterfaceArtifacts prepared segmentDirectory
                let pipeline = pprPipelineResult prepared
                    owner = tcg_mod (prTargetTcGblEnv pipeline)
                projection <- prepareCompilerProjectionContext prepared Map.empty owner "__result" [] Nothing
                (_,referenceProducts) <- prepareOriginalProducts (prHscEnv pipeline) Nothing
                  (pprProductInterfaces prepared) projection Set.empty (pprModules prepared)
                packageTargets <- forM artifacts $ \outputArtifact -> maybe
                  (fail "native artifact lost its paired package projection") pure (paPackageTarget outputArtifact)
                let captures = Map.fromList [(projectedPackageModule group,projectedPackageInterfaceBody group)
                      | target <- packageTargets,group <- packageTargetGroups target]
                provenance <- scopedNativeProductProvenance scope
                stageProducts <- retainPreparedPackageTargets provenance packageTargets captures referenceProducts
                staged <- writeCertifiedSegmentProducts [work] originalInterfaces segmentDirectory prepared stageProducts
                void (publishStagedOriginalProducts prepared staged)
                stageCertificate <- BS.readFile (segmentDirectory </> "certified-products.cbor")
                case deserialiseFromBytes decodeTerm (BL.fromStrict stageCertificate) of
                  Right (rest,TList [TString "TPCERT",TInt 13,TList
                    [TString "segment-originals",TList modules,_,TList witnesses,_,_,_,_,_]])
                    | BL.null rest -> do
                      let references = Set.fromList
                            [index | TList fields <- modules
                            , TList groups <- take 1 (drop 8 fields)
                            , TList [_,TList globals] <- groups
                            , TInt index <- globals]
                      unless (references == Set.fromList [0 .. length witnesses - 1])
                        (fail "stage validation added witnesses outside emitted HOME groups")
                  _ -> fail "stage validation changed the original-only certificate shape"
                packageTarget <- maybe (fail "native artifact lost its paired package projection") pure
                  (paPackageTarget artifact)
                createDirectory itemDirectory
                writeCertifiedSegmentItemProductsWithInputs prepared staged itemDirectory packageTarget
                  >>= \(_,inputs) -> unless (Set.null inputs)
                    (fail "source-only item acquired retained value inputs")
                itemCertificate <- BS.readFile (itemDirectory </> "certified-products.cbor")
                case deserialiseFromBytes decodeTerm (BL.fromStrict itemCertificate) of
                  Right (rest,TList [TString "TPCERT",TInt 13,TList
                    [TString "segment-item",_,_,TList
                      [TInt 2,TBytes stageId,TBytes programSha,TString target,TList _,TList _,TList []]]])
                    | BL.null rest
                    , stageId == ordinaryStageId
                    , programSha == SHA256.hash (paBytes artifact)
                    , target == "__result" -> pure ()
                  other -> fail ("segment-item package selection is not stage- or program-bound: " ++ show other)
              [] -> fail "native package fixture has no item target"
            witnesses <- forM packageRows $ \row -> case row of
              TList [TString unit,TString name,TString path,TString seal] -> do
                unless (takeDirectory (Text.unpack path) == work </> "package-interfaces")
                  (fail "selected package witness escaped its completed output")
                body <- captureArtifactBytes <$> BS.readFile (Text.unpack path)
                unless (Text.pack (artifactSha256 body) == seal)
                  (fail "selected package witness differs from its captured bytes")
                pure (unit,name,Text.unpack path,seal,body)
              _ -> fail "native writer emitted a malformed selected package witness"
            unless (wantsCode || any (\(_,name,_,_,_) -> name == "GHC.Types") witnesses)
              (fail "native package fixture lost its TYPE-only GHC.Types witness")
            when wantsCode $ forM_ ["Data.List","Prelude"] $ \direct ->
              unless (any (\(_,name,_,_,_) -> name == direct) witnesses)
                (fail "native output omitted a canonical sidecar package interface")
            unless (Set.fromList captured == Set.fromList
                [Text.unpack seal ++ ".hi" | (_,_,_,seal,_) <- witnesses])
              (fail "native package interfaces were duplicated or omitted from the witness table")
            case witnesses of
              (unit,name,_,seal,body):_ -> do
                let sourceCopy = work </> "captured-package-source.hi"
                    owner = (unit,name)
                    selected = [(unit,name,sourceCopy,seal)]
                    token = Map.singleton owner (sourceCopy,body)
                BS.writeFile sourceCopy (artifactBytes body)
                removeFile sourceCopy
                retained <- captureSelectedPackageWitnesses (work </> "retained-package-check") token
                  (selected ++ [(unit,name,work </> "same-captured-interface.hi",seal)])
                  >>= either fail pure
                unless (Map.size (fst retained) == 1 && length (snd retained) == 1)
                  (fail "selected package token was not shared across duplicate references")
                let refuse label result = case result of
                      Left _ -> pure ()
                      Right _ -> fail (label ++ " accepted invalid selected package custody")
                captureSelectedPackageWitnesses (work </> "missing-package-check") Map.empty selected
                  >>= refuse "missing interface"
                BS.writeFile sourceCopy (artifactBytes body)
                captureSelectedPackageWitnesses (work </> "forged-package-check") Map.empty
                  [(unit,name,sourceCopy,Text.replicate 64 "0")]
                  >>= refuse "forged seal"
                captureSelectedPackageWitnesses (work </> "conflicting-package-check") token
                  (selected ++ [(unit,name,sourceCopy,Text.replicate 64 "0")])
                  >>= refuse "conflicting owner"
              [] -> fail "native output omitted all selected package witnesses"

  where
    validOccurrence count value = case value of
      TList [TString "local",TInt occurrence] ->
        occurrence >= 0 && occurrence < fromIntegral count
      _ -> False
    cases =
      [("NativePackageTarget",
        ["module NativePackageTarget where", "import Data.List (reverse)"
        ,"{-# NOINLINE __result #-}", "__result :: [Int] -> [Int]", "__result = reverse"
        ,"{-# NOINLINE __other #-}", "__other :: [Bool] -> [Bool]", "__other = reverse"],
        ["__result","__other"],True)
      ,("NativeTypeSupport",
        ["{-# LANGUAGE NoImplicitPrelude #-}", "module NativeTypeSupport where"
        ,"data Token = First | Second", "first = First", "second = Second"],
        ["first","second"],False)]

-- Fixture requests pass the production pure admission before compiler binding.
admitFixtureRequest :: WorkerRequest -> AdmittedWorkerRequest
admitFixtureRequest request = either (error . show) id (admitWorkerRequest request)


-- FIFO source and an absent request CWD distinguish pure admission from later
-- filesystem, harness and executor failures through the actual worker binary.
workerRequestAdmissionBeforeEffects :: IO ()
workerRequestAdmissionBeforeEffects = withScratch $ \root -> do
  worker <- lookupEnv "TIDEPOOL_EXTRACT_WORKER" >>= maybe (fail "missing declared worker binary") pure
  inherited <- getEnvironment
  let environment = filter ((`notElem` bootNames) . fst) inherited
      bootNames = ["TIDEPOOL_COMPILER_PACKAGE_PROFILE", "TIDEPOOL_COMPILER_PACKAGE_PROFILE_SHA256"
        ,"TIDEPOOL_COMPILER_SERVICE_EPOCH", "TIDEPOOL_COMPILER_PACKAGE_SOURCE_ID"
        ,"TIDEPOOL_COMPILER_PACKAGE_PROFILE_ID", "TIDEPOOL_COMPILER_WORKER_INSTANCE"
        ,"TIDEPOOL_COMPILER_WORLD_DIRECTORY"]
      fifo = root </> "unreadable-source.hs"
      scratch = root </> "harness-copies"
      bad = workerArgv [Input fifo, OutputDir scratch, HarnessProfile, Turn, CellPlan
        ,TurnOut (root </> "turn.cbor"), CellTemplate "unused", CellOut (root </> "cell.cbor")]
      command arguments = (proc worker arguments) {env=Just environment}
      bounded action = timeout 30000000 action >>= maybe (fail "worker request admission timed out") pure
      noScratch = doesDirectoryExist scratch >>= \exists -> when exists (fail "refused request wrote harness scratch")
      noRequestWork diagnostics = forM_ ["BodyStarted", "CleanupStarted ExecutorStop", "CleanupStarted CapabilitiesRestore"] $ \phase ->
        when (phase `isInfixOf` diagnostics) (fail ("refused request entered " ++ phase))
  [retiredFlag,retiredPayload] <- pure (workerArgv
    [CellPlan, Input fifo, OutputDir scratch, HarnessProfile, CellTemplate "unused", CellOut (root </> "cell.cbor")])
  [_,oneField] <- pure (workerArgv [CellPlan])
  let fieldOffset = length oneField - 2
      retired = [retiredFlag,take fieldOffset retiredPayload ++ "1f" ++ drop (fieldOffset+2) retiredPayload]
      refused = [("conflicting supported operations",bad,Nothing)
        ,("retired finite cell field",retired,Just "retired field tag 31")]
  createNamedPipe fifo (ownerReadMode `unionFileModes` ownerWriteMode)
  forM_ refused $ \(label,arguments,expected) -> do
    (directCode,directOut,directErr) <- bounded (readCreateProcessWithExitCode (command arguments) "")
    unless (directCode /= ExitSuccess && "input-rejected" `isInfixOf` directOut)
      (fail (label ++ " direct request did not retain input rejection"))
    forM_ expected $ \message -> unless (message `isInfixOf` directOut)
      (fail (label ++ " direct request reported another refusal"))
    noRequestWork directErr
    noScratch
  let source = root </> "Classify.hs"
      verdicts = root </> "classified.json"
      good = workerArgv [Input source, Classify, ClassifyOut verdicts]
  writeFile source "let observed = 41\n"
  bounded $ withCreateProcess ((command ["--worker-loop-v2", "+RTS", "-I0", "-RTS"])
      {std_in=CreatePipe,std_out=CreatePipe,std_err=CreatePipe}) $ \input output diagnostics process -> do
    incoming <- maybe (fail "worker stdin missing") pure input
    outgoing <- maybe (fail "worker stdout missing") pure output
    errors <- maybe (fail "worker stderr missing") pure diagnostics
    mapM_ (`hSetBinaryMode` True) [incoming,outgoing,errors]
    drained <- newEmptyMVar
    _ <- forkFinally (BS.hGetContents errors) (putMVar drained)
    let exact count = do
          bytes <- BS.hGet outgoing count
          unless (BS.length bytes == count) (fail "worker admission response was truncated")
          pure bytes
        word value = BS.pack [fromIntegral (value `shiftR` bits) | bits <- [0,8,16,24]]
        readWord = do
          bytes <- exact 4
          pure (foldr (.|.) 0 [fromIntegral (BS.index bytes index) `shiftL` (8*index) | index <- [0..3]] :: Int)
        frame bytes = word (BS.length bytes) <> bytes
        readFrame = do
          size <- readWord
          when (size > 16384) (fail "worker admission response exceeded test bound")
          exact size
        acknowledge commandByte = do
          BS.hPut incoming (BS.singleton commandByte)
          hFlush incoming
          reply <- exact 1
          unless (reply == BS.singleton 1) (fail "worker admission lost BEGIN/END acknowledgement")
        request cwd arguments = do
          BS.hPut incoming (BS.singleton 1 <> frame (BSC.pack cwd) <> word (length arguments)
            <> foldMap (frame . BSC.pack) arguments)
          hFlush incoming
          (,,) <$> readWord <*> readFrame <*> readFrame
    acknowledge 1
    forM_ refused $ \(label,arguments,expected) -> do
      (badCode,badOut,_) <- request (root </> "absent-cwd") arguments
      unless (badCode /= 0 && "input-rejected" `BS.isInfixOf` badOut)
        (fail (label ++ " daemon request performed CWD/source work before admission"))
      forM_ expected $ \message -> unless (BSC.pack message `BS.isInfixOf` badOut)
        (fail (label ++ " daemon request reported another refusal"))
      noScratch
    (goodCode,goodOut,_) <- request root good
    unless (goodCode == 0 && "success" `BS.isInfixOf` goodOut)
      (fail "admitted classification failed after malformed request")
    doesFileExist verdicts >>= \written -> unless written (fail "classification did not write its required output")
    acknowledge 0
    acknowledge 1
    acknowledge 0
    hClose incoming
    code <- waitForProcess process
    unless (code == ExitSuccess) (fail "worker admission history failed to release")
    captured <- readMVar drained >>= either throwIO pure
    let phases = lines (BSC.unpack captured)
        count phase = length (filter (== "tidepool-request-phase-v1 " ++ phase) phases)
    unless (count "BodyStarted" == 1 && count "CleanupStarted ExecutorStop" == 1
        && count "CleanupStarted CapabilitiesRestore" == 1)
      (fail "malformed daemon request allocated an executor or changed RTS capabilities")
  noScratch
