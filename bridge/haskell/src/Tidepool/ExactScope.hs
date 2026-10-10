{-# LANGUAGE OverloadedStrings #-}

module Tidepool.ExactScope
  ( ExactScope, scopeManifestPath, scopeRequestSha256, scopeProducerSha256, scopeSemanticSha256, scopeLexical, scopeProducts, scopeExecutionGraphs, scopeExecutionOwners, scopePurpose, scopeRequestTypes, scopeSourceSelectedOwners, scopePublishedSourceOriginals, scopeInterfaces, scopeInterfaceEvidence
  , extendExactScopeInputs, extendExactScopeGeneration, extendCheckedValueScope, ExactScopePurpose(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..), SourceSelectedOriginals(..)
  , CheckedCellAdmission(..), CheckedCellPurpose(..), CheckedItemAdmission(..), CheckedItemPurpose(..)
  , ActivationPreviewAdmission(..), scopeActivationPreview
  , PlannedCellAdmission(..), PlannedCellSlot(..)
  , ExactInterfaceEvidence(..), CanonicalOrigin(..), CanonicalInterfaceProof, bindCanonicalProofInputs, CanonicalCoreArtifact
  , CanonicalInterfaceAdmission(ModuleInterfaceAdmission, LocalInterfaceAdmission), scopeCanonicalInterfaces, scopeSourceOriginalInterfaces, scopeModuleInterfaceProofs
  , admittedInterfaceHomeUnits, admittedInterfaceSourceSha256, resolveShippedHomeModule
  , admittedInterfaceRequirements, admittedInterfaceCore, captureCanonicalProofs, CanonicalInterfaceUse(..), canonicalProofInterfaceBytes, canonicalProofInterfaceBody, canonicalProofOriginalBytes, relocateCanonicalInterfaceProof, revalidateCanonicalProofInputs
  , captureFinalizedSourceOriginals, compilationOriginalSourceImports
  , validateCanonicalInterfaceProof, validateCandidateCanonicalInterfaceProof
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalCoreArtifact
  , canonicalCorePath, canonicalCoreSha256, canonicalHomeUnits, canonicalSourceSha256
  , canonicalRequirements, canonicalOrigin, canonicalSourceImports, isSourceOriginal, normalizeInterfaceEvidence
  , canonicalProofMatchesOwner
  , scopeCheckedCell, scopeCheckedItem, scopeIncludePaths
  , ExactInputOwner, newExactInputOwner, readExactScopeWithOwner
  , readExactScope, ExactScopeValidationReason(..), revalidateExactScope, revalidateExactScopesAt, validateExactScopeEnvironment, scopeValueInterfaces
  , scopeInterfaceBytes, scopeInterfaceToken, scopeOriginalBytes, readScopedInterfaces, readScopedInterfaceClosure, admittedInterfaceCoreBytes
  , writeExactCompilation, writeCheckedExactCompilation, writeCheckedExactCompilationWithPublication
  , writeRetainedExactCompilation, writeRetainedExactCompilationWithPublication
  , writeRetainedExactCompilationWithOutputsAndPublication, extendSourceSelectedOriginals
  , revalidateExactScopesAtWithOutputs
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget
  , scopeExecutionNativeOwners
  , scopeAvailableOriginalProducts
  , originalGroupFromProjected, originalGroupFromCandidate
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Codec.CBOR.Encoding as E
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try, throwIO, evaluate)
import Control.Concurrent.MVar (MVar, newMVar, modifyMVar)
import Control.Monad (foldM, forM, forM_, replicateM, unless, when)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.Char (isHexDigit)
import Data.IORef (IORef, newIORef, readIORef, modifyIORef', writeIORef)
import Data.List (isPrefixOf)
import Data.Maybe (isJust)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import GHC.Driver.Env (HscEnv, hsc_home_unit)
import GHC.Types.PkgQual (PkgQual(NoPkgQual))
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Module (Module, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Location (ml_hs_file)
import GHC.Unit.Types (unitString)
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (createDirectory, createDirectoryIfMissing, makeAbsolute, doesFileExist)
import System.FilePath (isAbsolute, takeDirectory, (</>))
import Tidepool.BoundedRead (readFileAtMost, FileObservations, FileObservation(..), withFileObservations, observeFile, fileObservationTotals)
import System.IO.Error (isAlreadyExistsError)
import System.Environment (lookupEnv)
import Text.Read (readMaybe)
import Tidepool.ExactHydration (ExactIfaceArtifact(..), checkedValueOwner, CheckedTemplateInterface(..), CheckedTemplateImports(..), RequestIfaceDecoder, newRequestIfaceDecoder, pruneRequestIfaceDecoder, VerifiedExactIfaceClosure, readCapturedExactIfaceArtifacts, readCapturedExactIfaceClosureWithCheckedValues)
import GHC.Unit.Module.ModIface (ModIface)
import Tidepool.ArtifactBytes (ArtifactBytes, artifactBytes, artifactSha256, checkArtifactSeal)
import Tidepool.RequestInputs (RequestOriginalInputs, RequestInputReader, RequestInputTokenReader, capturedRequestInputToken, requestInputRetained, requestInputCount, retainRequestEncodedBytes, captureRequestInputs, captureRequestInputTokens, mergeRequestInputs, aliasRequestInputs, capturedRequestInput, CapturedOriginalContent, emptyCapturedOriginalContent, OriginalInputReference(..), continueRequestInputs, selectedOriginalContent, capturedOriginalContentBytes, capturedOriginalContentKeys, mergeCapturedOriginalContent, requestCaptureByteLimit, requestInputBytes, requestInputBodies, transferRequestInputBodies)
import Tidepool.Session (Generation(..), SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString)
import Tidepool.CheckedPrefixImports (CompletedValueImport(..))
import Tidepool.CheckedCell
  ( CheckedSignature(..), RequestTypeSignatures, RequestHelperRecipe(..), decodeCheckedSignature, decodeRequestTypeSignatures
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
  , revalidateLocalFinalizedAdmission, revalidateLocalFinalizedAdmissionWith )
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
  , ownedScopeProducts :: [ExactProduct]
  , ownedScopeExecutionGraphs :: [ExecutionSourceGraph]
  , ownedScopeExecutionOwners :: [ExecutionSourceRef]
  , ownedScopePurpose :: ExactScopePurpose
  , ownedScopeRequestTypes :: Maybe (RequestHelperRecipe, RequestTypeSignatures)
  -- Request-local source proof roots are revalidated by subsequent stages;
  -- they are never serialized as baseline lexical authority.
  , ownedScopeSourceSelectedOwners :: Set.Set (String,String)
  , ownedScopePublishedSourceOriginals :: Set.Set (String,String)
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
scopeProducts = ownedScopeProducts

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
data AdmittedOriginalCensus = AdmittedOriginalCensus FilePath String ExactProduct OriginalNativeCensus
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
  [(String,CapturedOriginalContent)] RequestIfaceDecoder DecodedOriginalFacts

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
    <$> newMVar (RetainedOriginalInputs [] decoder emptyDecodedOriginalFacts)

data DecodedOriginalFacts = DecodedOriginalFacts
  (Map.Map String CanonicalModuleCertificate)
  (Map.Map (String,String,String,String) PackageImportEvidence)
  (Map.Map String OriginalNativeCensus)
  (Map.Map String ExecutionSourceGraph)

emptyDecodedOriginalFacts :: DecodedOriginalFacts
emptyDecodedOriginalFacts = DecodedOriginalFacts Map.empty Map.empty Map.empty Map.empty

memoOriginalFact :: Ord key => Bool -> String -> IORef (Map.Map key value)
  -> key -> IO value -> IO value
memoOriginalFact timing label state key acquire = do
  facts <- readIORef state
  case Map.lookup key facts of
    Just fact -> emitCount timing (label ++ ".hits") 1 >> pure fact
    Nothing -> do
      fact <- acquire
      modifyIORef' state (Map.insert key fact)
      emitCount timing (label ++ ".misses") 1
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
     Just (AdmittedOriginalCensus _ _ full _) -> full
     -- Request-private additions already carry the full groups issued by the
     -- current original inventory. They need no fabricated external receipt.
     Nothing -> selected
  | selected <- scopeProducts scope]
  where AdmittedScopeInputs _ _ _ census _ = scopeInputs scope

validateNativeSelection :: ExactProduct -> ExactProduct -> OriginalNativeCensus -> IO ()
validateNativeSelection selected full census = do
  unless (selected {originalGroups=[]} == full {originalGroups=[]})
    (fail "selected native product differs from its admitted full carrier")
  unless (nativeCensusSelectionMatches census (originalGroups selected))
    (fail "selected native groups differ from their authenticated census")

newtype NativeOrdinalSelection = NativeOrdinalSelection [Word]

type OfferedNativeProduct = (ExactProduct, NativeOrdinalSelection, (FilePath,String))

admitOriginalCensusWith :: RequestInputReader -> String -> Map.Map InterfaceOwner ExactInterfaceEvidence
  -> [OfferedNativeProduct] -> IO (Map.Map InterfaceOwner AdmittedOriginalCensus)
admitOriginalCensusWith readInput = admitOriginalCensusUsing (readOriginalNativeCensusWith readInput)

admitOriginalCensusUsing :: (FilePath -> String -> IO OriginalNativeCensus) -> String
  -> Map.Map InterfaceOwner ExactInterfaceEvidence -> [OfferedNativeProduct]
  -> IO (Map.Map InterfaceOwner AdmittedOriginalCensus)
admitOriginalCensusUsing readFacts producer evidence offered = Map.fromList <$> forM offered
  (\(selected,_,(path,sha)) -> do
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
    let full = selected {originalGroups=nativeCensusExactGroups native}
    pure (key,AdmittedOriginalCensus path sha full native))

selectedCensusProducts :: [OfferedNativeProduct] -> Map.Map InterfaceOwner AdmittedOriginalCensus -> IO [ExactProduct]
selectedCensusProducts offered admitted = forM offered $ \(product,NativeOrdinalSelection ordinals,_) -> do
  let key = (originalUnit product,originalModule product)
  case Map.lookup key admitted of
    Just (AdmittedOriginalCensus _ _ _ census) -> do
      groups <- either fail pure (selectNativeCensusGroups census ordinals)
      pure product {originalGroups=groups}
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
    (union,captured) <- captureRequestInputs (Just transferred) $ \readInput -> do
      forM_ added $ \((iface,packages,sha),evidence) -> do
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
  -> [ExactProduct] -> [((String,String),[(String,String)])]
  -> IO (Either String ExactScope)
extendExactScopeGeneration scope offered products lexical = do
  let oldProducts = Map.fromList [((originalUnit p,originalModule p),p) | p <- scopeProducts scope]
      oldLexical = Map.fromList (scopeLexical scope)
      merge selected values = foldM (\known (key,value) -> case Map.lookup key known of
        Just previous | previous /= value -> Left "scope generation replaces an admitted owner"
        _ -> Right (Map.insert key value known)) selected values
      admittedOwners = Set.fromList (map rowOwner (scopeInterfaces scope ++ map fst offered))
      lexicalOwners = Set.union (Map.keysSet oldLexical) (Set.fromList (map fst lexical))
      productKeys = [(originalUnit p,originalModule p) | p <- products]
  if Set.size (Set.fromList productKeys) /= length products
      || Set.size (Set.fromList (map fst lexical)) /= length lexical
    then pure (Left "scope generation repeats a product or lexical owner")
    else
     case (merge oldProducts [((originalUnit p,originalModule p),p) | p <- products], merge oldLexical lexical) of
       (Right _,Right _) | all (\(key,requirements) -> key `Set.member` admittedOwners
           && all (`Set.member` lexicalOwners) requirements) lexical ->
         extendExactScopeInputs (scope
           { ownedScopeProducts=scopeProducts scope ++ [p | p <- products, Map.notMember (originalUnit p,originalModule p) oldProducts]
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
        unless (Map.keysSet (canonicalRequirements proof) == Set.fromList (exactRequirements iface)
            && matching (canonicalRequirements proof))
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
  forM_ (Map.toAscList census) $ \(key,AdmittedOriginalCensus _ _ full native) ->
    case [product' | product' <- products, (originalUnit product',originalModule product') == key] of
      [selected] -> validateNativeSelection selected full native
      _ -> fail "admitted native census lacks its unique scope product"
  where firstOfThree (value,_,_) = value

observeSeal :: FileObservations -> FilePath -> Maybe Int -> String -> String -> IO ()
observeSeal observations path bound expected reason = do
  observed <- observeFile observations path bound
  unless (observedSha256 observed == expected) (fail reason)

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
admittedInterfaceCoreBytes (ModuleInterfaceAdmission proof) = do
  (path,sha) <- maybe (fail "admitted interface has no defining Core") pure (admittedInterfaceCore (ModuleInterfaceAdmission proof))
  canonicalProofOriginalBytes proof path sha
admittedInterfaceCoreBytes (LocalInterfaceAdmission proof) =
  maybe (fail "local defining Core has not been captured for execution") (pure . artifactBytes) (localFinalizedCoreBody proof)

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
    forM_ selections $ \(proof,use) -> do
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
canonicalProofOriginalBytes proof path sha = case proofInputCustody proof of
  CapturedCanonicalInputs bodies -> case Map.lookup path bodies of
    Just body | artifactSha256 body == sha -> pure (artifactBytes body)
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

data PlannedCellSlot = PlannedPrologue Word64 | PlannedDeclaration Word64
  | PlannedBind Word64 | PlannedExpression Word64 String
  deriving (Eq, Show)

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
readExactScope path = newExactInputOwner >>= \owner -> readExactScopeWithOwner owner path

readExactScopeWithOwner :: ExactInputOwner -> FilePath -> IO (Either String ExactScope)
readExactScopeWithOwner (ExactInputOwner retentionLimit state) path = modifyMVar state $ \previous@(RetainedOriginalInputs retained decoder (DecodedOriginalFacts certificates packages native graphs)) -> do
  timing <- readTimingEnabled
  certificatesState <- newIORef certificates
  packagesState <- newIORef packages
  nativeState <- newIORef native
  graphsState <- newIORef graphs
  timeDetailPhase timing "exact_scope" "read" $ do
    captured <- try (do
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
      (scope, originals) <- captureRequestInputTokens (Just base) $ \readToken -> do
       let readInput artifact bound = artifactBytes <$> readToken artifact bound
           readVerified artifact bound sha = do
             token <- readToken artifact bound
             unless (artifactSha256 token == sha) (fail "exact input differs from its selected seal")
             pure (artifactBytes token)
       graphs <- readExecutionSourceGraphsWithFacts (\sha artifact -> do
         token <- readToken artifact (64 * 1024 * 1024)
         either fail pure (checkArtifactSeal sha token)
         memoOriginalFact timing "original_inputs.graph" graphsState sha
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
             memoOriginalFact timing "original_inputs.certificate" certificatesState sha
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
         memoOriginalFact timing "original_inputs.census" nativeState sha
           (decodeOriginalNativeCensusBody sha token)) producer evidence nativeDescriptors
       selectedProducts <- selectedCensusProducts nativeDescriptors census
       roots <- extendAdmittedPackageImportsWithFacts (\(iface,artifact,sha) -> do
         payload <- readVerified artifact (4 * 1024 * 1024) sha
         _ <- readVerified (exactPath iface) (32 * 1024 * 1024) (exactSha256 iface)
         Right <$> memoOriginalFact timing "original_inputs.packages" packagesState
           (sha,exactUnit iface,exactModule iface,exactSha256 iface)
           (either fail pure (decodeCapturedPackageImports iface payload))) emptyAdmittedPackageImports rows >>= either fail pure
       let AdmittedScopeInputs order admitted _ _ _ = makeScopeInputs rows evidence roots
           inputs = AdmittedScopeInputs order admitted roots census Nothing
           sha = digest bytes
           scope = ExactScope path sha producer semantic inputs lexical selectedProducts graphs references
             purpose types Set.empty (Set.fromList published)
       forM_ (scopeValueInterfaces scope) $ \iface -> do
         _ <- readVerified (exactPath iface) (32 * 1024 * 1024) (exactSha256 iface)
         pure ()
       validateInputClosure producer inputs selectedProducts
       forM_ published $ \owner -> case Map.lookup owner evidence of
         Just (ModuleInterfaceEvidence proof) | isSourceOriginal (canonicalOrigin proof) -> pure ()
         _ -> fail "published original lacks its canonical source owner"
       validatePreviewOriginalTarget scope evidence
       validateExecutionSources scope graphs
       when timing $ do
         _ <- evaluate (length sha)
         emitCount timing ("hash_bytes.scope_metadata." ++ sha) (fromIntegral (BS.length bytes))
       pure scope
      let complete = retainCapturedInputsUsing originals decoder scope
      additions <- forM images $ \(OriginalInputImage _ _ key parts) -> do
        content <- either fail pure (selectedOriginalContent (map snd parts) originals)
        pure (key,content)
      let keys = Set.fromList (map fst additions)
          candidates = additions ++ filter ((`Set.notMember` keys) . fst) retained
          unionContent entries = foldl (\content (_,more) -> mergeCapturedOriginalContent content more)
            emptyCapturedOriginalContent entries
          keep _ [] = []
          keep content (entry@(_,more):rest)
            | capturedOriginalContentBytes combined <= retentionLimit = entry : keep combined rest
            | otherwise = keep content rest
            where combined = mergeCapturedOriginalContent content more
          settled = take 4096 (keep emptyCapturedOriginalContent candidates)
          retainedContent = unionContent settled
          bytesRetained = capturedOriginalContentBytes retainedContent
          bytesBefore = capturedOriginalContentBytes (unionContent candidates)
      decodedCount <- pruneRequestIfaceDecoder (Set.map fst (capturedOriginalContentKeys retainedContent)) decoder
      emitCount timing "original_inputs.retained_encoded_bytes" bytesRetained
      emitCount timing "original_inputs.evicted_encoded_bytes" (bytesBefore - bytesRetained)
      emitCount timing "original_inputs.live_request_encoded_bytes" (requestInputBytes originals)
      emitCount timing "original_inputs.receiving_images" (fromIntegral (length images))
      emitCount timing "original_inputs.decoded_interfaces" (fromIntegral decodedCount)
      emitCount timing "original_inputs.receiving_encoded_bytes" (capturedOriginalContentBytes (unionContent additions))
      let seals = Set.map fst (capturedOriginalContentKeys retainedContent)
      certificateFacts <- Map.restrictKeys <$> readIORef certificatesState <*> pure seals
      packageFacts <- Map.filterWithKey (\(sha,_,_,_) _ -> sha `Set.member` seals) <$> readIORef packagesState
      nativeFacts <- Map.restrictKeys <$> readIORef nativeState <*> pure seals
      graphFacts <- Map.restrictKeys <$> readIORef graphsState <*> pure seals
      emitCount timing "original_inputs.decoded_content_facts" (fromIntegral
        (Map.size certificateFacts + Map.size packageFacts + Map.size nativeFacts + Map.size graphFacts))
      pure (complete,RetainedOriginalInputs settled decoder
        (DecodedOriginalFacts certificateFacts packageFacts nativeFacts graphFacts)))
        :: IO (Either IOException (ExactScope,RetainedOriginalInputs))
    pure $ case captured of
      Left failure -> (previous,Left (show failure))
      Right (scope,settled) -> (settled,Right scope)

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
    certificate <- readCertificate descriptor
    unless (certificateProducer certificate == producer
        && certificateOwner certificate == key
        && certificateInterface certificate == exactSha256 iface
        && certificatePackages certificate == packageSha
        && certificateCore certificate == (canonicalCoreSha256 <$> descriptorCore descriptor))
      (fail "canonical module certificate differs from exact owner or payload")
    unless (case descriptorPurpose descriptor of
        CandidateCarrier -> True
        ScopeInterface role -> role == originRole (certificateOrigin certificate))
      (fail "canonical module origin differs from its exact interface role")
    let requirements = certificateRequirements certificate
    let matchesRequirement (required,seal) = Map.lookup required available == Just seal
    unless (Map.keysSet requirements == Set.fromList (exactRequirements iface)
        && all matchesRequirement (Map.toAscList requirements))
      (fail "canonical module requirements differ from selected exact interfaces")
    _ <- verifiedInput (exactPath iface) (32 * 1024 * 1024) (certificateInterface certificate)
    _ <- verifiedInput packages (4 * 1024 * 1024) (certificatePackages certificate)
    pure (key, CanonicalInterfaceProof
      { proofCertificatePath = descriptorCertificatePath descriptor
      , proofCertificateSha256 = descriptorCertificateSha256 descriptor
      , proofCoreArtifact = descriptorCore descriptor
      , proofInterfaceInput = (iface,packages,packageSha)
      , proofNativeInput = Nothing
      , proofExecutionGraphs = []
      , proofInputCustody = DescriptorInputs
      , canonicalFacts = certificate
      })
  pure (Map.fromList proofs)

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
revalidateExactScopesAt reason env scopes = revalidateExactScopesAtWithOutputs reason env scopes []

-- Freshly materialized outputs participate in the same bounded terminal proof
-- as selected scopes. Captured inputs are not reconstructed here: only output
-- seals supplied by the operation that wrote them are observed.
revalidateExactScopesAtWithOutputs
  :: ExactScopeValidationReason -> HscEnv -> [ExactScope]
  -> [(FilePath,String,Maybe Int)] -> IO (Either String ())
revalidateExactScopesAtWithOutputs reason env scopes outputs = do
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

-- Metadata has completed all compiler work at this boundary. Capture and
-- validate every consumed source before the terminal current scope/package
-- proof; publishing the receipt afterwards consumes only captured bytes.
writeCheckedExactCompilation
  :: HscEnv -> ExactCompilation -> DependencyEvidence -> IO ()
writeCheckedExactCompilation env = writeCheckedExactCompilationWithScopes
  CheckedReceiptPublication env [] [] (pure ())

writeCheckedExactCompilationWithPublication
  :: HscEnv -> ExactCompilation -> DependencyEvidence -> IO () -> IO ()
writeCheckedExactCompilationWithPublication env compilation evidence publish =
  writeCheckedExactCompilationWithScopes CheckedReceiptPublication env [] [] publish compilation evidence

-- Retention and the checked receipt share this final publication boundary.
-- Capture source evidence first, then check selected scopes and fresh outputs
-- in one proof. No observation or validation token reaches the publisher.
writeRetainedExactCompilation
  :: HscEnv -> ExactScope -> ExactCompilation -> DependencyEvidence -> IO ()
writeRetainedExactCompilation env retained = writeCheckedExactCompilationWithScopes
  RetainedReceiptPublication env [retained] [] (pure ())

writeRetainedExactCompilationWithPublication
  :: HscEnv -> ExactScope -> ExactCompilation -> DependencyEvidence -> IO () -> IO ()
writeRetainedExactCompilationWithPublication env retained compilation evidence publish =
  writeCheckedExactCompilationWithScopes RetainedReceiptPublication env [retained] [] publish compilation evidence

writeRetainedExactCompilationWithOutputsAndPublication
  :: HscEnv -> ExactScope -> ExactCompilation -> DependencyEvidence
  -> [(FilePath,String,Maybe Int)] -> IO () -> IO ()
writeRetainedExactCompilationWithOutputsAndPublication env retained compilation evidence outputs publish =
  writeCheckedExactCompilationWithScopes RetainedReceiptPublication env [retained] outputs publish compilation evidence

writeCheckedExactCompilationWithScopes
  :: ExactScopeValidationReason -> HscEnv -> [ExactScope] -> [(FilePath,String,Maybe Int)]
  -> IO () -> ExactCompilation -> DependencyEvidence -> IO ()
writeCheckedExactCompilationWithScopes reason env retained outputs publishCertificate compilation evidence = do
  selected <- either fail pure (extendSourceSelectedOriginals
    (compilationSourceSelection compilation) (compilationScope compilation))
  receipt <- captureExactCompilationReceipt compilation evidence
  revalidateExactScopesAtWithOutputs reason env (retained ++ [selected]) outputs >>= either fail pure
  publishCertificate
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
    unchangedSelection <- revalidateDependencyEvidence (selectedOriginalEvidence selected)
    negativeSelection <- and <$> forM (dependencyResolutions (selectedOriginalEvidence selected)) (\resolution -> do
      let absent = case dependencyResolutionSelected resolution of
            Nothing -> dependencyResolutionCandidates resolution
            Just chosen -> takeWhile (/= chosen) (dependencyResolutionCandidates resolution)
      and <$> mapM (fmap not . doesFileExist) absent)
    unless (unchangedSelection && negativeSelection)
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
        , case compilationSourceSelection compilation of
            Nothing -> encodeArray [encodeArray [], E.encodeNull]
            Just selected -> encodeArray
              [ encodeArray [encodeArray [text unit,text name,text certificate,text interfaceSha,text sourceSha]
                  | ((unit,name),certificate,interfaceSha,sourceSha) <- selectedOriginalRows selected]
              , text (renderDependencyEvidence (selectedOriginalEvidence selected))]]
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
  [(String,String)]

validateOwnedInputImages :: OfferedScope -> [(String,FilePath)]
  -> [((String,String),ParsedInterfaceEvidence)] -> [OfferedNativeProduct]
  -> [OriginalInputImage] -> IO ()
validateOwnedInputImages (OfferedScope producer _ rows _ products _ _ _ _) graphs evidence census images = do
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
        ++ [(InputCensus,path,sha) | (_,_,(path,sha)) <- census]
        ++ [(InputGraph,path,sha) | (sha,path) <- graphs]
      ownedParts = [((exactUnit iface,exactModule iface),(kind,path,sha)) | (iface,packages,packageSha) <- rows
        , (kind,path,sha) <- [(InputInterface,exactPath iface,exactSha256 iface),(InputPackages,packages,packageSha)]]
        ++ concat [case proof of
            ParsedModuleEvidence descriptor -> [(key,(InputCertificate,descriptorCertificatePath descriptor,descriptorCertificateSha256 descriptor))]
              ++ [(key,(InputCore,canonicalCorePath core,canonicalCoreSha256 core)) | core <- maybe [] pure (descriptorCore descriptor)]
            _ -> [] | (key,proof) <- evidence]
        ++ [((originalUnit product,originalModule product),(InputNative,originalProductPath product,originalProductSha256 product)) | product <- products]
        ++ [((originalUnit product,originalModule product),(InputCensus,path,sha)) | (product,_,(path,sha)) <- census]
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

decodeScope :: Decoder s (OfferedScope, [(String, FilePath)], [((String,String),ParsedInterfaceEvidence)],
  [OfferedNativeProduct],InputAcquisition)
decodeScope = do
  count <- decodeListLen
  magic <- string
  version <- string
  unless (magic == "TPEXACTSCOPE" && version == "13" && count == 11)
    (fail "unsupported exact scope")
  semantic <- digestField
  producer <- digestField
  interfaceRows <- bounded 4096 $ do
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
    array 8
    originalProduct <- ExactProduct <$> nonempty <*> nonempty <*> digestField
      <*> digestField <*> digestField <*> absolute <*> pure []
    ordinals <- bounded 65536 decodeWord
    unless (all (<= 4294967295) ordinals) (fail "exact native ordinal exceeds u32")
    unique "exact original ordinals" ordinals
    descriptor <- array 2 >> (,) <$> absolute <*> canonicalDigest
    pure (originalProduct,NativeOrdinalSelection ordinals,descriptor)
  let products = [product | (product,_,_) <- nativeProducts]
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
    array 7
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
    pure root
  unique "published source roots" published
  acquisition <- decodeInputAcquisition
  pure (OfferedScope producer semantic interfaces lexical products executionOwners
    checkedPurpose requestTypes published, descriptors, interfaceEvidence, nativeProducts,acquisition)
  where
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
