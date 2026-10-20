{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE RankNTypes #-}
{-# LANGUAGE ScopedTypeVariables #-}

module Tidepool.ExactScope
  ( ExactScope, scopeManifestPath, scopeRequestSha256, scopeProducerSha256, scopeSemanticSha256, scopeLexical, scopeProducts, scopeRecipeOriginals, scopeExecutionGraphs, scopeExecutionOwners, scopePurpose, scopeRequestTypes, scopeSourceSelectedOwners, scopePublishedSourceOriginals, scopeCompletedSourceOriginals, scopeInterfaces, scopeInterfaceEvidence
  , extendExactScopeInputs, extendExactScopeGeneration, extendCheckedValueScope, ExactScopePurpose(..), ExactScopeOriginal(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..), SourceSelectedOriginals(..), encodeSourceSelectedOriginals, revalidateSourceSelectedOriginals
  , CheckedCellAdmission(..), CheckedCellPurpose(..), CheckedItemAdmission(..), CheckedItemPurpose(..)
  , ActivationPreviewAdmission(..), scopeActivationPreview
  , PlannedCellAdmission(..), PlannedCellSlot(..)
  , ExactInterfaceEvidence(..), CanonicalOrigin(..), CanonicalInterfaceProof, bindCanonicalProofInputs, CanonicalCoreArtifact
  , CanonicalInterfaceAdmission(ModuleInterfaceAdmission, LocalInterfaceAdmission), scopeCanonicalInterfaces, scopeSourceOriginalInterfaces, scopeModuleInterfaceProofs
  , admittedInterfaceHomeUnits, admittedInterfaceSourceSha256, resolveShippedHomeModule
  , admittedInterfaceRequirements, admittedInterfaceCore, captureCanonicalProofs, CanonicalInterfaceUse(..), canonicalProofInterfaceBytes, canonicalProofInterfaceBody, canonicalProofOriginalBytes, canonicalProofOriginalBody, relocateCanonicalInterfaceProof, revalidateCanonicalProofInputs
  , captureFinalizedSourceOriginals, compilationOriginalSourceImports
  , validateCanonicalInterfaceProof, validateCandidateCanonicalInterfaceProof
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalCertificateBytes, canonicalCoreArtifact
  , canonicalCorePath, canonicalCoreSha256, canonicalHomeUnits, canonicalSourceSha256
  , canonicalRequirements, canonicalOrigin, canonicalSourceImports, isSourceOriginal, normalizeInterfaceEvidence
  , canonicalProofMatchesOwner
  , scopeCheckedCell, scopeCheckedItem, scopeIncludePaths
  , ExactInputOwner, newExactInputOwner, readExactScopeWithOwner
  , exactInputRetentionLimits, commitWorldInputRetention
  , readExactScope, ExactScopeValidationReason(..), revalidateExactScope, revalidateExactScopesAt, validateExactScopeEnvironment, ExactScopeRequestFailure(..), validateExactScopeRequest, scopeValueInterfaces
  , scopeInterfaceBytes, scopeInterfaceToken, scopeOriginalBytes, readScopedInterfaces, readScopedInterfaceClosure, admittedInterfaceCoreBytes, admittedInterfaceCoreBody
  , ExactCompilationPublication(..)
  , writeExactCompilation, writeCheckedExactCompilation
  , writeRetainedExactCompilation
  , writeRetainedExactCompilationWithOutputsAndPublication, extendSourceSelectedOriginals
  , writeCheckedExactCompilationWithOutputsAndPublication
  , FreshOutputSeals, emptyFreshOutputSeals, freshOutputSealsFromWrites, appendFreshOutputSeals, freshOutputSealFacts
  , revalidateExactScopesAtWithOutputs
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget
  , scopeExecutionNativeOwners
  , scopeAvailableOriginalProducts
  , originalGroupFromProjected, originalGroupFromCandidate
  , WorldInputFacts, CapturedWorldSources, WorldSourceRoot
  , WorldBinding, WorldBindingRow, worldInputsBindingScope, worldInputsBindings
  , WorldSourceClosureEntry(..)
  , WorldTypeClosureEntry, readWorldTypeInputs
  , worldTypeEntryArtifact, worldTypeEntryInterface, worldTypeEntryPackages, worldTypeEntryEvidence, worldTypeEntryInterfaceBody
  , VerifiedWorldDeclarationSurface, hydrateWorldDeclarationInputs
  , verifiedWorldDeclarationInterface, verifiedWorldDeclarationExports, verifiedWorldDeclarationTypeInputs
  , worldBindingSymbol, worldBindingSessionVar, worldBindingScope, worldBindingGeneration, worldBindingRow
  , worldBindingRowInterface, worldBindingCertifiedArtifact, worldBindingCompilerReceipt, worldBindingRowFactCID
  , worldBindingInterfaceArtifact, readWorldBindingInputs, worldHasSelectedSource, worldHasSelectedPrimary, readWorldSourceInputs
  , worldInputsIdentity, worldInputsIdentityBytes, worldInputsProducer, worldInputsProfile, worldInputsSources
  , worldInputsSourceModuleHints, worldInputsSamePrimary, worldInputsRetainedCharge, worldInputsPrimaryIsEmpty
  , WorldLexicalSurface, WorldDeclarationSurface, worldInputsLexicalSurface
  , WorldObservationProfile(..), WorldObservationScaffold, worldLexicalObservation
  , worldObservationProfile, worldObservationFragment, worldObservationImports, worldObservationRequestTypes, worldObservationInputType
  , worldLexicalImports, worldLexicalDeclaration, worldDeclarationInterface
  , worldSourceRoots, worldSourceDomainSha256, worldSourceOrigin, worldSourceFiles
  , readWorldInputsWithOwner, readWorldSuccessorWithOwner, WorldSuccessorInputs
  , worldSuccessorFacts, worldSuccessorChangedAliases, decodeWorldSources, readWorldStringMap
  , WorldInputsFailure(..), WorldInputsFailureCause(..)
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Codec.CBOR.Encoding as E
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (Exception, IOException, try, throwIO, evaluate, mask, catch)
import Control.Concurrent.MVar (MVar, newMVar, modifyMVarMasked, readMVar)
import Control.Monad.State.Strict (StateT(..), runStateT, get, put, lift)
import Control.Monad (foldM, forM, forM_, replicateM, unless, when, void)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Crypto.Hash as Crypto
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.Char (isHexDigit)
import Data.IORef (IORef, newIORef, readIORef, modifyIORef', writeIORef)
import Data.List (isPrefixOf, foldl')
import Data.Maybe (isJust, isNothing)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Data.Bits (shiftL, testBit, popCount)
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC.Driver.Env (HscEnv, hsc_home_unit)
import GHC.Types.PkgQual (PkgQual(NoPkgQual))
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Location (ml_hs_file)
import GHC.Unit.Types (unitString)
import Data.Word (Word32, Word64)
import Numeric (showHex)
import System.Directory (createDirectory, createDirectoryIfMissing, makeAbsolute, doesFileExist)
import System.FilePath (isAbsolute, normalise, hasTrailingPathSeparator, takeDirectory, splitDirectories, (</>))
import Tidepool.BoundedRead (readFileAtMost, FileObservations, FileObservation(..), withFileObservations, observeFile, fileObservationTotals)
import System.IO.Error (isAlreadyExistsError, isUserError)
import System.Environment (lookupEnv)
import Text.Read (readMaybe)
import Tidepool.ExactHydration (ExactIfaceArtifact(..), checkedValueOwner, CheckedTemplateInterface(..), CheckedTemplateImports(..), RequestIfaceDecoder, newRequestIfaceDecoder, pruneRequestIfaceDecoder, VerifiedExactIfaceClosure, readCapturedExactIfaceArtifacts, readCapturedExactIfaceClosureWithCheckedValues, selectVerifiedExactInterfaces, hydrateExactScope)
import GHC.Unit.Module.ModIface (ModIface)
import Tidepool.ExtractRequest (PlannedCellSlot(..))
import Tidepool.ArtifactBytes (ArtifactBytes, artifactBytes, artifactSha256, checkArtifactSeal)
import Tidepool.RequestInputs (RequestOriginalInputs, RequestInputReader, RequestInputTokenReader, withCapturedRequestInputTokens, capturedRequestInputToken, requestInputRetained, requestInputCount, retainRequestEncodedBytes, captureRequestInputs, captureRequestInputTokens, mergeRequestInputs, aliasRequestInputs, capturedRequestInput, CapturedOriginalContent, emptyCapturedOriginalContent, OriginalInputReference(..), continueRequestInputs, selectedOriginalContent, capturedOriginalContentBytes, capturedOriginalContentKeys, mergeCapturedOriginalContent, requestCaptureByteLimit, requestInputBytes, requestInputBodies, transferRequestInputBodies, editDirectRequestInputBodies)
import Tidepool.Session (Generation(..), SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString)
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.DeclarationJoin (DeclarationExport(..), decodeDeclarationExports, interfaceExports, normalizeDeclarationExports)
import Tidepool.CheckedCell
  ( CheckedSignature(..), RequestTypeSignatures, RequestHelperRecipe(..), decodeCheckedSignature, decodeRequestTypeSignatures, encodeCheckedSignature, encodeRequestTypeSignatures
  , CellExpressionPlan(..), ExpressionLiftPlan(..), decodeCellExpressionPlan
  , validateCheckedTypeWitnessBytes )
import Tidepool.ExecutionSchema
  ( SymbolIdentity(..), ProjectedGroup(..), ProjectedGroupBody(..), GlobalDecl(..) )
import Tidepool.ModuleCandidates
  ( CandidateGroup(..), CandidateGlobal(..), ModuleCandidate(..)
  , candidateCertificatePath, candidateCertificateSha256, candidateCoreDescriptor, candidateExecutionSources )
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), executionGraphBytes, executionGraphSha256, ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceRef(..), ExecutionSourceNode(..), decodeExecutionSourceDescriptors, decodeExecutionSourceReferences
  , readExecutionSourceGraphsWithFacts, decodeExecutionSourceBody, executionSourceGraphsFit
  , ExecutionSourceFailure(..), executionIdentityKey, executionSourceClosure, executionSourceOriginalNode
  , executionSourceOriginalClosure )
import Tidepool.LocalNativeDeclaration
  ( LocalNativeDeclarationAdmission, localNativeOwner, localNativeProof )
import Tidepool.PackageWitness
  ( AdmittedPackageImports, PackageImportEvidence, decodeCapturedPackageImports, emptyAdmittedPackageImports, extendAdmittedPackageImportsWith, extendAdmittedPackageImportsWithFacts
  , revalidateAdmittedPackageImports, validateAdmittedPackageSelection )
import Tidepool.OwnedInputTransport (OriginalInputKind(..), OriginalInputImage(..), InputAcquisition(..), decodeInputAcquisition)
import Tidepool.NativeOriginalCensus
  ( OriginalNativeCensus, readOriginalNativeCensusWith, decodeOriginalNativeCensusBody, nativeCensusOwner
  , ExactOriginalGroup(..), nativeCensusExactGroups, selectNativeCensusGroups, nativeCensusSelectionMatches
  , nativeCensusRequirements, nativeCensusCanonicalCertificate )
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, finalizedValueInterfaceSeals, finalizedLocalAdmissions, LocalFinalizedAdmission, localFinalizedInterface, localFinalizedHomeUnits
  , localFinalizedSourceSha256, localFinalizedRequirements, localFinalizedCore, localFinalizedInterfaceBody, localFinalizedPackageBody, localFinalizedCoreBody
  , revalidateLocalFinalizedAdmission )
import Tidepool.Timing (readTimingEnabled, readSummaryTimingEnabled, timeDetailPhase, emitCount, withValidationTiming)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyModule(..), DependencyImport(..), DependencyResolution(..), renderDependencyEvidence
  , DependencyQualifier(..), renderDependencyQualifier, parseDependencyQualifier, revalidateDependencyEvidence )

data ExactScope = ExactScope
  { ownedScopeManifestPath :: FilePath
  , ownedScopeRequestSha256 :: String
  , ownedScopeProducerSha256 :: String
  , ownedScopeSemanticSha256 :: String
  , scopeInputs :: AdmittedScopeInputs
  , ownedScopeLexical :: [((String, String), [(String, String)])]
  , ownedScopeProducts :: [ExactScopeOriginal]
  , ownedScopeExecutionGraphs :: [ExecutionSourceGraph]
  , ownedScopeExecutionOwners :: [ExecutionSourceRef]
  , ownedScopePurpose :: ExactScopePurpose
  , ownedScopeRequestTypes :: Maybe (RequestHelperRecipe, RequestTypeSignatures)
  -- Request-local source proof roots are revalidated by subsequent stages;
  -- they are never serialized as baseline lexical authority.
  , ownedScopeSourceSelectedOwners :: Set.Set (String,String)
  , ownedScopePublishedSourceOriginals :: Set.Set (String,String)
  , ownedScopeSourceOriginalDependencies :: Set.Set (String,String)
  } deriving (Eq, Show)

-- Ordinary accessors expose facts without exporting record-update labels.
scopeManifestPath :: ExactScope -> FilePath
scopeManifestPath = ownedScopeManifestPath

scopeRequestSha256 :: ExactScope -> String
scopeRequestSha256 = ownedScopeRequestSha256

scopeProducerSha256 :: ExactScope -> String
scopeProducerSha256 = ownedScopeProducerSha256

scopeSemanticSha256 :: ExactScope -> String
scopeSemanticSha256 = ownedScopeSemanticSha256

scopeLexical :: ExactScope -> [((String, String), [(String, String)])]
scopeLexical = ownedScopeLexical

scopeProducts :: ExactScope -> [ExactProduct]
scopeProducts = map scopeOriginalProduct . ownedScopeProducts

-- Physical body availability and compiler namespace ownership are independent.
-- Only the request or a checked current-product receipt issues this distinction.
data ExactScopeOriginal = CompilerOriginal ExactProduct | PrivateNative ExactProduct
  deriving (Eq, Show)

data NativeInputClassification = CompilerOriginalInput | PrivateNativeInput

classifyOriginal :: NativeInputClassification -> ExactProduct -> ExactScopeOriginal
classifyOriginal CompilerOriginalInput = CompilerOriginal
classifyOriginal PrivateNativeInput = PrivateNative

scopeOriginalProduct :: ExactScopeOriginal -> ExactProduct
scopeOriginalProduct (CompilerOriginal product) = product
scopeOriginalProduct (PrivateNative product) = product

scopeRecipeOriginals :: ExactScope -> [ExactProduct]
scopeRecipeOriginals scope = [product | CompilerOriginal product <- ownedScopeProducts scope]

scopeExecutionGraphs :: ExactScope -> [ExecutionSourceGraph]
scopeExecutionGraphs = ownedScopeExecutionGraphs

scopeExecutionOwners :: ExactScope -> [ExecutionSourceRef]
scopeExecutionOwners = ownedScopeExecutionOwners

scopePurpose :: ExactScope -> ExactScopePurpose
scopePurpose = ownedScopePurpose

scopeRequestTypes :: ExactScope -> Maybe (RequestHelperRecipe, RequestTypeSignatures)
scopeRequestTypes = ownedScopeRequestTypes

scopeSourceSelectedOwners :: ExactScope -> Set.Set (String,String)
scopeSourceSelectedOwners = ownedScopeSourceSelectedOwners

scopePublishedSourceOriginals :: ExactScope -> Set.Set (String,String)
scopePublishedSourceOriginals = ownedScopePublishedSourceOriginals

scopeCompletedSourceOriginals :: ExactScope -> Set.Set (String,String)
scopeCompletedSourceOriginals scope = ownedScopePublishedSourceOriginals scope
  `Set.union` ownedScopeSourceOriginalDependencies scope

-- One checked purpose owns its admission and ordered search inputs. Native
-- request-type evidence is an independent wrapper, not another checked stage.
data ExactScopePurpose
  = NoCheckedPurpose
  | ExactCellPurpose CheckedCellAdmission [FilePath]
  | ExactItemPurpose CheckedItemAdmission [FilePath]
  | ExactInspectionPurpose [ExactIfaceArtifact] [FilePath]
  | ExactReloadInspectionPurpose [ExactIfaceArtifact] [FilePath]
  | ExactActivationPreviewPurpose ActivationPreviewAdmission [FilePath]
  deriving (Eq, Show)

-- Renderer issuance retains the original type and instance environment. It
-- grants no mounted-value, authored completion or invocation authority.
data ActivationPreviewAdmission = ActivationPreviewAdmission
  { previewOriginalContextDigest :: String
  , previewBudget :: Word64
  , previewTemplateSha256 :: String
  , previewInputSignature :: CheckedSignature
  , previewInputWitness :: BS.ByteString
  , previewOriginalInterfaces :: [CheckedTemplateInterface]
  , previewOriginalTarget :: (String,String)
  } deriving (Eq, Show)

scopeActivationPreview :: ExactScope -> Maybe ActivationPreviewAdmission
scopeActivationPreview scope = case scopePurpose scope of
  ExactActivationPreviewPurpose admission _ -> Just admission
  _ -> Nothing

scopeCheckedCell :: ExactScope -> Maybe CheckedCellAdmission
scopeCheckedCell scope = case scopePurpose scope of
  ExactCellPurpose admission _ -> Just admission
  _ -> Nothing

scopeCheckedItem :: ExactScope -> Maybe CheckedItemAdmission
scopeCheckedItem scope = case scopePurpose scope of
  ExactItemPurpose admission _ -> Just admission
  _ -> Nothing

scopeIncludePaths :: ExactScope -> Maybe [FilePath]
scopeIncludePaths scope = case scopePurpose scope of
  NoCheckedPurpose -> Nothing
  ExactCellPurpose _ paths -> Just paths
  ExactItemPurpose _ paths -> Just paths
  ExactInspectionPurpose _ paths -> Just paths
  ExactReloadInspectionPurpose _ paths -> Just paths
  ExactActivationPreviewPurpose _ paths -> Just paths

-- Canonical proof belongs to its exact interface row. Core is a separate
-- compiler-input capability, never an imported declaration or native grant.
data CanonicalCoreArtifact = CanonicalCoreArtifact FilePath String deriving (Eq, Show)

canonicalCorePath :: CanonicalCoreArtifact -> FilePath
canonicalCorePath (CanonicalCoreArtifact path _) = path

canonicalCoreSha256 :: CanonicalCoreArtifact -> String
canonicalCoreSha256 (CanonicalCoreArtifact _ sha) = sha

type CanonicalSourceImport = (DependencyQualifier,String,Bool,Maybe String)

data CanonicalOrigin = SourceOriginal [CanonicalSourceImport] | NativeAuthoredDeclaration Generation
  deriving (Eq, Show)

isSourceOriginal :: CanonicalOrigin -> Bool
isSourceOriginal (SourceOriginal _) = True
isSourceOriginal _ = False

data CanonicalInterfaceRole = SourceOriginalRole | NativeDeclarationRole
  deriving (Eq, Show)

-- Candidate promotion validates a carrier without granting a scope role.
data CanonicalInterfacePurpose = ScopeInterface CanonicalInterfaceRole | CandidateCarrier
  deriving (Eq, Show)

originRole :: CanonicalOrigin -> CanonicalInterfaceRole
originRole (SourceOriginal _) = SourceOriginalRole
originRole (NativeAuthoredDeclaration _) = NativeDeclarationRole

data CanonicalInterfaceDescriptor = CanonicalInterfaceDescriptor
  { descriptorCertificatePath :: FilePath
  , descriptorCertificateSha256 :: String
  , descriptorCore :: Maybe CanonicalCoreArtifact
  , descriptorPurpose :: CanonicalInterfacePurpose
  } deriving (Eq, Show)

data CanonicalInterfaceProof = CanonicalInterfaceProof
  { proofCertificatePath :: FilePath
  , proofCertificateSha256 :: String
  , proofCoreArtifact :: Maybe CanonicalCoreArtifact
  , proofInterfaceInput :: InterfaceRow
  , proofNativeInput :: Maybe (FilePath,String)
  , proofExecutionGraphs :: [ExecutionSourceGraph]
  , proofInputCustody :: CanonicalInputCustody
  , canonicalFacts :: CanonicalModuleCertificate
  }

-- Opaque terminal evidence issued alongside files by their materialization
-- owner. Paths and expected bytes come from that same writer; callers cannot
-- silently reconstruct a fresh-output set from the final scope inventory.
newtype FreshOutputSeals = FreshOutputSeals [(FilePath,String,Maybe Int)]

-- At most 128 retained candidates issue three copies; 128 local finalized
-- owners issue three payloads plus one canonical certificate; and 4096
-- completed value interfaces issue two payloads. The fixed inventory writes
-- add fewer than ten entries; 16384 covers that schema-derived sum.
maxFreshOutputEntries :: Int
maxFreshOutputEntries = 16384

-- Match the default request-capture guard. More specific producers impose
-- tighter per-kind and aggregate limits before issuing these terminal seals.
maxFreshOutputBytes :: Integer
maxFreshOutputBytes = 4 * 1024 * 1024 * 1024

emptyFreshOutputSeals :: FreshOutputSeals
emptyFreshOutputSeals = FreshOutputSeals []

freshOutputSealFacts :: FreshOutputSeals -> [(FilePath,String)]
freshOutputSealFacts (FreshOutputSeals outputs) = [(path,sha) | (path,sha,_) <- outputs]

freshOutputSealsFromWrites :: [(FilePath,BS.ByteString)] -> Either String FreshOutputSeals
freshOutputSealsFromWrites writes
  | any (not . allEqual) (Map.elems grouped) =
      Left "fresh output writer issued conflicting bytes for one path"
  | length unique > maxFreshOutputEntries = Left "fresh output inventory exceeds its entry bound"
  | totalBytes > maxFreshOutputBytes = Left "fresh output inventory exceeds its byte bound"
  | any (BS.null . snd) unique = Left "fresh output inventory contains an empty artifact"
  | otherwise = Right (FreshOutputSeals [(path,digest bytes,Just (BS.length bytes)) | (path,bytes) <- unique])
  where
    grouped = Map.fromListWith (++) [(path,[bytes]) | (path,bytes) <- writes]
    allEqual [] = True
    allEqual (first:rest) = all (== first) rest
    unique = Map.toAscList (Map.mapMaybe first grouped)
    first [] = Nothing
    first (bytes:_) = Just bytes
    totalBytes = sum (map (toInteger . BS.length . snd) unique)

appendFreshOutputSeals :: FreshOutputSeals -> FreshOutputSeals -> Either String FreshOutputSeals
appendFreshOutputSeals (FreshOutputSeals left) (FreshOutputSeals right)
  | conflicts = Left "fresh output seals conflict for one path"
  | length joined > maxFreshOutputEntries = Left "fresh output inventory exceeds its entry bound"
  | totalBytes > maxFreshOutputBytes = Left "fresh output inventory exceeds its byte bound"
  | otherwise = Right (FreshOutputSeals joined)
  where
    grouped = Map.fromListWith (++) [(path,[(sha,bound)]) | (path,sha,bound) <- left ++ right]
    consistent [] = Nothing
    consistent (first:rest)
      | all (== first) rest = Just first
      | otherwise = Nothing
    joined = [(path,sha,bound) | (path,records) <- Map.toAscList grouped
      , Just (sha,bound) <- [consistent records]]
    conflicts = any (isNothing . consistent) (Map.elems grouped)
    totalBytes = sum [toInteger size | (_,_,Just size) <- joined]

-- Metadata validation grants no executable bytes. Promotion and exact-scope
-- admission bind a proof to the single budgeted request input owner.
data CanonicalInputCustody = DescriptorInputs | CapturedCanonicalInputs (Map.Map FilePath ArtifactBytes)

-- Encoded Core is excluded from proof comparisons: its artifact digest already
-- certifies equality. Comparing the retained payload would turn every scope
-- comparison into another full byte traversal.
instance Eq CanonicalInterfaceProof where
  a == b = proofCertificatePath a == proofCertificatePath b
    && proofCertificateSha256 a == proofCertificateSha256 b
    && proofCoreArtifact a == proofCoreArtifact b
    && canonicalFacts a == canonicalFacts b

instance Show CanonicalInterfaceProof where
  show proof = "CanonicalInterfaceProof " ++ show
    (proofCertificatePath proof,proofCertificateSha256 proof,proofCoreArtifact proof,canonicalFacts proof)

canonicalCertificatePath :: CanonicalInterfaceProof -> FilePath
canonicalCertificatePath = proofCertificatePath

canonicalCertificateSha256 :: CanonicalInterfaceProof -> String
canonicalCertificateSha256 = proofCertificateSha256

canonicalCertificateBytes :: CanonicalInterfaceProof -> BS.ByteString
canonicalCertificateBytes = toStrictByteString . encodeCanonicalModuleCertificate . canonicalFacts

canonicalCoreArtifact :: CanonicalInterfaceProof -> Maybe CanonicalCoreArtifact
canonicalCoreArtifact = proofCoreArtifact

canonicalHomeUnits :: CanonicalInterfaceProof -> Set.Set String
canonicalHomeUnits = certificateHomeUnits . canonicalFacts

canonicalSourceSha256 :: CanonicalInterfaceProof -> String
canonicalSourceSha256 = certificateSource . canonicalFacts

canonicalProofMatchesOwner :: String -> (String, String) -> CanonicalInterfaceProof -> Bool
canonicalProofMatchesOwner producer owner proof =
  certificateProducer (canonicalFacts proof) == producer
    && certificateOwner (canonicalFacts proof) == owner

canonicalRequirements :: CanonicalInterfaceProof -> Map.Map (String,String) String
canonicalRequirements = certificateRequirements . canonicalFacts

canonicalOrigin :: CanonicalInterfaceProof -> CanonicalOrigin
canonicalOrigin = certificateOrigin . canonicalFacts

canonicalSourceImports :: CanonicalInterfaceProof -> Maybe [CanonicalSourceImport]
canonicalSourceImports proof = case canonicalOrigin proof of
  SourceOriginal imports -> Just imports
  _ -> Nothing

data ExactInterfaceEvidence
  = ModuleInterfaceEvidence CanonicalInterfaceProof
  | LocalNativeDeclarationEvidence LocalNativeDeclarationAdmission
  | LexicalJoinEvidence
  | CheckedValueEvidence
  deriving (Eq, Show)

-- Completed module identity excludes custody locations, including relocated
-- certificate/Core files. Complete certificate facts contain no such paths.
normalizeInterfaceEvidence :: ExactInterfaceEvidence -> ExactInterfaceEvidence
normalizeInterfaceEvidence (ModuleInterfaceEvidence proof) = ModuleInterfaceEvidence (proof
  { proofCertificatePath="", proofInputCustody=DescriptorInputs
  , proofCoreArtifact=(\(CanonicalCoreArtifact _ sha) -> CanonicalCoreArtifact "" sha) <$> proofCoreArtifact proof })
normalizeInterfaceEvidence evidence = evidence

type InterfaceRow = (ExactIfaceArtifact,FilePath,String)
type InterfaceOwner = (String,String)

-- Rows and their admitted role have one owner. The ordered index preserves
-- transport order; package facts are issued once from their sealed sidecars.
data ScopeInterfaceInput = ScopeInterfaceInput InterfaceRow ExactInterfaceEvidence
  deriving (Eq, Show)
-- Native availability is an immutable projection of the original receipt. It
-- adds no selected executable groups to the scope.
data NativeAvailability = FullCarrier | IssuedGroups [ExactOriginalGroup]
  deriving (Eq, Show)
data AdmittedOriginalCensus = AdmittedOriginalCensus
  FilePath String ExactProduct OriginalNativeCensus NativeAvailability
  deriving (Eq, Show)
data AdmittedScopeInputs = AdmittedScopeInputs [InterfaceOwner]
  (Map.Map InterfaceOwner ScopeInterfaceInput) AdmittedPackageImports
  (Map.Map InterfaceOwner AdmittedOriginalCensus) (Maybe CapturedScopeInputs) deriving Show

data CapturedScopeInputs = CapturedScopeInputs RequestOriginalInputs RequestIfaceDecoder deriving Show

-- One compiler universe retains immutable original content independently of
-- completed environment variants. Each physical invocation still issues its
-- own exact selection, allowance, paths and publication obligations.
data ExactInputOwner = ExactInputOwner Integer (MVar RetainedOriginalInputs)
data RetainedOriginalInputs = RetainedOriginalInputs
  [(String,CapturedOriginalContent)] RequestIfaceDecoder DecodedOriginalFacts !Integer !Int

newExactInputOwner :: IO ExactInputOwner
newExactInputOwner = do
  allowance <- requestCaptureByteLimit
  configured <- lookupEnv "TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES"
  retention <- case configured of
    Nothing -> pure (128 * 1024 * 1024)
    Just value -> case readMaybe value of
      Just amount | amount >= 0 -> pure amount
      _ -> fail "TIDEPOOL_RETAINED_ORIGINAL_INPUT_BYTES must be a nonnegative integer"
  decoder <- newRequestIfaceDecoder
  -- Inactive acceleration is deliberately smaller than a request allowance.
  -- RSS admission/rotation continues to own total GHC residency.
  ExactInputOwner (min allowance retention)
    <$> newMVar (RetainedOriginalInputs [] decoder emptyDecodedOriginalFacts 0 0)

-- Original bytes and optional world backing consume this one allowance. The
-- world index belongs to CompilerUniverse; this owner stores only its charge.
exactInputRetentionLimits :: ExactInputOwner -> (Integer,Int)
exactInputRetentionLimits (ExactInputOwner limit _) = (limit,maxRetainedInputEntries)

-- The existing optional-image metadata bound also counts retained world raw
-- entries and copied filesystem nodes. Active receiving custody is separate.
maxRetainedInputEntries :: Int
maxRetainedInputEntries = 4096

selectRetainedOriginalInputs :: Integer -> Integer -> Int
  -> [(String,CapturedOriginalContent)] -> [(String,CapturedOriginalContent)]
selectRetainedOriginalInputs limit worldBytes worldCount =
  take (max 0 (maxRetainedInputEntries - worldCount)) . keep emptyCapturedOriginalContent
  where
    keep _ [] = []
    keep content (entry@(_,more):rest)
      | capturedOriginalContentBytes combined + worldBytes <= limit = entry : keep combined rest
      | otherwise = keep content rest
      where combined = mergeCapturedOriginalContent content more

-- The sole world consumer preforces its successor before entering this masked
-- commit. Publication performs only nonblocking owner writes after decoder
-- pruning, just like exact-scope raw/decoder publication below.
commitWorldInputRetention :: ExactInputOwner -> Integer -> Int -> IO () -> IO ()
commitWorldInputRetention (ExactInputOwner limit state) worldBytes worldCount publish =
  modifyMVarMasked state $ \(RetainedOriginalInputs retained decoder
      (DecodedOriginalFacts certificates packages native graphs) _ _) -> do
    unless (worldBytes >= 0 && worldBytes <= limit && worldCount >= 0 && worldCount <= maxRetainedInputEntries)
      (fail "optional world retention exceeds the shared input allowance")
    let settled = selectRetainedOriginalInputs limit worldBytes worldCount retained
        content = foldl (\known (_,more) -> mergeCapturedOriginalContent known more)
          emptyCapturedOriginalContent settled
        seals = Set.map fst (capturedOriginalContentKeys content)
        certificateFacts = Map.restrictKeys certificates seals
        packageFacts = Map.filterWithKey (\(sha,_,_,_) _ -> sha `Set.member` seals) packages
        nativeFacts = Map.restrictKeys native seals
        graphFacts = Map.restrictKeys graphs seals
        next = RetainedOriginalInputs settled decoder
          (DecodedOriginalFacts certificateFacts packageFacts nativeFacts graphFacts) worldBytes worldCount
    _ <- evaluate (length settled + Set.size seals + Map.size certificateFacts
      + Map.size packageFacts + Map.size nativeFacts + Map.size graphFacts)
    _ <- evaluate (capturedOriginalContentBytes content)
    _ <- evaluate next
    ready <- evaluate (next,())
    _ <- pruneRequestIfaceDecoder seals decoder
    publish
    pure ready

data DecodedOriginalFacts = DecodedOriginalFacts
  (Map.Map String CanonicalModuleCertificate)
  (Map.Map (String,String,String,String) PackageImportEvidence)
  (Map.Map String OriginalNativeCensus)
  (Map.Map String ExecutionSourceGraph)

emptyDecodedOriginalFacts :: DecodedOriginalFacts
emptyDecodedOriginalFacts = DecodedOriginalFacts Map.empty Map.empty Map.empty Map.empty

memoOriginalFact :: Ord key => IORef (Map.Map String Integer) -> Bool -> String -> IORef (Map.Map key value)
  -> key -> IO value -> IO value
memoOriginalFact counts timing label state key acquire = do
  let counted suffix = do
        let metric = label ++ suffix
        modifyIORef' counts (Map.adjust (+ 1) metric)
        emitCount timing metric 1
  facts <- readIORef state
  case Map.lookup key facts of
    Just fact -> counted ".hits" >> pure fact
    Nothing -> do
      fact <- acquire
      modifyIORef' state (Map.insert key fact)
      counted ".misses"
      pure fact


instance Eq AdmittedScopeInputs where
  AdmittedScopeInputs orderA inputsA rootsA censusA _ == AdmittedScopeInputs orderB inputsB rootsB censusB _ =
    orderA == orderB && inputsA == inputsB && rootsA == rootsB && censusA == censusB

scopeCapturedInputs :: ExactScope -> Maybe RequestOriginalInputs
scopeCapturedInputs scope = let AdmittedScopeInputs _ _ _ _ custody = scopeInputs scope
  in (\(CapturedScopeInputs inputs _) -> inputs) <$> custody

scopeIfaceDecoder :: ExactScope -> IO RequestIfaceDecoder
scopeIfaceDecoder scope = let AdmittedScopeInputs _ _ _ _ custody = scopeInputs scope
  in maybe (fail "exact scope decoder custody is not sealed") (\(CapturedScopeInputs _ decoder) -> pure decoder) custody

retainCapturedInputs :: RequestOriginalInputs -> ExactScope -> IO ExactScope
retainCapturedInputs custody scope = do
  let AdmittedScopeInputs _ _ _ _ previous = scopeInputs scope
  decoder <- maybe newRequestIfaceDecoder (\(CapturedScopeInputs _ retained) -> pure retained) previous
  pure (retainCapturedInputsUsing custody decoder scope)

retainCapturedInputsUsing :: RequestOriginalInputs -> RequestIfaceDecoder -> ExactScope -> ExactScope
retainCapturedInputsUsing custody decoder scope =
  let AdmittedScopeInputs order inputs roots census _ = scopeInputs scope
  in scope {scopeInputs=AdmittedScopeInputs order (bindCapturedProofs custody inputs) roots census (Just (CapturedScopeInputs custody decoder))}

bindCapturedProofs :: RequestOriginalInputs -> Map.Map InterfaceOwner ScopeInterfaceInput
  -> Map.Map InterfaceOwner ScopeInterfaceInput
bindCapturedProofs custody = Map.map bind
  where
    available = requestInputBodies custody
    bind (ScopeInterfaceInput row (ModuleInterfaceEvidence proof)) =
      ScopeInterfaceInput row (ModuleInterfaceEvidence (bindCanonicalBodies available proof))
    bind input = input

-- Each semantic admission retains only its own selected bodies. The receiving
-- request keeps allowance and current selection obligations separately.
bindCanonicalProofInputs :: RequestOriginalInputs -> CanonicalInterfaceProof -> CanonicalInterfaceProof
bindCanonicalProofInputs custody = bindCanonicalBodies (requestInputBodies custody)

bindCanonicalBodies :: Map.Map FilePath ArtifactBytes -> CanonicalInterfaceProof -> CanonicalInterfaceProof
bindCanonicalBodies available proof = proof {proofInputCustody=CapturedCanonicalInputs selected}
  where
    selected = Map.fromList [(path,body) | (path,sha) <- canonicalInputSeals proof
      , Just body <- [Map.lookup path available], artifactSha256 body == sha]

canonicalInputSeals :: CanonicalInterfaceProof -> [(FilePath,String)]
canonicalInputSeals proof =
  let (iface,packages,packageSha) = proofInterfaceInput proof
  in [(exactPath iface,exactSha256 iface),(packages,packageSha)
     ,(canonicalCertificatePath proof,canonicalCertificateSha256 proof)]
     ++ maybe [] (\core -> [(canonicalCorePath core,canonicalCoreSha256 core)]) (canonicalCoreArtifact proof)
     ++ maybe [] pure (proofNativeInput proof)

canonicalBodies :: CanonicalInterfaceProof -> [(FilePath,ArtifactBytes)]
canonicalBodies proof = case proofInputCustody proof of
  CapturedCanonicalInputs bodies -> Map.toAscList bodies
  DescriptorInputs -> []

scopeInterfaces :: ExactScope -> [InterfaceRow]
scopeInterfaces scope = inputRows (scopeInputs scope)

inputRows :: AdmittedScopeInputs -> [InterfaceRow]
inputRows (AdmittedScopeInputs order inputs _ _ _) =
  [row | owner <- order, let ScopeInterfaceInput row _ = inputs Map.! owner]

scopeInterfaceEvidence :: ExactScope -> Map.Map InterfaceOwner ExactInterfaceEvidence
scopeInterfaceEvidence = inputEvidence . scopeInputs

inputEvidence :: AdmittedScopeInputs -> Map.Map InterfaceOwner ExactInterfaceEvidence
inputEvidence (AdmittedScopeInputs _ inputs _ _ _) = Map.map (\(ScopeInterfaceInput _ evidence) -> evidence) inputs

makeScopeInputs :: [InterfaceRow] -> Map.Map InterfaceOwner ExactInterfaceEvidence
  -> AdmittedPackageImports -> AdmittedScopeInputs
makeScopeInputs rows evidence roots = AdmittedScopeInputs (map rowOwner rows)
  (Map.fromList [(rowOwner row,ScopeInterfaceInput row (evidence Map.! rowOwner row)) | row <- rows]) roots Map.empty Nothing

scopeAvailableOriginalProducts :: ExactScope -> [ExactProduct]
scopeAvailableOriginalProducts scope =
  [case Map.lookup (originalUnit selected,originalModule selected) census of
     Just (AdmittedOriginalCensus _ _ full _ availability) -> availableNativeProduct full availability
     -- Request-private additions already carry the full groups issued by the
     -- current original inventory. They need no fabricated external receipt.
     Nothing -> selected
  | selected <- scopeProducts scope]
  where AdmittedScopeInputs _ _ _ census _ = scopeInputs scope

availableNativeProduct :: ExactProduct -> NativeAvailability -> ExactProduct
availableNativeProduct full FullCarrier = full
availableNativeProduct full (IssuedGroups groups) = full {originalGroups=groups}

validateNativeSelection :: ExactProduct -> ExactProduct -> OriginalNativeCensus -> NativeAvailability -> IO ()
validateNativeSelection selected full census availability = do
  unless (selected {originalGroups=[]} == full {originalGroups=[]})
    (fail "selected native product differs from its admitted full carrier")
  unless (nativeCensusSelectionMatches census (originalGroups selected))
    (fail "selected native groups differ from their authenticated census")
  let available = Set.fromList (map originalOrdinal (originalGroups (availableNativeProduct full availability)))
  unless (Set.fromList (map originalOrdinal (originalGroups selected)) `Set.isSubsetOf` available)
    (fail "selected native groups exceed their issued availability")

newtype NativeOrdinalSelection = NativeOrdinalSelection [Word]

data OfferedNativeAvailability = OfferedFullCarrier | OfferedIssuedGroups NativeOrdinalSelection

type OfferedNativeProduct = (ExactProduct, NativeOrdinalSelection, (FilePath,String), OfferedNativeAvailability, NativeInputClassification)

admitOriginalCensusWith :: RequestInputReader -> String -> Map.Map InterfaceOwner ExactInterfaceEvidence
  -> [OfferedNativeProduct] -> IO (Map.Map InterfaceOwner AdmittedOriginalCensus)
admitOriginalCensusWith readInput = admitOriginalCensusUsing (readOriginalNativeCensusWith readInput)

admitOriginalCensusUsing :: (FilePath -> String -> IO OriginalNativeCensus) -> String
  -> Map.Map InterfaceOwner ExactInterfaceEvidence -> [OfferedNativeProduct]
  -> IO (Map.Map InterfaceOwner AdmittedOriginalCensus)
admitOriginalCensusUsing readFacts producer evidence offered = Map.fromList <$> forM offered
  (\(selected,_,(path,sha),offeredAvailability,_) -> do
    native <- readFacts path sha
    let key = (originalUnit selected,originalModule selected)
        expectedOwner = (originalUnit selected,originalModule selected,originalVersion selected,
          originalIfaceSha256 selected,originalProductSha256 selected)
    unless (nativeCensusOwner native == expectedOwner)
      (fail ("original native certification differs from its exact carrier: " ++ show key))
    case Map.lookup key evidence of
      Just (ModuleInterfaceEvidence proof)
        | canonicalProofMatchesOwner producer key proof
        , nativeCensusCanonicalCertificate native == Just (canonicalCertificateSha256 proof)
        , nativeCensusRequirements native == canonicalRequirements proof -> pure ()
      _ -> fail ("original native certification differs from its canonical owner: " ++ show key)
    availability <- case offeredAvailability of
      OfferedFullCarrier -> pure FullCarrier
      OfferedIssuedGroups (NativeOrdinalSelection ordinals) ->
        IssuedGroups <$> either fail pure (selectNativeCensusGroups native ordinals)
    let full = selected {originalGroups=nativeCensusExactGroups native}
    pure (key,AdmittedOriginalCensus path sha full native availability))

selectedCensusProducts :: [OfferedNativeProduct] -> Map.Map InterfaceOwner AdmittedOriginalCensus -> IO [ExactScopeOriginal]
selectedCensusProducts offered admitted = forM offered $ \(product,NativeOrdinalSelection ordinals,_,_,classify) -> do
  let key = (originalUnit product,originalModule product)
  case Map.lookup key admitted of
    Just (AdmittedOriginalCensus _ _ full census availability) -> do
      groups <- either fail pure (selectNativeCensusGroups census ordinals)
      let selected = product {originalGroups=groups}
      validateNativeSelection selected full census availability
      pure (classifyOriginal classify selected)
    Nothing -> fail "selected native owner lacks its authenticated census"

rowOwner :: InterfaceRow -> InterfaceOwner
rowOwner (iface,_,_) = (exactUnit iface,exactModule iface)

-- Batch additions before checking requirements: an original can depend on a
-- later new owner. No partially assembled scope reaches a compiler consumer.
extendExactScopeInputs :: ExactScope -> [(InterfaceRow,ExactInterfaceEvidence)]
  -> IO (Either String ExactScope)
extendExactScopeInputs scope offered = do
  checked <- try (do
    let AdmittedScopeInputs order previous roots census custody = scopeInputs scope
    (inputs,added) <- foldM insert (previous,[]) offered
    let prospective = AdmittedScopeInputs (order ++ map (rowOwner . fst) added) inputs roots census custody
    validateInputClosure (scopeProducerSha256 scope) prospective (scopeProducts scope)
    (_,base) <- captureRequestInputs (scopeCapturedInputs scope) (const (pure ()))
    transferred <- either fail pure (transferRequestInputBodies (concat [canonicalBodies proof | (_,ModuleInterfaceEvidence proof) <- offered]
      ++ concat [localBodies (localNativeProof native) | (_,LocalNativeDeclarationEvidence native) <- offered]) base)
    (union,captured) <- captureRequestInputTokens (Just transferred) $ \readToken -> do
      forM_ added $ \((iface,packages,sha),evidence) -> do
        let capture reader = do
              let readInput path bound = artifactBytes <$> reader path bound
              payload <- readInput (exactPath iface) (32 * 1024 * 1024)
              unless (digest payload == exactSha256 iface) (fail "extended interface bytes differ from their seal")
              packageBytes <- readInput packages (4 * 1024 * 1024)
              unless (digest packageBytes == sha) (fail "extended package imports differ from their seal")
              case evidence of
                ModuleInterfaceEvidence proof -> do
                  certificate <- readInput (canonicalCertificatePath proof) (4 * 1024 * 1024)
                  unless (digest certificate == canonicalCertificateSha256 proof) (fail "extended canonical certificate changed")
                  forM_ (canonicalCoreArtifact proof) $ \core -> do
                    bytes <- readInput (canonicalCorePath core) (32 * 1024 * 1024)
                    unless (digest bytes == canonicalCoreSha256 core) (fail "extended defining Core changed")
                LocalNativeDeclarationEvidence native -> do
                  -- The native issuer owns its complete proof, including source and
                  -- finalized Core linkage. Capture follows that current admission.
                  revalidateLocalFinalizedAdmission (localNativeProof native) >>= either fail pure
                  forM_ (localFinalizedCore (localNativeProof native)) $ \(path,seal) -> do
                    bytes <- readInput path (32 * 1024 * 1024)
                    unless (digest bytes == seal) (fail "extended local defining Core changed")
                _ -> pure ()
        case evidence of
          ModuleInterfaceEvidence proof -> withCanonicalInputReader transferred proof readToken capture
          _ -> capture readToken
      let readInput path bound = artifactBytes <$> readToken path bound
      forM_ [product | product <- scopeProducts scope
          , not (maybe False (\inputs -> requestInputRetained inputs (originalProductPath product)
              (originalProductSha256 product)) (scopeCapturedInputs scope))] $ \product -> do
        bytes <- readInput (originalProductPath product) (64 * 1024 * 1024)
        unless (digest bytes == originalProductSha256 product) (fail "extended original product changed")
      extendAdmittedPackageImportsWith readInput roots (map fst added) >>= either fail pure
    retainCapturedInputs captured scope {scopeInputs=AdmittedScopeInputs (order ++ map (rowOwner . fst) added) inputs union census custody})
    :: IO (Either IOException ExactScope)
  pure (either (Left . show) Right checked)
  where
    insert (selected,added) (row,evidence) = case Map.lookup (rowOwner row) selected of
      Nothing -> pure (Map.insert (rowOwner row) (ScopeInterfaceInput row evidence) selected,
        added ++ [(row,evidence)])
      Just old | old == ScopeInterfaceInput row evidence -> pure (selected,added)
      _ -> fail "exact input extension conflicts with an admitted original owner"

-- Complete additions form one immutable generation. Product and lexical roots
-- cannot be published independently of the interface evidence that owns them.
extendExactScopeGeneration :: ExactScope -> [(InterfaceRow,ExactInterfaceEvidence)]
  -> [ExactScopeOriginal] -> [((String,String),[(String,String)])]
  -> IO (Either String ExactScope)
extendExactScopeGeneration scope offered products lexical = do
  let oldProducts = Map.fromList [((originalUnit p,originalModule p),entry) | entry <- ownedScopeProducts scope, let p = scopeOriginalProduct entry]
      oldLexical = Map.fromList (scopeLexical scope)
      merge selected values = foldM (\known (key,value) -> case Map.lookup key known of
        Just previous | previous /= value -> Left "scope generation replaces an admitted owner"
        _ -> Right (Map.insert key value known)) selected values
      admittedOwners = Set.fromList (map rowOwner (scopeInterfaces scope ++ map fst offered))
      lexicalOwners = Set.union (Map.keysSet oldLexical) (Set.fromList (map fst lexical))
      productKeys = [(originalUnit p,originalModule p) | entry <- products, let p = scopeOriginalProduct entry]
  if Set.size (Set.fromList productKeys) /= length products
      || Set.size (Set.fromList (map fst lexical)) /= length lexical
    then pure (Left "scope generation repeats a product or lexical owner")
    else
     case (merge oldProducts [((originalUnit p,originalModule p),entry) | entry <- products, let p = scopeOriginalProduct entry], merge oldLexical lexical) of
       (Right _,Right _) | all (\(key,requirements) -> key `Set.member` admittedOwners
           && all (`Set.member` lexicalOwners) requirements) lexical ->
         extendExactScopeInputs (scope
           { ownedScopeProducts=ownedScopeProducts scope ++ [entry | entry <- products, let p = scopeOriginalProduct entry, Map.notMember (originalUnit p,originalModule p) oldProducts]
           , ownedScopeLexical=scopeLexical scope ++ [entry | entry@(key,_) <- lexical, Map.notMember key oldLexical] }) offered
       (Left reason,_) -> pure (Left reason)
       (_,Left reason) -> pure (Left reason)
       _ -> pure (Left "scope generation lexical edge leaves admitted interface owners")

extendCheckedValueScope :: ExactScope -> InterfaceRow -> [(String,String)] -> IO (Either String ExactScope)
extendCheckedValueScope scope row@(artifact,_,_) requirements = case scopePurpose scope of
  ExactCellPurpose admission paths | checkedValueOwner artifact -> do
    let extended = scope {ownedScopePurpose=ExactCellPurpose
          (admission {checkedValueInterfaces=checkedValueInterfaces admission ++ [artifact]}) paths}
    extendExactScopeGeneration extended [(row,CheckedValueEvidence)] [] [(rowOwner row,requirements)]
  _ -> pure (Left "completed value extension requires a checked cell scope and canonical value owner")

canonicalRequirementsSelected :: Map.Map (String,String) String -> ExactIfaceArtifact -> CanonicalInterfaceProof -> Bool
canonicalRequirementsSelected seals artifact proof =
  Map.keysSet (canonicalRequirements proof) == Set.fromList (exactRequirements artifact)
    && all (\(key,sha) -> Map.lookup key seals == Just sha) (Map.toAscList (canonicalRequirements proof))

validateInputClosure :: String -> AdmittedScopeInputs -> [ExactProduct] -> IO ()
validateInputClosure producer inputs products = do
  let rows = inputRows inputs
      interfaces = Map.fromList [(rowOwner row,row) | row <- rows]
      seals = Map.map (exactSha256 . firstOfThree) interfaces
      matching requirements = all (\(key,sha) -> Map.lookup key seals == Just sha) (Map.toAscList requirements)
  forM_ (Map.toAscList (inputEvidence inputs)) $ \(key,evidence) -> do
    let row@(iface,_,packagesSha) = interfaces Map.! key
    unless (all (`Map.member` interfaces) (exactRequirements iface))
      (fail "exact interface requirements leave the selected closure")
    case evidence of
      ModuleInterfaceEvidence proof -> do
        let facts = canonicalFacts proof
        unless (certificateProducer facts == producer && certificateOwner facts == key
            && certificateInterface facts == exactSha256 iface && certificatePackages facts == packagesSha
            && certificateCore facts == (canonicalCoreSha256 <$> canonicalCoreArtifact proof))
          (fail "canonical module certificate differs from exact owner or payload")
        unless (canonicalRequirementsSelected seals iface proof)
          (fail "canonical module requirements differ from selected exact interfaces")
      LocalNativeDeclarationEvidence native -> do
        let proof = localNativeProof native
        unless (localNativeOwner native == key && localFinalizedInterface proof == row)
          (fail "local finalization differs from its captured interface")
        unless (matching (localFinalizedRequirements proof))
          (fail "local finalization requirements leave its exact closure")
      _ -> pure ()
  forM_ products $ \product' -> do
    let key = (originalUnit product',originalModule product')
        hasCore = case Map.lookup key (inputEvidence inputs) of
          Just (ModuleInterfaceEvidence proof) -> isJust (canonicalCoreArtifact proof)
          Just (LocalNativeDeclarationEvidence native) -> isJust (localFinalizedCore (localNativeProof native))
          _ -> False
    unless (Map.lookup key seals == Just (originalIfaceSha256 product') && hasCore)
      (fail "exact interface evidence is incomplete or lacks native module proof")
  let AdmittedScopeInputs _ _ _ census _ = inputs
  forM_ (Map.toAscList census) $ \(key,AdmittedOriginalCensus _ _ full native availability) ->
    case [product' | product' <- products, (originalUnit product',originalModule product') == key] of
      [selected] -> validateNativeSelection selected full native availability
      _ -> fail "admitted native census lacks its unique scope product"
  where firstOfThree (value,_,_) = value

observeSeal :: FileObservations -> FilePath -> Maybe Int -> String -> String -> IO ()
observeSeal observations path bound expected reason =
  observeSealWith observations path bound expected (userError reason)

observeSealWith :: Exception failure => FileObservations -> FilePath -> Maybe Int -> String -> failure -> IO ()
observeSealWith observations path bound expected failure = do
  observed <- observeFile observations path bound
  unless (observedSha256 observed == expected) (throwIO failure)

data ParsedInterfaceEvidence
  = ParsedModuleEvidence CanonicalInterfaceDescriptor
  | ParsedJoinEvidence
  | ParsedValueEvidence

-- Source originals share one canonical proof across capture and persistence.
-- Protected native declarations retain their independent local admission.
data CanonicalInterfaceAdmission
  = ModuleInterfaceAdmission CanonicalInterfaceProof
  | LocalInterfaceAdmission LocalFinalizedAdmission
  deriving (Eq, Show)

scopeCanonicalInterfaces :: ExactScope -> Map.Map (String,String) CanonicalInterfaceAdmission
scopeCanonicalInterfaces scope = Map.mapMaybe select (scopeInterfaceEvidence scope)
  where
    select (ModuleInterfaceEvidence proof) = Just (ModuleInterfaceAdmission proof)
    select (LocalNativeDeclarationEvidence native) = Just (LocalInterfaceAdmission (localNativeProof native))
    select _ = Nothing

localBodies :: LocalFinalizedAdmission -> [(FilePath,ArtifactBytes)]
localBodies admission =
  let (iface,packages,_) = localFinalizedInterface admission
  in [(exactPath iface,localFinalizedInterfaceBody admission),(packages,localFinalizedPackageBody admission)]
    ++ [(path,body) | Just (path,_) <- [localFinalizedCore admission], Just body <- [localFinalizedCoreBody admission]]

-- GHC executable imports select source originals only. Native authored
-- declarations retain their independent runtime execution authority.
scopeSourceOriginalInterfaces :: ExactScope -> Map.Map (String,String) CanonicalInterfaceAdmission
scopeSourceOriginalInterfaces scope = Map.mapMaybe select (scopeInterfaceEvidence scope)
  where
    select (ModuleInterfaceEvidence proof)
      | isSourceOriginal (canonicalOrigin proof) = Just (ModuleInterfaceAdmission proof)
    select _ = Nothing

scopeModuleInterfaceProofs :: ExactScope -> Map.Map (String,String) CanonicalInterfaceProof
scopeModuleInterfaceProofs = Map.mapMaybe select . scopeInterfaceEvidence
  where
    select (ModuleInterfaceEvidence proof) = Just proof
    select _ = Nothing

admittedInterfaceHomeUnits :: CanonicalInterfaceAdmission -> Set.Set String
admittedInterfaceHomeUnits (ModuleInterfaceAdmission proof) = canonicalHomeUnits proof
admittedInterfaceHomeUnits (LocalInterfaceAdmission proof) = localFinalizedHomeUnits proof

admittedInterfaceSourceSha256 :: CanonicalInterfaceAdmission -> String
admittedInterfaceSourceSha256 (ModuleInterfaceAdmission proof) = canonicalSourceSha256 proof
admittedInterfaceSourceSha256 (LocalInterfaceAdmission proof) = localFinalizedSourceSha256 proof

-- | Authenticate one selected home owner against shipped source. A virtual
-- finder location uses only the exact request's verified canonical proof.
resolveShippedHomeModule :: HscEnv
  -> Map.Map (String,String) CanonicalInterfaceAdmission
  -> String -> BS.ByteString -> IO (Maybe Module)
resolveShippedHomeModule env admitted name expected = do
  found <- findImportedModule env (mkModuleName name) NoPkgQual
  case found of
    Found location exactHome
      | moduleUnit exactHome == homeUnitAsUnit (hsc_home_unit env) ->
        case ml_hs_file location of
          Just source -> do
            actual <- try (BS.readFile source) :: IO (Either IOException BS.ByteString)
            pure $ case actual of
              Right bytes | bytes == expected -> Just exactHome
              _ -> Nothing
          Nothing -> pure $ case Map.lookup
              (unitString (moduleUnit exactHome), moduleNameString (moduleName exactHome)) admitted of
            Just proof | admittedInterfaceSourceSha256 proof == digest expected -> Just exactHome
            _ -> Nothing
    _ -> pure Nothing

admittedInterfaceRequirements :: CanonicalInterfaceAdmission -> Map.Map (String,String) String
admittedInterfaceRequirements (ModuleInterfaceAdmission proof) = canonicalRequirements proof
admittedInterfaceRequirements (LocalInterfaceAdmission proof) = localFinalizedRequirements proof

admittedInterfaceCore :: CanonicalInterfaceAdmission -> Maybe (FilePath,String)
admittedInterfaceCore (ModuleInterfaceAdmission proof) =
  (\core -> (canonicalCorePath core,canonicalCoreSha256 core)) <$> canonicalCoreArtifact proof
admittedInterfaceCore (LocalInterfaceAdmission proof) = localFinalizedCore proof

-- An uncaptured descriptor can be inspected but cannot reach an executable
-- consumer. Every executable entry point requires explicitly acquired custody.
admittedInterfaceCoreBytes :: CanonicalInterfaceAdmission -> IO BS.ByteString
admittedInterfaceCoreBytes = fmap artifactBytes . admittedInterfaceCoreBody

admittedInterfaceCoreBody :: CanonicalInterfaceAdmission -> IO ArtifactBytes
admittedInterfaceCoreBody (ModuleInterfaceAdmission proof) = do
  (path,sha) <- maybe (fail "admitted interface has no defining Core") pure (admittedInterfaceCore (ModuleInterfaceAdmission proof))
  canonicalProofOriginalBody proof path sha
admittedInterfaceCoreBody (LocalInterfaceAdmission proof) =
  maybe (fail "local defining Core has not been captured for execution") pure (localFinalizedCoreBody proof)

-- Already captured canonical proofs cannot regain filesystem authority when
-- promoted or transferred. Descriptor proofs retain explicit fresh admission.
withCanonicalInputReader :: RequestOriginalInputs -> CanonicalInterfaceProof
  -> RequestInputTokenReader -> (RequestInputTokenReader -> IO a) -> IO a
withCanonicalInputReader inputs proof fresh action = case proofInputCustody proof of
  DescriptorInputs -> action fresh
  CapturedCanonicalInputs _ -> fst <$> withCapturedRequestInputTokens inputs action

data CanonicalInterfaceUse = MetadataInterfaceUse | ExecutableInterfaceUse
  deriving (Eq, Show)

-- Optional metadata validation retains no payload. Promotion captures every
-- selected original input in one owner; only executable use requires Core.
captureCanonicalProofs :: Maybe ExactScope -> [(CanonicalInterfaceProof,CanonicalInterfaceUse)]
  -> IO [CanonicalInterfaceProof]
captureCanonicalProofs scope selections = do
  let proofs = map fst selections
  (_,base) <- captureRequestInputs (scope >>= scopeCapturedInputs) (const (pure ()))
  transferred <- either fail pure (transferRequestInputBodies (concatMap canonicalBodies proofs) base)
  (_,captured) <- captureRequestInputTokens (Just transferred) $ \reader ->
    forM_ selections $ \(proof,use) -> withCanonicalInputReader transferred proof reader $ \reader -> do
      let (iface,packages,packageSha) = proofInterfaceInput proof
      sealedInput reader (canonicalCertificatePath proof) (4 * 1024 * 1024)
        (canonicalCertificateSha256 proof)
      sealedInput reader (exactPath iface) (32 * 1024 * 1024) (exactSha256 iface)
      sealedInput reader packages (4 * 1024 * 1024) packageSha
      forM_ (proofNativeInput proof) $ \(path,sha) ->
        sealedInput reader path (64 * 1024 * 1024) sha
      case use of
        MetadataInterfaceUse -> pure ()
        ExecutableInterfaceUse -> captureCoreWith reader (ModuleInterfaceAdmission proof)
  owner <- maybe (fail "canonical execution graphs exceed request capture budget") pure
    (retainRequestEncodedBytes (concatMap (map executionGraphBody . proofExecutionGraphs) proofs) captured)
  pure [bindCanonicalProofInputs owner proof | proof <- proofs]
  where
    sealedInput reader path bound sha = do
      body <- reader path bound
      unless (artifactSha256 body == sha) (fail ("canonical original input changed during capture: " ++ path))

canonicalProofInterfaceBytes :: CanonicalInterfaceProof -> ExactIfaceArtifact -> IO BS.ByteString
canonicalProofInterfaceBytes proof iface = artifactBytes <$> canonicalProofInterfaceBody proof iface

canonicalProofInterfaceBody :: CanonicalInterfaceProof -> ExactIfaceArtifact -> IO ArtifactBytes
canonicalProofInterfaceBody proof iface = do
  let (selected,_,_) = proofInterfaceInput proof
  unless (selected == iface) (fail "canonical interface leaves its captured original owner")
  case proofInputCustody proof of
    CapturedCanonicalInputs bodies -> case Map.lookup (exactPath iface) bodies of
      Just body | artifactSha256 body == exactSha256 iface -> pure body
      _ -> fail "canonical interface is absent or differs from its selected seal"
    DescriptorInputs -> fail "canonical interface has not been captured for consumption"

canonicalProofOriginalBytes :: CanonicalInterfaceProof -> FilePath -> String -> IO BS.ByteString
canonicalProofOriginalBytes proof path sha = artifactBytes <$> canonicalProofOriginalBody proof path sha

canonicalProofOriginalBody :: CanonicalInterfaceProof -> FilePath -> String -> IO ArtifactBytes
canonicalProofOriginalBody proof path sha = case proofInputCustody proof of
  CapturedCanonicalInputs bodies -> case Map.lookup path bodies of
    Just body | artifactSha256 body == sha -> pure body
    _ -> fail "canonical original is absent or differs from its selected seal"
  DescriptorInputs -> fail "canonical originals have not been captured for consumption"

revalidateCanonicalProofInputs :: [CanonicalInterfaceProof] -> IO (Either String ())
revalidateCanonicalProofInputs proofs = pure (() <$ mapM capturedOwner proofs)
  where
    capturedOwner proof = case proofInputCustody proof of
      CapturedCanonicalInputs bodies ->
        let required = filter (\(path,_) -> maybe True ((/=path) . canonicalCorePath) (canonicalCoreArtifact proof)) (canonicalInputSeals proof)
        in unless (all (\(path,sha) -> maybe False ((==sha) . artifactSha256) (Map.lookup path bodies)) required)
          (Left "canonical originals lack selected captured bodies")
      DescriptorInputs -> Left "canonical originals have not been captured for publication"

-- A retained support row changes paths, never its authenticated facts. The
-- request owner records aliases of the one captured payload for durable copies.
relocateCanonicalInterfaceProof :: CanonicalInterfaceProof -> InterfaceRow -> Maybe (FilePath,String)
  -> Either String CanonicalInterfaceProof
relocateCanonicalInterfaceProof proof row@(iface,packages,packageSha) native = do
  let (oldIface,oldPackages,oldPackageSha) = proofInterfaceInput proof
  unless (iface {exactPath=exactPath oldIface} == oldIface && packageSha == oldPackageSha
      && fmap snd native == fmap snd (proofNativeInput proof))
    (Left "retained canonical row changes its authenticated original facts")
  owner <- case proofInputCustody proof of
    CapturedCanonicalInputs captured -> Right captured
    DescriptorInputs -> Left "retained canonical row lacks captured custody"
  relocated <- foldM (\bodies (new,old,sha) -> case Map.lookup old bodies of
      Just body | artifactSha256 body == sha -> Right (Map.insert new body bodies)
      _ -> Left "retained canonical alias lacks selected bytes") owner
    ([(exactPath iface,exactPath oldIface,exactSha256 iface),(packages,oldPackages,packageSha)]
      ++ [(new,old,sha) | Just (new,sha) <- [native], Just (old,_) <- [proofNativeInput proof]])
  pure proof {proofInterfaceInput=row,proofNativeInput=native,proofInputCustody=CapturedCanonicalInputs relocated}

scopeOriginalBytes :: ExactScope -> FilePath -> String -> IO BS.ByteString
scopeOriginalBytes scope path sha = do
  owner <- maybe (fail "exact scope input custody is not sealed") pure (scopeCapturedInputs scope)
  capturedRequestInput owner path sha

captureCoreWith :: RequestInputTokenReader -> CanonicalInterfaceAdmission -> IO ()
captureCoreWith reader admission = do
  (path,sha) <- maybe (fail "admitted interface has no defining Core") pure (admittedInterfaceCore admission)
  body <- reader path (32 * 1024 * 1024)
  unless (artifactSha256 body == sha) (fail ("admitted defining Core changed during capture: " ++ path))

scopeInterfaceBytes :: ExactScope -> ExactIfaceArtifact -> IO BS.ByteString
scopeInterfaceBytes scope iface = artifactBytes <$> scopeInterfaceToken scope iface

scopeInterfaceToken :: ExactScope -> ExactIfaceArtifact -> IO ArtifactBytes
scopeInterfaceToken scope iface = do
  inputs <- maybe (fail "exact scope input custody is not sealed") pure (scopeCapturedInputs scope)
  unless (iface `elem` ([selected | (selected,_,_) <- scopeInterfaces scope] ++ scopeValueInterfaces scope))
    (fail "interface is outside the admitted scope")
  capturedRequestInputToken inputs (exactPath iface) (exactSha256 iface)

readScopedInterfaces :: HscEnv -> ExactScope -> [ExactIfaceArtifact]
  -> IO (Either String [(ExactIfaceArtifact,ModIface)])
readScopedInterfaces env scope artifacts = do
  decoder <- scopeIfaceDecoder scope
  readCapturedExactIfaceArtifacts decoder (scopeInterfaceToken scope) env artifacts

readScopedInterfaceClosure :: HscEnv -> ExactScope -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String VerifiedExactIfaceClosure)
readScopedInterfaceClosure env scope originals values = do
  decoder <- scopeIfaceDecoder scope
  readCapturedExactIfaceClosureWithCheckedValues decoder (scopeInterfaceToken scope) env originals values

validateExactScopeEnvironment :: HscEnv -> ExactScope -> IO (Either String ())
validateExactScopeEnvironment env scope = do
  let AdmittedScopeInputs _ _ roots _ _ = scopeInputs scope
  validateAdmittedPackageSelection env roots

data ExactScopeRequestFailure = ExactScopeRequestChanged FilePath
  deriving (Eq, Show)

instance Exception ExactScopeRequestFailure

-- Carried admission retains captured inputs but cannot authorize a changed
-- request envelope. Check once before source work; publication still observes
-- the envelope independently to reject drift during compilation.
validateExactScopeRequest :: ExactScope -> IO ()
validateExactScopeRequest scope = withFileObservations $ \observations ->
  observeSealWith observations (scopeManifestPath scope) (Just (4 * 1024 * 1024))
    (scopeRequestSha256 scope) (ExactScopeRequestChanged (scopeManifestPath scope))

data CanonicalModuleCertificate = CanonicalModuleCertificate
  { certificateProducer :: String
  , certificateHomeUnits :: Set.Set String
  , certificateOwner :: (String,String)
  , certificateSource :: String
  , certificateInterface :: String
  , certificatePackages :: String
  , certificateCore :: Maybe String
  , certificateRequirements :: Map.Map (String,String) String
  , certificateOrigin :: CanonicalOrigin
  } deriving (Eq, Show)

data CheckedCellPurpose = AuthoredCellCheck
  deriving (Eq, Show)

data CheckedCellAdmission = CheckedCellAdmission
  { checkedAdmissionDigest :: String
  , checkedCellSha256 :: String
  , checkedTemplateSha256 :: String
  , checkedTurnTemplates :: [(String, String)]
  , checkedInjectedModules :: [String]
  , checkedReservedModules :: [String]
  , checkedValueInterfaces :: [ExactIfaceArtifact]
  , checkedTemplateImports :: CheckedTemplateImports
  , checkedPlannedCell :: Maybe PlannedCellAdmission
  , checkedCellPurpose :: CheckedCellPurpose
  } deriving (Eq, Show)

data PlannedCellAdmission = PlannedCellAdmission
  { plannedParserDigest :: String
  , plannedParserPath :: FilePath
  , plannedParserSha256 :: String
  , plannedReservationDigest :: String
  , plannedSlots :: [PlannedCellSlot]
  } deriving (Eq, Show)

data CheckedItemPurpose = AuthoredCheckedItem
  deriving (Eq, Show)

data CheckedItemAdmission = CheckedItemAdmission
  { itemPurpose :: CheckedItemPurpose
  , itemAdmissionDigest :: String
  , itemCellReceiptDigest :: String
  , itemIndex :: Word64
  , itemSourceDigest :: String
  , itemKind :: String
  , itemBinders :: [String]
  , itemTurnTemplates :: [(String,String)]
  , itemInjectedModules :: [String]
  , itemSignatures :: [CheckedSignature]
  , itemExpressionLift :: Maybe String
  , itemGeneration :: Word64
  , itemPrefixDigest :: String
  , itemValueImports :: [(String,[String])]
  , itemObservationName :: Maybe String
  , itemPlannedDeclaration :: Maybe ((String,String),String)
  , itemCompletedValues :: [CompletedValueImport]
  , itemValueInterfaces :: [ExactIfaceArtifact]
  , itemTemplateImports :: CheckedTemplateImports
  } deriving (Eq, Show)

data ExactProduct = ExactProduct
  { originalUnit :: String, originalModule :: String
  , originalVersion :: String, originalIfaceSha256 :: String
  , originalProductSha256 :: String, originalProductPath :: FilePath
  , originalGroups :: [ExactOriginalGroup]
  } deriving (Eq, Show)

originalGroupFromProjected :: ProjectedGroup -> ExactOriginalGroup
originalGroupFromProjected group = ExactOriginalGroup
  (fromIntegral (projectedOriginalOrdinal group)) (projectedBinders group)
  [(globalIdentity global, globalRequiredGeneration global == Nothing)
   | global <- projectedGlobals (projectedBody group)]

originalGroupFromCandidate :: CandidateGroup -> ExactOriginalGroup
originalGroupFromCandidate group = ExactOriginalGroup
  (candidateGroupOrdinal group) (candidateGroupBinders group)
  [(candidateGlobalIdentity global, candidateGlobalGeneration global == Nothing)
   | global <- candidateGroupGlobals group]

-- Attach provenance after native originals have been promoted. Recipes can
-- supplement execution only for the exact native inventory already present;
-- neither their owner rows nor their dependency graph grants lexical imports.
extendExactExecutionSources :: [ExecutionSourceGraph] -> [ExecutionSourceRef]
  -> ExactScope -> Either ExecutionSourceFailure ExactScope
extendExactExecutionSources offeredGraphs offeredRefs scope = do
  admitted <- extendExactExecutionSourcesWithinBudget offeredGraphs offeredRefs scope
  maybe (Left (ExecutionSourceIncomplete ("","candidate execution parcel"))) Right admitted

-- Fresh transaction recipes are optional until demanded by a splice. Validate
-- all advertised originals before withholding a recipe at the parcel budget.
extendExactExecutionSourcesWithinBudget :: [ExecutionSourceGraph] -> [ExecutionSourceRef]
  -> ExactScope -> Either ExecutionSourceFailure (Maybe ExactScope)
extendExactExecutionSourcesWithinBudget offeredGraphs offeredRefs scope = do
  graphMap <- foldM insertGraph Map.empty (scopeExecutionGraphs scope ++ offeredGraphs)
  references <- foldM insertReference Map.empty (scopeExecutionOwners scope ++ offeredRefs)
  mapM_ (validateReference graphMap) (Map.elems references)
  forM_ offeredRefs $ \reference -> do
    _ <- executionSourceOriginalClosure (Map.elems graphMap) [reference]
    pure ()
  -- Current native admission can replace/reject a dependency while leaving
  -- its import ABI valid. Withhold that optional execution root transitively;
  -- the unchanged native product remains useful, and a later demanded splice
  -- still refuses the missing capability.
  available <- fmap concat $ mapM (\reference ->
    case executionSourceClosure (Map.elems graphMap) (Map.elems references) (scopeExecutionNativeOwners scope)
        [executionIdentityKey (executionRefIdentity reference)] of
      Right closureNodes -> Right [(reference,closureNodes)]
      Left (ExecutionSourceUnavailable _) -> Right []
      Left refusal -> Left refusal) offeredRefs
  let nodes = concatMap snd available
  let selectedRefs = [reference | node <- nodes
        , Just reference <- [Map.lookup (executionIdentityKey (executionNodeIdentity node)) references]]
  needed <- foldM (retainGraph graphMap) Set.empty
    [(executionRefIdentity reference,executionRefGraph reference) | reference <- selectedRefs]
  retainedReferences <- foldM insertReference Map.empty (scopeExecutionOwners scope ++ map fst available)
  let kept = Set.union (Set.map snd needed) (Set.fromList (map executionGraphSha256 (scopeExecutionGraphs scope)))
      graphs = [graph | (sha,graph) <- Map.toAscList graphMap, sha `Set.member` kept]
  if executionSourceGraphsFit graphs && Map.size references <= 4096
    then case scopeInputs scope of
      AdmittedScopeInputs order inputs roots census (Just (CapturedScopeInputs captured decoder)) ->
        let existing = Set.fromList (map executionGraphSha256 (scopeExecutionGraphs scope))
            newlyRetained = [executionGraphBody graph | graph <- graphs
              , executionGraphSha256 graph `Set.notMember` existing]
        in pure $ (\next -> scope
          { ownedScopeExecutionGraphs=graphs,ownedScopeExecutionOwners=Map.elems retainedReferences
          , scopeInputs=AdmittedScopeInputs order (bindCapturedProofs next inputs) roots census
              (Just (CapturedScopeInputs next decoder)) })
          <$> retainRequestEncodedBytes newlyRetained captured
      _ -> Left (ExecutionSourceConflicting ("","missing request input custody"))
    else pure Nothing
  where
    insertGraph selected graph = do
      unless (executionGraphProducer graph == scopeProducerSha256 scope)
        (Left (ExecutionSourceConflicting ("",executionGraphSha256 graph)))
      case Map.lookup (executionGraphSha256 graph) selected of
        Nothing -> Right (Map.insert (executionGraphSha256 graph) graph selected)
        Just previous | executionGraphBytes previous == executionGraphBytes graph -> Right selected
        _ -> Left (ExecutionSourceConflicting ("",executionGraphSha256 graph))
    insertReference selected reference =
      let key = executionIdentityKey (executionRefIdentity reference)
      in case Map.lookup key selected of
        Nothing -> Right (Map.insert key reference selected)
        Just previous | previous == reference -> Right selected
        _ -> Left (ExecutionSourceConflicting key)
    validateReference graphs reference = do
      let originalIdentity = executionRefIdentity reference
          key = executionIdentityKey originalIdentity
          matched product' = (originalUnit product',originalModule product') == key
            && originalVersion product' == executionVersion originalIdentity
            && originalIfaceSha256 product' == executionIfaceSha256 originalIdentity
            && originalProductSha256 product' == executionNativeSha256 originalIdentity
      unless (any matched (scopeProducts scope)
          && any (\(iface,_,_) -> (exactUnit iface,exactModule iface) == key
            && exactSha256 iface == executionIfaceSha256 originalIdentity) (scopeInterfaces scope))
        (Left (ExecutionSourceConflicting key))
      graph <- maybe (Left (ExecutionSourceMissing key)) Right
        (Map.lookup (executionRefGraph reference) graphs)
      unless (executionGraphProducer graph == scopeProducerSha256 scope
          && any ((== originalIdentity) . executionOwnerIdentity) (executionGraphOwners graph))
        (Left (ExecutionSourceConflicting key))
      _ <- executionSourceOriginalNode (Map.elems graphs) originalIdentity (executionRefGraph reference)
      pure ()
    retainGraph graphs selected (originalIdentity,sha)
      | (originalIdentity,sha) `Set.member` selected = Right selected
      | otherwise = do
          let key = executionIdentityKey originalIdentity
          graph <- maybe (Left (ExecutionSourceMissing key)) Right (Map.lookup sha graphs)
          unless (executionGraphProducer graph == scopeProducerSha256 scope)
            (Left (ExecutionSourceConflicting key))
          foldM (retainGraph graphs) (Set.insert (originalIdentity,sha) selected)
            [(originalIdentity,original) | graphOwner <- executionGraphOwners graph
              , executionOwnerIdentity graphOwner == originalIdentity
              , Just original <- [executionOwnerOriginalGraph graphOwner]]

scopeExecutionNativeOwners :: ExactScope -> [ExecutionSourceIdentity]
scopeExecutionNativeOwners scope =
  [ExecutionSourceIdentity (originalUnit product') (originalModule product')
    (originalVersion product') (originalIfaceSha256 product') (originalProductSha256 product')
  | product' <- scopeProducts scope
  , any (\(iface,_,_) -> (exactUnit iface,exactModule iface) == (originalUnit product',originalModule product')
      && exactSha256 iface == originalIfaceSha256 product') (scopeInterfaces scope)]

-- Current source-import authority is separate from retained native ownership.
-- Each row binds one admitted canonical certificate to its current source bytes.
data SourceSelectedOriginals = SourceSelectedOriginals
  { selectedOriginalRows :: [((String,String),String,String,String)]
  , selectedOriginalEvidence :: DependencyEvidence
  }

-- The same source-selection payload is used by exact and held-world receipts.
-- Encoding preserves issuing evidence; it does not authenticate caller facts.
encodeSourceSelectedOriginals :: Maybe SourceSelectedOriginals -> E.Encoding
encodeSourceSelectedOriginals Nothing = E.encodeListLen 2 <> E.encodeListLen 0 <> E.encodeNull
encodeSourceSelectedOriginals (Just selected) = E.encodeListLen 2
  <> E.encodeListLen (fromIntegral (length (selectedOriginalRows selected)))
  <> foldMap (\((unit,name),certificate,interfaceSha,sourceSha) -> E.encodeListLen 5
      <> text unit <> text name <> text certificate <> text interfaceSha <> text sourceSha) (selectedOriginalRows selected)
  <> text (renderDependencyEvidence (selectedOriginalEvidence selected))
  where text = E.encodeString . T.pack

revalidateSourceSelectedOriginals :: SourceSelectedOriginals -> IO Bool
revalidateSourceSelectedOriginals selected = do
  unchanged <- revalidateDependencyEvidence (selectedOriginalEvidence selected)
  absent <- and <$> forM (dependencyResolutions (selectedOriginalEvidence selected)) (\resolution -> do
    let candidates = case dependencyResolutionSelected resolution of
          Nothing -> dependencyResolutionCandidates resolution
          Just chosen -> takeWhile (/= chosen) (dependencyResolutionCandidates resolution)
    and <$> mapM (fmap not . doesFileExist) candidates)
  pure (unchanged && absent)

instance Eq SourceSelectedOriginals where
  left == right = selectedOriginalRows left == selectedOriginalRows right
    && renderDependencyEvidence (selectedOriginalEvidence left)
      == renderDependencyEvidence (selectedOriginalEvidence right)

instance Show SourceSelectedOriginals where
  show selected = "SourceSelectedOriginals " ++ show (selectedOriginalRows selected)

extendSourceSelectedOriginals :: Maybe SourceSelectedOriginals -> ExactScope -> Either String ExactScope
extendSourceSelectedOriginals Nothing scope = Right scope
extendSourceSelectedOriginals (Just selected) scope = do
  let rows = selectedOriginalRows selected
      selectedKeys = Set.fromList [key | (key,_,_,_) <- rows]
      nodes = dependencyModules (selectedOriginalEvidence selected)
      byKey = Map.fromList [((dependencyModuleUnit node,dependencyModuleName node),node) | node <- nodes]
      available = Map.fromList [((exactUnit artifact,exactModule artifact),artifact) | (artifact,_,_) <- scopeInterfaces scope]
      existing = Map.fromList (scopeLexical scope)
      adjacency node = Set.toAscList (Set.fromList
        [(dependencyModuleUnit node,dependencyImportName edge)
        | edge <- dependencyModuleImports node, dependencyImportSelected edge /= Nothing])
  unless (length rows == Set.size selectedKeys && Map.keysSet byKey == selectedKeys)
    (Left "source selection owner rows differ from their matched sources")
  lexical <- forM rows $ \(key,certificate,interfaceSha,sourceSha) -> do
    artifact <- maybe (Left "source-selected original lacks exact interface") Right (Map.lookup key available)
    proof <- case Map.lookup key (scopeInterfaceEvidence scope) of
      Just (ModuleInterfaceEvidence canonical) | isSourceOriginal (canonicalOrigin canonical) -> Right canonical
      _ -> Left "source-selected original lacks canonical source origin"
    unless (exactSha256 artifact == interfaceSha
        && canonicalCertificateSha256 proof == certificate
        && canonicalSourceSha256 proof == sourceSha
        && not ("Tidepool.Session." `isPrefixOf` snd key))
      (Left "source-selected original has another canonical owner")
    node <- maybe (Left "source-selected original lacks source adjacency") Right (Map.lookup key byKey)
    let imports = adjacency node
    unless (all (`Set.member` Set.union selectedKeys (Map.keysSet existing)) imports)
      (Left "source-selected adjacency leaves its admitted source graph")
    forM_ (Map.lookup key existing) $ \old -> unless (old == imports)
      (Left "source-selected original changed inherited lexical adjacency")
    pure (key,imports)
  pure scope
    { ownedScopeLexical=Map.toAscList (Map.union (Map.fromList lexical) existing)
    , ownedScopeSourceSelectedOwners=Set.union selectedKeys (scopeSourceSelectedOwners scope) }

data ExactCompilation = ExactCompilation
  { compilationScope :: ExactScope
  , compilationTransaction :: Word64
  , compilationSource :: FilePath
  -- | Direct imports to retained exact owners, excluded from fresh source lookup.
  , compilationExactImports :: [((String, String, Bool), [(DependencyQualifier, String, Bool, String)])]
  , compilationSourceSelection :: Maybe SourceSelectedOriginals
  } deriving (Eq, Show)

scopeValueInterfaces :: ExactScope -> [ExactIfaceArtifact]
scopeValueInterfaces scope = case scopePurpose scope of
  NoCheckedPurpose -> []
  ExactCellPurpose admission _ -> checkedValueInterfaces admission
  ExactItemPurpose admission _ -> itemValueInterfaces admission
  ExactInspectionPurpose values _ -> values
  ExactReloadInspectionPurpose values _ -> values
  ExactActivationPreviewPurpose _ _ -> []

-- The exact scope separates the bounded metadata envelope from the independently
-- bounded original graph bytes. The request hash seals each path and digest.
readExactScope :: FilePath -> IO (Either String ExactScope)
readExactScope path = do
  result <- newExactInputOwner >>= \owner -> readExactScopeWithOwner owner path
  case result of
    Left reason -> pure (Left reason)
    Right scope -> case scopeCapturedInputs scope of
      Nothing -> pure (Left "exact scope lacks its captured input owner")
      Just captured -> do
        -- This scope owns the decoder; no inactive universe retains it.
        -- Its live reads may share decoded facts even without retained images.
        decoder <- newRequestIfaceDecoder
        pure (Right (retainCapturedInputsUsing captured decoder scope))

readExactScopeWithOwner :: ExactInputOwner -> FilePath -> IO (Either String ExactScope)
readExactScopeWithOwner (ExactInputOwner retentionLimit state) path = do
  summary <- readSummaryTimingEnabled
  counts <- newIORef (Map.fromList [("original_inputs." ++ kind ++ suffix,0)
    | kind <- ["certificate","packages","census","graph"], suffix <- [".hits",".misses"]])
  timing <- readTimingEnabled
  (result,decodedCount) <- mask $ \restore ->
    modifyMVarMasked state $ \previous@(RetainedOriginalInputs retained decoder (DecodedOriginalFacts certificates packages native graphs) worldBytes worldCount) -> do
    captured <- try (restore $ do
      certificatesState <- newIORef certificates
      packagesState <- newIORef packages
      nativeState <- newIORef native
      graphsState <- newIORef graphs
      withValidationTiming summary "original_inputs" "scope_acquisition" (Map.toList <$> readIORef counts) $
       timeDetailPhase timing "exact_scope" "read" $ do
        acquisition <- try (do
          unless (isAbsolute path) (fail "exact scope path must be absolute")
          ((bytes,(offered,descriptors,interfaceEvidence,nativeDescriptors,acquisition)),envelope) <-
            captureRequestInputs Nothing $ \readInput -> do
              bytes <- readInput path (4 * 1024 * 1024)
              decoded <- timeDetailPhase timing "exact_scope" "decode" $ case deserialiseFromBytes decodeScope (BL.fromStrict bytes) of
                Left failure -> fail (show failure)
                Right (remaining,result)
                  | BL.null remaining -> pure result
                  | otherwise -> fail "exact scope has trailing bytes"
              pure (bytes,decoded)
          let images = case acquisition of FreshFiles -> []; ContinueOwnedOriginals values -> values
              imageReferences = [reference | OriginalInputImage _ _ _ parts <- images, (_,reference) <- parts]
              available = foldl (\content (_,more) -> mergeCapturedOriginalContent content more)
                emptyCapturedOriginalContent retained
          case acquisition of
            FreshFiles -> pure ()
            ContinueOwnedOriginals _ -> validateOwnedInputImages offered descriptors interfaceEvidence nativeDescriptors images
          base <- case acquisition of
            FreshFiles -> pure envelope
            ContinueOwnedOriginals _ -> do
              selected <- continueRequestInputs available imageReferences
              either fail pure (mergeRequestInputs selected [envelope])
          let captureOriginals = case acquisition of
                FreshFiles -> captureRequestInputTokens (Just base)
                ContinueOwnedOriginals _ -> withCapturedRequestInputTokens base
          (scope, originals) <- captureOriginals $ \readToken -> do
           let readInput artifact bound = artifactBytes <$> readToken artifact bound
               readVerified artifact bound sha = do
                 token <- readToken artifact bound
                 unless (artifactSha256 token == sha) (fail "exact input differs from its selected seal")
                 pure (artifactBytes token)
           graphs <- readExecutionSourceGraphsWithFacts (\sha artifact -> do
             token <- readToken artifact (64 * 1024 * 1024)
             either fail pure (checkArtifactSeal sha token)
             memoOriginalFact counts timing "original_inputs.graph" graphsState sha
               (either fail pure (decodeExecutionSourceBody token))) path descriptors
           let OfferedScope producer semantic rows lexical products references purpose types published = offered
           forM_ rows $ \(iface,packages,_) -> do
             _ <- readInput (exactPath iface) (32 * 1024 * 1024)
             _ <- readInput packages (4 * 1024 * 1024)
             pure ()
           let certificateFacts descriptor = do
                 let artifact = descriptorCertificatePath descriptor
                     sha = descriptorCertificateSha256 descriptor
                 payload <- readVerified artifact (4 * 1024 * 1024) sha
                 memoOriginalFact counts timing "original_inputs.certificate" certificatesState sha
                   (decodeCertificateBytes payload)
           evidence <- validateInterfaceEvidenceUsing readInput readVerified certificateFacts producer rows interfaceEvidence
           forM_ (Map.elems evidence) $ \entry -> case entry of
             ModuleInterfaceEvidence proof -> forM_ (canonicalCoreArtifact proof) $ \core -> do
               _ <- readVerified (canonicalCorePath core) (32 * 1024 * 1024) (canonicalCoreSha256 core)
               pure ()
             _ -> pure ()
           forM_ products $ \product -> do
             _ <- readVerified (originalProductPath product) (64 * 1024 * 1024) (originalProductSha256 product)
             pure ()
           census <- admitOriginalCensusUsing (\artifact sha -> do
             token <- readToken artifact (32 * 1024 * 1024)
             either fail pure (checkArtifactSeal sha token)
             memoOriginalFact counts timing "original_inputs.census" nativeState sha
               (decodeOriginalNativeCensusBody sha token)) producer evidence nativeDescriptors
           selectedProducts <- selectedCensusProducts nativeDescriptors census
           roots <- extendAdmittedPackageImportsWithFacts (\(iface,artifact,sha) -> do
             payload <- readVerified artifact (4 * 1024 * 1024) sha
             _ <- readVerified (exactPath iface) (32 * 1024 * 1024) (exactSha256 iface)
             Right <$> memoOriginalFact counts timing "original_inputs.packages" packagesState
               (sha,exactUnit iface,exactModule iface,exactSha256 iface)
               (either fail pure (decodeCapturedPackageImports iface payload))) emptyAdmittedPackageImports rows >>= either fail pure
           let AdmittedScopeInputs order admitted _ _ _ = makeScopeInputs rows evidence roots
               inputs = AdmittedScopeInputs order admitted roots census Nothing
               sha = digest bytes
               scope = ExactScope path sha producer semantic inputs lexical selectedProducts graphs references
                 purpose types Set.empty (Set.fromList (map fst published)) (Set.fromList (concatMap snd published))
           case acquisition of
             FreshFiles -> forM_ (scopeValueInterfaces scope) $ \iface -> do
               _ <- readVerified (exactPath iface) (32 * 1024 * 1024) (exactSha256 iface)
               pure ()
             ContinueOwnedOriginals _ -> pure ()
           validateInputClosure producer inputs (map scopeOriginalProduct selectedProducts)
           forM_ (map fst published ++ concatMap snd published) $ \owner -> case Map.lookup owner evidence of
             Just (ModuleInterfaceEvidence proof) | isSourceOriginal (canonicalOrigin proof) -> pure ()
             _ -> fail "published original lacks its canonical source owner"
           validatePreviewOriginalTarget scope evidence
           validateExecutionSources scope graphs
           when timing $ do
             _ <- evaluate (length sha)
             emitCount timing ("hash_bytes.scope_metadata." ++ sha) (fromIntegral (BS.length bytes))
           pure scope
          admittedInputs <- case acquisition of
            FreshFiles -> pure originals
            ContinueOwnedOriginals _ -> either fail pure (bindCapturedPurposeValues scope originals)
          let complete = retainCapturedInputsUsing admittedInputs decoder scope
          additions <- forM images $ \(OriginalInputImage _ _ key parts) -> do
            content <- either fail pure (selectedOriginalContent (map snd parts) originals)
            pure (key,content)
          let keys = Set.fromList (map fst additions)
              candidates = additions ++ filter ((`Set.notMember` keys) . fst) retained
              unionContent entries = foldl (\content (_,more) -> mergeCapturedOriginalContent content more)
                emptyCapturedOriginalContent entries
              settled = selectRetainedOriginalInputs retentionLimit worldBytes worldCount candidates
              retainedContent = unionContent settled
              bytesRetained = capturedOriginalContentBytes retainedContent
              bytesBefore = capturedOriginalContentBytes (unionContent candidates)
          emitCount timing "original_inputs.retained_encoded_bytes" bytesRetained
          emitCount timing "original_inputs.evicted_encoded_bytes" (bytesBefore - bytesRetained)
          emitCount timing "original_inputs.live_request_encoded_bytes" (requestInputBytes originals)
          emitCount timing "original_inputs.receiving_images" (fromIntegral (length images))
          emitCount timing "original_inputs.receiving_encoded_bytes" (capturedOriginalContentBytes (unionContent additions))
          emitCount timing "original_inputs.world_reserved_bytes" worldBytes
          emitCount timing "original_inputs.world_reserved_entries" (toInteger worldCount)
          emitCount timing "original_inputs.shared_optional_charged_bytes" (bytesRetained + worldBytes)
          let seals = Set.map fst (capturedOriginalContentKeys retainedContent)
          certificateFacts <- Map.restrictKeys <$> readIORef certificatesState <*> pure seals
          packageFacts <- Map.filterWithKey (\(sha,_,_,_) _ -> sha `Set.member` seals) <$> readIORef packagesState
          nativeFacts <- Map.restrictKeys <$> readIORef nativeState <*> pure seals
          graphFacts <- Map.restrictKeys <$> readIORef graphsState <*> pure seals
          emitCount timing "original_inputs.decoded_content_facts" (fromIntegral
            (Map.size certificateFacts + Map.size packageFacts + Map.size nativeFacts + Map.size graphFacts))
          let next = RetainedOriginalInputs settled decoder
                (DecodedOriginalFacts certificateFacts packageFacts nativeFacts graphFacts) worldBytes worldCount
              staged = (seals,complete,next)
          -- Force the candidate's retention metadata while cancellation and all
          -- acquisition/timing failures still restore the previous owner.
          _ <- evaluate (Set.size seals + length settled + Map.size certificateFacts
            + Map.size packageFacts + Map.size nativeFacts + Map.size graphFacts)
          _ <- evaluate (bytesRetained + bytesBefore)
          _ <- evaluate complete
          _ <- evaluate next
          evaluate staged)
          :: IO (Either IOException (Set.Set String,ExactScope,RetainedOriginalInputs))
        pure (either (Left . show) Right acquisition))
          :: IO (Either IOException (Either String (Set.Set String,ExactScope,RetainedOriginalInputs)))
    case captured of
      Left failure -> pure (previous,(Left (show failure),Nothing))
      Right (Left reason) -> pure (previous,(Left reason,Nothing))
      Right (Right (seals,scope,next)) -> do
        -- The outer slot is empty and this callback is masked. The inner lock
        -- can be interrupted before pruning; after it commits, only the outer
        -- nonblocking publication remains. No fallible observer can roll back
        -- raw custody while leaving the shared decoder's policy advanced.
        ready <- evaluate (Right scope)
        count <- pruneRequestIfaceDecoder seals decoder
        pure (next,(ready,Just count))
  -- Observation follows publication, so an observer failure cannot roll back
  -- either half of the already committed owner state.
  forM_ decodedCount (emitCount timing "original_inputs.decoded_interfaces" . fromIntegral)
  pure result

-- Protected value paths and materialized transport paths can differ. Their
-- typed owner and seal select already captured bytes, never a filesystem read.
bindCapturedPurposeValues :: ExactScope -> RequestOriginalInputs -> Either String RequestOriginalInputs
bindCapturedPurposeValues scope inputs = do
  let AdmittedScopeInputs _ admitted _ _ _ = scopeInputs scope
  aliases <- forM (scopeValueInterfaces scope) $ \value -> do
    let owner = (exactUnit value,exactModule value)
    case Map.lookup owner admitted of
      Just (ScopeInterfaceInput (iface,_,_) CheckedValueEvidence)
        | exactSha256 iface == exactSha256 value ->
            Right (exactPath value,exactPath iface,exactSha256 value)
      _ -> Left "checked purpose value differs from its captured value interface owner"
  aliasRequestInputs aliases inputs

validatePreviewOriginalTarget :: ExactScope -> Map.Map (String,String) ExactInterfaceEvidence -> IO ()
validatePreviewOriginalTarget scope evidence = forM_ (scopeActivationPreview scope) $ \admission -> do
  let target = previewOriginalTarget admission
      selected = [interface | interface <- previewOriginalInterfaces admission
        , (templateInterfaceUnit interface,templateInterfaceModule interface) == target]
  seal <- case selected of
    [interface] -> pure (templateInterfaceSha256 interface)
    _ -> fail "activation preview lacks its exact original target graph owner"
  unless (any (\(interface,_,_) -> (exactUnit interface,exactModule interface) == target
      && exactSha256 interface == seal) (scopeInterfaces scope))
    (fail "activation preview original target interface differs from its graph seal")
  case Map.lookup target evidence of
    Just ModuleInterfaceEvidence{} -> pure ()
    _ -> fail "activation preview original target lacks canonical module authority"

-- A bounded read also closes the stat/read growth race without allocating an
-- unbounded input.
readBoundedFile :: FilePath -> Int -> IO BS.ByteString
readBoundedFile path limit = do
  bytes <- readFileAtMost path (limit + 1)
  when (BS.length bytes > limit) (fail "exact scope artifact exceeds its byte bound")
  pure bytes

validateExecutionSources :: ExactScope -> [ExecutionSourceGraph] -> IO ()
validateExecutionSources scope graphs = do
  forM_ graphs $ \graph -> unless (executionGraphProducer graph == scopeProducerSha256 scope)
    (fail "original execution graph has another compiler producer")
  forM_ (scopeExecutionOwners scope) $ \reference -> do
    let original = executionRefIdentity reference
        matchingProduct product' = originalUnit product' == executionUnit original
          && originalModule product' == executionModule original
          && originalVersion product' == executionVersion original
          && originalIfaceSha256 product' == executionIfaceSha256 original
          && originalProductSha256 product' == executionNativeSha256 original
        matchingGraph graph = executionGraphSha256 graph == executionRefGraph reference
          && any ((== original) . executionOwnerIdentity) (executionGraphOwners graph)
    unless (any matchingProduct (scopeProducts scope) && any matchingGraph graphs)
      (fail "original execution reference leaves its admitted native owner")

-- Validate canonical certificates before any interface hydration. The complete
-- home-unit census is producer evidence, not a classification inferred from the
-- retained subset. Core bytes are decoded only by their demanding recovery owner.
validateInterfaceEvidenceWith :: RequestInputReader -> String -> [InterfaceRow]
  -> [(InterfaceOwner,ParsedInterfaceEvidence)] -> IO (Map.Map InterfaceOwner ExactInterfaceEvidence)
validateInterfaceEvidenceWith readInput = validateInterfaceEvidenceUsing readInput (verifyInputWith readInput) (readCertificateWith readInput)

validateInterfaceEvidenceUsing :: RequestInputReader -> VerifiedInputReader
  -> (CanonicalInterfaceDescriptor -> IO CanonicalModuleCertificate) -> String -> [InterfaceRow]
  -> [(InterfaceOwner,ParsedInterfaceEvidence)] -> IO (Map.Map InterfaceOwner ExactInterfaceEvidence)
validateInterfaceEvidenceUsing readInput verifiedInput readCertificate producer rows offered = do
  proofs <- validateCanonicalInterfacesUsing readInput verifiedInput readCertificate producer rows Map.empty
    [(key,descriptor) | (key,ParsedModuleEvidence descriptor) <- offered]
  let evidence = Map.fromList [(key,case value of
        ParsedModuleEvidence _ -> ModuleInterfaceEvidence (proofs Map.! key)
        ParsedJoinEvidence -> LexicalJoinEvidence
        ParsedValueEvidence -> CheckedValueEvidence) | (key,value) <- offered]
  unless (Map.keysSet evidence == Set.fromList (map rowOwner rows))
    (fail "exact interface evidence is incomplete")
  pure evidence

-- A completed same-program capture already has the request's admitted producer.
-- Cold captures receive their producer from Rust at admission. Both paths use
-- TPFINALMODULE identity; capture paths are custody, never semantic identity.
-- The current target is retained by its own checked/native declaration owner.
-- Only checked cells retain supporting source originals between their segments.
-- Other requests keep finalization evidence for its independent origin issuer.
captureFinalizedSourceOriginals
  :: ExactCompilation -> [ModuleCandidate] -> (String,String) -> FinalizedModuleArtifacts
  -> DependencyEvidence -> IO (Map.Map (String,String) CanonicalInterfaceProof)
captureFinalizedSourceOriginals compilation accepted target finalized evidence
  | Nothing <- scopeCheckedCell (compilationScope compilation) = pure Map.empty
  | otherwise = captureCheckedSourceOriginals compilation accepted target finalized evidence

captureCheckedSourceOriginals
  :: ExactCompilation -> [ModuleCandidate] -> (String,String) -> FinalizedModuleArtifacts
  -> DependencyEvidence -> IO (Map.Map (String,String) CanonicalInterfaceProof)
captureCheckedSourceOriginals compilation accepted target finalized evidence = do
  let scope = compilationScope compilation
      admissions = Map.delete target (finalizedLocalAdmissions finalized)
      rows = map localFinalizedInterface (Map.elems admissions)
        ++ [(ExactIfaceArtifact (candidateUnit candidate) (candidateModule candidate)
              (candidateInterface candidate) (candidateInterfaceSha256 candidate)
              (candidateInterfaceRequirements candidate),candidatePackageImports candidate,
              candidatePackageImportsSha256 candidate) | candidate <- accepted]
      inherited = scopeInterfaces scope
      keyOf (artifact,_,_) = (exactUnit artifact,exactModule artifact)
  interfaces <- foldM (\selected row -> case filter ((== keyOf row) . keyOf) selected of
      [] -> pure (row:selected)
      [old] | let (oldIface,_,oldPackage) = old
                  (iface,_,package) = row
               in exactSha256 oldIface == exactSha256 iface
                 && exactRequirements oldIface == exactRequirements iface
                 && oldPackage == package -> pure selected
      _ -> fail "captured source original conflicts with an admitted interface") inherited rows
  descriptors <- forM (Map.toAscList admissions) $ \(key,admission) -> do
    either fail pure =<< revalidateLocalFinalizedAdmission admission
    unless (parseSessionModule (snd key) == Nothing)
      (fail "source original capture uses a reserved native/session owner")
    node <- case [node | node <- dependencyModules evidence
        , (dependencyModuleUnit node,dependencyModuleName node) == key
        , not (dependencyModuleBoot node)] of
      [node] -> pure node
      _ -> fail "captured source original lacks its original import receipt"
    unless ([dependencySourceSha256 source | source <- dependencySources evidence
        , dependencySourcePath source == dependencyModuleSource node]
        == [localFinalizedSourceSha256 admission])
      (fail "captured source original import receipt differs from its consumed source")
    imports <- either fail pure (compilationOriginalSourceImports compilation evidence key)
    let (iface,_,packageSha) = localFinalizedInterface admission
        core = localFinalizedCore admission
        certificate = CanonicalModuleCertificate
          (scopeProducerSha256 scope) (localFinalizedHomeUnits admission) key
          (localFinalizedSourceSha256 admission) (exactSha256 iface) packageSha (snd <$> core)
          (localFinalizedRequirements admission) (SourceOriginal imports)
        bytes = toStrictByteString (encodeCanonicalModuleCertificate certificate)
        seal = digest bytes
        path = takeDirectory (exactPath iface) </> seal ++ ".finalized.certificate.cbor"
    BS.writeFile path bytes
    pure (key,CanonicalInterfaceDescriptor path seal
      (uncurry CanonicalCoreArtifact <$> core) (ScopeInterface SourceOriginalRole))
  let valueSeals = Map.fromList
        [((T.unpack unit,T.unpack name),T.unpack sha)
        | ((unit,name),sha) <- finalizedValueInterfaceSeals finalized]
  validateCanonicalInterfacesWithValueSeals (scopeProducerSha256 scope) interfaces valueSeals descriptors

-- The completed source graph and exact-import graph jointly own original
-- authored adjacency, for both canonical issuance and same-cell retention.
compilationOriginalSourceImports
  :: ExactCompilation -> DependencyEvidence -> (String,String)
  -> Either String [CanonicalSourceImport]
compilationOriginalSourceImports compilation evidence key = do
  node <- case [node | node <- dependencyModules evidence
      , (dependencyModuleUnit node,dependencyModuleName node) == key
      , not (dependencyModuleBoot node)] of
    [node] -> Right node
    _ -> Left "captured source original lacks its original import receipt"
  ordinary <- forM (dependencyModuleImports node) $ \edge -> do
    home <- case dependencyImportSelected edge of
      Nothing -> Right Nothing
      Just path -> case [dependencyModuleUnit child | child <- dependencyModules evidence
          , dependencyModuleName child == dependencyImportName edge
          , dependencyModuleBoot child == dependencyImportBoot edge
          , dependencyModuleSource child == path] of
        [unit] -> Right (Just unit)
        _ -> Left "captured source import has no unique original home owner"
    pure (dependencyImportQualifier edge,dependencyImportName edge,dependencyImportBoot edge,home)
  let exact = [(qualifier,name,boot,Just unit)
        | ((ownerUnit,ownerName,False),edges) <- compilationExactImports compilation
        , (ownerUnit,ownerName) == key
        , (qualifier,name,boot,unit) <- edges]
      importKey (qualifier,name,boot,_) = (qualifier,name,boot)
  Map.elems <$> foldM (\selected row -> case Map.lookup (importKey row) selected of
      Nothing -> Right (Map.insert (importKey row) row selected)
      Just old | old == row -> Right selected
      _ -> Left "captured source original has conflicting original import owners")
    Map.empty (ordinary ++ exact)

-- Candidate descriptors are optional cache suggestions until this same owner
-- validates them against the complete selected interface closure.
validateCanonicalInterfaceProof
  :: ExactScope -> (String,String) -> FilePath -> String -> Maybe (FilePath,String)
  -> IO (Either String CanonicalInterfaceProof)
validateCanonicalInterfaceProof scope =
  validateCanonicalProof (scopeProducerSha256 scope) (scopeInterfaces scope)

validateCanonicalProof
  :: String -> [(ExactIfaceArtifact,FilePath,String)] -> (String,String)
  -> FilePath -> String -> Maybe (FilePath,String)
  -> IO (Either String CanonicalInterfaceProof)
validateCanonicalProof producer interfaces key certificatePath certificateSha core = do
  result <- try (do
    unless (isCanonicalDigest producer && isAbsolute certificatePath && isCanonicalDigest certificateSha
        && maybe True (\(path,seal) -> isAbsolute path && isCanonicalDigest seal) core)
      (fail "invalid canonical interface descriptor")
    proofs <- validateCanonicalInterfaces producer interfaces [(key, CanonicalInterfaceDescriptor
      certificatePath certificateSha (uncurry CanonicalCoreArtifact <$> core) CandidateCarrier)]
    maybe (fail "canonical proof has no selected owner") pure (Map.lookup key proofs))
    :: IO (Either IOException CanonicalInterfaceProof)
  pure (either (Left . show) Right result)

-- The expected producer is admitted request configuration, never the offered
-- producer field alone. Missing closure owners decline this cache suggestion.
validateCandidateCanonicalInterfaceProof
  :: String -> [(ExactIfaceArtifact,FilePath,String)] -> ModuleCandidate
  -> IO (Either String CanonicalInterfaceProof)
validateCandidateCanonicalInterfaceProof producer interfaces candidate = do
  let descriptor = candidateModuleInterface candidate
      key = (candidateUnit candidate,candidateModule candidate)
  case [(iface,packageSha) | (iface,_,packageSha) <- interfaces
      , (exactUnit iface,exactModule iface) == key] of
    [(iface,packageSha)]
      | exactSha256 iface == candidateInterfaceSha256 candidate
      , packageSha == candidatePackageImportsSha256 candidate
      , Set.fromList (exactRequirements iface) == Set.fromList (candidateInterfaceRequirements candidate) -> do
          result <- try (do
            let reader = case candidateInputCustody candidate of
                  Nothing -> readBoundedFile
                  Just custody -> \path bound -> do
                    bytes <- capturedRequestInput custody path (expectedSeal path)
                    unless (BS.length bytes <= bound) (fail "candidate captured artifact exceeds bound")
                    pure bytes
                expectedSeal path
                  | path == candidateCertificatePath descriptor = candidateCertificateSha256 descriptor
                  | path == fst (candidateCoreDescriptor descriptor) = snd (candidateCoreDescriptor descriptor)
                  | otherwise = case [(sha) | (iface,packages,sha) <- interfaces, packages == path] ++
                      [exactSha256 iface | (iface,_,_) <- interfaces, exactPath iface == path] of
                      [sha] -> sha
                      _ -> ""
            let verified path bound sha = case candidateInputCustody candidate of
                  Nothing -> verifyInputWith readBoundedFile path bound sha
                  Just custody -> do
                    token <- capturedRequestInputToken custody path sha
                    unless (BS.length (artifactBytes token) <= bound) (fail "candidate captured artifact exceeds bound")
                    pure (artifactBytes token)
                certificate descriptor' = verified (descriptorCertificatePath descriptor') (4*1024*1024)
                  (descriptorCertificateSha256 descriptor') >>= decodeCertificateBytes
            proofs <- validateCanonicalInterfacesUsing reader verified certificate producer interfaces Map.empty
              [(key,CanonicalInterfaceDescriptor (candidateCertificatePath descriptor)
                (candidateCertificateSha256 descriptor) (Just (uncurry CanonicalCoreArtifact (candidateCoreDescriptor descriptor))) CandidateCarrier)]
            proof <- maybe (fail "candidate canonical owner disappeared") pure (Map.lookup key proofs)
            pure proof)
            :: IO (Either IOException CanonicalInterfaceProof)
          pure $ either (Left . show) Right result >>= \proof -> do
            unless (candidateProducerSha256 candidate == producer
                && canonicalSourceSha256 proof == candidateSourceSha256 candidate
                && Map.keys (canonicalRequirements proof) == candidateInterfaceRequirements candidate)
              (Left "candidate canonical proof differs from source, producer or requirements")
            let complete = proof
                  {proofNativeInput=Just (candidateProductPath candidate,candidateProductSha256 candidate)
                  ,proofExecutionGraphs=maybe [] fst (candidateExecutionSources candidate)}
            pure (maybe complete (\custody -> bindCanonicalProofInputs custody complete) (candidateInputCustody candidate))
    _ -> pure (Left "candidate canonical proof lacks its selected exact interface")

validateCanonicalInterfaces
  :: String -> [(ExactIfaceArtifact,FilePath,String)]
  -> [((String,String),CanonicalInterfaceDescriptor)]
  -> IO (Map.Map (String,String) CanonicalInterfaceProof)
validateCanonicalInterfaces producer selectedInterfaces =
  validateCanonicalInterfacesWithValueSeals producer selectedInterfaces Map.empty

validateCanonicalInterfacesWithValueSeals
  :: String -> [(ExactIfaceArtifact,FilePath,String)] -> Map.Map (String,String) String
  -> [((String,String),CanonicalInterfaceDescriptor)]
  -> IO (Map.Map (String,String) CanonicalInterfaceProof)
validateCanonicalInterfacesWithValueSeals = validateCanonicalInterfacesWithReader readBoundedFile

validateCanonicalInterfacesWithReader :: RequestInputReader -> String
  -> [(ExactIfaceArtifact,FilePath,String)] -> Map.Map (String,String) String
  -> [((String,String),CanonicalInterfaceDescriptor)]
  -> IO (Map.Map (String,String) CanonicalInterfaceProof)
type VerifiedInputReader = FilePath -> Int -> String -> IO BS.ByteString

verifyInputWith :: RequestInputReader -> VerifiedInputReader
verifyInputWith readInput path bound sha = do
  bytes <- readInput path bound
  unless (digest bytes == sha) (fail "selected exact input changed")
  pure bytes

readCertificateWith :: RequestInputReader -> CanonicalInterfaceDescriptor -> IO CanonicalModuleCertificate
readCertificateWith readInput descriptor = do
  bytes <- verifyInputWith readInput (descriptorCertificatePath descriptor) (4 * 1024 * 1024)
    (descriptorCertificateSha256 descriptor)
  decodeCertificateBytes bytes

decodeCertificateBytes :: BS.ByteString -> IO CanonicalModuleCertificate
decodeCertificateBytes bytes = do
  timing <- readTimingEnabled
  emitCount timing "exact_scope.certificate_decodes" 1
  certificate <- case deserialiseFromBytes (decodeCanonicalModuleCertificate (BS.length bytes)) (BL.fromStrict bytes) of
    Left failure -> fail (show failure)
    Right (remaining,value)
      | BL.null remaining -> pure value
      | otherwise -> fail "canonical module certificate has trailing bytes"
  unless (toStrictByteString (encodeCanonicalModuleCertificate certificate) == bytes)
    (fail "noncanonical module certificate encoding")
  pure certificate

validateCanonicalInterfacesWithReader readInput = validateCanonicalInterfacesUsing
  readInput (verifyInputWith readInput) (readCertificateWith readInput)

validateCanonicalInterfacesUsing :: RequestInputReader -> VerifiedInputReader
  -> (CanonicalInterfaceDescriptor -> IO CanonicalModuleCertificate) -> String
  -> [(ExactIfaceArtifact,FilePath,String)] -> Map.Map (String,String) String
  -> [((String,String),CanonicalInterfaceDescriptor)]
  -> IO (Map.Map (String,String) CanonicalInterfaceProof)
validateCanonicalInterfacesUsing _readInput verifiedInput readCertificate producer selectedInterfaces valueSeals descriptors = do
  let interfaces = Map.fromList
        [((exactUnit iface,exactModule iface),(iface,packages,packageSha))
        | (iface,packages,packageSha) <- selectedInterfaces]
  unless (Map.size interfaces == length selectedInterfaces)
    (fail "canonical proof has conflicting selected interface owners")
  let selectedSeals = Map.map (exactSha256 . (\(iface,_,_) -> iface)) interfaces
  unless (all (\(key,seal) -> maybe True (== seal) (Map.lookup key selectedSeals))
      (Map.toAscList valueSeals))
    (fail "captured value interface conflicts with selected canonical owner")
  let available = Map.union selectedSeals valueSeals
  proofs <- forM descriptors $ \(key,descriptor) -> do
    (iface,packages,packageSha) <- maybe (fail "canonical proof has no exact interface") pure
      (Map.lookup key interfaces)
    proof <- validateCanonicalRowUsing verifiedInput readCertificate producer
      (iface,packages,packageSha) descriptor
    let certificate = canonicalFacts proof
    let requirements = certificateRequirements certificate
    let matchesRequirement (required,seal) = Map.lookup required available == Just seal
    unless (Map.keysSet requirements == Set.fromList (exactRequirements iface)
        && all matchesRequirement (Map.toAscList requirements))
      (fail "canonical module requirements differ from selected exact interfaces")
    pure (key,proof)
  pure (Map.fromList proofs)

-- Authenticate one producer-issued row independently of any receiving
-- selection. Closure selection belongs to the demanding compiler operation;
-- passive historical custody cannot issue that authority.
validateCanonicalRowUsing :: VerifiedInputReader
  -> (CanonicalInterfaceDescriptor -> IO CanonicalModuleCertificate)
  -> String -> InterfaceRow -> CanonicalInterfaceDescriptor -> IO CanonicalInterfaceProof
validateCanonicalRowUsing verifiedInput readCertificate producer row@(iface,packages,packageSha) descriptor = do
  certificate <- readCertificate descriptor
  unless (certificateProducer certificate == producer
      && certificateOwner certificate == rowOwner row
      && certificateInterface certificate == exactSha256 iface
      && certificatePackages certificate == packageSha
      && certificateCore certificate == (canonicalCoreSha256 <$> descriptorCore descriptor)
      && Map.keysSet (certificateRequirements certificate) == Set.fromList (exactRequirements iface))
    (fail "canonical module certificate differs from exact owner or payload")
  unless (case descriptorPurpose descriptor of
      CandidateCarrier -> True
      ScopeInterface role -> role == originRole (certificateOrigin certificate))
    (fail "canonical module origin differs from its exact interface role")
  _ <- verifiedInput (exactPath iface) (32 * 1024 * 1024) (certificateInterface certificate)
  _ <- verifiedInput packages (4 * 1024 * 1024) (certificatePackages certificate)
  pure CanonicalInterfaceProof
    { proofCertificatePath = descriptorCertificatePath descriptor
    , proofCertificateSha256 = descriptorCertificateSha256 descriptor
    , proofCoreArtifact = descriptorCore descriptor
    , proofInterfaceInput = row
    , proofNativeInput = Nothing
    , proofExecutionGraphs = []
    , proofInputCustody = DescriptorInputs
    , canonicalFacts = certificate
    }

-- Every encoded item consumes at least one byte. The bounded certificate
-- payload limits list cardinality without imposing the cache's candidate cap.
decodeCanonicalModuleCertificate :: Int -> Decoder s CanonicalModuleCertificate
decodeCanonicalModuleCertificate payloadBytes = do
  array 13
  magic <- string
  version <- decodeWord
  profile <- string
  unless (magic == "TPFINALMODULE" && version == 3
      && profile == "tidepool-ghc-finalized-module-v1")
    (fail "unsupported canonical module certificate")
  producer <- canonicalDigest
  homes <- bounded payloadBytes nonempty
  unless (not (null homes) && and (zipWith (<) homes (drop 1 homes)))
    (fail "invalid complete home unit inventory")
  let homeSet = Set.fromDistinctAscList homes
  key@(unit,_) <- (,) <$> nonempty <*> nonempty
  source <- canonicalDigest
  interface <- canonicalDigest
  packages <- canonicalDigest
  token <- peekTokenType
  core <- if token == TypeNull then decodeNull >> pure Nothing else Just <$> canonicalDigest
  requirements <- bounded payloadBytes (array 3 >> ((,) <$> ((,) <$> nonempty <*> nonempty) <*> canonicalDigest))
  unless (and (zipWith (<) (map fst requirements) (drop 1 (map fst requirements)))
      && unit `Set.member` homeSet && all ((`Set.member` homeSet) . fst . fst) requirements
      && key `notElem` map fst requirements)
    (fail "invalid canonical module requirement inventory")
  originCount <- decodeListLen
  originTag <- string
  origin <- case (originTag,originCount) of
    ("source-original",2) -> do
      imports <- bounded 4096 $ do
        array 4
        rawQualifier <- string
        qualifier <- maybe (fail "invalid canonical source qualifier") pure (parseDependencyQualifier rawQualifier)
        name <- nonempty
        boot <- decodeBool
        token' <- peekTokenType
        home <- if token' == TypeNull then decodeNull >> pure Nothing else Just <$> nonempty
        unless (case home of
          Nothing -> case qualifier of
            DependencyThisUnit _ -> False
            _ -> True
          Just importedUnit -> importedUnit `Set.member` homeSet && case qualifier of
            DependencyUnqualified -> True
            DependencyThisUnit selectedUnit -> selectedUnit == importedUnit
            DependencyOtherUnit _ -> False) (fail "invalid canonical source owner")
        pure (qualifier,name,boot,home)
      let shape (qualifier,name,boot,_) = (qualifier,name,boot)
      unless (and (zipWith (<) imports (drop 1 imports))
          && and (zipWith (/=) (map shape imports) (drop 1 (map shape imports))))
        (fail "invalid canonical source import order")
      pure (SourceOriginal imports)
    ("native-authored-declaration",2) -> do
      generation <- decodeWord64
      let nativeOwner = SessionModule LibMod (Generation generation)
      unless (generation > 0 && key == ("main",sessionModuleString nativeOwner))
        (fail "native canonical origin differs from its reserved identity")
      pure (NativeAuthoredDeclaration (Generation generation))
    _ -> fail "unsupported canonical module origin"
  pure (CanonicalModuleCertificate producer homeSet key source interface packages core (Map.fromList requirements) origin)

encodeCanonicalModuleCertificate :: CanonicalModuleCertificate -> E.Encoding
encodeCanonicalModuleCertificate certificate = E.encodeListLen 13
  <> text "TPFINALMODULE" <> E.encodeWord 3 <> text "tidepool-ghc-finalized-module-v1"
  <> text (certificateProducer certificate)
  <> list text (Set.toAscList (certificateHomeUnits certificate))
  <> text (fst (certificateOwner certificate)) <> text (snd (certificateOwner certificate))
  <> text (certificateSource certificate) <> text (certificateInterface certificate)
  <> text (certificatePackages certificate)
  <> maybe E.encodeNull text (certificateCore certificate)
  <> list (\((unit,name),seal) -> E.encodeListLen 3 <> text unit <> text name <> text seal)
      (Map.toAscList (certificateRequirements certificate))
  <> case certificateOrigin certificate of
    SourceOriginal imports -> E.encodeListLen 2 <> text "source-original"
      <> list (\(qualifier,name,boot,home) -> E.encodeListLen 4
        <> text (renderDependencyQualifier qualifier) <> text name <> E.encodeBool boot
        <> maybe E.encodeNull text home) imports
    NativeAuthoredDeclaration (Generation generation) ->
      E.encodeListLen 2 <> text "native-authored-declaration" <> E.encodeWord64 generation
  where
    text = E.encodeString . T.pack
    list encode values = E.encodeListLen (fromIntegral (length values)) <> foldMap encode values

canonicalDigest :: Decoder s String
canonicalDigest = do
  value <- digestField
  unless (isCanonicalDigest value)
    (fail "invalid canonical digest")
  pure value

isCanonicalDigest :: String -> Bool
isCanonicalDigest value = length value == 64 && value /= replicate 64 '0'
  && all (`elem` ("0123456789abcdef" :: String)) value

-- Recheck the entire producer-owned closure in the consuming transaction;
-- no source file is a substitute for an admitted original interface.
data ExactScopeValidationReason
  = ExplicitScopeValidation
  | CheckedReceiptPublication
  | RetainedReceiptPublication
  | RetainedProductsPublication
  deriving (Eq, Show)

validationReasonName :: ExactScopeValidationReason -> String
validationReasonName reason = case reason of
  ExplicitScopeValidation -> "explicit_scope_validation"
  CheckedReceiptPublication -> "checked_receipt_publication"
  RetainedReceiptPublication -> "retained_receipt_publication"
  RetainedProductsPublication -> "retained_products_publication"

revalidateExactScope :: HscEnv -> ExactScope -> IO (Either String ())
revalidateExactScope env scope = revalidateExactScopesAt ExplicitScopeValidation env [scope]

-- A terminal operation can own several scopes with shared paths. It observes
-- current manifests, package resolution and the operation's fresh outputs in
-- one bounded map. Captured original bodies are carried by their byte owner.
revalidateExactScopesAt :: ExactScopeValidationReason -> HscEnv -> [ExactScope] -> IO (Either String ())
revalidateExactScopesAt reason env scopes = revalidateExactScopesAtWithOutputs reason env scopes emptyFreshOutputSeals

-- Freshly materialized outputs participate in the same bounded terminal proof
-- as selected scopes. Captured inputs are not reconstructed here: only output
-- seals supplied by the operation that wrote them are observed.
revalidateExactScopesAtWithOutputs
  :: ExactScopeValidationReason -> HscEnv -> [ExactScope]
  -> FreshOutputSeals -> IO (Either String ())
revalidateExactScopesAtWithOutputs reason env scopes (FreshOutputSeals outputs) = do
  timing <- readTimingEnabled
  summary <- readSummaryTimingEnabled
  totals <- newIORef Nothing
  let inputCount = sum [requestInputCount inputs
        | scope <- scopes, Just inputs <- [scopeCapturedInputs scope]]
      readCounts = do
        observed <- readIORef totals
        pure ([ ("scope_count",fromIntegral (length scopes))
          ,("input_count",fromIntegral inputCount)]
          ++ case observed of
            Nothing -> []
            Just (observations,bytes) ->
              [("observed_file_count",fromIntegral observations),("observed_bytes",bytes)])
      validate = timeDetailPhase timing "exact_scope" "revalidate" $ do
        result <- try (withFileObservations $ \observations -> do
          outcome <- try (forM_ scopes $ \scope -> do
            observeSeal observations (scopeManifestPath scope) (Just (4 * 1024 * 1024))
              (scopeRequestSha256 scope) "exact scope request changed"
            validateInputClosure (scopeProducerSha256 scope) (scopeInputs scope) (scopeProducts scope)
            validatePreviewOriginalTarget scope (scopeInterfaceEvidence scope)
            let AdmittedScopeInputs _ _ roots _ _ = scopeInputs scope
            revalidateAdmittedPackageImports observations env roots >>= either fail pure
            manifest <- observeFile observations (scopeManifestPath scope) (Just (4 * 1024 * 1024))
            -- Preserve the proof marker; observed_file alone counts actual reads.
            emitCount timing ("hash_bytes.scope_revalidation." ++ scopeRequestSha256 scope)
              (fromIntegral (observedByteCount manifest))
            pure ())
            :: IO (Either IOException ())
          outputOutcome <- try (forM_ outputs $ \(path,sha,bound) ->
            observeSeal observations path bound sha "fresh exact output changed before publication")
            :: IO (Either IOException ())
          observed <- fileObservationTotals observations
          writeIORef totals (Just observed)
          either throwIO pure outcome
          either throwIO pure outputOutcome)
          :: IO (Either IOException ())
        pure $ either (Left . show) Right result
  withValidationTiming summary "exact_scope" (validationReasonName reason) readCounts validate

-- Every successful compile owns a distinct immutable source snapshot. Check,
-- fold and inspection requests can consume several generated modules, so a
-- later successful transaction must not replace an earlier witness.
writeExactCompilation
  :: ExactCompilation -> DependencyEvidence -> IO ()
writeExactCompilation compilation evidence =
  captureExactCompilationReceipt compilation evidence >>= publishExactCompilationReceipt

-- Publication is data, so no compiler work or caller callback can intervene
-- between the terminal observations and the receipt. Certified bytes are
-- supplied by their product writer; this operation grants no artifact authority.
data ExactCompilationPublication
  = ReceiptOnly
  | PublishCertificate FilePath BS.ByteString

-- Metadata has completed all compiler work at this boundary. Capture and
-- validate every consumed source before the terminal current scope/package
-- proof; publishing the receipt afterwards consumes only captured bytes.
writeCheckedExactCompilation
  :: HscEnv -> ExactCompilation -> DependencyEvidence -> IO ()
writeCheckedExactCompilation env = writeCheckedExactCompilationWithScopes
  CheckedReceiptPublication env [] emptyFreshOutputSeals ReceiptOnly

writeCheckedExactCompilationWithOutputsAndPublication
  :: HscEnv -> ExactCompilation -> DependencyEvidence -> FreshOutputSeals -> ExactCompilationPublication -> IO ()
writeCheckedExactCompilationWithOutputsAndPublication env compilation evidence outputs publish =
  writeCheckedExactCompilationWithScopes CheckedReceiptPublication env [] outputs publish compilation evidence

-- Retention and the checked receipt share this final publication boundary.
-- Capture source evidence first, then check selected scopes and fresh outputs
-- in one proof. No observation or validation token reaches the publisher.
writeRetainedExactCompilation
  :: HscEnv -> ExactScope -> ExactCompilation -> DependencyEvidence -> IO ()
writeRetainedExactCompilation env retained = writeCheckedExactCompilationWithScopes
  RetainedReceiptPublication env [retained] emptyFreshOutputSeals ReceiptOnly

writeRetainedExactCompilationWithOutputsAndPublication
  :: HscEnv -> ExactScope -> ExactCompilation -> DependencyEvidence
  -> FreshOutputSeals -> ExactCompilationPublication -> IO ()
writeRetainedExactCompilationWithOutputsAndPublication env retained compilation evidence outputs publish =
  writeCheckedExactCompilationWithScopes RetainedReceiptPublication env [retained] outputs publish compilation evidence

writeCheckedExactCompilationWithScopes
  :: ExactScopeValidationReason -> HscEnv -> [ExactScope] -> FreshOutputSeals
  -> ExactCompilationPublication -> ExactCompilation -> DependencyEvidence -> IO ()
writeCheckedExactCompilationWithScopes reason env retained outputs publishCertificate compilation evidence = do
  selected <- either fail pure (extendSourceSelectedOriginals
    (compilationSourceSelection compilation) (compilationScope compilation))
  receipt <- captureExactCompilationReceipt compilation evidence
  revalidateExactScopesAtWithOutputs reason env (retained ++ [selected]) outputs >>= either fail pure
  case publishCertificate of
    ReceiptOnly -> pure ()
    PublishCertificate path bytes -> BS.writeFile path bytes
  publishExactCompilationReceipt receipt

-- Kept private so a receipt cannot be constructed from unobserved inputs.
data ExactCompilationReceipt = ExactCompilationReceipt
  FilePath Word64 BS.ByteString (FilePath -> BS.ByteString)

captureExactCompilationReceipt
  :: ExactCompilation -> DependencyEvidence -> IO ExactCompilationReceipt
captureExactCompilationReceipt compilation evidence = do
  let scope = compilationScope compilation
      transaction = compilationTransaction compilation
      source = compilationSource compilation
      imports = compilationExactImports compilation
  path <- makeAbsolute source
  bytes <- BS.readFile path
  unless (any (\item -> dependencySourcePath item == path
      && dependencySourceSha256 item == digest bytes) (dependencySources evidence))
    (fail "exact compile source differs from consumed source")
  unchanged <- revalidateDependencyEvidence evidence
  unless unchanged (fail "exact compile consumed source changed before receipt")
  forM_ (compilationSourceSelection compilation) $ \selected -> do
    unchangedSelection <- revalidateSourceSelectedOriginals selected
    unless unchangedSelection
      (fail "current original source selection changed before receipt")
  let parent = takeDirectory path </> ".exact-compilations"
      encodeArray values = E.encodeListLen (fromIntegral (length values)) <> mconcat values
      text = E.encodeString . T.pack
      importRow (qualifier, name, boot, unit) = encodeArray
        [text (renderDependencyQualifier qualifier), text name, E.encodeBool boot, text unit]
      moduleRow ((unit, name, boot), edges) = encodeArray
        [text unit, text name, E.encodeBool boot, encodeArray (map importRow edges)]
      receipt snapshot = toStrictByteString $ encodeArray
        [text "TPEXACTCOMPILE", text "3", text (scopeRequestSha256 scope)
        , text (scopeSemanticSha256 scope), text path, text (digest bytes)
        , text snapshot, text (renderDependencyEvidence evidence)
        , encodeArray (map moduleRow imports)
        , encodeSourceSelectedOriginals (compilationSourceSelection compilation)]
  pure (ExactCompilationReceipt parent transaction bytes receipt)

publishExactCompilationReceipt :: ExactCompilationReceipt -> IO ()
publishExactCompilationReceipt (ExactCompilationReceipt parent transaction bytes receipt) = do
  createDirectoryIfMissing True parent
  directory <- reserveCompilationDirectory parent transaction
  let snapshot = directory </> "source.hs"
  BS.writeFile snapshot bytes
  BS.writeFile (directory </> "receipt.cbor") (receipt snapshot)

-- One worker request can check, refine and compile several sources. The
-- request identity correlates diagnostics; it cannot identify one immutable
-- compilation snapshot. Atomic directory creation also keeps concurrent
-- writers from replacing an earlier successful receipt.
reserveCompilationDirectory :: FilePath -> Word64 -> IO FilePath
reserveCompilationDirectory parent transaction = attempt (0 :: Int)
  where
    attempt ordinal
      | ordinal >= 4096 = fail "excessive exact compilations in one request"
      | otherwise = do
          let path = parent </> (show transaction ++ "-" ++ show ordinal)
          reserved <- try (createDirectory path) :: IO (Either IOException ())
          case reserved of
            Right () -> pure path
            Left failure | isAlreadyExistsError failure -> attempt (ordinal + 1)
            Left failure -> throwIO failure

data OfferedScope = OfferedScope String String [(ExactIfaceArtifact,FilePath,String)]
  [((String,String),[(String,String)])] [ExactProduct] [ExecutionSourceRef]
  ExactScopePurpose (Maybe (RequestHelperRecipe,RequestTypeSignatures))
  [((String,String),[(String,String)])]

validateOwnedInputImages :: OfferedScope -> [(String,FilePath)]
  -> [((String,String),ParsedInterfaceEvidence)] -> [OfferedNativeProduct]
  -> [OriginalInputImage] -> IO ()
validateOwnedInputImages (OfferedScope producer _ rows _ products _ _ _ _) graphs evidence census images = do
  unless (Set.size (Set.fromList [key | OriginalInputImage _ key _ _ <- images]) == length images)
    (fail "selected exact scope has conflicting original image owners")
  let parts = [(kind,originalInputPath reference,originalInputSha256 reference)
        | OriginalInputImage _ _ _ inputs <- images, (kind,reference) <- inputs]
      expected = [(kind,path,sha) | (iface,packages,packageSha) <- rows
        , (kind,path,sha) <- [(InputInterface,exactPath iface,exactSha256 iface),(InputPackages,packages,packageSha)]]
        ++ concat [case proof of
            ParsedModuleEvidence descriptor ->
              [(InputCertificate,descriptorCertificatePath descriptor,descriptorCertificateSha256 descriptor)]
                ++ [(InputCore,canonicalCorePath core,canonicalCoreSha256 core) | core <- maybe [] pure (descriptorCore descriptor)]
            _ -> [] | (_,proof) <- evidence]
        ++ [(InputNative,originalProductPath product,originalProductSha256 product) | product <- products]
        ++ [(InputCensus,path,sha) | (_,_,(path,sha),_,_) <- census]
        ++ [(InputGraph,path,sha) | (sha,path) <- graphs]
      ownedParts = [((exactUnit iface,exactModule iface),(kind,path,sha)) | (iface,packages,packageSha) <- rows
        , (kind,path,sha) <- [(InputInterface,exactPath iface,exactSha256 iface),(InputPackages,packages,packageSha)]]
        ++ concat [case proof of
            ParsedModuleEvidence descriptor -> [(key,(InputCertificate,descriptorCertificatePath descriptor,descriptorCertificateSha256 descriptor))]
              ++ [(key,(InputCore,canonicalCorePath core,canonicalCoreSha256 core)) | core <- maybe [] pure (descriptorCore descriptor)]
            _ -> [] | (key,proof) <- evidence]
        ++ [((originalUnit product,originalModule product),(InputNative,originalProductPath product,originalProductSha256 product)) | product <- products]
        ++ [((originalUnit product,originalModule product),(InputCensus,path,sha)) | (product,_,(path,sha),_,_) <- census]
      byOwner = Map.fromListWith Set.union [(key,Set.singleton part) | (key,part) <- ownedParts]
      owners = Set.fromList [(exactUnit iface,exactModule iface) | (iface,_,_) <- rows]
  unless (Set.fromList parts == Set.fromList expected)
    (fail "owned original images differ from the receiving exact input projection")
  forM_ images $ \(OriginalInputImage issued key _ inputs) -> do
    unless (issued == producer && key `Set.member` owners)
      (fail "owned original image has another compiler producer or owner")
    let supplied = Set.fromList [(kind,originalInputPath reference,originalInputSha256 reference)
          | (kind,reference) <- inputs, kind /= InputGraph]
    unless (Map.lookup key byOwner == Just supplied)
      (fail "owned original image parts differ from their exact semantic owner")

decodeInterfaceRows :: Decoder s [(InterfaceRow,ParsedInterfaceEvidence)]
decodeInterfaceRows = bounded 4096 decodeInterfaceRow

decodeInterfaceRow :: Decoder s (InterfaceRow,ParsedInterfaceEvidence)
decodeInterfaceRow = do
  array 8
  unit <- nonempty
  name <- nonempty
  path <- absolute
  sha <- digestField
  requirements <- bounded 4096 owner
  packages <- absolute
  packageSha <- digestField
  unique "exact requirements" requirements
  evidenceCount <- decodeListLen
  evidenceRole <- string
  evidence <- case (evidenceRole,evidenceCount) of
    (role,5) | role == "module" || role == "native-declaration" -> do
      certificatePath <- absolute
      certificateSha <- canonicalDigest
      coreToken <- peekTokenType
      core <- if coreToken == TypeNull then do
        decodeNull
        decodeNull
        pure Nothing
        else Just <$> (CanonicalCoreArtifact <$> absolute <*> canonicalDigest)
      pure (ParsedModuleEvidence (CanonicalInterfaceDescriptor certificatePath certificateSha core
        (ScopeInterface (if role == "module" then SourceOriginalRole else NativeDeclarationRole))))
    ("join",1) -> pure ParsedJoinEvidence
    ("value",1) -> pure ParsedValueEvidence
    _ -> fail "unsupported exact interface evidence role"
  pure ((ExactIfaceArtifact unit name path sha requirements, packages, packageSha),evidence)

-- Logical world inputs contain only immutable bytes and authenticated facts.
-- The request interface decoder remains owned by the compiler universe.
data WorldInputFacts = WorldInputFacts
  { worldInputsIdentity :: !(String)
  , worldInputsIdentityBytes :: !(BS.ByteString)
  , worldInputsProducer :: !(BS.ByteString)
  , worldInputsProfile :: !(BS.ByteString)
  , worldInputsSources :: !(CapturedWorldSources)
  , worldInputsBaseRows :: !(Map.Map InterfaceOwner WorldAdmittedRow)
  , worldInputsVisibleRows :: !(Map.Map InterfaceOwner WorldAdmittedRow)
  , worldInputsVisibleSemantic :: !(Map.Map BS.ByteString ScopeInterfaceInput)
  , worldInputsCustody :: !(RequestOriginalInputs)
  , worldInputsCustodyUsers :: !(Map.Map FilePath Int)
  , worldInputsFrontier :: !(VerifiedWorldFrontier)
  , worldInputsSourceCharge :: !((Integer,Int))
  , worldInputsRowCharge :: !((Integer,Int))
  , worldInputsBindingRows :: !(Map.Map BS.ByteString (WorldBindingRow,BS.ByteString,Integer,Int))
  , worldInputsBindingSemanticUsers :: !(Map.Map BS.ByteString Int)
  , worldInputsPrimaryRoot :: !(BS.ByteString)
  , worldInputsRuntimeVisibility :: !(BS.ByteString)
  , worldInputsBindingScope :: !(Word64)
  , worldInputsBindings :: !(Map.Map String WorldBinding)
  , worldInputsBindingCharge :: !((Integer,Int))
  , worldInputsPrimary :: !(WorldPrimaryFacts)
  , ownedWorldInputsLexicalSurface :: !(WorldLexicalSurface)
  }

-- This result contains reconstruction facts, never a lineage or execution
-- grant. Request admission selects the exact held predecessor; Rust's checked
-- item owner issues the prefix relationship. A compatible verified predecessor
-- may reconstruct the same immutable next product.
data WorldSuccessorInputs = WorldSuccessorInputs WorldInputFacts (Set.Set String)

worldSuccessorFacts :: WorldSuccessorInputs -> WorldInputFacts
worldSuccessorFacts (WorldSuccessorInputs facts _) = facts

worldSuccessorChangedAliases :: WorldSuccessorInputs -> Set.Set String
worldSuccessorChangedAliases (WorldSuccessorInputs _ aliases) = aliases

data WorldAdmittedRow = WorldAdmittedRow !OfferedWorldRow !ScopeInterfaceInput !BS.ByteString !(Integer,Int)

-- Incoming block bodies are copied out of transport envelopes once. Persistent
-- maps share unchanged bodies and child sets; root reference counts retire only
-- newly unreachable paths, without scanning the retained graph.
data VerifiedWorldFrontier = VerifiedWorldFrontier
  !(Map.Map BS.ByteString BS.ByteString) !(Map.Map BS.ByteString (Set.Set BS.ByteString))
  !(Map.Map BS.ByteString Int) ![BS.ByteString] !(Integer,Int)

worldFrontierBodies :: VerifiedWorldFrontier -> Map.Map BS.ByteString BS.ByteString
worldFrontierBodies (VerifiedWorldFrontier blocks _ _ _ _) = blocks

-- Ordered imports and an explicitly selected declaration facade belong to the
-- logical product. Custody of other interfaces cannot populate this surface.
data WorldLexicalSurface = WorldLexicalSurface [String] (Maybe WorldDeclarationSurface)
  (Maybe WorldObservationScaffold) (Integer,Int)

data WorldObservationProfile = WorldWorkbenchObservation | WorldHostCarrierObservation
  deriving (Eq,Show)

-- This immutable source/type witness is part of the logical lexical product.
-- Its generated bodies are never runtime values or native execution inputs.
data WorldObservationScaffold = WorldObservationScaffold WorldObservationProfile String [String]
  (Maybe (RequestHelperRecipe,RequestTypeSignatures)) (Maybe CheckedSignature)

worldLexicalObservation :: WorldLexicalSurface -> Maybe WorldObservationScaffold
worldLexicalObservation (WorldLexicalSurface _ _ observation _) = observation

worldObservationProfile :: WorldObservationScaffold -> WorldObservationProfile
worldObservationProfile (WorldObservationScaffold profile _ _ _ _) = profile

worldObservationFragment :: WorldObservationScaffold -> String
worldObservationFragment (WorldObservationScaffold _ fragment _ _ _) = fragment

worldObservationImports :: WorldObservationScaffold -> [String]
worldObservationImports (WorldObservationScaffold _ _ imports _ _) = imports

worldObservationRequestTypes :: WorldObservationScaffold -> Maybe (RequestHelperRecipe,RequestTypeSignatures)
worldObservationRequestTypes (WorldObservationScaffold _ _ _ request _) = request

worldObservationInputType :: WorldObservationScaffold -> Maybe CheckedSignature
worldObservationInputType (WorldObservationScaffold _ _ _ _ input) = input

data WorldDeclarationSurface = WorldDeclarationSurface ScopeInterfaceInput [DeclarationExport]

worldInputsLexicalSurface :: WorldInputFacts -> WorldLexicalSurface
worldInputsLexicalSurface = ownedWorldInputsLexicalSurface

worldLexicalImports :: WorldLexicalSurface -> [String]
worldLexicalImports (WorldLexicalSurface imports _ _ _) = imports

worldLexicalDeclaration :: WorldLexicalSurface -> Maybe WorldDeclarationSurface
worldLexicalDeclaration (WorldLexicalSurface _ declaration _ _) = declaration

worldLexicalCharge :: WorldLexicalSurface -> (Integer,Int)
worldLexicalCharge (WorldLexicalSurface _ _ _ charge) = charge

worldDeclarationInterface :: WorldDeclarationSurface -> ExactIfaceArtifact
worldDeclarationInterface (WorldDeclarationSurface (ScopeInterfaceInput (artifact,_,_) _) _) = artifact

worldDeclarationExports :: WorldDeclarationSurface -> [DeclarationExport]
worldDeclarationExports (WorldDeclarationSurface _ exports) = exports

-- One demanded Context row carries its sealed interface bytes, canonical proof,
-- decoded interface and package evidence together. Consumers borrow this
-- collection rather than rejoining parallel maps by owner.
data WorldSourceClosureEntry = WorldSourceClosureEntry
  { worldSourceEntryArtifact :: ExactIfaceArtifact
  , worldSourceEntryProof :: CanonicalInterfaceProof
  , worldSourceEntryInterface :: ModIface
  , worldSourceEntryPackages :: PackageImportEvidence
  }

-- Current occurrence selection is distinct from the referenced row's owner.
-- In particular, a reexport's defining SymbolIdentity need not name that row.
data WorldBinding = WorldBinding
  { worldBindingSymbol :: SymbolIdentity
  , worldBindingSessionVar :: Word64
  , worldBindingScope :: Word64
  , worldBindingGeneration :: Word64
  , worldBindingRow :: WorldBindingRow
  }

-- The semantic row is authenticated against captured visible input bytes.
-- Certified artifact/receipt identities are passive provenance issued by the
-- closed Rust CheckedValueArtifact constructor, not an independently checked
-- compiler receipt or a native/runtime permission in this receiver.
data WorldBindingRow = WorldBindingRow
  { worldBindingRowInterface :: InterfaceRow
  , worldBindingCertifiedArtifact :: BS.ByteString
  , worldBindingCompilerReceipt :: BS.ByteString
  , worldBindingRowFactCID :: BS.ByteString
  }

worldBindingInterfaceArtifact :: WorldBinding -> ExactIfaceArtifact
worldBindingInterfaceArtifact binding =
  let (artifact,_,_) = worldBindingRowInterface (worldBindingRow binding)
  in artifact

-- Select by submitted occurrences in the already admitted namespace. Value
-- bodies remain undecoded for the checked-value injection owner; only their
-- demanded primary type closure enters the shared interface decoder here.
readWorldBindingInputs :: ExactInputOwner -> HscEnv -> WorldInputFacts -> [String]
  -> IO (Either String ([(String,WorldBinding)],[(ExactIfaceArtifact,ArtifactBytes,ArtifactBytes)],
      Map.Map InterfaceOwner WorldTypeClosureEntry))
readWorldBindingInputs owner env facts occurrences = do
  let selected = [(name,value) | name <- occurrences,Just value <- [Map.lookup name (worldInputsBindings facts)]]
      rows = [worldBindingRowInterface (worldBindingRow value) | (_,value) <- selected]
      key (artifact,_,_) = (exactUnit artifact,exactModule artifact)
      distinct = Map.fromList [(key row,row) | row <- rows]
      tokens = worldInputsCustody facts
  if length selected /= length occurrences
    then pure (Left "world value demand contains an unbound occurrence")
    else if any (\row -> Map.lookup (key row) distinct /= Just row) rows
      then pure (Left "world value aliases select conflicting captured interface versions")
      else do
        typeInputs <- readWorldTypeInputs owner env facts
          (concatMap (\(artifact,_,_) -> exactRequirements artifact) (Map.elems distinct))
        case typeInputs of
          Left reason -> pure (Left reason)
          Right types -> do
            packages <- readWorldPackageInputs env facts distinct
            case packages of
              Left reason -> pure (Left reason)
              Right _ -> do
                captured <- forM (Map.elems distinct) $ \(artifact,path,sha) -> do
                  body <- capturedRequestInputToken tokens (exactPath artifact) (exactSha256 artifact)
                  packageBody <- capturedRequestInputToken tokens path sha
                  pure (artifact,body,packageBody)
                pure (Right (selected,captured,types))

-- Artifact, decoded compiler identity, admitted role and package facts travel
-- together. This is type availability, never source or native execution rights.
data WorldTypeClosureEntry = WorldTypeClosureEntry ScopeInterfaceInput ModIface PackageImportEvidence ArtifactBytes

worldTypeEntryArtifact :: WorldTypeClosureEntry -> ExactIfaceArtifact
worldTypeEntryArtifact (WorldTypeClosureEntry (ScopeInterfaceInput (artifact,_,_) _) _ _ _) = artifact

worldTypeEntryInterface :: WorldTypeClosureEntry -> ModIface
worldTypeEntryInterface (WorldTypeClosureEntry _ iface _ _) = iface

worldTypeEntryPackages :: WorldTypeClosureEntry -> PackageImportEvidence
worldTypeEntryPackages (WorldTypeClosureEntry _ _ packages _) = packages

worldTypeEntryEvidence :: WorldTypeClosureEntry -> ExactInterfaceEvidence
worldTypeEntryEvidence (WorldTypeClosureEntry (ScopeInterfaceInput _ evidence) _ _ _) = evidence

worldTypeEntryInterfaceBody :: WorldTypeClosureEntry -> ArtifactBytes
worldTypeEntryInterfaceBody (WorldTypeClosureEntry _ _ _ body) = body

-- Decode each selected sidecar once. Its immutable token authenticates the
-- offered seal; the collective package owner checks installed bytes and the
-- current compiler selection before these facts become available to callers.
readWorldPackageInputs :: HscEnv -> WorldInputFacts -> Map.Map InterfaceOwner InterfaceRow
  -> IO (Either String (Map.Map InterfaceOwner PackageImportEvidence))
readWorldPackageInputs env facts rows = do
  decoded <- forM (Map.toAscList rows) $ \(key,(artifact,path,sha)) -> do
    body <- capturedRequestInputToken (worldInputsCustody facts) path sha
    pure ((,) key <$> decodeCapturedPackageImports artifact (artifactBytes body))
  case sequence decoded of
    Left reason -> pure (Left reason)
    Right pairs -> do
      let packages = Map.fromList pairs
          readFacts row = pure $ maybe (Left "world package proof lost its selected owner") Right
            (Map.lookup (rowOwner row) packages)
      admitted <- extendAdmittedPackageImportsWithFacts readFacts emptyAdmittedPackageImports (Map.elems rows)
      case admitted of
        Left reason -> pure (Left reason)
        Right selection -> do
          valid <- validateAdmittedPackageSelection env selection
          pure (packages <$ valid)

-- Only exact Context-selected associations can supply required home types.
-- Passive custody and other world versions cannot widen this lexical closure.
readWorldTypeInputs :: ExactInputOwner -> HscEnv -> WorldInputFacts -> [InterfaceOwner]
  -> IO (Either String (Map.Map InterfaceOwner WorldTypeClosureEntry))
readWorldTypeInputs (ExactInputOwner _ state) env facts roots = case walk Map.empty roots >>= validate of
  Left reason -> pure (Left reason)
  Right rows -> do
    packages <- readWorldPackageInputs env facts
      (Map.map (\(ScopeInterfaceInput row _) -> row) rows)
    case packages of
      Left reason -> pure (Left reason)
      Right packageFacts -> do
        RetainedOriginalInputs _ decoder _ _ _ <- readMVar state
        let artifacts = [artifact | ScopeInterfaceInput (artifact,_,_) _ <- Map.elems rows]
            readInput artifact = capturedRequestInputToken (worldInputsCustody facts)
              (exactPath artifact) (exactSha256 artifact)
        bodies <- Map.fromList <$> mapM (\artifact -> do
          body <- readInput artifact
          pure (exactPath artifact,body)) artifacts
        let selectedBody artifact = maybe (fail "world type closure lost its captured interface token") pure
              (Map.lookup (exactPath artifact) bodies)
        closure <- readCapturedExactIfaceClosureWithCheckedValues decoder selectedBody env artifacts []
        pure $ do
          verified <- closure
          interfaces <- selectVerifiedExactInterfaces verified artifacts
          let byOwner = Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,iface)) | (artifact,iface) <- interfaces]
          unless (Map.size byOwner == length interfaces && Map.keysSet byOwner == Map.keysSet rows)
            (Left "world type closure differs from its selected exact interfaces")
          Map.traverseWithKey (\key row@(ScopeInterfaceInput (selectedArtifact,_,_) _) -> do
            (artifact,iface) <- maybe (Left "world type closure lost its decoded interface") Right (Map.lookup key byOwner)
            unless (artifact == selectedArtifact)
              (Left "world type interface differs from its selected artifact")
            evidence <- maybe (Left "world type closure lost its package proof") Right (Map.lookup key packageFacts)
            body <- maybe (Left "world type closure lost its captured interface token") Right
              (Map.lookup (exactPath artifact) bodies)
            pure (WorldTypeClosureEntry row iface evidence body)) rows
  where
    validate rows = do
      let seals = Map.map (\(ScopeInterfaceInput (artifact,_,_) _) -> exactSha256 artifact) rows
      forM_ (Map.elems rows) $ \(ScopeInterfaceInput (artifact,_,_) evidence) -> case evidence of
        ModuleInterfaceEvidence proof -> unless (canonicalRequirementsSelected seals artifact proof)
          (Left "world canonical requirements differ from selected exact interfaces")
        LocalNativeDeclarationEvidence native -> do
          let requirements = localFinalizedRequirements (localNativeProof native)
          unless (Map.keysSet requirements == Set.fromList (exactRequirements artifact)
              && all (\(key,sha) -> Map.lookup key seals == Just sha) (Map.toAscList requirements))
            (Left "world native declaration requirements differ from selected exact interfaces")
        LexicalJoinEvidence -> pure ()
        CheckedValueEvidence -> Left "world primary type closure contains a checked value role"
      pure rows
    walk selected [] = Right selected
    walk selected (key:rest) | Map.member key selected = walk selected rest
    walk selected (key:rest) = do
      association <- maybe (Left "demanded type lacks its exact Context association") Right
        (Map.lookup key (worldPrimarySelected (worldInputsPrimary facts)))
      let row@(ScopeInterfaceInput (artifact,_,_) evidence) = worldPrimaryRow association
      unless ((exactUnit artifact,exactModule artifact) == key)
        (Left "world demanded type has another selected interface owner")
      case evidence of
        CheckedValueEvidence -> Left "world primary type closure contains a checked value role"
        _ -> walk (Map.insert key row selected) (exactRequirements artifact ++ rest)

-- An offered export vector is passive until it has been compared to GHC's
-- normalized exports from the selected, hydrated exact interface.
data VerifiedWorldDeclarationSurface = VerifiedWorldDeclarationSurface ExactIfaceArtifact [DeclarationExport] [WorldTypeClosureEntry]

verifiedWorldDeclarationInterface :: VerifiedWorldDeclarationSurface -> ExactIfaceArtifact
verifiedWorldDeclarationInterface (VerifiedWorldDeclarationSurface artifact _ _) = artifact

verifiedWorldDeclarationExports :: VerifiedWorldDeclarationSurface -> [DeclarationExport]
verifiedWorldDeclarationExports (VerifiedWorldDeclarationSurface _ exports _) = exports

verifiedWorldDeclarationTypeInputs :: VerifiedWorldDeclarationSurface -> [WorldTypeClosureEntry]
verifiedWorldDeclarationTypeInputs (VerifiedWorldDeclarationSurface _ _ entries) = entries

hydrateWorldDeclarationInputs :: ExactInputOwner -> HscEnv -> WorldInputFacts
  -> IO (Either String (HscEnv,Maybe VerifiedWorldDeclarationSurface))
hydrateWorldDeclarationInputs owner env facts = case worldLexicalDeclaration (worldInputsLexicalSurface facts) of
  Nothing -> pure (Right (env,Nothing))
  Just offered -> do
    let artifact = worldDeclarationInterface offered
        key = (exactUnit artifact,exactModule artifact)
    closure <- readWorldTypeInputs owner env facts [key]
    case closure of
      Left reason -> pure (Left reason)
      Right entries -> case Map.lookup key entries of
        Nothing -> pure (Left "world declaration facade lost its selected exact interface")
        Just entry | worldTypeEntryArtifact entry /= artifact ->
          pure (Left "world declaration facade differs from its selected exact interface")
        Just entry -> do
          hydrated <- hydrateExactScope env
            [(worldTypeEntryArtifact value,worldTypeEntryInterface value) | value <- Map.elems entries]
          actual <- interfaceExports hydrated (worldTypeEntryInterface entry)
          let exports = normalizeDeclarationExports actual
          pure $ if exports == normalizeDeclarationExports (worldDeclarationExports offered)
            then Right (hydrated,Just (VerifiedWorldDeclarationSurface artifact exports (Map.elems entries)))
            else Left "world declaration exports differ from its selected exact interface"

-- The Context role and acquired binding jointly selected this row at cold
-- admission. A passive row with the same module name is never a substitute.
worldHasSelectedPrimary :: WorldInputFacts -> (String,String) -> Bool
worldHasSelectedPrimary facts key = Map.member key (worldPrimarySelected (worldInputsPrimary facts))

worldHasSelectedSource :: WorldInputFacts -> (String,String) -> Bool
worldHasSelectedSource facts key = case Map.lookup key (worldPrimarySelected (worldInputsPrimary facts)) of
  Just association -> case worldPrimaryRow association of
    ScopeInterfaceInput _ (ModuleInterfaceEvidence proof) -> isSourceOriginal (canonicalOrigin proof)
    _ -> False
  _ -> False

-- Resolve only demanded Context owners and their exact declared type closure.
-- This is canonical source/type custody; Original/native execution remains a
-- distinct consumer. No universe inventory or historical base-row scan occurs.
readWorldSourceInputs :: ExactInputOwner -> HscEnv -> WorldInputFacts -> [InterfaceOwner]
  -> IO (Either String (Map.Map InterfaceOwner WorldSourceClosureEntry,
      Map.Map InterfaceOwner WorldTypeClosureEntry))
readWorldSourceInputs owner env facts roots
  | any (not . worldHasSelectedSource facts) roots =
      pure (Left "demanded source lacks its exact source Context association")
  | otherwise = do
      closure <- readWorldTypeInputs owner env facts roots
      pure $ do
        entries <- closure
        let sources = Map.mapMaybe sourceEntry entries
        unless (all (`Map.member` sources) roots)
          (Left "demanded Context source lost its canonical proof")
        pure (sources,entries)
  where
    sourceEntry entry = case worldTypeEntryEvidence entry of
      ModuleInterfaceEvidence proof | isSourceOriginal (canonicalOrigin proof) ->
        Just (WorldSourceClosureEntry (worldTypeEntryArtifact entry) proof
          (worldTypeEntryInterface entry) (worldTypeEntryPackages entry))
      _ -> Nothing

-- Conservative backing charge: receiving allowances count every retained
-- encoded body/path, even when a separate world happens to have equal bytes.
-- Equal seals alone do not establish physical sharing between acquisitions.
worldInputsRetainedCharge :: WorldInputFacts -> (Integer,Int)
worldInputsRetainedCharge facts =
  (requestInputBytes custody + bindingBytes + demandBytes + primaryBytes + lexicalBytes + frontierBytes + sourceBytes + rowBytes
    ,requestInputCount custody + bindingEntries + demandEntries + primaryEntries + lexicalEntries + frontierEntries + sourceEntries + rowEntries)
  where
    custody = worldInputsCustody facts
    (bindingBytes,bindingEntries) = worldInputsBindingCharge facts
    -- Reserve the encoded binding basis again for the NameCache-bound demand
    -- index. Each alias adds a GRE and a reverse-association entry; referenced
    -- binding rows remain shared. This is conservative input accounting, not
    -- a measurement of GHC heap size, and is settled before cache admission.
    demandBytes = bindingBytes
    demandEntries = 2 * Map.size (worldInputsBindings facts)
    (primaryBytes,primaryEntries) = worldPrimaryRetainedCharge (worldInputsPrimary facts)
    (lexicalBytes,lexicalEntries) = worldLexicalCharge (worldInputsLexicalSurface facts)
    VerifiedWorldFrontier _ _ _ _ (frontierBytes,frontierEntries) = worldInputsFrontier facts
    (sourceBytes,sourceEntries) = worldInputsSourceCharge facts
    (rowBytes,rowEntries) = worldInputsRowCharge facts

data CapturedWorldSources = CapturedWorldSources
  { worldSourceOrigin :: BS.ByteString
  , worldSourceDomainSha256 :: BS.ByteString
  , worldSourceRoots :: [WorldSourceRoot]
  }

newtype WorldSourceRoot = WorldSourceRoot { worldSourceFiles :: Map.Map FilePath BS.ByteString }

-- Only a Context-selected acquired source row can prevent an absence result.
-- The hint grants no HPT hydration or source replay; captured source/version
-- resolution remains with the world module resolver.
-- Equal authenticated Context and association roots select the same primary
-- rows. Successors can borrow their source-name hint set without a row census.
worldInputsSamePrimary :: WorldInputFacts -> WorldInputFacts -> Bool
worldInputsSamePrimary left right = case (worldInputsFrontier left,worldInputsFrontier right) of
  (VerifiedWorldFrontier _ _ _ [a,b,_,_] _,VerifiedWorldFrontier _ _ _ [c,d,_,_] _) -> a == c && b == d
  _ -> False

worldInputsSourceModuleHints :: WorldInputFacts -> Set.Set String
worldInputsSourceModuleHints facts = Set.fromList
  [exactModule iface
  | association <- Map.elems (worldPrimarySelected (worldInputsPrimary facts))
  , ScopeInterfaceInput (iface,_,_) (ModuleInterfaceEvidence proof) <- [worldPrimaryRow association]
  , isSourceOriginal (canonicalOrigin proof)]

data WorldInputsFailureCause = WorldInputsUnavailable | WorldInputsCorrupt deriving (Eq,Show)
data WorldInputsFailure = WorldInputsFailure WorldInputsFailureCause String deriving Show
instance Exception WorldInputsFailure

data OfferedWorldInputs = OfferedWorldInputs BS.ByteString FilePath BS.ByteString BS.ByteString
  [OfferedWorldRow] [OfferedWorldRow]
  InputAcquisition BS.ByteString BS.ByteString [(BS.ByteString,BS.ByteString)]

type OfferedWorldRow = ((InterfaceRow,ParsedInterfaceEvidence),BS.ByteString)

data WorldRestoration = WorldRestoration BS.ByteString BS.ByteString BS.ByteString BS.ByteString
  Word64 BS.ByteString BS.ByteString BS.ByteString

worldCorrupt :: String -> IO a
worldCorrupt = throwIO . WorldInputsFailure WorldInputsCorrupt

-- The same input owner supplies retained immutable content. Acquisition does
-- not mutate its decoder or publish a selected HPT from these custody rows.
readWorldInputsWithOwner :: ExactInputOwner -> FilePath -> IO WorldInputFacts
readWorldInputsWithOwner (ExactInputOwner _ state) path = acquire `catch` classify
  where
    text = E.encodeString . T.pack
    classify (failure :: IOException) = throwIO (WorldInputsFailure
      (if isUserError failure then WorldInputsCorrupt else WorldInputsUnavailable) (show failure))
    acquire = do
      unless (isAbsolute path) (worldCorrupt "world descriptor path must be absolute")
      limit <- requestCaptureByteLimit
      let bound = fromInteger (min limit (toInteger (maxBound :: Int) - 1))
      ((offered,sourceBytes),_envelope) <- captureRequestInputs Nothing $ \readInput -> do
        bytes <- readInput path bound
        offered@(OfferedWorldInputs _ sourcePath _ _ _ _ _ _ _ _) <-
          decodeWorldBytes (decodeWorldInputs (BS.length bytes)) bytes
        sourceBytes <- readInput sourcePath bound
        pure (offered,sourceBytes)
      let OfferedWorldInputs restoration _sourcePath producer profile baseRows visibleRows acquisition visibility primary blocks = offered
      WorldRestoration expectedPrimary domainSeal expectedProducer expectedProfile expectedScope associationRoot expectedBindings lexicalRoot <-
        decodeWorldBytes decodeWorldRestoration restoration
      (bindingScope,bindingRoot) <- decodeWorldBytes decodeWorldBindingRoot visibility
      -- Fixed roots are Context, primary associations, occurrences and lexical
      -- surface. The semantic product authenticates all four together.
      frontier <- verifyWorldBlocks [primary,associationRoot,bindingRoot,lexicalRoot] blocks
      context <- readWorldContext frontier primary producer
      sources <- either worldCorrupt pure (decodeWorldSources sourceBytes)
      let canonicalRestoration = toStrictByteString (E.encodeListLen 10 <> text "TPCWREST" <> E.encodeWord 4
            <> E.encodeBytes expectedPrimary <> E.encodeBytes domainSeal <> E.encodeBytes expectedProducer
            <> E.encodeBytes expectedProfile <> E.encodeWord64 expectedScope
            <> E.encodeBytes associationRoot <> E.encodeBytes expectedBindings <> E.encodeBytes lexicalRoot)
      unless (restoration == canonicalRestoration && primary == expectedPrimary && producer == expectedProducer && profile == expectedProfile
          && bindingScope == expectedScope && bindingRoot == expectedBindings && worldSourceDomainSha256 sources == domainSeal)
        (worldCorrupt "world restoration differs from captured source, profile, context or roots")
      (admitted,custody,users) <- admitWorldRows state producer acquisition (baseRows ++ visibleRows)
      base <- worldRowsByOwner (take (length baseRows) admitted)
      visible <- worldRowsByOwner (drop (length baseRows) admitted)
      let pairs rows = [(offered,input) | WorldAdmittedRow offered input _ _ <- Map.elems rows]
      primaryFacts <- readWorldPrimary frontier associationRoot context producer (pairs base)
      lexical <- readWorldLexicalSurface frontier lexicalRoot primaryFacts
      (bindings,bindingCharge) <- readWorldBindings frontier bindingRoot producer (pairs visible)
      verified <- retainVerifiedWorldFrontier [primary,associationRoot,bindingRoot,lexicalRoot] frontier
      bindingRows <- retainWorldBindingRows frontier producer visible bindings
      rowCharge <- foldM (\charge (WorldAdmittedRow _ _ _ more) -> checkedWorldCharge charge more)
        (0,0) admitted
      let identity = SHA.hash restoration
          sourceEntries = toInteger (length (worldSourceRoots sources))
            + sum [toInteger (Map.size files) | WorldSourceRoot files <- worldSourceRoots sources]
      when (sourceEntries > toInteger (maxBound :: Int)) (worldCorrupt "compiler world source count exceeds host range")
      let facts = WorldInputFacts (hexBytes identity) identity producer profile sources
            base visible (worldVisibleSemantic visible) custody users verified
            (toInteger (BS.length sourceBytes),fromInteger sourceEntries) rowCharge bindingRows
            (Map.fromListWith (+) [(semantic,n) | (_,semantic,_,n) <- Map.elems bindingRows])
            (BS.copy primary) (BS.copy visibility) bindingScope bindings bindingCharge primaryFacts lexical
      validateWorldRetainedCharge facts
      pure facts

-- Row admission captures only the row parts. Transport descriptors and source
-- envelopes never enter successor custody, and canonical proofs retain only
-- their own sealed bodies through the existing proof owner.
admitWorldRows :: MVar RetainedOriginalInputs -> BS.ByteString -> InputAcquisition -> [OfferedWorldRow]
  -> IO ([WorldAdmittedRow],RequestOriginalInputs,Map.Map FilePath Int)
admitWorldRows state producer acquisition offered = do
  images <- case acquisition of
    ContinueOwnedOriginals values -> pure values
    FreshFiles -> worldCorrupt "world descriptor requires owned original acquisition"
  -- Available custody can include historical originals unrelated to the
  -- selected semantic rows. Only those rows select images for capture; passive
  -- images neither authorize types nor require access to their DATA endpoints.
  selectedParts <- mapM (selectWorldRowParts producer images) offered
  retained <- if null offered then pure [] else do
    RetainedOriginalInputs values _ _ _ _ <- readMVar state
    pure values
  let available = foldl' (\content (_,more) -> mergeCapturedOriginalContent content more)
        emptyCapturedOriginalContent retained
  originals <- continueRequestInputs available (concat selectedParts)
  (rows,_) <- withCapturedRequestInputTokens originals $ \readToken -> do
    let verified artifact cap sha = do
          token <- readToken artifact cap
          either worldCorrupt pure (checkArtifactSeal sha token)
          pure (artifactBytes token)
    forM offered $ \entry@((row@(iface,packages,packageSha),evidence),_) -> do
      _ <- verified (exactPath iface) (32 * 1024 * 1024) (exactSha256 iface)
      packageBytes <- verified packages (4 * 1024 * 1024) packageSha
      _ <- either worldCorrupt pure (decodeCapturedPackageImports iface packageBytes)
      admitted <- case evidence of
        ParsedModuleEvidence descriptor -> do
          proof <- validateCanonicalRowUsing verified (readCertificateWith (\artifact cap -> artifactBytes <$> readToken artifact cap))
            (hexBytes producer) row descriptor
          forM_ (descriptorCore descriptor) $ \core -> do
            _ <- verified (canonicalCorePath core) (32 * 1024 * 1024) (canonicalCoreSha256 core)
            pure ()
          pure (ModuleInterfaceEvidence (bindCanonicalProofInputs originals proof))
        ParsedJoinEvidence -> pure LexicalJoinEvidence
        ParsedValueEvidence -> pure CheckedValueEvidence
      semantic <- toStrictByteString <$> worldSemanticRowEncoding producer entry
      let charge = (sum [toInteger (BS.length (TE.encodeUtf8 (T.pack path))) + toInteger (length sha)
            | (path,sha) <- worldOfferedSeals entry] + toInteger (BS.length semantic),1)
      pure (WorldAdmittedRow entry (ScopeInterfaceInput row admitted) semantic charge)
  let users = Map.fromListWith (+) [(path,1) | entry <- offered, (path,_) <- worldOfferedSeals entry]
  let capturedBodies = requestInputBodies originals
  selected <- forM (Map.keys users) $ \path -> do
    body <- maybe (worldCorrupt "world row part lost its captured body") pure (Map.lookup path capturedBodies)
    pure (path,body)
  (_,empty) <- captureRequestInputs Nothing (const (pure ()))
  custody <- either worldCorrupt pure (transferRequestInputBodies selected empty)
  pure (rows,custody,users)

worldOfferedSeals :: OfferedWorldRow -> [(FilePath,String)]
worldOfferedSeals (((iface,packages,packageSha),evidence),_) =
  [(exactPath iface,exactSha256 iface),(packages,packageSha)] ++ case evidence of
    ParsedModuleEvidence descriptor ->
      [(descriptorCertificatePath descriptor,descriptorCertificateSha256 descriptor)]
      ++ [(canonicalCorePath core,canonicalCoreSha256 core) | core <- maybe [] pure (descriptorCore descriptor)]
    _ -> []

worldRowsByOwner :: [WorldAdmittedRow] -> IO (Map.Map InterfaceOwner WorldAdmittedRow)
worldRowsByOwner rows = foldM insert Map.empty rows
  where
    insert indexed value@(WorldAdmittedRow ((row,_),_) _ _ _) = do
      when (Map.member (rowOwner row) indexed) (worldCorrupt "compiler world repeats an acquired row owner")
      pure (Map.insert (rowOwner row) value indexed)

worldVisibleSemantic :: Map.Map InterfaceOwner WorldAdmittedRow -> Map.Map BS.ByteString ScopeInterfaceInput
worldVisibleSemantic rows = Map.fromList
  [(semantic,input) | WorldAdmittedRow _ input semantic _ <- Map.elems rows]

checkedWorldCharge :: (Integer,Int) -> (Integer,Int) -> IO (Integer,Int)
checkedWorldCharge (bytes,entries) (more,count) = do
  let next = toInteger entries + toInteger count
  unless (bytes + more >= 0 && next >= 0 && next <= toInteger (maxBound :: Int))
    (worldCorrupt "compiler world retained charge exceeds host range")
  pure (bytes + more,fromInteger next)

validateWorldRetainedCharge :: WorldInputFacts -> IO ()
validateWorldRetainedCharge facts = do
  let custody = worldInputsCustody facts
      VerifiedWorldFrontier _ _ _ _ frontier = worldInputsFrontier facts
      charges = [(requestInputBytes custody,requestInputCount custody),worldInputsBindingCharge facts
        ,worldPrimaryRetainedCharge (worldInputsPrimary facts),worldLexicalCharge (worldInputsLexicalSurface facts)
        ,frontier,worldInputsSourceCharge facts,worldInputsRowCharge facts
        ,(fst (worldInputsBindingCharge facts),0)]
      demandEntries = 2 * toInteger (Map.size (worldInputsBindings facts))
  (bytes,entries) <- foldM checkedWorldCharge (0,0) charges
  unless (toInteger entries + demandEntries <= toInteger (maxBound :: Int))
    (worldCorrupt "compiler world retained entry count exceeds host range")
  limit <- requestCaptureByteLimit
  let backing = requestInputBytes custody + fst frontier
        + fst (worldInputsSourceCharge facts) + fst (worldInputsRowCharge facts)
  when (backing > limit)
    (worldCorrupt ("compiler world captured backing exceeds receiving byte budget: observed " ++ show backing ++ ", limit " ++ show limit))
  _ <- evaluate bytes
  pure ()

-- The wire describes exact next facts. Lineage is checked by the surrounding
-- held-world request owner, independently of this immutable reconstruction.
readWorldSuccessorWithOwner :: ExactInputOwner -> WorldInputFacts -> FilePath -> IO WorldSuccessorInputs
readWorldSuccessorWithOwner (ExactInputOwner _ state) previous path = acquire `catch` classify
  where
    classify (failure :: IOException) = throwIO (WorldInputsFailure
      (if isUserError failure then WorldInputsCorrupt else WorldInputsUnavailable) (show failure))
    acquire = do
      unless (isAbsolute path) (worldCorrupt "world successor descriptor path must be absolute")
      limit <- requestCaptureByteLimit
      let bound = fromInteger (min limit (toInteger (maxBound :: Int)-1))
      (offered,_) <- captureRequestInputs Nothing $ \readInput -> do
        bytes <- readInput path bound
        decodeWorldBytes (decodeWorldSuccessor (BS.length bytes)) bytes
      let OfferedWorldSuccessor restoration blocks primaryChanges visibleChanges acquisition = offered
      WorldRestoration primary domainSeal producer profile scope associations bindings lexicalRoot <-
        decodeWorldBytes decodeWorldRestoration restoration
      let canonical = encodeWorldRestoration primary domainSeal producer profile scope associations bindings lexicalRoot
      unless (restoration == canonical && producer == worldInputsProducer previous
          && profile == worldInputsProfile previous && scope == worldInputsBindingScope previous
          && domainSeal == worldSourceDomainSha256 (worldInputsSources previous))
        (worldCorrupt "world successor differs from captured source, profile, producer or scope")
      let VerifiedWorldFrontier _ _ _ oldRoots _ = worldInputsFrontier previous
      (oldPrimary,oldAssociations,oldBindings,oldLexical) <- case oldRoots of
        [a,b,c,d] -> pure (a,b,c,d)
        _ -> worldCorrupt "world predecessor lost its fixed semantic roots"
      verified <- advanceVerifiedWorldFrontier (worldInputsFrontier previous)
        [primary,associations,bindings,lexicalRoot] blocks
      let frontier = worldFrontierBodies verified
          -- Old changed HAMT paths remain available only during comparison.
          comparison = foldl' (\allBlocks (cid,_) -> case Map.lookup cid frontier of
            Just body -> Map.insert cid body allBlocks
            Nothing -> allBlocks) (worldFrontierBodies (worldInputsFrontier previous)) blocks
      (newRows,incoming,_) <- admitWorldRows state producer acquisition
        [row | (_,Just row) <- primaryChanges ++ visibleChanges]
      let (primaryAdmitted,visibleAdmitted) = splitAt (length [() | (_,Just _) <- primaryChanges]) newRows
      (base,removedBase,addedBase) <- applyWorldRows (worldInputsBaseRows previous) primaryChanges primaryAdmitted
      (visible,removedVisible,addedVisible) <- applyWorldRows (worldInputsVisibleRows previous) visibleChanges visibleAdmitted
      let removed = removedBase ++ removedVisible
          added = addedBase ++ addedVisible
      (custody,users) <- editWorldCustody (worldInputsCustody previous) (worldInputsCustodyUsers previous)
        incoming removed added
      rowCharge <- foldM (\cost (WorldAdmittedRow _ _ _ (bytes,count)) -> checkedWorldCharge cost (-bytes,-count))
        (worldInputsRowCharge previous) removed
        >>= \cost -> foldM (\next (WorldAdmittedRow _ _ _ charge) -> checkedWorldCharge next charge) cost added
      let semantic = foldl' (\rows (WorldAdmittedRow _ _ key _) -> Map.delete key rows)
            (worldInputsVisibleSemantic previous) removedVisible
          nextSemantic = foldl' (\rows (WorldAdmittedRow _ input key _) -> Map.insert key input rows) semantic addedVisible
      (aliasChanges,nodeCharge) <- worldHamtChanges comparison oldBindings bindings
        (T.unpack <$> worldBindingText) (E.encodeString . T.pack) decodeWorldBindingValue
      (nextBindings,nextRows,semanticUsers,bindingCharge) <- applyWorldBindings frontier nextSemantic
        (worldInputsBindings previous) (worldInputsBindingRows previous) (worldInputsBindingSemanticUsers previous) (worldInputsBindingCharge previous)
        nodeCharge aliasChanges
      validateChangedVisibleRows nextSemantic (worldInputsBindingRows previous) nextRows semanticUsers removedVisible addedVisible aliasChanges
      primaryFacts <- if primary == oldPrimary && associations == oldAssociations
        then do
          unless (null removedBase && null addedBase)
            (worldCorrupt "world primary transport changed without its semantic roots")
          pure (worldInputsPrimary previous)
        else do
          timing <- readTimingEnabled
          emitCount timing "world.successor.primary_full_validation_rows" (toInteger (Map.size base))
          context <- if primary == oldPrimary then pure (worldPrimaryContext (worldInputsPrimary previous))
            else readWorldContext frontier primary producer
          readWorldPrimary frontier associations context producer
            [(row,input) | WorldAdmittedRow row input _ _ <- Map.elems base]
      lexical <- if lexicalRoot == oldLexical && primary == oldPrimary && associations == oldAssociations
        then pure (worldInputsLexicalSurface previous)
        else readWorldLexicalSurface frontier lexicalRoot primaryFacts
      let identity = SHA.hash restoration
          visibility = toStrictByteString (E.encodeListLen 3 <> E.encodeString "TPCWBIND4"
            <> E.encodeWord64 scope <> E.encodeBytes bindings)
          facts = WorldInputFacts (hexBytes identity) identity producer profile (worldInputsSources previous)
            base visible nextSemantic custody users verified (worldInputsSourceCharge previous) rowCharge nextRows semanticUsers
            primary visibility scope nextBindings bindingCharge primaryFacts lexical
      validateWorldRetainedCharge facts
      timing <- readTimingEnabled
      emitCount timing "world.successor.changed_aliases" (toInteger (Map.size aliasChanges))
      emitCount timing "world.successor.changed_rows" (toInteger (length removed + length added))
      pure (WorldSuccessorInputs facts (Map.keysSet aliasChanges))

encodeWorldRestoration :: BS.ByteString -> BS.ByteString -> BS.ByteString -> BS.ByteString
  -> Word64 -> BS.ByteString -> BS.ByteString -> BS.ByteString -> BS.ByteString
encodeWorldRestoration primary domain producer profile scope associations bindings lexical =
  toStrictByteString (E.encodeListLen 10 <> E.encodeString "TPCWREST" <> E.encodeWord 4
    <> E.encodeBytes primary <> E.encodeBytes domain <> E.encodeBytes producer <> E.encodeBytes profile
    <> E.encodeWord64 scope <> E.encodeBytes associations <> E.encodeBytes bindings <> E.encodeBytes lexical)

data OfferedWorldSuccessor = OfferedWorldSuccessor BS.ByteString [(BS.ByteString,BS.ByteString)]
  [(InterfaceOwner,Maybe OfferedWorldRow)] [(InterfaceOwner,Maybe OfferedWorldRow)] InputAcquisition

decodeWorldSuccessor :: Int -> Decoder s OfferedWorldSuccessor
decodeWorldSuccessor payloadBytes = do
  array 6
  magic <- string
  unless (magic == "TPCWSUCC4") (fail "unsupported compiler world successor")
  restoration <- decodeBytes
  blocks <- bounded payloadBytes (array 2 >> ((,) <$> decodeBytes <*> decodeBytes))
  primary <- changes
  visible <- changes
  acquisition <- decodeInputAcquisition
  pure (OfferedWorldSuccessor restoration blocks primary visible acquisition)
  where
    changes = do
      rows <- bounded payloadBytes $ do
        array 3
        key <- (,) <$> nonempty <*> nonempty
        token <- peekTokenType
        row <- if token == TypeNull then decodeNull >> pure Nothing
          else Just <$> (array 2 >> ((,) <$> decodeInterfaceRow <*> rawWorldDigest))
        forM_ row $ \((input,_),_) -> unless (rowOwner input == key)
          (fail "compiler world row edit has another owner")
        pure (key,row)
      unique "compiler world row edits" (map fst rows)
      pure rows

applyWorldRows :: Map.Map InterfaceOwner WorldAdmittedRow -> [(InterfaceOwner,Maybe OfferedWorldRow)]
  -> [WorldAdmittedRow] -> IO (Map.Map InterfaceOwner WorldAdmittedRow,[WorldAdmittedRow],[WorldAdmittedRow])
applyWorldRows original changes admitted = do
  incoming <- worldRowsByOwner admitted
  foldM (edit incoming) (original,[],[]) changes
  where
    edit incoming (rows,removed,added) (key,replacement) = do
      let before = Map.lookup key rows
      after <- case replacement of
        Nothing -> do
          when (isNothing before) (worldCorrupt "compiler world removes an absent row owner")
          pure Nothing
        Just _ -> Just <$> maybe (worldCorrupt "compiler world row edit lost its admitted input") pure (Map.lookup key incoming)
      case (before,after) of
        -- Physical aliases do not replace immutable semantic facts. Retain the
        -- existing correlated row and its sealed bodies when its image agrees.
        (Just (WorldAdmittedRow _ _ old _),Just (WorldAdmittedRow _ _ new _)) | old == new -> pure (rows,removed,added)
        _ -> pure (maybe (Map.delete key rows) (\row -> Map.insert key row rows) after
          ,maybe removed (:removed) before,maybe added (:added) after)

editWorldCustody :: RequestOriginalInputs -> Map.Map FilePath Int -> RequestOriginalInputs
  -> [WorldAdmittedRow] -> [WorldAdmittedRow] -> IO (RequestOriginalInputs,Map.Map FilePath Int)
editWorldCustody original users incoming removed added = do
  (released,remaining) <- foldM release (original,users)
    [path | WorldAdmittedRow offered _ _ _ <- removed,(path,_) <- worldOfferedSeals offered]
  foldM acquire (released,remaining)
    [(path,sha) | WorldAdmittedRow offered _ _ _ <- added,(path,sha) <- worldOfferedSeals offered]
  where
    release (custody,counts) path = case Map.lookup path counts of
      Just 1 -> do
        next <- either worldCorrupt pure (editDirectRequestInputBodies [(path,Nothing)] custody)
        pure (next,Map.delete path counts)
      Just n | n > 1 -> pure (custody,Map.insert path (n-1) counts)
      _ -> worldCorrupt "compiler world row part has no custody reference"
    acquire (custody,counts) (path,sha) = do
      body <- capturedRequestInputToken incoming path sha
      case Map.lookup path counts of
        Nothing -> do
          next <- either worldCorrupt pure (editDirectRequestInputBodies [(path,Just body)] custody)
          pure (next,Map.insert path 1 counts)
        Just n -> do
          _ <- capturedRequestInputToken custody path sha
          when (n == maxBound) (worldCorrupt "compiler world row part references exceed host range")
          pure (custody,Map.insert path (n+1) counts)

applyWorldBindings :: Map.Map BS.ByteString BS.ByteString -> Map.Map BS.ByteString ScopeInterfaceInput
  -> Map.Map String WorldBinding -> Map.Map BS.ByteString (WorldBindingRow,BS.ByteString,Integer,Int)
  -> Map.Map BS.ByteString Int -> (Integer,Int) -> (Integer,Int)
  -> Map.Map String (Maybe (SymbolIdentity,Word64,Word64,Word64,BS.ByteString),Maybe (SymbolIdentity,Word64,Word64,Word64,BS.ByteString))
  -> IO (Map.Map String WorldBinding,Map.Map BS.ByteString (WorldBindingRow,BS.ByteString,Integer,Int),Map.Map BS.ByteString Int,(Integer,Int))
applyWorldBindings frontier eligible original retained semanticUsers charge nodeCharge changes = do
  initialCharge <- checkedWorldCharge charge nodeCharge
  (without,rows,uses,cost) <- foldM remove (original,retained,semanticUsers,initialCharge) (Map.toAscList changes)
  foldM insert (without,rows,uses,cost) (Map.toAscList changes)
  where
    remove (bindings,rows,uses,cost) (alias,(before,_)) = do
      unless (fmap worldBindingIdentity (Map.lookup alias bindings) == before)
        (worldCorrupt "compiler world alias delta differs from its predecessor")
      case before of
        Nothing -> pure (bindings,rows,uses,cost)
        Just (_,_,_,_,cid) -> case Map.lookup cid rows of
          Just (row,semantic,bytes,users) -> do
            next <- checkedWorldCharge cost (if users == 1 then (-bytes,-2) else (0,-1))
            let kept = if users == 1 then Map.delete cid rows else Map.insert cid (row,semantic,bytes,users-1) rows
            nextUses <- case Map.lookup semantic uses of
              Just 1 -> pure (Map.delete semantic uses)
              Just n | n > 1 -> pure (Map.insert semantic (n-1) uses)
              _ -> worldCorrupt "compiler world binding semantic reference is absent"
            pure (Map.delete alias bindings,kept,nextUses,next)
          Nothing -> worldCorrupt "compiler world alias lost its retained row"
    insert state (_,(_,Nothing)) = pure state
    insert (bindings,rows,uses,cost) (alias,(_,Just value@(_,_,_,generation,cid))) = do
      (binding,semantic,bytes) <- case Map.lookup cid rows of
        Just (row,semantic,bytes,_) -> do
          unless (Map.member semantic eligible) (worldCorrupt "compiler world retained row is no longer visible")
          let (symbol,variable,scope,_,_) = value
              (artifact,_,_) = worldBindingRowInterface row
          unless (sessionModuleString (SessionModule ValMod (Generation generation)) == exactModule artifact)
            (worldCorrupt "compiler world binding generation differs from its checked value owner")
          pure (WorldBinding symbol variable scope generation row,semantic,bytes)
        Nothing -> readChangedWorldBinding frontier eligible value
      let users = maybe 0 (\(_,_,_,n) -> n) (Map.lookup cid rows)
      when (users == maxBound) (worldCorrupt "compiler world row references exceed host range")
      next <- checkedWorldCharge cost (if users == 0 then (bytes,2) else (0,1))
      let semanticCount = Map.findWithDefault 0 semantic uses
      when (semanticCount == maxBound) (worldCorrupt "compiler world semantic references exceed host range")
      pure (Map.insert alias binding bindings,Map.insert cid (worldBindingRow binding,semantic,bytes,users+1) rows
        ,Map.insert semantic (semanticCount+1) uses,next)

validateChangedVisibleRows :: Map.Map BS.ByteString ScopeInterfaceInput
  -> Map.Map BS.ByteString (WorldBindingRow,BS.ByteString,Integer,Int)
  -> Map.Map BS.ByteString (WorldBindingRow,BS.ByteString,Integer,Int)
  -> Map.Map BS.ByteString Int -> [WorldAdmittedRow] -> [WorldAdmittedRow]
  -> Map.Map String (Maybe (SymbolIdentity,Word64,Word64,Word64,BS.ByteString),Maybe (SymbolIdentity,Word64,Word64,Word64,BS.ByteString))
  -> IO ()
validateChangedVisibleRows eligible previous retained semanticUsers removed added changes = do
  let changedCIDs = Set.fromList [cid | (before,after) <- Map.elems changes
        ,(_,_,_,_,cid) <- maybe [] pure before ++ maybe [] pure after]
      touched = Set.fromList ([semantic | WorldAdmittedRow _ _ semantic _ <- removed ++ added]
        ++ [semantic | cid <- Set.toList changedCIDs,rows <- [previous,retained]
          ,Just (_,semantic,_,_) <- [Map.lookup cid rows]])
      -- One visible semantic row can have several provenance records; check
      -- only rows affected by aliases or transport edits against the new census.
      changedRows = Map.restrictKeys retained changedCIDs
  forM_ (Map.elems changedRows) $ \(_,semantic,_,_) -> unless (Map.member semantic eligible)
    (worldCorrupt "compiler world occurrence references a removed visible row")
  -- Transport-only removal must not leave aliases borrowing that semantic row.
  -- This bounded check is indexed below by the retained semantic user counts.
  forM_ (Set.toList touched) $ \semantic -> do
    let users = Map.findWithDefault 0 semantic semanticUsers
    unless ((users > 0) == Map.member semantic eligible)
      (worldCorrupt "compiler world visible input census differs from occurrence facts")

decodeWorldBytes :: (forall s. Decoder s a) -> BS.ByteString -> IO a
decodeWorldBytes parser bytes = case deserialiseFromBytes parser (BL.fromStrict bytes) of
  Left failure -> worldCorrupt (show failure)
  Right (remaining,value) | BL.null remaining -> pure value
  Right _ -> worldCorrupt "world input has trailing bytes"

-- The existing Materializer issues one fixed CID family. Require its canonical
-- CIDv1/DAG-CBOR/Blake2b-256 framing before hashing; SHA transfer commitments do
-- not independently authenticate the block's content address.
verifyWorldBlock :: (BS.ByteString,BS.ByteString) -> IO ()
verifyWorldBlock (cid,body)
  | not (supportedWorldCID cid) =
      worldCorrupt "unsupported compiler world block CID"
  | Crypto.digestFromByteString (BS.drop 6 cid)
      /= Just (Crypto.hash body :: Crypto.Digest Crypto.Blake2b_256) =
      worldCorrupt "compiler world block CID differs from its body"
  | otherwise = pure ()

supportedWorldCID :: BS.ByteString -> Bool
supportedWorldCID cid = BS.length cid == 38 && BS.take 6 cid == BS.pack [1,0x71,0xa0,0xe4,2,32]

verifyWorldBlocks :: [BS.ByteString] -> [(BS.ByteString,BS.ByteString)] -> IO (Map.Map BS.ByteString BS.ByteString)
verifyWorldBlocks roots blocks = do
  unless (Set.size (Set.fromList (map fst blocks)) == length blocks)
    (worldCorrupt "duplicate compiler world block CID")
  mapM_ verifyWorldBlock blocks
  linked <- forM blocks $ \(cid,body) -> do
    links <- decodeWorldBytes (worldBlockLinks (BS.length body)) body
    pure (cid,links)
  let available = Map.fromList linked
      visit seen pending = case Set.minView pending of
        Nothing -> pure seen
        Just (cid,remaining) | cid `Set.member` seen -> visit seen remaining
        Just (cid,remaining) -> case Map.lookup cid available of
          Nothing -> worldCorrupt "compiler world block child is absent"
          Just children -> visit (Set.insert cid seen) (Set.union children remaining)
  visited <- visit Set.empty (Set.fromList roots)
  unless (visited == Map.keysSet available) (worldCorrupt "compiler world frontier contains unreachable blocks")
  pure (Map.fromList [(BS.copy cid,BS.copy body) | (cid,body) <- blocks])

retainVerifiedWorldFrontier :: [BS.ByteString] -> Map.Map BS.ByteString BS.ByteString -> IO VerifiedWorldFrontier
retainVerifiedWorldFrontier roots bodies = do
  links <- Map.traverseWithKey (\_ body -> decodeWorldBytes (worldBlockLinks (BS.length body)) body) bodies
  let refs = Map.fromListWith (+) ([(cid,1) | cid <- roots]
        ++ [(child,1) | children <- Map.elems links,child <- Set.toList children])
      bytes = sum [toInteger (BS.length cid) + toInteger (BS.length body)
        + toInteger (Set.size children) * 38
        | (cid,body) <- Map.toList bodies,Just children <- [Map.lookup cid links]]
  pure (VerifiedWorldFrontier bodies links refs (map BS.copy roots) (bytes,Map.size bodies))

advanceVerifiedWorldFrontier :: VerifiedWorldFrontier -> [BS.ByteString]
  -> [(BS.ByteString,BS.ByteString)] -> IO VerifiedWorldFrontier
advanceVerifiedWorldFrontier (VerifiedWorldFrontier bodies links refs oldRoots charge) roots offered = do
  unless (Set.size (Set.fromList (map fst offered)) == length offered)
    (worldCorrupt "duplicate compiler world successor block CID")
  blocks <- forM offered $ \block@(cid,body) -> do
    verifyWorldBlock block
    case (Map.lookup cid bodies,Map.lookup cid links) of
      (Just held,Just children) -> pure (BS.copy cid,(held,children))
      _ -> do
        let ownedCID = BS.copy cid
            ownedBody = BS.copy body
        children <- decodeWorldBytes (worldBlockLinks (BS.length ownedBody)) ownedBody
        pure (ownedCID,(ownedBody,children))
  let overlay (nextBodies,nextLinks) (cid,(body,children)) =
        (Map.insert cid body nextBodies,Map.insert cid children nextLinks)
      (available,adjacent) = foldl' overlay (bodies,links) blocks
      blockCharge cid body children =
        (toInteger (BS.length cid) + toInteger (BS.length body) + toInteger (Set.size children) * 38,1)
      add (counts,cost) cid = case Map.lookup cid counts of
        Just count -> do
          when (count == maxBound) (worldCorrupt "compiler world block reference count exceeds host range")
          pure (Map.insert cid (count+1) counts,cost)
        Nothing -> do
          body <- maybe (worldCorrupt "compiler world successor block child is absent") pure (Map.lookup cid available)
          children <- maybe (worldCorrupt "compiler world successor block links are absent") pure (Map.lookup cid adjacent)
          nextCost <- checkedWorldCharge cost (blockCharge cid body children)
          foldM add (Map.insert cid 1 counts,nextCost) (Set.toAscList children)
      remove (nextBodies,nextLinks,counts,cost) cid = case Map.lookup cid counts of
        Just count | count > 1 -> pure (nextBodies,nextLinks,Map.insert cid (count-1) counts,cost)
        Just 1 -> do
          body <- maybe (worldCorrupt "compiler world retired block body is absent") pure (Map.lookup cid nextBodies)
          children <- maybe (worldCorrupt "compiler world retired block links are absent") pure (Map.lookup cid nextLinks)
          let (bytes,entries) = blockCharge cid body children
          nextCost <- checkedWorldCharge cost (-bytes,-entries)
          foldM remove (Map.delete cid nextBodies,Map.delete cid nextLinks,Map.delete cid counts,nextCost)
            (Set.toAscList children)
        _ -> worldCorrupt "compiler world retired block reference is absent"
  -- Add before releasing old roots so unchanged shared branches never cross
  -- zero references and require no traversal or reauthentication.
  (addedRefs,addedCharge) <- foldM add (refs,charge) roots
  (nextBodies,nextLinks,nextRefs,nextCharge) <- foldM remove
    (available,adjacent,addedRefs,addedCharge) oldRoots
  forM_ blocks $ \(cid,_) -> unless (Map.member cid nextRefs)
    (worldCorrupt "compiler world successor contains unreachable blocks")
  timing <- readTimingEnabled
  emitCount timing "world.successor.changed_blocks" (toInteger (length blocks))
  emitCount timing "world.successor.frontier_blocks" (toInteger (Map.size nextBodies))
  emitCount timing "world.successor.frontier_bytes" (fst nextCharge)
  pure (VerifiedWorldFrontier nextBodies nextLinks nextRefs (map BS.copy roots) nextCharge)

-- This data reader uses the same fixed HAMT fold as the owning context maps.
-- It cannot issue a compiler world, scope hold, runtime grant or Ready result.
readWorldStringMap :: BS.ByteString -> [(BS.ByteString,BS.ByteString)] -> IO (Map.Map String String)
readWorldStringMap root blocks = do
  frontier <- verifyWorldBlocks [root] blocks
  worldHamtMap frontier root (T.unpack <$> decodeStringCanonical) (E.encodeString . T.pack)
    (T.unpack <$> decodeStringCanonical)

data WorldHamtPointer value = WorldHamtLink BS.ByteString | WorldHamtBucket [(BS.ByteString,value)]

-- Fixed fvm_ipld_hamt 0.10.6 V3: bit width five, minimum data depth zero,
-- maximum inline bucket three. Keys hash their exact canonical SnapshotKey
-- bytes with SHA256; key/value codecs belong to the selected map's owner.
worldHamtMap :: forall key value. Ord key
  => Map.Map BS.ByteString BS.ByteString -> BS.ByteString
  -> (forall s. Decoder s key) -> (key -> E.Encoding)
  -> (forall s. Decoder s value) -> IO (Map.Map key value)
worldHamtMap frontier root keyDecoder keyEncoder valueDecoder =
  fst <$> worldHamtMapWithCharge frontier root keyDecoder keyEncoder valueDecoder

-- The stored charge is the known encoded backing and actual node count, not
-- exact heap bytes. Callers add decoded records to the SAME input-owner budget.
worldHamtMapWithCharge :: forall key value. Ord key
  => Map.Map BS.ByteString BS.ByteString -> BS.ByteString
  -> (forall s. Decoder s key) -> (key -> E.Encoding)
  -> (forall s. Decoder s value) -> IO (Map.Map key value,(Integer,Int))
worldHamtMapWithCharge frontier root keyDecoder keyEncoder valueDecoder = walk Set.empty Map.empty 0 [(root,[])]
  where
    walk seen facts bytes [] = pure (facts,(bytes,Set.size seen))
    walk seen facts bytes ((cid,route):rest) = do
      when (cid `Set.member` seen) (worldCorrupt "repeated compiler world HAMT node")
      let depth = length route
      when (depth >= 52) (worldCorrupt "compiler world HAMT exceeds SHA256 routing depth")
      body <- maybe (worldCorrupt "compiler world HAMT child is absent") pure (Map.lookup cid frontier)
      pointers <- decodeWorldBytes (worldHamtNode valueDecoder depth) body
      (next,children) <- foldM (pointer route) (facts,[]) pointers
      charged <- evaluate (bytes + toInteger (BS.length body))
      walk (Set.insert cid seen) next charged (children ++ rest)
    pointer route (facts,children) (slot,WorldHamtLink cid) =
      pure (facts,(cid,route ++ [slot]):children)
    pointer route (facts,children) (slot,WorldHamtBucket entries) = do
      next <- foldM (insert (route ++ [slot])) facts entries
      pure (next,children)
    insert route facts (encoded,value) = do
      key <- decodeWorldBytes (worldArray 1 >> keyDecoder) encoded
      unless (toStrictByteString (E.encodeListLen 1 <> keyEncoder key) == encoded)
        (worldCorrupt "noncanonical typed compiler world HAMT key")
      let hashed = SHA.hash encoded
          bucket level = foldl (\n bit -> n * 2 + if testBit (BS.index hashed (bit `div` 8)) (7 - bit `mod` 8) then 1 else 0)
            0 [level*5 .. min 255 (level*5+4)]
      unless (and [bucket level == slot | (level,slot) <- zip [0..] route])
        (worldCorrupt "compiler world HAMT key has another hash route")
      when (Map.member key facts) (worldCorrupt "duplicate typed compiler world HAMT key")
      pure (Map.insert key value facts)

worldHamtNode :: (forall s. Decoder s value) -> Int -> Decoder s [(Int,WorldHamtPointer value)]
worldHamtNode valueDecoder depth = do
  worldArray 2
  bits <- decodeBytesCanonical
  unless (BS.length bits <= 4 && (BS.null bits || BS.head bits /= 0))
    (fail "invalid compiler world HAMT bitmap")
  let bitmap = BS.foldl' (\n byte -> n * 256 + fromIntegral byte) (0 :: Word32) bits
      slots = [slot | slot <- [0..31], testBit bitmap slot]
      width = min 5 (256-depth*5)
  unless (all (< (1 `shiftL` width)) slots) (fail "compiler world HAMT bitmap exceeds routing width")
  count <- decodeListLenCanonical
  unless (count <= 32 && count == popCount bitmap && (depth == 0 || count /= 0))
    (fail "compiler world HAMT pointers differ from bitmap")
  forM slots $ \slot -> do
    token <- peekTokenType
    selected <- case token of
      TypeTag -> WorldHamtLink <$> worldCID
      TypeListLen -> do
        count <- decodeListLenCanonical
        unless (count > 0 && count <= 3) (fail "invalid compiler world HAMT bucket width")
        entries <- replicateM count $ do
          worldArray 2
          (,) <$> decodeBytesCanonical <*> valueDecoder
        unless (and (zipWith (<) (map fst entries) (drop 1 (map fst entries))))
          (fail "duplicate or unsorted compiler world HAMT bucket keys")
        pure (WorldHamtBucket entries)
      _ -> fail "unsupported compiler world HAMT pointer"
    pure (slot,selected)

-- Compare immutable HAMT paths by CID before decoding. Only changed branches
-- produce keyed edits; pointer/bucket transitions decode their bounded subtree.
-- The signed node charge updates the retained index without a whole-map fold.
worldHamtChanges :: forall key value. (Ord key,Eq value)
  => Map.Map BS.ByteString BS.ByteString -> BS.ByteString -> BS.ByteString
  -> (forall s. Decoder s key) -> (key -> E.Encoding) -> (forall s. Decoder s value)
  -> IO (Map.Map key (Maybe value,Maybe value),(Integer,Int))
worldHamtChanges frontier oldRoot newRoot keyDecoder keyEncoder valueDecoder = do
  (changes,(bytes,nodes)) <- compareNodes [] oldRoot newRoot
  unless (nodes >= toInteger (minBound :: Int) && nodes <= toInteger (maxBound :: Int))
    (worldCorrupt "compiler world HAMT delta count exceeds host range")
  pure (changes,(bytes,fromInteger nodes))
  where
    readNode route cid = do
      when (length route >= 52) (worldCorrupt "compiler world HAMT exceeds SHA256 routing depth")
      body <- maybe (worldCorrupt "compiler world HAMT child is absent") pure (Map.lookup cid frontier)
      values <- decodeWorldBytes (worldHamtNode valueDecoder (length route)) body
      pure (Map.fromList values,(toInteger (BS.length body),1 :: Integer))
    keyAt route encoded = do
      key <- decodeWorldBytes (worldArray 1 >> keyDecoder) encoded
      unless (toStrictByteString (E.encodeListLen 1 <> keyEncoder key) == encoded)
        (worldCorrupt "noncanonical typed compiler world HAMT key")
      let hashed = SHA.hash encoded
          bucket level = foldl' (\n bit -> n * 2 + if testBit (BS.index hashed (bit `div` 8)) (7-bit `mod` 8) then 1 else 0)
            0 [level*5 .. min 255 (level*5+4)]
      unless (and [bucket level == slot | (level,slot) <- zip [0..] route])
        (worldCorrupt "compiler world HAMT key has another hash route")
      pure key
    bucket route entries = foldM (\values (encoded,value) -> do
      key <- keyAt route encoded
      when (Map.member key values) (worldCorrupt "duplicate typed compiler world HAMT key")
      pure (Map.insert key value values)) Map.empty entries
    combine values more = do
      unless (Map.null (Map.intersection values more)) (worldCorrupt "duplicate typed compiler world HAMT key")
      pure (Map.union values more)
    flattenPointer _ Nothing = pure (Map.empty,(0,0))
    flattenPointer route (Just (WorldHamtBucket entries)) = do
      values <- bucket route entries
      pure (values,(0,0))
    flattenPointer route (Just (WorldHamtLink cid)) = do
      (pointers,charge) <- readNode route cid
      foldM (\(values,cost) (slot,pointer) -> do
        (more,next) <- flattenPointer (route ++ [slot]) (Just pointer)
        merged <- combine values more
        pure (merged,addWorldCharge cost next)) (Map.empty,charge) (Map.toAscList pointers)
    comparePointer route (Just (WorldHamtLink old)) (Just (WorldHamtLink new)) = compareNodes route old new
    comparePointer route old new = do
      (before,beforeCharge) <- flattenPointer route old
      (after,afterCharge) <- flattenPointer route new
      let keys = Set.union (Map.keysSet before) (Map.keysSet after)
          changes = Map.fromSet (\key -> (Map.lookup key before,Map.lookup key after)) keys
      pure (Map.filter (uncurry (/=)) changes,subtractCharge afterCharge beforeCharge)
    compareNodes _ old new | old == new = pure (Map.empty,(0,0))
    compareNodes route old new = do
      (before,beforeCharge) <- readNode route old
      (after,afterCharge) <- readNode route new
      let slots = Set.union (Map.keysSet before) (Map.keysSet after)
      foldM (\(changes,cost) slot -> do
        (more,next) <- comparePointer (route ++ [slot]) (Map.lookup slot before) (Map.lookup slot after)
        merged <- combine changes more
        pure (merged,addWorldCharge cost next))
        (Map.empty,subtractCharge afterCharge beforeCharge) (Set.toAscList slots)
    subtractCharge (bytes,count) (oldBytes,oldCount) = (bytes-oldBytes,count-oldCount)

worldArray :: Int -> Decoder s ()
worldArray expected = decodeListLenCanonical >>= \count -> unless (count == expected) (fail "invalid compiler world tuple")

worldCID :: Decoder s BS.ByteString
worldCID = do
  tag <- decodeTagCanonical
  unless (tag == 42) (fail "invalid compiler world link tag")
  encoded <- decodeBytesCanonical
  unless (not (BS.null encoded) && BS.head encoded == 0 && supportedWorldCID (BS.tail encoded))
    (fail "unsupported compiler world child CID")
  pure (BS.copy (BS.tail encoded))

worldArrayDigest :: Decoder s BS.ByteString
worldArrayDigest = worldArray 32 >> BS.pack <$> replicateM 32 decodeWord8Canonical

encodeWorldArrayDigest :: BS.ByteString -> E.Encoding
encodeWorldArrayDigest bytes = E.encodeListLen 32 <> foldMap E.encodeWord8 (BS.unpack bytes)

decodeWorldModuleKey :: Decoder s (String,String)
decodeWorldModuleKey = do
  count <- decodeMapLenCanonical
  unless (count == 2) (fail "invalid compiler world module key")
  unitField <- decodeStringCanonical
  unit <- decodeStringCanonical
  moduleField <- decodeStringCanonical
  name <- decodeStringCanonical
  unless (unitField == "unit" && moduleField == "module")
    (fail "invalid compiler world module key fields")
  pure (T.unpack unit,T.unpack name)

encodeWorldModuleKey :: (String,String) -> E.Encoding
encodeWorldModuleKey (unit,name) = E.encodeMapLen 2
  <> E.encodeString "unit" <> E.encodeString (T.pack unit)
  <> E.encodeString "module" <> E.encodeString (T.pack name)

decodeWorldBinding :: Decoder s (BS.ByteString,BS.ByteString)
decodeWorldBinding = do
  count <- decodeMapLenCanonical
  unless (count == 2) (fail "invalid compiler world artifact binding")
  artifactField <- decodeStringCanonical
  artifact <- worldArrayDigest
  selectionField <- decodeStringCanonical
  selection <- worldArrayDigest
  unless (artifactField == "artifact" && selectionField == "selection")
    (fail "invalid compiler world artifact binding fields")
  pure (artifact,selection)

encodeWorldBinding :: (BS.ByteString,BS.ByteString) -> E.Encoding
encodeWorldBinding (artifact,selection) = E.encodeMapLen 2
  <> E.encodeString "artifact" <> encodeWorldArrayDigest artifact
  <> E.encodeString "selection" <> encodeWorldArrayDigest selection

-- These are immutable logical facts under the existing world/input owner.
-- Artifact IDs, binding scopes and private Rust certification stay provenance;
-- only an exact selected association can identify an authenticated type row.
type WorldArtifactBinding = (BS.ByteString,BS.ByteString)

data WorldScopeDependency
  = WorldInterfaceDependency
  | WorldNativeGroupDependency Word32 Word32
  | WorldNativeBindingDependency Word32 Word64 String String (Maybe String)
  deriving (Eq,Ord)

data WorldScopeProof = WorldScopeProof BS.ByteString (Set.Set WorldBindingNodeKey)
  [(WorldBindingNodeKey,WorldBindingNodeKey,WorldScopeDependency)]

data WorldSelection = WorldSelection
  { worldSelectionScopes :: Map.Map BS.ByteString WorldScopeProof
  , worldSelectionRoots :: Set.Set WorldBindingNodeKey
  , worldSelectionNamespace :: Map.Map BS.ByteString WorldArtifactBinding
  , worldSelectionCustody :: Set.Set WorldBindingNodeKey
  , worldSelectionNodes :: Set.Set WorldBindingNodeKey
  }

data WorldCompilerRole
  = WorldInterfaceRole BS.ByteString
  | WorldReusableRole BS.ByteString BS.ByteString
  | WorldPublishedRole BS.ByteString BS.ByteString String String BS.ByteString BS.ByteString BS.ByteString

worldRoleInterface :: WorldCompilerRole -> BS.ByteString
worldRoleInterface role = case role of
  WorldInterfaceRole value -> value
  WorldReusableRole value _ -> value
  WorldPublishedRole value _ _ _ _ _ _ -> value

worldRoleOriginal :: WorldCompilerRole -> Maybe BS.ByteString
worldRoleOriginal role = case role of
  WorldInterfaceRole _ -> Nothing
  WorldReusableRole _ value -> Just value
  WorldPublishedRole _ value _ _ _ _ _ -> Just value

-- Native-domain maps retain the Rust-issued birth provenance. Neither their
-- presence nor a CID grants native execution or selects a compiler original.
type WorldNativeProvenance =
  (Map.Map BS.ByteString BS.ByteString,Map.Map BS.ByteString BS.ByteString,Map.Map (String,String) BS.ByteString)

data WorldNativeAuthority = WorldNativeAuthority
  { worldAuthorityRoles :: Map.Map (String,String) (WorldCompilerRole,Maybe (WorldSelection,WorldNativeAuthority))
  , worldAuthorityNative :: WorldNativeProvenance
  }

data WorldContextFacts = WorldContextFacts
  { worldContextRoles :: Map.Map (String,String) (WorldCompilerRole,Maybe (WorldSelection,WorldNativeAuthority))
  , worldContextLexical :: Map.Map (String,String) [(String,String)]
  , worldContextSelection :: WorldSelection
  , worldContextPassive :: WorldNativeProvenance
  , worldContextCharge :: (Integer,Integer)
  }

data WorldPrimaryAssociation = WorldPrimaryAssociation
  { worldPrimaryInterfaceBinding :: WorldArtifactBinding
  , worldPrimaryOriginalBinding :: Maybe WorldArtifactBinding
  , worldPrimaryRow :: ScopeInterfaceInput
  }

data WorldPrimaryFacts = WorldPrimaryFacts
  { worldPrimaryContext :: WorldContextFacts
  , worldPrimaryAssociations :: Map.Map WorldArtifactBinding WorldPrimaryAssociation
  , worldPrimarySelected :: Map.Map (String,String) WorldPrimaryAssociation
  , worldPrimaryRetainedCharge :: (Integer,Int)
  }

worldInputsPrimaryIsEmpty :: WorldInputFacts -> Bool
worldInputsPrimaryIsEmpty facts =
  let context = worldPrimaryContext (worldInputsPrimary facts)
      selected = worldContextSelection context
      (offers,availability,decisions) = worldContextPassive context
  in Map.null (worldContextRoles context) && Map.null (worldContextLexical context)
    && Map.null offers && Map.null availability && Map.null decisions
    && Map.null (worldSelectionScopes selected) && Map.null (worldSelectionNamespace selected)
    && Set.null (worldSelectionRoots selected) && Set.null (worldSelectionCustody selected)
    && Map.null (worldPrimaryAssociations (worldInputsPrimary facts))

readWorldLexicalSurface :: Map.Map BS.ByteString BS.ByteString -> BS.ByteString
  -> WorldPrimaryFacts -> IO WorldLexicalSurface
readWorldLexicalSurface frontier root primary = do
  body <- maybe (worldCorrupt "compiler world lexical surface is absent") pure (Map.lookup root frontier)
  (imports,selected,observation) <- decodeWorldBytes (decodeSurface body) body
  declaration <- forM selected $ \(owner,interface,encodedExports) -> do
    association <- maybe (worldCorrupt "world declaration facade has no primary selected role") pure
      (Map.lookup owner (worldPrimarySelected primary))
    unless (fst (worldPrimaryInterfaceBinding association) == interface)
      (worldCorrupt "world declaration facade has another selected interface artifact")
    exports <- either worldCorrupt pure (decodeDeclarationExports encodedExports)
    case worldPrimaryRow association of
      row@(ScopeInterfaceInput (artifact,_,_) _) -> do
        unless ((exactUnit artifact,exactModule artifact) == owner)
          (worldCorrupt "world declaration facade has another acquired interface owner")
        pure (WorldDeclarationSurface row exports)
  let exports = maybe [] worldDeclarationExports declaration
      entries = 1 + toInteger (length imports) + toInteger (length exports)
        + sum [toInteger (length (exportChildren value)) | value <- exports]
        + maybe 0 (\value -> 1 + toInteger (length (worldObservationImports value))) observation
  when (entries > toInteger (maxBound :: Int))
    (worldCorrupt "compiler world lexical retention count exceeds host range")
  pure (WorldLexicalSurface imports declaration observation (toInteger (BS.length body),fromInteger entries))
  where
    decodeSurface body = do
      worldArray 4
      magic <- decodeStringCanonical
      unless (magic == "TPCWLEX2") (fail "unsupported compiler world lexical surface")
      imports <- importSpecs body
      selected <- peekTokenType >>= \case
        TypeNull -> decodeNull >> pure Nothing
        _ -> do
          worldArray 4
          unit <- T.unpack <$> worldBindingText
          name <- T.unpack <$> worldBindingText
          artifact <- rawWorldDigestCanonical
          exports <- decodeBytesCanonical
          pure (Just ((unit,name),artifact,exports))
      observation <- optional $ do
        worldArray 4
        profile <- decodeWordCanonical >>= \case
          0 -> pure WorldWorkbenchObservation
          1 -> pure WorldHostCarrierObservation
          _ -> fail "unsupported world observation profile"
        fragment <- T.unpack <$> decodeStringCanonical
        when ('\0' `elem` fragment) (fail "invalid world observation source fragment")
        staticImports <- importSpecs body
        (request,input) <- optional (worldArray 2 >> ((,) <$> optional requestTypes <*> optional decodeCheckedSignature))
          >>= pure . maybe (Nothing,Nothing) id
        pure (WorldObservationScaffold profile fragment staticImports request input)
      let text = E.encodeString . T.pack
          encodedDeclaration = case selected of
            Nothing -> E.encodeNull
            Just ((unit,name),artifact,exports) -> E.encodeListLen 4 <> text unit <> text name
              <> E.encodeBytes artifact <> E.encodeBytes exports
          canonical = toStrictByteString (E.encodeListLen 4 <> text "TPCWLEX2"
            <> encodeImports imports <> encodedDeclaration <> maybe E.encodeNull encodeObservation observation)
      unless (canonical == body) (fail "world lexical surface must be canonical CBOR")
      pure (imports,selected,observation)
    optional :: Decoder s value -> Decoder s (Maybe value)
    optional decoder = peekTokenType >>= \case
      TypeNull -> decodeNull >> pure Nothing
      _ -> Just <$> decoder
    importSpecs :: BS.ByteString -> Decoder s [String]
    importSpecs body = worldFactList (BS.length body) $ do
      spec <- T.unpack <$> worldBindingText
      when (any (`elem` ['\0','\n']) spec) (fail "invalid published compiler import specification")
      pure spec
    requestTypes :: Decoder s (RequestHelperRecipe,RequestTypeSignatures)
    requestTypes = do
      worldArray 2
      recipe <- decodeStringCanonical >>= \case
        "none" -> pure NoRequestHelpers
        "actor-reply" -> pure ActorReplyHelpers
        _ -> fail "unsupported world observation request helper recipe"
      (,) recipe <$> decodeRequestTypeSignatures
    encodeImports imports = E.encodeListLen (fromIntegral (length imports))
      <> foldMap (E.encodeString . T.pack) imports
    encodeObservation (WorldObservationScaffold profile fragment imports request input) =
      E.encodeListLen 4 <> E.encodeWord (case profile of WorldWorkbenchObservation -> 0; WorldHostCarrierObservation -> 1)
        <> E.encodeString (T.pack fragment) <> encodeImports imports
        <> (if isNothing request && isNothing input then E.encodeNull else E.encodeListLen 2
          <> maybe E.encodeNull (\(recipe,types) -> E.encodeListLen 2
            <> E.encodeString (case recipe of NoRequestHelpers -> "none"; ActorReplyHelpers -> "actor-reply")
            <> encodeRequestTypeSignatures types) request
          <> maybe E.encodeNull encodeCheckedSignature input)

worldFactField :: String -> Decoder s ()
worldFactField expected = do
  actual <- decodeStringCanonical
  unless (actual == T.pack expected) (fail "invalid compiler world fact fields")

worldFactMap :: Int -> Decoder s ()
worldFactMap expected = decodeMapLenCanonical >>= \actual ->
  unless (actual == expected) (fail "invalid compiler world fact map")

worldFactList :: Int -> Decoder s a -> Decoder s [a]
worldFactList bytes item = do
  count <- decodeListLenCanonical
  unless (count <= bytes) (fail "compiler world fact collection exceeds encoded body")
  replicateM count item

worldStrictlyOrdered :: Ord a => String -> [a] -> Decoder s ()
worldStrictlyOrdered label values = unless (and (zipWith (<) values (drop 1 values))) (fail label)

worldBindingNodeScope :: WorldBindingNodeKey -> BS.ByteString
worldBindingNodeScope node = case node of
  WorldArtifactBinding (_,scope) -> scope
  WorldGroupBinding (_,scope) _ -> scope

decodeWorldScopeDependency :: Decoder s WorldScopeDependency
decodeWorldScopeDependency = do
  token <- peekTokenType
  case token of
    TypeString -> do
      label <- decodeStringCanonical
      unless (label == "interface") (fail "unsupported compiler world scope dependency")
      pure WorldInterfaceDependency
    TypeMapLen -> do
      worldFactMap 1
      label <- decodeStringCanonical
      case label of
        "native_group" -> do
          worldFactMap 2
          worldFactField "required_ordinal"
          required <- decodeWord32Canonical
          worldFactField "dependent_ordinal"
          dependent <- decodeWord32Canonical
          pure (WorldNativeGroupDependency dependent required)
        "native_binding" -> do
          worldFactMap 5
          worldFactField "namespace"
          namespace <- T.unpack <$> worldBindingText
          worldFactField "generation"
          generation <- decodeWord64Canonical
          worldFactField "occurrence"
          occurrence <- T.unpack <$> worldBindingText
          worldFactField "record_parent"
          token <- peekTokenType
          parent <- if token == TypeNull then decodeNull >> pure Nothing else Just . T.unpack <$> decodeStringCanonical
          worldFactField "dependent_ordinal"
          dependent <- decodeWord32Canonical
          pure (WorldNativeBindingDependency dependent generation namespace occurrence parent)
        _ -> fail "unsupported compiler world scope dependency"
    _ -> fail "invalid compiler world scope dependency"

decodeWorldScopeProof :: Int -> Decoder s WorldScopeProof
decodeWorldScopeProof bytes = do
  worldFactMap 3
  worldFactField "nodes"
  nodes <- worldFactList bytes decodeWorldBindingNode
  worldStrictlyOrdered "noncanonical compiler world scope nodes" nodes
  unless (not (null nodes)) (fail "empty compiler world scope proof")
  worldFactField "selection"
  scope <- worldArrayDigest
  unless (all ((== scope) . worldBindingNodeScope) nodes)
    (fail "compiler world scope node has another issuer")
  worldFactField "dependencies"
  dependencies <- worldFactList bytes (worldArray 3 >> ((,,) <$> decodeWorldBindingNode <*> decodeWorldBindingNode <*> decodeWorldScopeDependency))
  worldStrictlyOrdered "noncanonical compiler world scope dependencies" dependencies
  let owned = Set.fromDistinctAscList nodes
  unless (all (\(source,_,_) -> Set.member source owned) dependencies)
    (fail "compiler world scope dependency source is outside its issuer")
  pure (WorldScopeProof scope owned dependencies)

decodeWorldCompilerRole :: Decoder s WorldCompilerRole
decodeWorldCompilerRole = do
  count <- decodeMapLenCanonical
  worldFactField "kind"
  kind <- decodeStringCanonical
  case (kind,count) of
    ("interface_only",2) -> worldFactField "interface" >> WorldInterfaceRole <$> worldArrayDigest
    ("reusable_original",3) -> do
      worldFactField "original"
      original <- worldArrayDigest
      worldFactField "interface"
      interface <- worldArrayDigest
      pure (WorldReusableRole interface original)
    ("published_source_original",8) -> do
      worldFactField "original"
      original <- worldArrayDigest
      worldFactField "interface"
      interface <- worldArrayDigest
      worldFactField "selection"
      selection <- worldCID
      worldFactField "source_revision"
      revision <- T.unpack <$> worldBindingText
      worldFactField "native_authority"
      authority <- worldCID
      worldFactField "selection_sha256"
      seal <- worldArrayDigest
      worldFactField "original_input_identity"
      identity <- T.unpack <$> worldBindingText
      pure (WorldPublishedRole interface original revision identity selection seal authority)
    _ -> fail "unsupported compiler world compiler role"

decodeWorldLexicalFact :: Int -> Decoder s ((String,String),[(String,String)])
decodeWorldLexicalFact bytes = do
  worldFactMap 2
  worldFactField "owner"
  owner <- decodeWorldModuleKey
  worldFactField "imports"
  imports <- worldFactList bytes decodeWorldModuleKey
  worldStrictlyOrdered "noncanonical compiler world lexical imports" imports
  pure (owner,imports)

-- The acquisition association preserves the actual scoped bindings from the
-- admitted Rust role owner. Image equality cannot replace either binding.
decodeWorldPrimaryAssociation :: Int -> Decoder s (WorldArtifactBinding,Maybe WorldArtifactBinding,BS.ByteString)
decodeWorldPrimaryAssociation bytes = do
  worldArray 4
  magic <- decodeStringCanonical
  unless (magic == "TPCWPRIMARY1") (fail "unsupported compiler world primary association")
  interface <- decodeWorldBinding
  token <- peekTokenType
  original <- if token == TypeNull then decodeNull >> pure Nothing else Just <$> decodeWorldBinding
  semantic <- decodeWorldSemanticRow bytes
  pure (interface,original,semantic)

decodeWorldSemanticRow :: Int -> Decoder s BS.ByteString
decodeWorldSemanticRow bytes = do
  worldArray 6
  unit <- worldBindingText
  name <- worldBindingText
  producer <- rawWorldDigestCanonical
  image <- rawWorldDigestCanonical
  role <- decodeStringCanonical
  unless (role `elem` ["module","native-declaration","join","value"])
    (fail "unsupported compiler world semantic row role")
  required <- worldFactList bytes (worldArray 2 >> ((,) <$> worldBindingText <*> worldBindingText))
  worldStrictlyOrdered "compiler world row requirements are duplicate or unsorted" required
  pure (toStrictByteString (E.encodeListLen 6 <> E.encodeString unit <> E.encodeString name
    <> E.encodeBytes producer <> E.encodeBytes image <> E.encodeString role
    <> E.encodeListLen (fromIntegral (length required))
    <> foldMap (\(u,n) -> E.encodeListLen 2 <> E.encodeString u <> E.encodeString n) required))

rawWorldDigestCanonical :: Decoder s BS.ByteString
rawWorldDigestCanonical = do
  bytes <- decodeBytesCanonical
  unless (BS.length bytes == 32) (fail "invalid compiler world provenance digest")
  pure bytes

data WorldBindingNodeKey = WorldArtifactBinding (BS.ByteString,BS.ByteString)
  | WorldGroupBinding (BS.ByteString,BS.ByteString) Word32 deriving (Eq,Ord)

decodeWorldBindingNode :: Decoder s WorldBindingNodeKey
decodeWorldBindingNode = do
  count <- decodeMapLenCanonical
  unless (count == 1) (fail "invalid compiler world binding node")
  kind <- decodeStringCanonical
  case kind of
    "artifact" -> WorldArtifactBinding <$> decodeWorldBinding
    "group" -> do
      fields <- decodeMapLenCanonical
      unless (fields == 2) (fail "invalid compiler world group binding")
      bindingField <- decodeStringCanonical
      binding <- decodeWorldBinding
      ordinalField <- decodeStringCanonical
      ordinal <- decodeWord32Canonical
      unless (bindingField == "binding" && ordinalField == "original_ordinal")
        (fail "invalid compiler world group binding fields")
      pure (WorldGroupBinding binding ordinal)
    _ -> fail "unsupported compiler world binding node"

encodeWorldBindingNode :: WorldBindingNodeKey -> E.Encoding
encodeWorldBindingNode node = E.encodeMapLen 1 <> case node of
  WorldArtifactBinding binding -> E.encodeString "artifact" <> encodeWorldBinding binding
  WorldGroupBinding binding ordinal -> E.encodeString "group" <> E.encodeMapLen 2
    <> E.encodeString "binding" <> encodeWorldBinding binding
    <> E.encodeString "original_ordinal" <> E.encodeWord32 ordinal

-- TPCWBIND4 names the occurrence root; raw CID bytes in visibility do not
-- become a Context DAG link by themselves. No legacy visibility is inferred.
decodeWorldBindingRoot :: Decoder s (Word64,BS.ByteString)
decodeWorldBindingRoot = do
  worldArray 3
  magic <- decodeStringCanonical
  unless (magic == "TPCWBIND4") (fail "unsupported compiler world bindings")
  scope <- decodeWord64Canonical
  root <- decodeBytesCanonical
  unless (supportedWorldCID root) (fail "unsupported compiler world binding root CID")
  pure (scope,root)

worldBindingText :: Decoder s T.Text
worldBindingText = do
  value <- decodeStringCanonical
  unless (not (T.null value) && not (T.any (== '\0') value))
    (fail "invalid compiler world binding identity")
  pure value

decodeWorldBindingValue :: Decoder s (SymbolIdentity,Word64,Word64,Word64,BS.ByteString)
decodeWorldBindingValue = do
  token <- peekTokenType
  unless (token == TypeListLen) (fail "invalid compiler world binding value tuple")
  count <- decodeListLenCanonical
  unless (count == 5) (fail "invalid compiler world binding value tuple")
  worldArray 5
  unit <- worldBindingText
  name <- worldBindingText
  namespace <- worldBindingText
  occurrence <- worldBindingText
  parentToken <- peekTokenType
  parent <- if parentToken == TypeNull then decodeNull >> pure Nothing else do
    value <- decodeStringCanonical
    when (T.any (== '\0') value) (fail "invalid compiler world record parent")
    pure (Just value)
  variable <- decodeWord64Canonical
  scope <- decodeWord64Canonical
  generation <- decodeWord64Canonical
  row <- worldCID
  pure (SymbolIdentity unit name namespace occurrence parent,variable,scope,generation,row)

-- Compare one exact canonical semantic row, including role/requirements, with
-- the existing receiving projection. A byte-image key alone omits those facts.
decodeWorldBindingRow :: Int -> Decoder s (BS.ByteString,BS.ByteString,BS.ByteString)
decodeWorldBindingRow payloadBytes = do
  worldArray 3
  semantic <- decodeWorldSemanticRow payloadBytes
  certified <- rawWorldDigestCanonical
  receipt <- rawWorldDigestCanonical
  pure (semantic,certified,receipt)

readWorldBindings :: Map.Map BS.ByteString BS.ByteString -> BS.ByteString -> BS.ByteString
  -> [(OfferedWorldRow,ScopeInterfaceInput)] -> IO (Map.Map String WorldBinding,(Integer,Int))
readWorldBindings frontier root producer visible = do
  (selected,(nodeBytes,nodeCount)) <- worldHamtMapWithCharge frontier root (T.unpack <$> worldBindingText) (E.encodeString . T.pack)
    decodeWorldBindingValue
  eligible <- forM [(offered,input) | (offered@((_,ParsedValueEvidence),_),input) <- visible] $ \(offered,input) -> do
    semantic <- toStrictByteString <$> worldSemanticRowEncoding producer offered
    pure (semantic,input)
  let rows = Map.fromList eligible
      referenced = Set.fromList [cid | (_,_,_,_,cid) <- Map.elems selected]
  -- Decode each shared row once; aliases retain that same immutable row value.
  decoded <- forM (Set.toAscList referenced) $ \cid -> do
    body <- maybe (worldCorrupt "compiler world binding row is absent") pure (Map.lookup cid frontier)
    (semantic,certified,receipt) <- decodeWorldBytes (decodeWorldBindingRow (BS.length body)) body
    input <- maybe (worldCorrupt "compiler world binding row differs from admitted visible value inputs") pure
      (Map.lookup semantic rows)
    checked <- case input of
      ScopeInterfaceInput row CheckedValueEvidence -> pure row
      _ -> worldCorrupt "compiler world binding row lost its checked value evidence"
    pure (cid,(WorldBindingRow checked certified receipt cid,semantic,toInteger (BS.length body)))
  unless (length eligible == length visible && Map.keysSet rows == Set.fromList [semantic | (_,(_,semantic,_)) <- decoded])
    (worldCorrupt "compiler world visible input census differs from occurrence facts")
  let actual = Map.fromDistinctAscList decoded
      bytes = foldl' (\total (_,(_,_,size)) -> total + size) nodeBytes decoded
      -- The raw descriptor already charges these encoded bytes once. This
      -- additional conservative basis accounts for the decoded index without
      -- pretending to measure exact heap. Every alias owns a name record;
      -- only its distinct row record/backing is shared.
      entries = toInteger (Map.size selected) + toInteger nodeCount + toInteger (Set.size referenced)
  when (entries > toInteger (maxBound :: Int))
    (worldCorrupt "compiler world binding retention count exceeds host range")
  bindings <- forM selected $ \(symbol,variable,scope,generation,cid) -> case Map.lookup cid actual of
    Just (row@(WorldBindingRow (artifact,_,_) _ _ _),_,_) -> do
      unless (parseSessionModule (exactModule artifact) == Just (SessionModule ValMod (Generation generation))
          && sessionModuleString (SessionModule ValMod (Generation generation)) == exactModule artifact)
        (worldCorrupt "compiler world binding generation differs from its checked value owner")
      pure (WorldBinding symbol variable scope generation row)
    _ -> worldCorrupt "compiler world binding has no decoded checked value row"
  -- Settle all charge work during acquisition, never at a hit or END.
  _ <- evaluate (bytes + entries + toInteger (Map.size bindings))
  pure (bindings,(bytes,fromInteger entries))

worldBindingIdentity :: WorldBinding -> (SymbolIdentity,Word64,Word64,Word64,BS.ByteString)
worldBindingIdentity binding = (worldBindingSymbol binding,worldBindingSessionVar binding
  ,worldBindingScope binding,worldBindingGeneration binding,worldBindingRowFactCID (worldBindingRow binding))

retainWorldBindingRows :: Map.Map BS.ByteString BS.ByteString -> BS.ByteString
  -> Map.Map InterfaceOwner WorldAdmittedRow -> Map.Map String WorldBinding
  -> IO (Map.Map BS.ByteString (WorldBindingRow,BS.ByteString,Integer,Int))
retainWorldBindingRows frontier _producer visible bindings = foldM retain Map.empty (Map.elems bindings)
  where
    retain indexed binding = do
      let row = worldBindingRow binding
          cid = worldBindingRowFactCID row
          (artifact,_,_) = worldBindingRowInterface row
      case Map.lookup cid indexed of
        Just (held,semantic,bytes,users) -> pure (Map.insert cid (held,semantic,bytes,users+1) indexed)
        Nothing -> do
          WorldAdmittedRow _ _ semantic _ <- maybe (worldCorrupt "world binding lost its visible row owner") pure
            (Map.lookup (exactUnit artifact,exactModule artifact) visible)
          body <- maybe (worldCorrupt "world binding row body is absent") pure (Map.lookup cid frontier)
          pure (Map.insert cid (row,semantic,toInteger (BS.length body),1) indexed)

readChangedWorldBinding :: Map.Map BS.ByteString BS.ByteString
  -> Map.Map BS.ByteString ScopeInterfaceInput
  -> (SymbolIdentity,Word64,Word64,Word64,BS.ByteString)
  -> IO (WorldBinding,BS.ByteString,Integer)
readChangedWorldBinding frontier eligible (symbol,variable,scope,generation,cid) = do
  body <- maybe (worldCorrupt "compiler world binding row is absent") pure (Map.lookup cid frontier)
  (semantic,certified,receipt) <- decodeWorldBytes (decodeWorldBindingRow (BS.length body)) body
  checked <- case Map.lookup semantic eligible of
    Just (ScopeInterfaceInput row CheckedValueEvidence) -> pure row
    _ -> worldCorrupt "compiler world binding row differs from admitted visible value inputs"
  let (artifact,_,_) = checked
  unless (parseSessionModule (exactModule artifact) == Just (SessionModule ValMod (Generation generation))
      && sessionModuleString (SessionModule ValMod (Generation generation)) == exactModule artifact)
    (worldCorrupt "compiler world binding generation differs from its checked value owner")
  pure (WorldBinding symbol variable scope generation (WorldBindingRow checked certified receipt cid)
    ,semantic,toInteger (BS.length body))


data WorldDescriptorField = WorldDescriptorVersion Word64 | WorldDescriptorProducer BS.ByteString
  | WorldDescriptorLink BS.ByteString

worldDescriptor :: [String] -> Bool -> Decoder s (Map.Map String WorldDescriptorField)
worldDescriptor roots producer = do
  count <- decodeMapLenCanonical
  let expected = Set.fromList ("version" : roots ++ ["producer" | producer])
  unless (count == Set.size expected) (fail "invalid compiler world descriptor fields")
  fields <- replicateM count $ do
    name <- T.unpack <$> decodeStringCanonical
    value <- case name of
      "version" -> WorldDescriptorVersion <$> decodeWord64Canonical
      "producer" | producer -> WorldDescriptorProducer <$> worldArrayDigest
      _ | name `elem` roots -> WorldDescriptorLink <$> worldCID
      _ -> fail "unsupported compiler world descriptor field"
    pure (name,value)
  let order name = let bytes = TE.encodeUtf8 (T.pack name) in (BS.length bytes,bytes)
  unless (Set.fromList (map fst fields) == expected
      && and (zipWith (<) (map (order . fst) fields) (drop 1 (map (order . fst) fields))))
    (fail "noncanonical compiler world descriptor fields")
  pure (Map.fromList fields)

readWorldDescriptor :: Map.Map BS.ByteString BS.ByteString -> BS.ByteString
  -> (forall s. Decoder s a) -> IO (a,Integer)
readWorldDescriptor frontier cid parser = do
  body <- maybe (worldCorrupt "compiler world descriptor block is absent") pure (Map.lookup cid frontier)
  value <- decodeWorldBytes parser body
  pure (value,toInteger (BS.length body))

worldDescriptorLink :: String -> Map.Map String WorldDescriptorField -> IO BS.ByteString
worldDescriptorLink name fields = case Map.lookup name fields of
  Just (WorldDescriptorLink cid) -> pure cid
  _ -> worldCorrupt "compiler world descriptor link is absent"

requireWorldDescriptorVersion :: Word64 -> Map.Map String WorldDescriptorField -> IO ()
requireWorldDescriptorVersion expected fields = case Map.lookup "version" fields of
  Just (WorldDescriptorVersion actual) | actual == expected -> pure ()
  _ -> worldCorrupt "unsupported compiler world descriptor version"

addWorldCharge :: (Integer,Integer) -> (Integer,Integer) -> (Integer,Integer)
addWorldCharge (bytes,entries) (more,count) = (bytes+more,entries+count)

worldMapCharge :: Map.Map key value -> (Integer,Int) -> (Integer,Integer)
worldMapCharge values (bytes,nodes) = (bytes,toInteger nodes + toInteger (Map.size values))

-- Scope digest issuance remains Rust inventory provenance. This receiver
-- checks the exact decoded cohort/edge/namespace correspondence before using
-- any association; a claimed digest cannot replace a missing proof body.
readWorldSelection :: Map.Map BS.ByteString BS.ByteString -> BS.ByteString -> IO (WorldSelection,(Integer,Integer))
readWorldSelection frontier root = do
  (descriptor,descriptorBytes) <- readWorldDescriptor frontier root
    (worldDescriptor ["scopes","roots","namespace","custody_roots"] False)
  requireWorldDescriptorVersion 2 descriptor
  scopeRoot <- worldDescriptorLink "scopes" descriptor
  (scopeLinks,scopeMapCharge) <- worldHamtMapWithCharge frontier scopeRoot worldArrayDigest encodeWorldArrayDigest worldCID
  decoded <- forM (Map.toAscList scopeLinks) $ \(identity,cid) -> do
    body <- maybe (worldCorrupt "compiler world scope proof is absent") pure (Map.lookup cid frontier)
    proof@(WorldScopeProof actual nodes dependencies) <- decodeWorldBytes (decodeWorldScopeProof (BS.length body)) body
    unless (identity == actual) (worldCorrupt "compiler world scope proof has another binding identity")
    pure (identity,(proof,(toInteger (BS.length body),toInteger (Set.size nodes) + toInteger (length dependencies) + 1)))
  let proofs = Map.fromDistinctAscList [(identity,proof) | (identity,(proof,_)) <- decoded]
      proofHas node = case Map.lookup (worldBindingNodeScope node) proofs of
        Just (WorldScopeProof _ nodes _) -> Set.member node nodes
        Nothing -> False
      ancestors = Map.map (\(WorldScopeProof own _ dependencies) -> Set.fromList
        [worldBindingNodeScope target | (_,target,_) <- dependencies,worldBindingNodeScope target /= own]) proofs
  forM_ (Map.elems proofs) $ \(WorldScopeProof _ _ dependencies) ->
    unless (all (\(_,target,_) -> proofHas target) dependencies)
      (worldCorrupt "compiler world scope target is absent from its exact issuer")
  let pending = [(scope,Set.size required) | (scope,required) <- Map.toAscList ancestors]
      counts = Map.fromDistinctAscList pending
      dependents = Map.fromListWith (++) [(ancestor,[scope]) | (scope,required) <- Map.toAscList ancestors,ancestor <- Set.toAscList required]
      settle count _ [] = unless (count == Map.size proofs) (worldCorrupt "compiler world binding scope ancestry cycle")
      settle count remaining (scope:rest) = do
        let (next,newlyReady) = foldl' (\(current,ready) child ->
              let waiting = current Map.! child - 1
              in (Map.insert child waiting current,if waiting == 0 then child:ready else ready))
              (remaining,[]) (Map.findWithDefault [] scope dependents)
        settle (count+1) next (newlyReady ++ rest)
  settle 0 counts [scope | (scope,0) <- pending]
  rootsCID <- worldDescriptorLink "roots" descriptor
  (roots,rootsCharge) <- worldHamtMapWithCharge frontier rootsCID decodeWorldBindingNode encodeWorldBindingNode decodeNull
  custodyCID <- worldDescriptorLink "custody_roots" descriptor
  (custody,custodyCharge) <- worldHamtMapWithCharge frontier custodyCID decodeWorldBindingNode encodeWorldBindingNode decodeNull
  namespaceCID <- worldDescriptorLink "namespace" descriptor
  (namespace,namespaceCharge) <- worldHamtMapWithCharge frontier namespaceCID worldArrayDigest encodeWorldArrayDigest decodeWorldBinding
  let outgoing = Map.fromListWith (++) [(source,[target])
        | WorldScopeProof _ _ dependencies <- Map.elems proofs,(source,target,_) <- dependencies]
      visit seen [] = pure seen
      visit seen (node:rest)
        | Set.member node seen = visit seen rest
        | not (proofHas node) = worldCorrupt "compiler world selected node is outside its scope proof"
        | otherwise = visit (Set.insert node seen) (Map.findWithDefault [] node outgoing ++ rest)
  selected <- visit Set.empty (Map.keys roots ++ Map.keys custody)
  unless (all (\(artifact,binding@(actual,_)) -> artifact == actual && Set.member (WorldArtifactBinding binding) selected)
      (Map.toAscList namespace))
    (worldCorrupt "compiler world namespace differs from selected scope nodes")
  let charge = foldl' addWorldCharge (descriptorBytes,1 + toInteger (Set.size selected))
        ([worldMapCharge scopeLinks scopeMapCharge,worldMapCharge roots rootsCharge
         ,worldMapCharge custody custodyCharge,worldMapCharge namespace namespaceCharge]
         ++ [cost | (_,(_,cost)) <- decoded])
  pure (WorldSelection proofs (Map.keysSet roots) namespace (Map.keysSet custody) selected,charge)

-- Retain current role, lexical and scope facts under the same world owner.
-- Offer/native decision links are passive custody; no native payload or group
-- inventory is loaded merely to open a type/query world.
readWorldNativeMaps :: Map.Map BS.ByteString BS.ByteString -> Map.Map String WorldDescriptorField
  -> IO (WorldNativeProvenance,(Integer,Integer))
readWorldNativeMaps frontier descriptor = do
  offersCID <- worldDescriptorLink "offers" descriptor
  (offers,offersCharge) <- worldHamtMapWithCharge frontier offersCID worldArrayDigest encodeWorldArrayDigest worldCID
  availabilityCID <- worldDescriptorLink "native_availability" descriptor
  (availability,availabilityCharge) <- worldHamtMapWithCharge frontier availabilityCID worldArrayDigest encodeWorldArrayDigest worldCID
  decisionsCID <- worldDescriptorLink "input_decisions" descriptor
  (decisions,decisionsCharge) <- worldHamtMapWithCharge frontier decisionsCID decodeWorldModuleKey encodeWorldModuleKey worldCID
  pure ((offers,availability,decisions),foldl' addWorldCharge (0,0)
    [worldMapCharge offers offersCharge,worldMapCharge availability availabilityCharge,worldMapCharge decisions decisionsCharge])

type WorldAuthorityMemo = IORef (Map.Map BS.ByteString (Maybe WorldNativeAuthority))

readWorldNativeAuthority :: WorldAuthorityMemo -> Map.Map BS.ByteString BS.ByteString -> BS.ByteString
  -> IO (WorldNativeAuthority,(Integer,Integer))
readWorldNativeAuthority memo frontier root = do
  decoded <- readIORef memo
  case Map.lookup root decoded of
    Just Nothing -> worldCorrupt "cyclic compiler world publication authority"
    Just (Just authority) -> pure (authority,(0,1))
    Nothing -> do
      modifyIORef' memo (Map.insert root Nothing)
      (descriptor,descriptorBytes) <- readWorldDescriptor frontier root
        (worldDescriptor ["roles","offers","native_availability","input_decisions"] False)
      requireWorldDescriptorVersion 2 descriptor
      rolesCID <- worldDescriptorLink "roles" descriptor
      (roles,roleCharge) <- readWorldRoles memo frontier rolesCID
      (facts,charge) <- readWorldNativeMaps frontier descriptor
      let authority = WorldNativeAuthority roles facts
      modifyIORef' memo (Map.insert root (Just authority))
      -- One immutable decode per authority CID plus each retained reference.
      pure (authority,foldl' addWorldCharge (descriptorBytes,2) [roleCharge,charge])

-- Current and publication-birth domains share the same strict role decoder.
-- A born role's own publication owner remains independent and is retained too.
readWorldRoles :: WorldAuthorityMemo -> Map.Map BS.ByteString BS.ByteString -> BS.ByteString
  -> IO (Map.Map (String,String) (WorldCompilerRole,Maybe (WorldSelection,WorldNativeAuthority)),(Integer,Integer))
readWorldRoles memo frontier root = do
  (links,hamtCharge) <- worldHamtMapWithCharge frontier root decodeWorldModuleKey encodeWorldModuleKey worldCID
  roles <- forM links $ \cid -> do
    (role,bytes) <- readWorldDescriptor frontier cid decodeWorldCompilerRole
    (selection,charge) <- case role of
      WorldPublishedRole _ _ _ _ selectedRoot _ authorityRoot -> do
        (selected,selectedCharge) <- readWorldSelection frontier selectedRoot
        (authority,authorityCharge) <- readWorldNativeAuthority memo frontier authorityRoot
        pure (Just (selected,authority),addWorldCharge selectedCharge authorityCharge)
      _ -> pure (Nothing,(0,0))
    pure ((role,selection),addWorldCharge (bytes,1) charge)
  pure (fmap fst roles,foldl' addWorldCharge (worldMapCharge links hamtCharge) (map snd (Map.elems roles)))

readWorldContext :: Map.Map BS.ByteString BS.ByteString -> BS.ByteString -> BS.ByteString -> IO WorldContextFacts
readWorldContext frontier root producer = do
  (descriptor,descriptorBytes) <- readWorldDescriptor frontier root (worldDescriptor
    ["selection","roles","lexical","offers","native_availability","input_decisions"] True)
  requireWorldDescriptorVersion 6 descriptor
  case Map.lookup "producer" descriptor of
    Just (WorldDescriptorProducer actual) | actual == producer -> pure ()
    _ -> worldCorrupt "compiler world context producer differs"
  rolesCID <- worldDescriptorLink "roles" descriptor
  authorityMemo <- newIORef Map.empty
  (roles,roleCharge) <- readWorldRoles authorityMemo frontier rolesCID
  lexicalCID <- worldDescriptorLink "lexical" descriptor
  (lexicalLinks,lexicalCharge) <- worldHamtMapWithCharge frontier lexicalCID decodeWorldModuleKey encodeWorldModuleKey worldCID
  lexical <- forM (Map.toAscList lexicalLinks) $ \(owner,cid) -> do
    body <- maybe (worldCorrupt "compiler world lexical fact is absent") pure (Map.lookup cid frontier)
    (actual,imports) <- decodeWorldBytes (decodeWorldLexicalFact (BS.length body)) body
    unless (actual == owner) (worldCorrupt "compiler world lexical fact has another owner")
    pure (owner,(imports,(toInteger (BS.length body),1+toInteger (length imports))))
  let lexicalFacts = Map.fromDistinctAscList [(owner,imports) | (owner,(imports,_)) <- lexical]
  unless (all (\(owner,imports) -> Map.member owner roles && all (`Map.member` lexicalFacts) imports)
      (Map.toAscList lexicalFacts))
    (worldCorrupt "compiler world lexical graph leaves selected roles or lexical facts")
  selectionCID <- worldDescriptorLink "selection" descriptor
  (selection,selectionCharge) <- readWorldSelection frontier selectionCID
  (passive,passiveCharge) <- readWorldNativeMaps frontier descriptor
  let charge = foldl' addWorldCharge (descriptorBytes,1)
        ([roleCharge,worldMapCharge lexicalLinks lexicalCharge,selectionCharge,passiveCharge]
         ++ [cost | (_,(_,cost)) <- lexical])
  pure (WorldContextFacts roles lexicalFacts selection passive charge)

readWorldPrimary :: Map.Map BS.ByteString BS.ByteString -> BS.ByteString -> WorldContextFacts -> BS.ByteString
  -> [(OfferedWorldRow,ScopeInterfaceInput)] -> IO WorldPrimaryFacts
readWorldPrimary frontier root context producer base = do
  (links,hamtCharge) <- worldHamtMapWithCharge frontier root decodeWorldBinding encodeWorldBinding worldCID
  eligible <- forM base $ \(offered,input) -> do
    semantic <- toStrictByteString <$> worldSemanticRowEncoding producer offered
    pure (semantic,input)
  let rows = Map.fromList eligible
      facts = Set.fromList (Map.elems links)
  decoded <- forM (Set.toAscList facts) $ \cid -> do
    body <- maybe (worldCorrupt "compiler world primary association is absent") pure (Map.lookup cid frontier)
    (interface,original,semantic) <- decodeWorldBytes (decodeWorldPrimaryAssociation (BS.length body)) body
    input <- maybe (worldCorrupt "compiler world primary association differs from admitted base inputs") pure (Map.lookup semantic rows)
    pure (cid,(WorldPrimaryAssociation interface original input,semantic,toInteger (BS.length body)))
  let records = Map.fromDistinctAscList decoded
      referenced = Set.fromList [semantic | (_,(_,semantic,_)) <- decoded]
  unless (referenced == Map.keysSet rows)
    (worldCorrupt "compiler world base input census differs from primary associations")
  associations <- forM (Map.toAscList links) $ \(binding,cid) -> case Map.lookup cid records of
    Just (association,_,_) | binding == maybe (worldPrimaryInterfaceBinding association) id (worldPrimaryOriginalBinding association) ->
      pure (binding,association)
    _ -> worldCorrupt "compiler world primary association key differs from acquired transport binding"
  let indexed = Map.fromDistinctAscList associations
      selection = worldContextSelection context
      namespace = worldSelectionNamespace selection
      requiredBinding artifact = case Map.lookup artifact namespace of
        Just binding | Set.member (WorldArtifactBinding binding) (worldSelectionNodes selection) -> pure binding
        _ -> worldCorrupt "compiler world role has no exact selected namespace binding"
  selected <- forM (worldContextRoles context) $ \(role,_) -> do
    interface <- requiredBinding (worldRoleInterface role)
    original <- traverse requiredBinding (worldRoleOriginal role)
    let transport = maybe interface id original
    association <- maybe (worldCorrupt "compiler world selected role lacks its acquired association") pure (Map.lookup transport indexed)
    unless (interface == worldPrimaryInterfaceBinding association && original == worldPrimaryOriginalBinding association)
      (worldCorrupt "compiler world selected role differs from acquired binding scopes")
    forM_ original $ \binding -> case Map.lookup (snd binding) (worldSelectionScopes selection) of
      Just (WorldScopeProof _ _ dependencies)
        | (WorldArtifactBinding binding,WorldArtifactBinding interface,WorldInterfaceDependency) `elem` dependencies -> pure ()
      _ -> worldCorrupt "compiler world original lacks its exact canonical interface edge"
    pure association
  forM_ (Map.toAscList selected) $ \(owner,association) -> case worldPrimaryRow association of
    ScopeInterfaceInput row _ -> unless (owner == rowOwner row)
      (worldCorrupt "compiler world selected role has another authenticated row owner")
  let selectedKeys = Set.fromList [maybe (worldPrimaryInterfaceBinding value) id (worldPrimaryOriginalBinding value) | value <- Map.elems selected]
  unless (selectedKeys == Map.keysSet indexed)
    (worldCorrupt "compiler world primary association has no selected role")
  let (bytes,entries) = foldl' addWorldCharge (worldContextCharge context)
        [worldMapCharge links hamtCharge
        ,(sum [size | (_,(_,_,size)) <- decoded],toInteger (Set.size facts) + toInteger (Map.size selected))]
  when (entries > toInteger (maxBound :: Int)) (worldCorrupt "compiler world primary retention count exceeds host range")
  _ <- evaluate (bytes + entries)
  pure (WorldPrimaryFacts context indexed selected (bytes,fromInteger entries))

-- Scan the fixed DAG-CBOR block envelope for CID links before schema-specific
-- decoding. Counting pending values avoids an unbounded recursive term tree;
-- every pending value needs at least one byte of the already bounded body.
worldBlockLinks :: Int -> Decoder s (Set.Set BS.ByteString)
worldBlockLinks bytes = scan 1 bytes Set.empty
  where
    scan 0 _ links = pure links
    scan pending budget links = do
      unless (budget > 0 && pending <= toInteger budget) (fail "world block exceeds its encoded structure bound")
      token <- peekTokenType
      let scalar action = action >> scan (pending-1) (budget-1) links
          container multiplier decoder = do
            count <- decoder
            let next = pending-1 + multiplier * toInteger count
            unless (next <= toInteger (budget-1)) (fail "world block container exceeds its encoded body")
            scan next (budget-1) links
      case token of
        TypeUInt -> scalar (void decodeIntegerCanonical)
        TypeUInt64 -> scalar (void decodeIntegerCanonical)
        TypeNInt -> scalar (void decodeIntegerCanonical)
        TypeNInt64 -> scalar (void decodeIntegerCanonical)
        TypeBytes -> scalar (void decodeBytesCanonical)
        TypeString -> scalar (void decodeStringCanonical)
        TypeBool -> scalar (void decodeBool)
        TypeNull -> scalar decodeNull
        TypeFloat64 -> do
          value <- decodeDouble
          when (isNaN value || isInfinite value) (fail "world block has a nonfinite number")
          scan (pending-1) (budget-1) links
        TypeListLen -> container 1 decodeListLenCanonical
        TypeListLen64 -> container 1 decodeListLenCanonical
        TypeMapLen -> container 2 decodeMapLenCanonical
        TypeMapLen64 -> container 2 decodeMapLenCanonical
        TypeTag -> do
          tag <- decodeTagCanonical
          unless (tag == 42) (fail "unsupported world block tag")
          encoded <- decodeBytesCanonical
          unless (not (BS.null encoded) && BS.head encoded == 0 && supportedWorldCID (BS.tail encoded))
            (fail "unsupported world block child CID")
          let next = Set.insert (BS.tail encoded) links
          Set.size next `seq` scan (pending-1) (budget-1) next
        _ -> fail "unsupported world block encoding"

-- A cold transfer row is matched against the exact root-bound semantic fact.
-- Physical paths and arena aliases never replace logical association identity.
worldSemanticRowEncoding :: BS.ByteString -> OfferedWorldRow -> IO E.Encoding
worldSemanticRowEncoding producer ((row@(iface,_,_),evidence),image) = do
  role <- case evidence of
    ParsedModuleEvidence descriptor -> case descriptorPurpose descriptor of
      ScopeInterface SourceOriginalRole -> pure "module"
      ScopeInterface NativeDeclarationRole -> pure "native-declaration"
      CandidateCarrier -> worldCorrupt "world row has no semantic scope role"
    ParsedJoinEvidence -> pure "join"
    ParsedValueEvidence -> pure "value"
  let (unit,name) = rowOwner row
      required = Set.toAscList (Set.fromList (exactRequirements iface))
  pure (E.encodeListLen 6 <> text unit <> text name <> E.encodeBytes producer <> E.encodeBytes image
    <> text role <> E.encodeListLen (fromIntegral (length required))
    <> foldMap (\(ownerUnit,ownerName) -> E.encodeListLen 2 <> text ownerUnit <> text ownerName) required)
  where text = E.encodeString . T.pack

selectWorldRowParts :: BS.ByteString -> [OriginalInputImage] -> OfferedWorldRow -> IO [OriginalInputReference]
selectWorldRowParts producer images ((row@(iface,packages,packageSha),evidence),expectedImage) = case matching of
  [OriginalInputImage _ _ _ parts] -> pure
    [reference | (kind,reference) <- parts
      , (kind,originalInputPath reference,originalInputSha256 reference) `elem` expected]
  _ -> worldCorrupt "world interface row lacks one exact owned input image"
  where
    expected = [(InputInterface,exactPath iface,exactSha256 iface),(InputPackages,packages,packageSha)]
      ++ case evidence of
        ParsedModuleEvidence descriptor ->
          [(InputCertificate,descriptorCertificatePath descriptor,descriptorCertificateSha256 descriptor)]
            ++ [(InputCore,canonicalCorePath core,canonicalCoreSha256 core) | core <- maybe [] pure (descriptorCore descriptor)]
        _ -> []
    matching = [image | image@(OriginalInputImage issuer key seal parts) <- images
      , issuer == hexBytes producer, key == rowOwner row, seal == hexBytes expectedImage
      , all (`elem` [(kind,originalInputPath reference,originalInputSha256 reference) | (kind,reference) <- parts]) expected]

decodeWorldInputs :: Int -> Decoder s OfferedWorldInputs
decodeWorldInputs payloadBytes = do
  array 11
  magic <- string
  version <- decodeWord
  unless (magic == "TPCWORLD" && version == 4) (fail "unsupported compiler world descriptor")
  restoration <- decodeBytes
  source <- absolute
  producer <- rawWorldDigest
  profile <- rawWorldDigest
  array 2
  base <- worldRows
  visible <- worldRows
  acquisition <- decodeInputAcquisition
  visibility <- decodeBytes
  primary <- decodeBytes
  blocks <- bounded payloadBytes (array 2 >> ((,) <$> decodeBytes <*> decodeBytes))
  unless (not (BS.null primary) && any ((== primary) . fst) blocks)
    (fail "primary context root block is absent")
  pure (OfferedWorldInputs restoration source producer profile base visible acquisition visibility primary blocks)
  where worldRows = bounded payloadBytes (array 2 >> ((,) <$> decodeInterfaceRow <*> rawWorldDigest))

decodeWorldRestoration :: Decoder s WorldRestoration
decodeWorldRestoration = do
  worldArray 10
  magic <- decodeStringCanonical
  version <- decodeWordCanonical
  unless (magic == "TPCWREST" && version == 4) (fail "unsupported compiler world restoration")
  WorldRestoration <$> worldRawCID <*> rawWorldDigestCanonical <*> rawWorldDigestCanonical <*> rawWorldDigestCanonical
    <*> decodeWord64Canonical <*> worldRawCID <*> worldRawCID <*> worldRawCID

worldRawCID :: Decoder s BS.ByteString
worldRawCID = do
  cid <- decodeBytesCanonical
  unless (supportedWorldCID cid) (fail "unsupported compiler world root CID")
  pure (BS.copy cid)

rawWorldDigest :: Decoder s BS.ByteString
rawWorldDigest = decodeBytes >>= \bytes -> if BS.length bytes == 32 then pure (BS.copy bytes) else fail "invalid world digest"

hexBytes :: BS.ByteString -> String
hexBytes = concatMap (\byte -> let value = showHex byte "" in replicate (2-length value) '0' ++ value) . BS.unpack

type WorldSourceParser = StateT BS.ByteString (Either String)

decodeWorldSources :: BS.ByteString -> Either String CapturedWorldSources
decodeWorldSources bytes = do
  ((origin,roots),remaining) <- runStateT parse bytes
  unless (BS.null remaining) (Left "source domain has trailing bytes")
  pure (CapturedWorldSources origin (SHA.hash bytes) roots)
  where
    parse = do
      magic <- worldTake 8
      unless (magic == "TPCSRC01") (lift (Left "unsupported captured source domain"))
      origin <- worldFrame
      roots <- worldCounted 4 $ do
        files <- worldCounted 8 $ do
          pathBytes <- worldFrame
          path <- either (const (lift (Left "source path is not UTF-8"))) (pure . T.unpack) (TE.decodeUtf8' pathBytes)
          unless (not (null path) && not (isAbsolute path) && '\0' `notElem` path
            && normalise path == path && not (hasTrailingPathSeparator path)
            && all (\part -> not (null part) && part /= "." && part /= "..") (splitDirectories path))
            (lift (Left "captured source path escapes its root"))
          body <- worldFrame
          pure (path,body)
        unless (and (zipWith (<) (map fst files) (drop 1 (map fst files))))
          (lift (Left "captured source paths are not strictly sorted"))
        pure (WorldSourceRoot (Map.fromDistinctAscList files))
      pure (origin,roots)

worldTake :: Int -> WorldSourceParser BS.ByteString
worldTake count = do
  remaining <- get
  if count < 0 || count > BS.length remaining then lift (Left "truncated captured source domain")
    else do
      let (value,rest) = BS.splitAt count remaining
      put rest
      pure value

worldWord :: WorldSourceParser Int
worldWord = do
  bytes <- worldTake 4
  let value = sum [toInteger byte `shiftL` (8 * position) | (position,byte) <- zip [0..] (BS.unpack bytes)]
  if value > toInteger (maxBound :: Int) then lift (Left "source frame exceeds host range") else pure (fromInteger value)

worldFrame :: WorldSourceParser BS.ByteString
worldFrame = worldWord >>= worldTake

worldCounted :: Int -> WorldSourceParser a -> WorldSourceParser [a]
worldCounted minimumBytes item = do
  count <- worldWord
  remaining <- get
  when (count > BS.length remaining `div` minimumBytes) (lift (Left "source count exceeds remaining input"))
  replicateM count item

decodeScope :: Decoder s (OfferedScope, [(String, FilePath)], [((String,String),ParsedInterfaceEvidence)],
  [OfferedNativeProduct],InputAcquisition)
decodeScope = do
  count <- decodeListLen
  magic <- string
  version <- string
  unless (magic == "TPEXACTSCOPE" && version == "16" && count == 11)
    (fail "unsupported exact scope")
  semantic <- digestField
  producer <- digestField
  interfaceRows <- decodeInterfaceRows
  let interfaces = map fst interfaceRows
      interfaceEvidence = [((exactUnit iface,exactModule iface),evidence)
        | ((iface,_,_),evidence) <- interfaceRows]
      canonicalOwners = [key | (key,ParsedModuleEvidence _) <- interfaceEvidence]
  lexical <- bounded 4096 $ do
    array 2
    node <- owner
    imports <- bounded 4096 owner
    unique "exact lexical imports" imports
    pure (node, imports)
  nativeProducts <- bounded 4096 $ do
    array 10
    originalProduct <- ExactProduct <$> nonempty <*> nonempty <*> digestField
      <*> digestField <*> digestField <*> absolute <*> pure []
    selected <- nativeOrdinals "selected"
    descriptor <- array 2 >> (,) <$> absolute <*> canonicalDigest
    availabilityCount <- decodeListLen
    availabilityTag <- nonempty
    availability <- case (availabilityTag,availabilityCount) of
      ("full-carrier",1) -> pure OfferedFullCarrier
      ("issued-groups",2) -> OfferedIssuedGroups <$> nativeOrdinals "available"
      _ -> fail "invalid exact native availability"
    classification <- nonempty >>= \case
      "compiler-original" -> pure CompilerOriginalInput
      "private-native" -> pure PrivateNativeInput
      _ -> fail "invalid exact native input classification"
    pure (originalProduct,selected,descriptor,availability,classification)
  let products = [product | (product,_,_,_,_) <- nativeProducts]
      keys = [(exactUnit iface, exactModule iface) | (iface, _, _) <- interfaces]
      selected = map fst lexical
      productKeys = [(originalUnit originalProduct, originalModule originalProduct) | originalProduct <- products]
  unique "exact interface owners" keys
  unique "exact module names" (map snd keys)
  unique "exact lexical owners" selected
  unique "exact product owners" productKeys
  unless (all (`elem` keys) selected
      && all (`elem` selected) (concatMap snd lexical)
      && all (`elem` keys) productKeys
      && all (`elem` canonicalOwners) productKeys
      && all (\(iface, _, _) -> all (`elem` keys) (exactRequirements iface)) interfaces
      && all (\originalProduct -> any (\(iface, _, _) ->
          (exactUnit iface, exactModule iface) == (originalUnit originalProduct, originalModule originalProduct)
          && exactSha256 iface == originalIfaceSha256 originalProduct) interfaces) products)
    (fail "incomplete or conflicting exact owner closure")
  executionToken <- peekTokenType
  (descriptors, executionOwners) <- if executionToken /= TypeNull then do
    array 2
    graphs <- decodeExecutionSourceDescriptors
    references <- decodeExecutionSourceReferences
    pure (graphs, references)
    else decodeNull >> pure ([], [])
  nullPurpose <- (== TypeNull) <$> peekTokenType
  (requestTypes, checkedPurpose) <- if nullPurpose
    then do
      decodeNull
      pure (Nothing, NoCheckedPurpose)
    else do
    outerCount <- decodeListLen
    outerPurpose <- string
    (requestTypes, purpose) <- if outerPurpose == "request-types2" then do
      unless (outerCount == 4) (fail "invalid native request type admission")
      native <- decodeRequestTypeSignatures
      recipe <- string >>= \tag -> case tag of
        "none" -> pure NoRequestHelpers
        "actor-reply" -> pure ActorReplyHelpers
        _ -> fail "unsupported request helper recipe"
      token <- peekTokenType
      inner <- if token == TypeNull then decodeNull >> pure Nothing
        else Just <$> ((,) <$> decodeListLen <*> string)
      pure (Just (recipe, native), inner)
      else pure (Nothing, Just (outerCount, outerPurpose))
    admission <- maybe (pure NoCheckedPurpose)
      (uncurry decodePurpose) purpose
    pure (requestTypes, admission)
  published <- bounded 4096 $ do
    array 8
    root <- (,) <$> nonempty <*> nonempty
    interfaceSha <- digestField
    productSha <- digestField
    _revision <- nonempty
    _inputIdentity <- nonempty
    _selectionSha <- digestField
    unless (root `elem` selected && any (\originalProduct ->
      (originalUnit originalProduct,originalModule originalProduct) == root
        && originalIfaceSha256 originalProduct == interfaceSha
        && originalProductSha256 originalProduct == productSha) products)
      (fail "published source selection differs from exact original custody")
    dependencies <- bounded 4096 $ do
      array 4
      owner <- (,) <$> nonempty <*> nonempty
      interface <- digestField
      product <- digestField
      unless (owner /= root && owner `elem` selected && any (\original ->
        (originalUnit original,originalModule original) == owner
          && originalIfaceSha256 original == interface
          && originalProductSha256 original == product) products)
        (fail "completed source dependency differs from selected original custody")
      pure owner
    unique "completed source dependencies" dependencies
    pure (root,dependencies)
  unique "published source roots" (map fst published)
  acquisition <- decodeInputAcquisition
  pure (OfferedScope producer semantic interfaces lexical products executionOwners
    checkedPurpose requestTypes published, descriptors, interfaceEvidence, nativeProducts,acquisition)
  where
    nativeOrdinals role = do
      ordinals <- bounded 65536 decodeWord
      unless (all (<= 4294967295) ordinals) (fail ("exact native " ++ role ++ " ordinal exceeds u32"))
      unique ("exact native " ++ role ++ " ordinals") ordinals
      pure (NativeOrdinalSelection ordinals)
    decodePurpose authCount purpose = case purpose of
      "host-activation-renderer1" -> do
        unless (authCount == 9) (fail "invalid activation renderer admission")
        originalDigest <- digestField
        budget <- decodeWord64
        templateSha <- digestField
        native <- signature
        witness <- decodeBytes
        unless (not (BS.null witness) && BS.length witness <= 4 * 1024 * 1024)
          (fail "activation input witness exceeds bound")
        either fail pure (validateCheckedTypeWitnessBytes witness)
        unless (budget <= fromIntegral (maxBound :: Int)
            && originalDigest /= replicate 64 '0' && signatureKey native == "activation-input")
          (fail "invalid activation renderer input identity")
        originalInputs <- templateInterfaces
        unless (all ((== "main") . templateInterfaceUnit) originalInputs)
          (fail "activation renderer original graph belongs to another home unit")
        array 2
        originalTarget <- (,) <$> nonempty <*> nonempty
        unless (originalTarget `elem` [(templateInterfaceUnit input,templateInterfaceModule input)
            | input <- originalInputs])
          (fail "activation renderer original target leaves its sealed graph")
        paths <- includePaths
        pure (ExactActivationPreviewPurpose (ActivationPreviewAdmission originalDigest budget
          templateSha native witness originalInputs originalTarget) paths)
      "inspection1" -> do
        unless (authCount == 4) (fail "invalid inspection admission")
        injected <- bounded 4096 nonempty
        values <- valueInterfaces
        unique "inspection injected modules" injected
        validateInterfaces injected values
        paths <- includePaths
        pure (ExactInspectionPurpose values paths)
      "reload-inspection1" -> do
        unless (authCount == 4) (fail "invalid reload inspection admission")
        injected <- bounded 4096 nonempty
        values <- valueInterfaces
        unique "reload inspection injected modules" injected
        validateInterfaces injected values
        paths <- includePaths
        pure (ExactReloadInspectionPurpose values paths)
      "cell-check4" -> do
        unless (authCount == 10) (fail "invalid cell-check admission")
        admission <- CheckedCellAdmission <$> digestField <*> digestField <*> digestField
          <*> bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
          <*> bounded 4096 nonempty <*> bounded 4096 nonempty <*> valueInterfaces <*> checkedTemplateImports <*> pure Nothing
          <*> pure AuthoredCellCheck
        validateInterfaces (checkedInjectedModules admission) (checkedValueInterfaces admission)
        unique "checked injected modules" (checkedInjectedModules admission)
        unique "checked reserved modules" (checkedReservedModules admission)
        paths <- includePaths
        pure (ExactCellPurpose admission paths)
      "cell-program3" -> do
        unless (authCount == 15) (fail "invalid compiled cell admission")
        admission <- CheckedCellAdmission <$> digestField <*> digestField <*> digestField
          <*> bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
          <*> bounded 4096 nonempty <*> bounded 4096 nonempty <*> valueInterfaces <*> checkedTemplateImports
          <*> (Just <$> (PlannedCellAdmission <$> digestField <*> absolute <*> digestField <*> digestField
            <*> bounded 10000 (do
              slotFields <- decodeListLen
              kind <- nonempty
              case (kind,slotFields) of
                ("prologue",2) -> PlannedPrologue <$> decodeWord64
                ("decl",2) -> PlannedDeclaration <$> decodeWord64
                ("bind",2) -> PlannedBind <$> decodeWord64
                ("expr",3) -> PlannedExpression <$> decodeWord64 <*> nonempty
                _ -> fail "invalid compiled cell reservation")))
          <*> pure AuthoredCellCheck
        validateInterfaces (checkedInjectedModules admission) (checkedValueInterfaces admission)
        paths <- includePaths
        pure (ExactCellPurpose admission paths)
      "checked-item5" -> do
        unless (authCount == 20) (fail "invalid checked-item admission")
        admissionDigest <- digestField
        receiptDigest <- digestField
        index <- decodeWord64
        sourceDigest <- digestField
        kind <- nonempty
        unless (kind `elem` ["bind","expr"]) (fail "unsupported checked-item kind")
        binders <- bounded 65536 nonempty
        templates <- bounded 64 (array 2 >> (,) <$> nonempty <*> digestField)
        injected <- bounded 4096 nonempty
        signatures <- bounded 65536 signature
        unique "checked item binders" binders
        unique "checked item injected modules" injected
        unique "checked item signatures" (map signatureKey signatures)
        token <- peekTokenType
        liftPlan <- if token == TypeNull then decodeNull >> pure Nothing else do
          expression <- decodeCellExpressionPlan
          unless (kind == "expr") (fail "expression plan belongs to a non-expression item")
          pure (Just (case expressionPlanLift expression of
              ExpressionPure -> "pure"; ExpressionEffectful -> "effectful"))
        generation <- decodeWord64
        prefix <- digestField
        valueImports <- bounded 4096 (array 2 >> ((,) <$> nonempty <*> bounded 65536 nonempty))
        unique "completed value import owners" (map fst valueImports)
        unique "completed value import names" (concatMap snd valueImports)
        unless (all ((`elem` injected) . fst) valueImports) (fail "completed value import has no exact injected owner")
        observationToken <- peekTokenType
        observation <- if observationToken == TypeNull then decodeNull >> pure Nothing else Just <$> nonempty
        unless ((kind == "expr") == maybe False (const True) observation)
          (fail "checked observation identity differs from item kind")
        planned <- plannedDeclaration
        values <- completedValues
        valueInputs <- valueInterfaces
        templateInputs <- checkedTemplateImports
        validateInterfaces injected valueInputs
        validateValues valueImports values
        paths <- includePaths
        pure (ExactItemPurpose (CheckedItemAdmission AuthoredCheckedItem admissionDigest receiptDigest index sourceDigest kind binders
          templates injected signatures liftPlan generation prefix valueImports observation planned values valueInputs templateInputs) paths)
      _ -> fail "unsupported exact compile purpose"
    nullable decoder = do
      token <- peekTokenType
      if token == TypeNull then decodeNull >> pure Nothing else Just <$> decoder
    includePaths = bounded 4096 $ do
      path <- absolute
      when (T.length (T.pack path) > 65536) (fail "checked search path exceeds bound")
      pure path
    valueInterfaces = bounded 4096 $ do
      array 4
      ExactIfaceArtifact <$> nonempty <*> nonempty <*> absolute <*> digestField <*> pure []
    validateInterfaces injected values = do
      unique "checked value interface owners" (map exactModule values)
      unless (all checkedValueOwner values
          && length values == length injected && all (`elem` injected) (map exactModule values))
        (fail "checked value bytes differ from injected owner inventory")
    completedValues = bounded 4096 $ do
      array 5
      CompletedValueImport <$> nonempty <*> nonempty <*> absolute <*> digestField
        <*> bounded 65536 (array 2 >> (,) <$> nonempty <*> decodeWord64)
    validateValues imports values = unless
      (map (\value -> (completedValueModule value,map fst (completedValueBinders value))) values == imports
        && all ((== "main") . completedValueUnit) values)
      (fail "completed value identities differ from exact prefix imports")
    templateInterfaces = do
      inputs <- bounded 4096 (array 4 >> CheckedTemplateInterface <$> nonempty <*> nonempty <*> digestField
        <*> bounded 4096 (array 2 >> (,) <$> nonempty <*> nonempty))
      let owners = [(templateInterfaceUnit input,templateInterfaceModule input) | input <- inputs]
          ownerSet = Set.fromList owners
      unique "checked template interfaces" owners
      unless (sum (map (length . templateInterfaceImports) inputs) <= 65536)
        (fail "checked template graph exceeds its edge bound")
      forM_ inputs $ \input -> do
        unique "checked template edges" (templateInterfaceImports input)
        unless (all (`Set.member` ownerSet) (templateInterfaceImports input))
          (fail "checked template edge leaves its captured graph")
      pure inputs
    checkedTemplateImports = do
      array 2
      roots <- bounded 4096 (array 2 >> (,) <$> nonempty <*> nonempty)
      graph <- templateInterfaces
      let rootSet = Set.fromList roots
          graphOwners = Set.fromList
            [(templateInterfaceUnit input,templateInterfaceModule input) | input <- graph]
      unique "checked template direct roots" roots
      unless (rootSet `Set.isSubsetOf` graphOwners)
        (fail "checked template direct root leaves its sealed graph")
      pure (CheckedTemplateImports roots graph)
    plannedDeclaration = do
      token <- peekTokenType
      if token == TypeNull then decodeNull >> pure Nothing else do
        array 3
        unit <- nonempty
        ownerModule <- nonempty
        fingerprint <- nonempty
        unless (unit == "main" && "Tidepool.Session.Lib.G" `isPrefixOf` ownerModule
            && length fingerprint == 32 && all isHexDigit fingerprint)
          (fail "invalid completed original declaration identity")
        pure (Just ((unit,ownerModule),fingerprint))
    signature = decodeCheckedSignature

identity :: Decoder s SymbolIdentity
identity = do
  array 5
  unit <- decodeString
  name <- decodeString
  namespace <- decodeString
  occurrence <- decodeString
  token <- peekTokenType
  parent <- if token == TypeNull then decodeNull >> pure Nothing else Just <$> decodeString
  pure (SymbolIdentity unit name namespace occurrence parent)

owner :: Decoder s (String, String)
owner = array 2 >> (,) <$> nonempty <*> nonempty

array :: Int -> Decoder s ()
array count = decodeListLen >>= \actual -> unless (actual == count) (fail "invalid exact scope row")

bounded :: Int -> Decoder s a -> Decoder s [a]
bounded limit item = do
  count <- decodeListLen
  when (count > limit) (fail "exact scope inventory exceeds bound")
  replicateM count item

unique :: Ord a => String -> [a] -> Decoder s ()
unique label values = unless (Set.size (Set.fromList values) == length values) (fail ("duplicate " ++ label))

string :: Decoder s String
string = T.unpack <$> decodeString

nonempty :: Decoder s String
nonempty = do
  value <- string
  unless (not (null value)) (fail "empty exact owner")
  pure value

absolute :: Decoder s FilePath
absolute = do
  value <- string
  unless (isAbsolute value) (fail "relative exact artifact path")
  pure value

digestField :: Decoder s String
digestField = do
  value <- string
  unless (length value == 64 && all isHexDigit value) (fail "invalid exact digest")
  pure value

digest :: BS.ByteString -> String
digest = concatMap (\byte -> let value = showHex byte "" in replicate (2 - length value) '0' ++ value)
  . BS.unpack . SHA.hash
