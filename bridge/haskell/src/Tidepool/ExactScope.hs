{-# LANGUAGE OverloadedStrings #-}

module Tidepool.ExactScope
  ( ExactScope(..), AdmittedScopeInputs, emptyScopeInputs, scopeInterfaces, scopeInterfaceEvidence
  , extendExactScopeInputs, ExactScopePurpose(..), ExactProduct(..), ExactOriginalGroup(..), ExactCompilation(..), SourceSelectedOriginals(..)
  , CheckedCellAdmission(..), CheckedCellPurpose(..), CheckedItemAdmission(..), CheckedItemPurpose(..)
  , ActivationPreviewAdmission(..), ActivationPreviewInputMetadata(..), scopeActivationPreview
  , PlannedCellAdmission(..), PlannedCellSlot(..)
  , ExactInterfaceEvidence(..), CanonicalOrigin(..), CanonicalInterfaceProof, CanonicalCoreArtifact
  , CanonicalInterfaceAdmission(..), scopeCanonicalInterfaces, scopeSourceOriginalInterfaces, scopeModuleInterfaceProofs
  , admittedInterfaceHomeUnits, admittedInterfaceSourceSha256, resolveShippedHomeModule
  , admittedInterfaceRequirements, admittedInterfaceCore
  , captureFinalizedSourceOriginals, compilationOriginalSourceImports
  , validateCanonicalInterfaceProof, validateCandidateCanonicalInterfaceProof
  , canonicalCertificatePath, canonicalCertificateSha256, canonicalCoreArtifact
  , canonicalCorePath, canonicalCoreSha256, canonicalHomeUnits, canonicalSourceSha256
  , canonicalRequirements, canonicalOrigin, canonicalSourceImports, isSourceOriginal, normalizeInterfaceEvidence
  , scopeCheckedCell, scopeCheckedItem, scopeIncludePaths
  , readExactScope, revalidateExactScope, scopeValueInterfaces
  , writeExactCompilation, writeCheckedExactCompilation, extendSourceSelectedOriginals
  , extendExactExecutionSources, extendExactExecutionSourcesWithinBudget
  , scopeExecutionNativeOwners
  , originalGroupFromProjected, originalGroupFromCandidate
  ) where

import Codec.CBOR.Decoding
import Codec.CBOR.Read (deserialiseFromBytes)
import qualified Codec.CBOR.Encoding as E
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try, throwIO, evaluate)
import Control.Monad (foldM, forM, forM_, replicateM, unless, when)
import qualified Crypto.Hash.SHA256 as SHA
import qualified Data.ByteString as BS
import qualified Data.ByteString.Lazy as BL
import Data.Char (isHexDigit)
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
import Tidepool.BoundedRead (readFileAtMost, FileObservations, FileObservation(..), withFileObservations, observeFile)
import System.IO.Error (isAlreadyExistsError)
import Tidepool.ExactHydration (ExactIfaceArtifact(..), CheckedTemplateInterface(..), CheckedTemplateImports(..))
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
  , candidateCertificatePath, candidateCertificateSha256, candidateCoreDescriptor )
import Tidepool.ExecutionSource
  ( ExecutionSourceGraph(..), ExecutionSourceIdentity(..), ExecutionSourceOwner(..)
  , ExecutionSourceRef(..), ExecutionSourceNode(..), decodeExecutionSourceDescriptors, decodeExecutionSourceReferences
  , ExecutionSourceFiles(..), readExecutionSourceGraphs, executionSourceGraphsFit
  , ExecutionSourceFailure(..), executionIdentityKey, executionSourceClosure, executionSourceOriginalNode
  , executionSourceOriginalClosure )
import Tidepool.LocalNativeDeclaration
  ( LocalNativeDeclarationAdmission, localNativeOwner, localNativeProof )
import Tidepool.PackageWitness
  ( AdmittedPackageImports, emptyAdmittedPackageImports, extendAdmittedPackageImports
  , revalidateAdmittedPackageImports )
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, finalizedValueInterfaceSeals, finalizedLocalAdmissions, LocalFinalizedAdmission, localFinalizedInterface, localFinalizedHomeUnits
  , localFinalizedSourceSha256, localFinalizedRequirements, localFinalizedCore
  , revalidateLocalFinalizedAdmission, revalidateLocalFinalizedAdmissionWith )
import Tidepool.Timing (readTimingEnabled, timeDetailPhase, emitCount)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencySource(..), DependencyModule(..), DependencyImport(..), DependencyResolution(..), renderDependencyEvidence
  , DependencyQualifier(..), renderDependencyQualifier, parseDependencyQualifier, revalidateDependencyEvidence )

data ExactScope = ExactScope
  { scopeManifestPath :: FilePath
  , scopeRequestSha256 :: String
  , scopeProducerSha256 :: String
  , scopeSemanticSha256 :: String
  , scopeInputs :: AdmittedScopeInputs
  , scopeLexical :: [((String, String), [(String, String)])]
  , scopeProducts :: [ExactProduct]
  , scopeExecutionGraphs :: [ExecutionSourceGraph]
  , scopeExecutionOwners :: [ExecutionSourceRef]
  , scopePurpose :: ExactScopePurpose
  , scopeRequestTypes :: Maybe (RequestHelperRecipe, RequestTypeSignatures)
  -- Request-local source proof roots are revalidated by subsequent stages;
  -- they are never serialized as baseline lexical authority.
  , scopeSourceSelectedOwners :: Set.Set (String,String)
  } deriving (Eq, Show)

-- One checked purpose owns its admission and ordered search inputs. Native
-- request-type evidence is an independent wrapper, not another checked stage.
data ExactScopePurpose
  = NoCheckedPurpose
  | ExactCellPurpose CheckedCellAdmission [FilePath]
  | ExactItemPurpose CheckedItemAdmission [FilePath]
  | ExactInspectionPurpose [ExactIfaceArtifact] [FilePath]
  | ExactActivationPreviewPurpose ActivationPreviewAdmission [FilePath]
  deriving (Eq, Show)

-- A preview consumes the original live input's native type and mounted interface.
-- It grants no authored item, value export, or declaration completion authority.
data ActivationPreviewInputMetadata = ActivationPreviewInputMetadata
  { previewInputName :: String
  , previewInputVarId :: Word64
  , previewInputModule :: String
  , previewInputTier :: String
  , previewInputTypeDisplay :: String
  , previewInputRootHead :: Maybe (T.Text,T.Text,T.Text)
  , previewInputHostAuthority :: Maybe String
  } deriving (Eq, Show)

data ActivationPreviewAdmission = ActivationPreviewAdmission
  { previewAdmissionDigest :: String
  , previewGeneration :: Word64
  , previewBudget :: Word64
  , previewTemplateSha256 :: String
  , previewInputGeneration :: Word64
  , previewInputMetadata :: ActivationPreviewInputMetadata
  , previewInputSignature :: CheckedSignature
  , previewInputWitness :: BS.ByteString
  , previewInputInterface :: ExactIfaceArtifact
  , previewOriginalInterfaces :: [CheckedTemplateInterface]
  , previewOriginalTarget :: (String,String)
  , previewInputPackagesSha256 :: String
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
  , canonicalFacts :: CanonicalModuleCertificate
  } deriving (Eq, Show)

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
  { proofCertificatePath=""
  , proofCoreArtifact=(\(CanonicalCoreArtifact _ sha) -> CanonicalCoreArtifact "" sha) <$> proofCoreArtifact proof })
normalizeInterfaceEvidence evidence = evidence

type InterfaceRow = (ExactIfaceArtifact,FilePath,String)
type InterfaceOwner = (String,String)

-- Rows and their admitted role have one owner. The ordered index preserves
-- transport order; package facts are issued once from their sealed sidecars.
data ScopeInterfaceInput = ScopeInterfaceInput InterfaceRow ExactInterfaceEvidence
  deriving (Eq, Show)
data AdmittedScopeInputs = AdmittedScopeInputs [InterfaceOwner]
  (Map.Map InterfaceOwner ScopeInterfaceInput) AdmittedPackageImports deriving (Eq, Show)

-- A vacuous inventory grants no interface, Core or package authority.
emptyScopeInputs :: AdmittedScopeInputs
emptyScopeInputs = AdmittedScopeInputs [] Map.empty emptyAdmittedPackageImports

scopeInterfaces :: ExactScope -> [InterfaceRow]
scopeInterfaces scope = inputRows (scopeInputs scope)

inputRows :: AdmittedScopeInputs -> [InterfaceRow]
inputRows (AdmittedScopeInputs order inputs _) =
  [row | owner <- order, let ScopeInterfaceInput row _ = inputs Map.! owner]

scopeInterfaceEvidence :: ExactScope -> Map.Map InterfaceOwner ExactInterfaceEvidence
scopeInterfaceEvidence = inputEvidence . scopeInputs

inputEvidence :: AdmittedScopeInputs -> Map.Map InterfaceOwner ExactInterfaceEvidence
inputEvidence (AdmittedScopeInputs _ inputs _) = Map.map (\(ScopeInterfaceInput _ evidence) -> evidence) inputs

makeScopeInputs :: [InterfaceRow] -> Map.Map InterfaceOwner ExactInterfaceEvidence
  -> AdmittedPackageImports -> AdmittedScopeInputs
makeScopeInputs rows evidence roots = AdmittedScopeInputs (map rowOwner rows)
  (Map.fromList [(rowOwner row,ScopeInterfaceInput row (evidence Map.! rowOwner row)) | row <- rows]) roots

rowOwner :: InterfaceRow -> InterfaceOwner
rowOwner (iface,_,_) = (exactUnit iface,exactModule iface)

-- Batch additions before checking requirements: an original can depend on a
-- later new owner. No partially assembled scope reaches a compiler consumer.
extendExactScopeInputs :: ExactScope -> [(InterfaceRow,ExactInterfaceEvidence)]
  -> IO (Either String ExactScope)
extendExactScopeInputs scope offered = do
  checked <- try (do
    let AdmittedScopeInputs order previous roots = scopeInputs scope
    (inputs,added) <- foldM insert (previous,[]) offered
    let prospective = AdmittedScopeInputs (order ++ map (rowOwner . fst) added) inputs roots
    validateInputClosure (scopeProducerSha256 scope) prospective (scopeProducts scope)
    let fresh = makeScopeInputs (map fst added)
          (Map.fromList [(rowOwner row,evidence) | (row,evidence) <- added]) emptyAdmittedPackageImports
    withFileObservations (\observations -> revalidateScopeInputs observations fresh)
    union <- extendAdmittedPackageImports roots (map fst added) >>= either fail pure
    pure scope {scopeInputs=AdmittedScopeInputs (order ++ map (rowOwner . fst) added) inputs union})
    :: IO (Either IOException ExactScope)
  pure (either (Left . show) Right checked)
  where
    insert (selected,added) (row,evidence) = case Map.lookup (rowOwner row) selected of
      Nothing -> pure (Map.insert (rowOwner row) (ScopeInterfaceInput row evidence) selected,
        added ++ [(row,evidence)])
      Just old | old == ScopeInterfaceInput row evidence -> pure (selected,added)
      _ -> fail "exact input extension conflicts with an admitted original owner"

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
  where firstOfThree (value,_,_) = value

revalidateScopeInputs :: FileObservations -> AdmittedScopeInputs -> IO ()
revalidateScopeInputs observations (AdmittedScopeInputs _ inputs _) =
  forM_ (Map.elems inputs) $ \(ScopeInterfaceInput (iface,packages,packagesSha) evidence) -> do
    let interfaceBound = case evidence of
          ModuleInterfaceEvidence{} -> Just (32 * 1024 * 1024)
          LocalNativeDeclarationEvidence{} -> Just (32 * 1024 * 1024)
          _ -> Nothing
    observeSeal observations (exactPath iface) interfaceBound (exactSha256 iface)
      "canonical module interface or package imports changed"
    observeSeal observations packages (Just (4 * 1024 * 1024)) packagesSha
      "canonical module interface or package imports changed"
    case evidence of
      ModuleInterfaceEvidence proof -> observeSeal observations (canonicalCertificatePath proof)
        (Just (4 * 1024 * 1024)) (canonicalCertificateSha256 proof) "canonical module certificate changed"
      LocalNativeDeclarationEvidence native ->
        revalidateLocalFinalizedAdmissionWith observations (localNativeProof native) >>= either fail pure
      _ -> pure ()

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
scopeCanonicalInterfaces = Map.mapMaybe select . scopeInterfaceEvidence
  where
    select (ModuleInterfaceEvidence proof) = Just (ModuleInterfaceAdmission proof)
    select (LocalNativeDeclarationEvidence native) = Just (LocalInterfaceAdmission (localNativeProof native))
    select _ = Nothing

-- GHC executable imports select source originals only. Native authored
-- declarations retain their independent runtime execution authority.
scopeSourceOriginalInterfaces :: ExactScope -> Map.Map (String,String) CanonicalInterfaceAdmission
scopeSourceOriginalInterfaces = Map.mapMaybe select . scopeInterfaceEvidence
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

data ExactOriginalGroup = ExactOriginalGroup
  { originalOrdinal :: Word, originalBinders :: [SymbolIdentity]
  -- True imports need executable recovery; retained generations are boundaries.
  , originalGlobals :: [(SymbolIdentity, Bool)]
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
    then pure (Just scope {scopeExecutionGraphs=graphs,scopeExecutionOwners=Map.elems retainedReferences})
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
    { scopeLexical=Map.toAscList (Map.union (Map.fromList lexical) existing)
    , scopeSourceSelectedOwners=Set.union selectedKeys (scopeSourceSelectedOwners scope) }

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
  ExactActivationPreviewPurpose admission _ -> [previewInputInterface admission]

-- The exact scope separates the bounded metadata envelope from the independently
-- bounded original graph bytes. The request hash seals each path and digest.
readExactScope :: FilePath -> IO (Either String ExactScope)
readExactScope path = do
  timing <- readTimingEnabled
  timeDetailPhase timing "exact_scope" "read" $ do
    captured <- try (do
      unless (isAbsolute path) (fail "exact scope path must be absolute")
      bytes <- readBoundedFile path (4 * 1024 * 1024)
      (offered, descriptors, interfaceEvidence) <- timeDetailPhase timing "exact_scope" "decode" $ case deserialiseFromBytes decodeScope (BL.fromStrict bytes) of
        Left failure -> fail (show failure)
        Right (remaining, result)
          | BL.null remaining -> pure result
          | otherwise -> fail "exact scope has trailing bytes"
      graphs <- readExecutionSourceGraphs (RetainedScopeGraphFiles path) [] descriptors
      let OfferedScope producer semantic rows lexical products references purpose types = offered
      evidence <- validateInterfaceEvidence producer rows interfaceEvidence
      roots <- extendAdmittedPackageImports emptyAdmittedPackageImports rows >>= either fail pure
      let inputs = makeScopeInputs rows evidence roots
          sha = digest bytes
          scope = ExactScope path sha producer semantic inputs lexical products graphs references
            purpose types Set.empty
      validateInputClosure producer inputs products
      validatePreviewOriginalTarget scope evidence
      validateExecutionSources scope graphs
      when timing $ do
        _ <- evaluate (length sha)
        emitCount timing ("hash_bytes.scope_metadata." ++ sha) (fromIntegral (BS.length bytes))
      pure scope) :: IO (Either IOException ExactScope)
    pure (either (Left . show) Right captured)

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
-- retained subset. Core bytes are loaded only by their demanding recovery owner.
validateInterfaceEvidence
  :: String -> [InterfaceRow] -> [(InterfaceOwner,ParsedInterfaceEvidence)]
  -> IO (Map.Map InterfaceOwner ExactInterfaceEvidence)
validateInterfaceEvidence producer rows offered = do
  proofs <- validateCanonicalInterfaces producer rows
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
captureFinalizedSourceOriginals
  :: ExactCompilation -> [ModuleCandidate] -> (String,String) -> FinalizedModuleArtifacts
  -> DependencyEvidence -> IO (Map.Map (String,String) CanonicalInterfaceProof)
captureFinalizedSourceOriginals compilation accepted target finalized evidence = do
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
          result <- validateCanonicalProof producer interfaces key (candidateCertificatePath descriptor)
            (candidateCertificateSha256 descriptor) (Just (candidateCoreDescriptor descriptor))
          pure $ result >>= \proof -> do
            unless (candidateProducerSha256 candidate == producer
                && canonicalSourceSha256 proof == candidateSourceSha256 candidate
                && Map.keys (canonicalRequirements proof) == candidateInterfaceRequirements candidate)
              (Left "candidate canonical proof differs from source, producer or requirements")
            pure proof
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
validateCanonicalInterfacesWithValueSeals producer selectedInterfaces valueSeals descriptors = do
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
    bytes <- readBoundedFile (descriptorCertificatePath descriptor) (4 * 1024 * 1024)
    unless (digest bytes == descriptorCertificateSha256 descriptor)
      (fail "canonical module certificate changed")
    timing <- readTimingEnabled
    emitCount timing "exact_scope.certificate_decodes" 1
    certificate <- case deserialiseFromBytes (decodeCanonicalModuleCertificate (BS.length bytes)) (BL.fromStrict bytes) of
      Left failure -> fail (show failure)
      Right (remaining,value)
        | BL.null remaining -> pure value
        | otherwise -> fail "canonical module certificate has trailing bytes"
    unless (toStrictByteString (encodeCanonicalModuleCertificate certificate) == bytes)
      (fail "noncanonical module certificate encoding")
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
    interfaceBytes <- readBoundedFile (exactPath iface) (32 * 1024 * 1024)
    packageBytes <- readBoundedFile packages (4 * 1024 * 1024)
    unless (digest interfaceBytes == certificateInterface certificate
        && digest packageBytes == certificatePackages certificate)
      (fail "canonical module interface or package imports changed")
    pure (key, CanonicalInterfaceProof
      { proofCertificatePath = descriptorCertificatePath descriptor
      , proofCertificateSha256 = descriptorCertificateSha256 descriptor
      , proofCoreArtifact = descriptorCore descriptor
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
revalidateExactScope :: HscEnv -> ExactScope -> IO (Either String ())
revalidateExactScope env scope = do
  timing <- readTimingEnabled
  timeDetailPhase timing "exact_scope" "revalidate" $ do
    result <- try (withFileObservations $ \observations -> do
      observeSeal observations (scopeManifestPath scope) (Just (4 * 1024 * 1024))
        (scopeRequestSha256 scope) "exact scope request changed"
      validateInputClosure (scopeProducerSha256 scope) (scopeInputs scope) (scopeProducts scope)
      revalidateScopeInputs observations (scopeInputs scope)
      validatePreviewOriginalTarget scope (scopeInterfaceEvidence scope)
      let AdmittedScopeInputs _ _ roots = scopeInputs scope
      revalidateAdmittedPackageImports observations env roots >>= either fail pure
      manifest <- observeFile observations (scopeManifestPath scope) (Just (4 * 1024 * 1024))
      -- Preserve the proof marker; observed_file alone counts actual reads.
      emitCount timing ("hash_bytes.scope_revalidation." ++ scopeRequestSha256 scope)
        (fromIntegral (observedByteCount manifest))
      mapM_ (checkProduct observations) (scopeProducts scope)
      mapM_ (checkValue observations) (scopeValueInterfaces scope)
      forM_ (scopeActivationPreview scope) $ \admission ->
        observeSeal observations (exactPath (previewInputInterface admission) ++ ".packages") Nothing
          (previewInputPackagesSha256 admission) "activation preview input package interface changed")
      :: IO (Either IOException ())
    pure $ either (Left . show) Right result
  where
    checkValue observations value = observeSeal observations (exactPath value) Nothing
      (exactSha256 value) "checked value interface changed"
    checkProduct observations originalProduct = observeSeal observations (originalProductPath originalProduct) Nothing
      (originalProductSha256 originalProduct) "exact original product changed"

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
writeCheckedExactCompilation env compilation evidence = do
  selected <- either fail pure (extendSourceSelectedOriginals
    (compilationSourceSelection compilation) (compilationScope compilation))
  receipt <- captureExactCompilationReceipt compilation evidence
  revalidateExactScope env selected >>= either fail pure
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

decodeScope :: Decoder s (OfferedScope, [(String, FilePath)], [((String,String),ParsedInterfaceEvidence)])
decodeScope = do
  count <- decodeListLen
  magic <- string
  version <- string
  unless (magic == "TPEXACTSCOPE" && version == "9" && count == 9)
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
  products <- bounded 4096 $ do
    array 7
    originalProduct <- ExactProduct <$> nonempty <*> nonempty <*> digestField
      <*> digestField <*> digestField <*> absolute
      <*> bounded 65536 (do
        array 3
        ExactOriginalGroup <$> decodeWord <*> bounded 65536 identity
          <*> bounded 65536 (array 2 >> (,) <$> identity <*> decodeBool))
    unique "exact original ordinals" (map originalOrdinal (originalGroups originalProduct))
    let binders = concatMap originalBinders (originalGroups originalProduct)
    unique "exact original binders" binders
    unless (all (\binder -> T.unpack (symbolUnit binder) == originalUnit originalProduct
        && T.unpack (symbolModule binder) == originalModule originalProduct) binders)
      (fail "exact binder has another original owner")
    pure originalProduct
  let keys = [(exactUnit iface, exactModule iface) | (iface, _, _) <- interfaces]
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
  pure (OfferedScope producer semantic interfaces lexical products executionOwners
    checkedPurpose requestTypes, descriptors, interfaceEvidence)
  where
    decodePurpose authCount purpose = case purpose of
      "host-activation-preview3" -> do
        unless (authCount == 13) (fail "invalid activation preview admission")
        admissionDigest <- digestField
        generation <- decodeWord64
        budget <- decodeWord64
        templateSha <- digestField
        inputGeneration <- decodeWord64
        array 7
        binder <- ActivationPreviewInputMetadata <$> nonempty <*> decodeWord64 <*> nonempty
          <*> (nonempty >>= \tier -> if tier `elem` ["ForceData","RetainOpaque"] then pure tier else fail "invalid activation input tier")
          <*> string <*> nullable (array 3 >> (,,) <$> decodeString <*> decodeString <*> decodeString)
          <*> nullable (nonempty >>= \authority -> if authority `elem` ["JsonValue","Text","CommandJob"]
            then pure authority else fail "invalid activation input host authority")
        native <- signature
        witness <- decodeBytes
        unless (not (BS.null witness) && BS.length witness <= 4 * 1024 * 1024)
          (fail "activation input witness exceeds bound")
        either fail pure (validateCheckedTypeWitnessBytes witness)
        array 4
        owner <- nonempty
        interface <- ExactIfaceArtifact "main" owner <$> absolute <*> digestField <*> pure []
        packagesSha <- digestField
        unless (generation > 0 && inputGeneration > 0 && generation /= inputGeneration && budget <= fromIntegral (maxBound :: Int)
            && admissionDigest /= replicate 64 '0' && signatureKey native == "activation-input"
            && previewInputName binder == "sessionInput"
            && previewInputModule binder == owner
            && parseSessionModule owner == Just (SessionModule ValMod (Generation inputGeneration)))
          (fail "invalid activation preview input identity")
        originalInputs <- templateInterfaces
        unless (all ((== "main") . templateInterfaceUnit) originalInputs)
          (fail "activation preview original graph belongs to another home unit")
        array 2
        originalTarget <- (,) <$> nonempty <*> nonempty
        unless (originalTarget `elem` [(templateInterfaceUnit input,templateInterfaceModule input)
            | input <- originalInputs])
          (fail "activation preview original target leaves its sealed graph")
        paths <- includePaths
        pure (ExactActivationPreviewPurpose (ActivationPreviewAdmission admissionDigest generation budget
          templateSha inputGeneration binder native witness interface originalInputs originalTarget packagesSha) paths)
      "inspection1" -> do
        unless (authCount == 4) (fail "invalid inspection admission")
        injected <- bounded 4096 nonempty
        values <- valueInterfaces
        unique "inspection injected modules" injected
        validateInterfaces injected values
        paths <- includePaths
        pure (ExactInspectionPurpose values paths)
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
      let canonicalValue value = case parseSessionModule (exactModule value) of
            Just valueOwner -> smKind valueOwner == ValMod && sessionModuleString valueOwner == exactModule value
            Nothing -> False
      unless (all ((== "main") . exactUnit) values && all canonicalValue values
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
