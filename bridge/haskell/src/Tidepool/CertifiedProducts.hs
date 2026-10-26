{-# LANGUAGE OverloadedStrings #-}

module Tidepool.CertifiedProducts
  ( encodeReceiptIdentity
  , encodeCertifiedProducts, encodeCertifiedProductsWithOriginals
  , CertifiedProductKind(..), TargetCertificationContext
  , encodeCertifiedOriginalProducts, encodeCertifiedOriginalProductsWithPrior
  , ItemPackageSelection, selectItemPackage, encodeCertifiedItemProductsWithInputs
  , issuePackageLinkIndex
  , certifiedContextBinders, certifiedBinderInputClosure, certifiedOriginalExternalDemand, certifiedOriginalReference, validatePriorHomeGroups
  , resolvePackageGlobal, captureSelectedPackageWitnesses, homeInterfaceUsageOwners, readyNativeProductSourceSha256
  , PackageOccurrence, PackageLinks, PackageLinkIndex
  , emptyPackageLinks, emptyPackageLinkIndex, encodePackageLinks
  , validatePackageTargetRoots
  , OriginalUseClosureFailure(..), validateOriginalUseClosure, retainedInputClosureFromWitnesses
  , WitnessOwnerFor(..), CertifiedGroupFor(..), mergeCertifiedGroupObservations
  , NativeSourceSealFailure(..), mergeNativeSourceSeals, lookupNativeSourceSeal ) where

import Prelude hiding (product)
import Codec.CBOR.Encoding
  ( Encoding, encodeBool, encodeBytes, encodeListLen, encodeNull, encodeString, encodeWord
  , encodeWord64 )
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, try)
import Control.Monad (foldM, forM, forM_, unless, when)
import qualified Crypto.Hash.SHA256 as SHA256
import qualified Data.ByteString as BS
import Data.Foldable (fold)
import Data.Bits (shiftR)
import Data.List (find, intercalate, mapAccumL)
import qualified Data.Map.Strict as Map
import Data.Maybe (catMaybes)
import qualified Data.Set as Set
import Data.IORef (IORef, newIORef, modifyIORef', readIORef)
import qualified Data.Text as T
import qualified Data.Text.Encoding as TE
import Data.Word (Word32, Word64)
import GHC.Driver.Env (HscEnv, hsc_all_home_unit_ids)
import GHC.Unit.Module (Module, ModuleName, mkModule, mkModuleName, moduleUnit, moduleName)
import GHC.Unit.Module.ModIface (ModIface, mi_decls, mi_exports, mi_module)
import GHC.Unit.Module.Location (ml_hi_file)
import GHC.Unit.Types (stringToUnit, toUnitId)
import GHC.Iface.Syntax (IfaceDecl(..), ifaceDeclImplicitBndrs)
import GHC.Iface.Load (importDecl)
import GHC.Tc.Utils.Monad (initIfaceLoad)
import GHC.Types.Id (Id)
import GHC.Types.Name (Name, getName, nameModule_maybe, nameOccName, wiredInNameTyThing_maybe)
import GHC.Types.Name.Occurrence (OccName, mkVarOcc, isVarOcc)
import GHC.Types.Avail (availNames)
import GHC.Types.TyThing (TyThing(..), implicitTyThings)
import GHC.Data.Maybe (MaybeErr(..))
import Numeric (showHex)
import System.Directory (createDirectoryIfMissing)
import System.FilePath ((</>))

import Tidepool.ExecutionSource (WorkerExecutionSource, encodeWorkerExecutionSource)
import Tidepool.DependencyEvidence
  ( DependencyEvidence(..), DependencyModule(..), DependencySource(..)
  , ProductAvailability(..) )
import Tidepool.ExecutionSchema
  ( ConstructorDecl(..), ConstructorId(..), GlobalDecl(..), Group(..)
  , HeapBinding(..), HeapRhs(..), TopBinding(..), ProjectedGroup(..), ProjectedGroupBody(..)
  , ResultContract(..), RuntimeRep(..), Signature(..), SignatureId(..)
  , SymbolIdentity(..), WireProgram(..) )
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionProjection
  ( PreparedPackageTarget, packageTargetName, packageTargetProgram, packageTargetImports
  , packageTargetOriginalImports, packageTargetPriorHomeImports, packageTargetLocalHomeBindings )
import Tidepool.ExactScope
  ( ExactScope , scopeAvailableNativeGroupObservations, scopeNativeOriginalResolution, scopeProducts, scopeInterfaces, ExactProduct(..), ExactOriginalGroup(..), scopeValueInterfaces
  , WorldOriginalReference, worldOriginalReferenceOwner, worldOriginalReferenceBindings
  , CanonicalInterfaceProof, canonicalCertificateSha256, canonicalSourceSha256, originalGroupFromProjected
  , canonicalInterfaceInput, canonicalProofOriginalBody )
import Tidepool.NativeOriginalCensus qualified as Native
import Tidepool.OriginalProductRoots
  ( ReconciledOriginalProducts, reconcileOriginalProducts, unrecoveredExactProducts
  , PriorHomeGroup, PriorOriginalExternalDemand(..) )
import Tidepool.ModuleCandidates
  ( CandidateGlobal(..), CandidateGroup(..), ModuleCandidate(..) )
import Tidepool.PackageWitness
  ( PackageImportRoot(..), PackageImportEvidence(..), packageImportRoot, validatePackageImportRoot
  , decodeCapturedPackageImports )
import Tidepool.FatIface (readPackageInterface, readHomeInterface)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), OriginalInterfaceArtifacts, originalInterfaceSha256 )
import Tidepool.FinalizedModule (homeInterfaceUsageOwners)
import Tidepool.FinalizedModuleArtifacts
  ( FinalizedModuleArtifacts, encodeFinalizedModuleArtifacts, finalizedInterfaceSeals
  , finalizedLocalAdmissions, localFinalizedPackageEvidence )
import Tidepool.Timing (readTimingEnabled, emitCount)
import Tidepool.BoundedRead (readFileAtMost)
import Tidepool.RequestInputs (capturedRequestInputToken)
import Tidepool.ArtifactBytes (ArtifactBytes, artifactBytes, artifactSha256, artifactLength, captureArtifactBytes)

data CertifiedProductKind = OrdinaryProductFacts | SegmentOriginalFacts

-- One producer-issued package occurrence. Its identity is the row position,
-- never its envelope SHA: two rows may share an envelope only under distinct
-- ordered child histories, and their identity remains their distinct id.
data PackageOccurrence = PackageOccurrence
  { packageOccurrenceEnvelopeSha256 :: !BS.ByteString
  , packageOccurrenceUnit :: !T.Text
  , packageOccurrenceModule :: !T.Text
  , packageOccurrenceDefiningInterfaceSha256 :: !BS.ByteString
  , packageOccurrenceDefiningVersionSha256 :: !BS.ByteString
  , packageOccurrenceImports :: [Word32]
  } deriving (Eq, Show)

-- The complete frozen linkage manifest. Target roots name the original
-- projection key. HOME roots qualify that key once more by unit, module and
-- original ordinal so equal module names from separate projections never merge.
-- Original-use roots retain the occurrence choices for external native demand;
-- Rust joins these exact uses to its already acquired group and slot obligations.
data PackageLinks = PackageLinks
  { packageLinksStageId :: !BS.ByteString
  , packageLinksOccurrences :: [PackageOccurrence]
  , packageLinksTargetRoots :: Map.Map T.Text [Word32]
  , packageLinksHomeRoots :: Map.Map (T.Text,T.Text,T.Text,Word) [Word32]
  , packageLinksOriginalRoots :: Map.Map (T.Text,WorldOriginalReference,SymbolIdentity) [(SymbolIdentity,Word32)]
  , packageLinksProducts :: [(FilePath, BS.ByteString)]
  } deriving (Eq, Show)

emptyPackageLinks :: PackageLinks
emptyPackageLinks = PackageLinks (packageLinkIndexStageId emptyPackageLinkIndex)
  [] Map.empty Map.empty Map.empty []

-- The stage owns one occurrence table. Each ordinary/item consumer retains its
-- ordered links, and fresh HOME groups retain the links issued by their actual
-- product projection.
data PackageLinkIndex = PackageLinkIndex
  { packageLinkIndexStageId :: !BS.ByteString
  , packageLinkIndexOccurrences :: [PackageOccurrence]
  , packageLinkIndexTargetOwners :: Map.Map T.Text (Map.Map SymbolIdentity Word32)
  , packageLinkIndexTargetRoots :: Map.Map T.Text [Word32]
  , packageLinkIndexHomeRoots :: Map.Map (T.Text,T.Text,T.Text,Word) [Word32]
  , packageLinkIndexExpectedHomeRoots :: Set.Set (T.Text,T.Text,T.Text,Word)
  , packageLinkIndexPriorHomeGroups :: Map.Map T.Text (Map.Map PriorHomeGroup [SymbolIdentity])
  , packageLinkIndexOriginalRoots :: Map.Map (T.Text,WorldOriginalReference,SymbolIdentity) [(SymbolIdentity,Word32)]
  , packageLinkIndexProducts :: [(FilePath, BS.ByteString)]
  } deriving (Eq, Show)

-- Original-only captures still seal their empty stage through the package issuer.
emptyPackageLinkIndex :: PackageLinkIndex
emptyPackageLinkIndex = case issuePackageLinkIndex [] Map.empty Map.empty Map.empty
    Set.empty Map.empty Map.empty [] of
  Right issued -> issued
  Left reason -> error ("empty package stage issuance failed: " ++ reason)

issuePackageLinkIndex
  :: [(BS.ByteString, T.Text, T.Text, BS.ByteString, BS.ByteString, [Word32])]
  -> Map.Map T.Text (Map.Map SymbolIdentity Word32)
  -> Map.Map T.Text [Word32]
  -> Map.Map (T.Text,T.Text,T.Text,Word) [Word32]
  -> Set.Set (T.Text,T.Text,T.Text,Word)
  -> Map.Map T.Text (Map.Map PriorHomeGroup [SymbolIdentity])
  -> Map.Map (T.Text,WorldOriginalReference,SymbolIdentity) [(SymbolIdentity,Word32)]
  -> [(FilePath, BS.ByteString)]
  -> Either String PackageLinkIndex
issuePackageLinkIndex occurrenceFacts owners targetRoots homeRoots expected priorHomeGroups originalRoots products
  | Map.keysSet homeRoots /= expected =
      Left "fresh HOME package roots are incomplete or crossed"
  | not (Map.keysSet priorHomeGroups `Set.isSubsetOf` Map.keysSet owners)
      || not (Set.fromList
        [(target,unit,name,ordinal)
        | (target,groups) <- Map.toAscList priorHomeGroups
        , ((unit,name,_,ordinal),_) <- Map.toAscList groups] `Set.isSubsetOf` expected) =
      Left "prior HOME package roots name an unissued target or group"
  | any (\(envelope,unit,name,interface,version,_) ->
      BS.length envelope /= 32
        || BS.length interface /= 32
        || BS.length version /= 32
        || T.null unit
        || T.null name) occurrenceFacts =
      Left "package occurrence contains malformed immutable facts"
  | Map.keysSet targetRoots /= Map.keysSet owners =
      Left "package target roots do not cover their target owners"
  | any (any (>= fromIntegral (length occurrenceFacts))) (Map.elems owners) =
      Left "package target owner is out of range"
  | any (any (>= fromIntegral (length occurrenceFacts))) (Map.elems targetRoots) =
      Left "package target root is out of range"
  | any (\(_,_,_,_,_,imports) ->
      any (>= fromIntegral (length occurrenceFacts)) imports) occurrenceFacts =
      Left "package occurrence import is out of range"
  | any (any (>= fromIntegral (length occurrenceFacts))) (Map.elems homeRoots) =
      Left "fresh HOME package root is out of range"
  | not (all (\(target,_,_,_) -> Map.member target owners) (Set.toList expected)) =
      Left "fresh HOME package root names an unissued target"
  | any (\((target,_,_),roots) -> Map.notMember target owners
      || map fst roots /= Set.toAscList (Set.fromList (map fst roots))
      || any ((>= fromIntegral (length occurrenceFacts)) . snd) roots)
      (Map.toAscList originalRoots) =
      Left "Original use package roots are invalid or name an unissued target"
  | any (\(_, seal) -> BS.length seal /= 32) products =
      Left "package product seal is malformed"
  | otherwise =
      let occurrences = [PackageOccurrence envelope unit name interface version imports
                        | (envelope,unit,name,interface,version,imports) <- occurrenceFacts]
          stageId = packageStageIdentity occurrences targetRoots homeRoots originalRoots products
      in Right (PackageLinkIndex stageId occurrences owners targetRoots homeRoots expected priorHomeGroups originalRoots products)

encodePackageLinks :: PackageLinks -> Encoding
encodePackageLinks links = array
  [ encodeWord 4
  , encodeBytes (packageLinksStageId links)
  , list encodeOccurrenceRow (zip [0 :: Word32 ..] (packageLinksOccurrences links))
  , list encodeTargetRoots (Map.toAscList (packageLinksTargetRoots links))
  , list encodeHomeRoots (Map.toAscList (packageLinksHomeRoots links))
  , list encodeOriginalUseRoot (Map.toAscList (packageLinksOriginalRoots links))
  , list encodeProduct (packageLinksProducts links)
  ]
  where
    encodeOccurrenceRow (index, occurrence) = array
      [ encodeWord (fromIntegral index)
      , encodeBytes (packageOccurrenceEnvelopeSha256 occurrence)
      , encodeString (packageOccurrenceUnit occurrence)
      , encodeString (packageOccurrenceModule occurrence)
      , encodeBytes (packageOccurrenceDefiningInterfaceSha256 occurrence)
      , encodeBytes (packageOccurrenceDefiningVersionSha256 occurrence)
      , list encodeLink (packageOccurrenceImports occurrence)
      ]
    encodeTargetRoots (target, roots) = array
      [encodeString target, list encodeLink roots]
    encodeHomeRoots ((target, unit, name, ordinal), roots) = array
      [array [encodeString target, encodeString unit, encodeString name, encodeWord ordinal]
      , list encodeLink roots]
    encodeProduct (path, seal) =
      array [encodeString (T.pack path), encodeBytes seal]

encodeLink :: Word32 -> Encoding
encodeLink occurrence =
  array [encodeString "local", encodeWord (fromIntegral occurrence)]

encodeOriginalUseRoot
  :: ((T.Text,WorldOriginalReference,SymbolIdentity),[(SymbolIdentity,Word32)]) -> Encoding
encodeOriginalUseRoot ((target,reference,binder),roots) = array
  [ array [encodeString target,encodeOriginalUse reference binder]
  , list (\(identity,occurrence) -> array [encodeReceiptIdentity identity,encodeLink occurrence]) roots
  ]

encodeOriginalUse :: WorldOriginalReference -> SymbolIdentity -> Encoding
encodeOriginalUse reference binder =
  let (unit,name) = worldOriginalReferenceOwner reference
      (interface,original) = worldOriginalReferenceBindings reference
      binding (artifact,scope) = array [encodeBytes artifact,encodeBytes scope]
  in array [encodeString unit,encodeString name,binding interface,binding original,encodeReceiptIdentity binder]

packageStageIdentity
  :: [PackageOccurrence]
  -> Map.Map T.Text [Word32]
  -> Map.Map (T.Text,T.Text,T.Text,Word) [Word32]
  -> Map.Map (T.Text,WorldOriginalReference,SymbolIdentity) [(SymbolIdentity,Word32)]
  -> [(FilePath, BS.ByteString)]
  -> BS.ByteString
packageStageIdentity occurrences targetRoots homeRoots originalRoots products = SHA256.hash
  (toStrictByteString (array
    [ encodeWord 2
    , list encodeOccurrence occurrences
    , list encodeTarget (Map.toAscList targetRoots)
    , list encodeHome (Map.toAscList homeRoots)
    , list encodeOriginalUseRoot (Map.toAscList originalRoots)
    , list encodeProduct products
    ]))
  where
    encodeOccurrence occurrence = array
      [ encodeBytes (packageOccurrenceEnvelopeSha256 occurrence)
      , encodeString (packageOccurrenceUnit occurrence)
      , encodeString (packageOccurrenceModule occurrence)
      , encodeBytes (packageOccurrenceDefiningInterfaceSha256 occurrence)
      , encodeBytes (packageOccurrenceDefiningVersionSha256 occurrence)
      , list encodeLink (packageOccurrenceImports occurrence)
      ]
    encodeTarget (target, roots) = array
      [encodeString target, list encodeLink roots]
    encodeHome ((target,unit,name,ordinal), roots) = array
      [array [encodeString target,encodeString unit,encodeString name,encodeWord ordinal]
      , list encodeLink roots]
    encodeProduct (path, seal) = array [encodeString (T.pack path),encodeBytes seal]

validatePackageTargetRoots
  :: Map.Map T.Text [Word32]
  -> Map.Map T.Text [Word32]
  -> Either String ()
validatePackageTargetRoots expected produced =
  if expected == produced
    then Right ()
    else Left "target package roots changed after stage identity issuance"

data TargetCertificationContext = TargetCertificationContext
  { targetBinderOwners :: Map.Map SymbolIdentity BinderOwner
  , targetOriginalReferences :: Map.Map SymbolIdentity WorldOriginalReference
  , targetHomeModules :: Set.Set (T.Text,T.Text)
  , targetModulePackages :: Map.Map (T.Text,T.Text) (FilePath,T.Text)
  , targetBinderAvailability :: Set.Set SymbolIdentity
  , targetNativeOwners :: Set.Set (T.Text,T.Text)
  , targetInterfaceSeals :: Map.Map (T.Text,T.Text) T.Text
  , targetGroupWitnesses :: Map.Map BinderOwner (CertifiedGroupFor WorldOriginalReference)
  , targetNativeSourceSeals :: NativeSourceSeals
  , targetPackageLinkIndex :: PackageLinkIndex
  }

-- These are observations, not a source of compilation or certificate authority.
data OriginalUseClosureFailure reference
  = OriginalUseMissingLocalBinding SymbolIdentity
  | OriginalUseMissingGroup BinderOwner
  | OriginalUseSourceOwnerMismatch SymbolIdentity BinderOwner
  | OriginalUseKeysDiffer Int Int [(reference,SymbolIdentity)] [(reference,SymbolIdentity)]
  deriving (Eq, Show)

validateOriginalUseClosure
  :: Ord reference => Map.Map SymbolIdentity BinderOwner
  -> Map.Map BinderOwner (CertifiedGroupFor reference)
  -> Set.Set SymbolIdentity
  -> [GlobalWitnessFor reference]
  -> Set.Set (reference,SymbolIdentity)
  -> Either (OriginalUseClosureFailure reference) ()
validateOriginalUseClosure binders groups localBindings witnesses declared = do
  roots <- forM (Set.toAscList localBindings) $ \identity ->
    maybe (Left (OriginalUseMissingLocalBinding identity)) Right (Map.lookup identity binders)
  actual <- visit Set.empty Set.empty roots witnesses
  unless (actual == declared)
    (Left (OriginalUseKeysDiffer (Set.size (actual `Set.difference` declared))
      (Set.size (declared `Set.difference` actual))
      (take 3 (Set.toAscList (actual `Set.difference` declared)))
      (take 3 (Set.toAscList (declared `Set.difference` actual)))))
  where
    visit seen uses pending ((identity,_,_,_,owner):rest) = case owner of
      OriginalBindingOwner reference -> visit seen (Set.insert (reference,identity) uses) pending rest
      SourceOwner unit name version ordinal -> do
        let coordinate = (unit,name,version,ordinal)
        group <- maybe (Left (OriginalUseMissingGroup coordinate)) Right (Map.lookup coordinate groups)
        unless (identity `elem` certifiedGroupBinders group)
          (Left (OriginalUseSourceOwnerMismatch identity coordinate))
        visit seen uses (coordinate:pending) rest
      _ -> visit seen uses pending rest
    visit _ uses [] [] = Right uses
    visit seen uses (coordinate:pending) []
      | coordinate `Set.member` seen = visit seen uses pending []
      | otherwise = do
          imports <- maybe (Left (OriginalUseMissingGroup coordinate)) Right (Map.lookup coordinate groups)
          visit (Set.insert coordinate seen) uses pending (certifiedGroupGlobals imports)

validateTargetOriginalUses :: T.Text -> TargetCertificationContext -> Set.Set SymbolIdentity
  -> [GlobalWitness] -> Set.Set (WorldOriginalReference,SymbolIdentity) -> Either String ()
validateTargetOriginalUses target context roots witnesses declared =
  case validateOriginalUseClosure (targetBinderOwners context) (targetGroupWitnesses context)
      roots witnesses declared of
    Right () -> Right ()
    Left failure -> Left $ "Original use target " ++ take 160 (T.unpack target) ++ ": " ++ case failure of
      OriginalUseMissingLocalBinding identity -> "closure lacks local HOME binding " ++ take 320 (show identity)
      OriginalUseMissingGroup coordinate -> "closure lacks certified HOME group " ++ take 320 (show coordinate)
      OriginalUseSourceOwnerMismatch identity expected -> "source dependency is absent from its exact destination group: "
        ++ take 640 (show (identity,expected))
      OriginalUseKeysDiffer missing extra missingSample extraSample -> "keys differ from final executable closure: missing="
        ++ show missing ++ "; extra=" ++ show extra
        ++ "; missing sample=" ++ take 640 (show missingSample)
        ++ "; extra sample=" ++ take 640 (show extraSample)

-- Census observations retain every field of an authenticated native global.
-- They add context topology only; emitted module and target rows remain local.
nativeGlobalWitness :: ExactScope -> ExactProduct -> Word -> Word
  -> Native.NativeGlobalObservation -> Either String GlobalWitness
nativeGlobalWitness scope product ordinal index global = do
  owner <- case Native.nativeGlobalImport global of
      Native.NativeSourceImport unit name version ordinal _ ->
        Right (SourceOwner (T.pack unit) (T.pack name) (Just (T.pack version)) ordinal)
      Native.NativeRetainedImport _ generation -> Right (RetainedOwner generation)
      Native.NativePackageImport unit name seal _ -> Right (PackageOwner (T.pack unit) (T.pack name) (T.pack seal))
      Native.NativeOriginalBindingImport{} -> do
        (unit,name,version,targetOrdinal) <- scopeNativeOriginalResolution scope product ordinal index global
        Right (SourceOwner (T.pack unit) (T.pack name) (Just (T.pack version)) targetOrdinal)
  pure (Native.nativeGlobalIdentity global, Native.nativeGlobalRep global,
    Native.nativeGlobalSignature global, Native.nativeGlobalEvaluated global, owner)

mergeCertifiedGroupObservations :: Eq reference => Map.Map BinderOwner (CertifiedGroupFor reference)
  -> Map.Map BinderOwner (CertifiedGroupFor reference)
  -> Either String (Map.Map BinderOwner (CertifiedGroupFor reference))
mergeCertifiedGroupObservations left right = Map.foldlWithKey' insert (Right left) right
  where
    insert result coordinate witnesses = do
      groups <- result
      case Map.lookup coordinate groups of
        Just previous | previous /= witnesses -> Left
          ("certified native group observations conflict: " ++ show coordinate)
        _ -> Right (Map.insert coordinate witnesses groups)

validateSourceWitnessOwner :: TargetCertificationContext -> SymbolIdentity -> BinderOwner -> Either String ()
validateSourceWitnessOwner context identity coordinate = do
  group <- maybe (Left ("source dependency lacks its exact certified group: " ++ show coordinate)) Right
    (Map.lookup coordinate (targetGroupWitnesses context))
  unless (identity `elem` certifiedGroupBinders group)
    (Left ("source dependency is absent from its exact certified group: " ++ show (identity,coordinate)))

certifiedGroupWitnesses :: TargetCertificationContext -> BinderOwner -> Either String [GlobalWitness]
certifiedGroupWitnesses context coordinate = do
  group <- maybe (Left ("closure lacks certified native group: " ++ show coordinate)) Right
    (Map.lookup coordinate (targetGroupWitnesses context))
  let witnesses = certifiedGroupGlobals group
  forM_ witnesses $ \(identity,_,_,_,owner) -> case owner of
    SourceOwner unit name version ordinal ->
      validateSourceWitnessOwner context identity (unit,name,version,ordinal)
    _ -> Right ()
  pure witnesses

-- A checked item's exact program and selected roots, bound to the immutable
-- stage certificate that owns the occurrence table. The item does not copy that
-- table; consumers authenticate this reference before selecting its closure.
data ItemPackageSelection = ItemPackageSelection
  { itemSelectionProgram :: WireProgram
  , itemSelectionStageSha256 :: !BS.ByteString
  , itemSelectionTarget :: !T.Text
  , itemSelectionTargetRoots :: [Word32]
  , itemSelectionHomeRoots :: Map.Map (T.Text,T.Text,T.Text,Word) [Word32]
  , itemSelectionLocalHomeBindings :: Set.Set SymbolIdentity
  , itemSelectionOriginalRoots :: Map.Map (T.Text,WorldOriginalReference,SymbolIdentity) [(SymbolIdentity,Word32)]
  }

selectItemPackage :: TargetCertificationContext -> PreparedPackageTarget
  -> Either String ItemPackageSelection
selectItemPackage context target = do
  let index = targetPackageLinkIndex context
      name = packageTargetName target
      owners = Map.findWithDefault Map.empty name (packageLinkIndexTargetOwners index)
      expected = packageTargetImports target
  selectedRoots <- forM expected $ \identity ->
    maybe (Left ("item target package import lacks occurrence: " ++ show identity))
      Right
      (Map.lookup identity owners)
  let originalRoots = Map.filterWithKey (\(targetName,_,_) _ -> targetName == name)
        (packageLinkIndexOriginalRoots index)
      originalIdentities = Map.fromList
        [ ((reference,binder),map fst roots)
        | ((_,reference,binder),roots) <- Map.toAscList originalRoots ]
  unless (originalIdentities == packageTargetOriginalImports target)
    (Left "item Original use package roots differ from its issued target")
  unless (Map.findWithDefault Map.empty name (packageLinkIndexPriorHomeGroups index)
      == packageTargetPriorHomeImports target)
    (Left "item prior HOME package roots differ from its issued target")
  pure ItemPackageSelection
    { itemSelectionProgram = packageTargetProgram target
    , itemSelectionStageSha256 = packageLinkIndexStageId index
    , itemSelectionTarget = name
    , itemSelectionTargetRoots = selectedRoots
    , itemSelectionHomeRoots = Map.fromList
        [ ((homeTarget,unit,moduleName,original), roots)
        | ((homeTarget,unit,moduleName,original), roots) <-
            Map.toAscList (packageLinkIndexHomeRoots index)
        , homeTarget == name ]
    , itemSelectionLocalHomeBindings = packageTargetLocalHomeBindings target
    , itemSelectionOriginalRoots = originalRoots
    }

certifiedContextBinders :: TargetCertificationContext -> Set.Set SymbolIdentity
certifiedContextBinders = targetBinderAvailability

certifiedOriginalReference :: TargetCertificationContext -> SymbolIdentity -> Maybe WorldOriginalReference
certifiedOriginalReference context identity = Map.lookup identity (targetOriginalReferences context)

-- Publication roots are real runtime binders, so every one must resolve to
-- the same certified group. Availability cannot silently narrow publication.
certifiedBinderInputClosure :: TargetCertificationContext -> Set.Set SymbolIdentity
  -> Either String (Set.Set (SymbolIdentity,Word64))
certifiedBinderInputClosure context roots = do
  witnesses <- forM (Set.toAscList roots) $ \identity -> do
    coordinate <- maybe (Left ("published runtime binder lacks its certified native owner: " ++ show identity)) Right
      (Map.lookup identity (targetBinderOwners context))
    certifiedGroupWitnesses context coordinate
  retainedInputClosure context (concat witnesses)

-- Read the already-certified group topology without reconstructing an outline
-- or discarding the source owner's version. Unknown native binders fail closed;
-- unrelated owners can still be handled by the current package selection.
certifiedOriginalExternalDemand :: TargetCertificationContext -> SymbolIdentity
  -> Either String (Maybe PriorOriginalExternalDemand)
certifiedOriginalExternalDemand context identity = case Map.lookup identity (targetBinderOwners context) of
  Nothing
    | (symbolUnit identity,symbolModule identity) `Set.member` targetNativeOwners context ->
        Left "prior native package demand lacks its certified binder"
    | otherwise -> Right Nothing
  Just coordinate -> Just <$> visit Set.empty Set.empty Map.empty [coordinate]
  where
    visit _ packages groups [] = Right (PriorOriginalExternalDemand packages groups)
    visit seen packages groups (coordinate:pending)
      | coordinate `Set.member` seen = visit seen packages groups pending
      | otherwise = do
          witnesses <- certifiedGroupWitnesses context coordinate
          let next = [(childUnit,childName,childVersion,childOrdinal)
                | (_,_,_,_,SourceOwner childUnit childName childVersion childOrdinal) <- witnesses]
              leaves = Set.fromList [symbol
                | (symbol,_,_,_,owner) <- witnesses
                , case owner of PackageOwner _ _ _ -> True; OriginalBindingOwner _ -> True; _ -> False]
              direct = directPackageIdentities witnesses
          visit (Set.insert coordinate seen) (Set.union packages leaves)
            (Map.insert coordinate direct groups) (next ++ pending)

-- Reached prior HOME coordinates stay bound to the immediate predecessor's
-- exact witnesses, including the order of each group's direct package globals.
validatePriorHomeGroups :: TargetCertificationContext
  -> Map.Map PriorHomeGroup [SymbolIdentity] -> Either String ()
validatePriorHomeGroups context groups = forM_ (Map.toAscList groups) $ \((unit,name,version,ordinal),identities) -> do
  witnesses <- certifiedGroupWitnesses context (unit,name,version,ordinal)
  unless (identities == directPackageIdentities witnesses)
    (Left "prior HOME package identities differ from its certified group")

directPackageIdentities :: [GlobalWitness] -> [SymbolIdentity]
directPackageIdentities witnesses =
  [symbol | (symbol,_,_,_,PackageOwner _ _ _) <- witnesses]

data ProductOrigin = FreshProduct | CachedProduct | RetainedCoreProduct deriving (Eq)

renderProductOrigin :: ProductOrigin -> T.Text
renderProductOrigin FreshProduct = "fresh"
renderProductOrigin CachedProduct = "cached"
renderProductOrigin RetainedCoreProduct = "retained-core"

data Product = Product
  { productOrigin :: ProductOrigin
  , productUnit :: T.Text
  , productModule :: T.Text
  , productVersion :: Maybe T.Text
  , productSourceSha :: T.Text
  , productIfaceSha :: T.Text
  , productBytesSha :: T.Text
  , productEvidenceSha :: T.Text
  , productInterfaces :: [(T.Text, T.Text, T.Text)]
  , productGroups :: [CandidateGroup]
  }

type BinderOwner = PriorHomeGroup
type PackageWitness = (T.Text, T.Text, FilePath, T.Text)

-- Keep one counterexample, rather than retaining or rendering whole inventories.
-- These causes are private to certification; the worker keeps its existing error
-- transport and renders the selected cause only when returning a refusal.
data OriginalInventoryRefusal
  = ConflictingInterfaceSeals (T.Text,T.Text) Int [T.Text]
  | PriorInterfaceSealConflict (T.Text,T.Text) T.Text T.Text
  | MissingFreshSourceEvidence (T.Text,T.Text)
  | MissingFreshSignature (T.Text,T.Text) Word SymbolIdentity Word32 Int
  | DuplicateOriginalBinder SymbolIdentity BinderOwner BinderOwner
  | PriorBinderOwnerConflict SymbolIdentity BinderOwner BinderOwner
  | PriorOriginalReferenceConflict SymbolIdentity WorldOriginalReference WorldOriginalReference
  | OriginalBinderReferenceConflict SymbolIdentity BinderOwner WorldOriginalReference
  | PriorNativeOwnerConflict (T.Text,T.Text) (Maybe BinderOwner) (Maybe BinderOwner)

renderOriginalInventoryRefusal :: OriginalInventoryRefusal -> String
renderOriginalInventoryRefusal refusal = limitInventoryDiagnostic inventoryDiagnosticCharacters $ case refusal of
  ConflictingInterfaceSeals owner count seals ->
    "conflicting original interface owner seals: owner=" ++ renderOwner owner
      ++ "; expected count=1; observed count=" ++ show count
      ++ "; first seals=[" ++ intercalate "," (map component (take 2 seals)) ++ "]"
  PriorInterfaceSealConflict owner expected observed ->
    "prior original interface owner seal changed: owner=" ++ renderOwner owner
      ++ "; expected=" ++ component expected ++ "; observed=" ++ component observed
  MissingFreshSourceEvidence owner ->
    "fresh original lacks ready source evidence: owner=" ++ renderOwner owner
  MissingFreshSignature owner ordinal binder index count ->
    "fresh original signature is out of range: owner=" ++ renderOwner owner
      ++ "; group=" ++ show ordinal ++ "; binder=" ++ renderBinder binder
      ++ "; signature=" ++ show index ++ "; signature count=" ++ show count
  DuplicateOriginalBinder binder first second ->
    "duplicate current original binder: binder=" ++ renderBinder binder
      ++ "; first=" ++ renderCoordinate first ++ "; second=" ++ renderCoordinate second
  PriorBinderOwnerConflict binder expected observed ->
    "prior original binder owner changed: binder=" ++ renderBinder binder
      ++ "; expected=" ++ renderCoordinate expected ++ "; observed=" ++ renderCoordinate observed
  PriorOriginalReferenceConflict binder expected observed ->
    "prior original reference changed: binder=" ++ renderBinder binder
      ++ "; expected=" ++ renderReference expected ++ "; observed=" ++ renderReference observed
  OriginalBinderReferenceConflict binder owner reference ->
    "current original binder collides with an original reference: binder=" ++ renderBinder binder
      ++ "; owner=" ++ renderCoordinate owner ++ "; reference=" ++ renderReference reference
  PriorNativeOwnerConflict owner expected observed ->
    "native original owner was already certified by a prior stage: owner=" ++ renderOwner owner
      ++ "; prior group=" ++ maybe "Nothing" renderCoordinate expected
      ++ "; current group=" ++ maybe "Nothing" renderCoordinate observed
  where
    component :: Show a => a -> String
    component = limitInventoryDiagnostic inventoryDiagnosticComponentCharacters . show
    renderOwner :: (T.Text,T.Text) -> String
    renderOwner (unit,name) = "(" ++ component unit ++ "," ++ component name ++ ")"
    renderCoordinate :: BinderOwner -> String
    renderCoordinate (unit,name,version,ordinal) =
      "(" ++ component unit ++ "," ++ component name
        ++ "," ++ maybe "Nothing" component version ++ "," ++ show ordinal ++ ")"
    renderBinder :: SymbolIdentity -> String
    renderBinder identity = "(" ++ component (symbolUnit identity)
      ++ "," ++ component (symbolModule identity)
      ++ "," ++ component (symbolNamespace identity)
      ++ "," ++ component (symbolOccurrence identity)
      ++ "," ++ maybe "Nothing" component (symbolRecordParent identity) ++ ")"
    renderReference :: WorldOriginalReference -> String
    renderReference reference =
      let ((interface,interfaceScope),(original,originalScope)) = worldOriginalReferenceBindings reference
      in "(" ++ renderOwner (worldOriginalReferenceOwner reference)
        ++ ",interface=" ++ renderBytes interface ++ ",interface scope=" ++ renderBytes interfaceScope
        ++ ",original=" ++ renderBytes original ++ ",original scope=" ++ renderBytes originalScope ++ ")"
    renderBytes :: BS.ByteString -> String
    renderBytes = limitInventoryDiagnostic inventoryDiagnosticComponentCharacters
      . concatMap (\byte -> (if byte < 16 then "0" else "") ++ showHex byte "") . BS.unpack

-- These are presentation budgets, never limits on valid owners or binders.
-- Per-component limits retain both sides of a comparison, including version and
-- ordinal after long authored names; the total bounds future renderer changes.
-- At most four UTF-8 bytes are emitted per retained character.
inventoryDiagnosticCharacters, inventoryDiagnosticComponentCharacters :: Int
inventoryDiagnosticCharacters = 4096
inventoryDiagnosticComponentCharacters = 128

limitInventoryDiagnostic :: Int -> String -> String
limitInventoryDiagnostic limit value = case splitAt limit value of
  (prefix,[]) -> prefix
  (prefix,_) -> take (limit - length marker) prefix ++ marker
  where marker = "..."

duplicateOriginalBinder :: [(SymbolIdentity,BinderOwner)] -> Maybe OriginalInventoryRefusal
duplicateOriginalBinder = visit Map.empty
  where
    visit _ [] = Nothing
    visit seen ((binder,owner):rest) = case Map.lookup binder seen of
      Just previous -> Just (DuplicateOriginalBinder binder previous owner)
      Nothing -> visit (Map.insert binder owner seen) rest

-- An owner has the witnessed binder already carried by its global. Coordinates
-- remain independent: a source group's owner need not be the binder's module.
data OwnerCoordinate
  = SourceCoordinate T.Text T.Text (Maybe T.Text)
  | PackageCoordinate T.Text T.Text T.Text
  | OriginalBindingCoordinate WorldOriginalReference
  deriving (Eq, Ord)

data WitnessOwnerFor reference
  = SourceOwner T.Text T.Text (Maybe T.Text) Word
  | RetainedOwner Word64
  | OriginalBindingOwner reference
  | PackageOwner T.Text T.Text T.Text
  deriving (Eq, Ord)

type WitnessOwner = WitnessOwnerFor WorldOriginalReference
type GlobalWitnessFor reference = (SymbolIdentity, RuntimeRep, Maybe Signature, Bool, WitnessOwnerFor reference)
type GlobalWitness = GlobalWitnessFor WorldOriginalReference

-- The exact group owns its binding membership and ordered dependency facts
-- together. Current symbol selection is used for roots, not historical edges.
data CertifiedGroupFor reference = CertifiedGroup
  { certifiedGroupBinders :: [SymbolIdentity]
  , certifiedGroupGlobals :: [GlobalWitnessFor reference]
  } deriving (Eq)

ownerCoordinate :: WitnessOwner -> Maybe OwnerCoordinate
ownerCoordinate (SourceOwner unit name version _) = Just (SourceCoordinate unit name version)
ownerCoordinate (PackageOwner unit name sha) = Just (PackageCoordinate unit name sha)
ownerCoordinate (RetainedOwner _) = Nothing
ownerCoordinate (OriginalBindingOwner reference) = Just (OriginalBindingCoordinate reference)

encodeCoordinate :: OwnerCoordinate -> Encoding
encodeCoordinate (SourceCoordinate unit name version) = array
  [encodeString "source", encodeString unit, encodeString name
  , maybe encodeNull encodeString version]
encodeCoordinate (PackageCoordinate unit name sha) = array
  [encodeString "package", encodeString unit, encodeString name, encodeString sha]
encodeCoordinate (OriginalBindingCoordinate reference) = encodeOriginalBinding reference

encodeOriginalBinding :: WorldOriginalReference -> Encoding
encodeOriginalBinding reference =
  let (unit,name) = worldOriginalReferenceOwner reference
      ((interface,interfaceScope),(original,originalScope)) = worldOriginalReferenceBindings reference
  in array [encodeString "original-binding",encodeString unit,encodeString name
    ,array [encodeBytes interface,encodeBytes interfaceScope]
    ,array [encodeBytes original,encodeBytes originalScope]]

encodeWitness :: Map.Map OwnerCoordinate Word -> GlobalWitness -> Encoding
encodeWitness coordinates (identity, rep, signature, evaluated, owner) = array
  [encodeReceiptIdentity identity, encodeRep rep, maybe encodeNull encodeSignature signature
  , encodeBool evaluated, encodeCompactOwner coordinates owner]

encodeCompactOwner :: Map.Map OwnerCoordinate Word -> WitnessOwner -> Encoding
encodeCompactOwner coordinates owner = case owner of
  SourceOwner unit name version ordinal -> array
    [encodeString "source", index (SourceCoordinate unit name version), encodeWord ordinal]
  RetainedOwner generation -> array [encodeString "retained", encodeWord64 generation]
  OriginalBindingOwner reference -> array [encodeWord 3,index (OriginalBindingCoordinate reference)]
  PackageOwner unit name sha -> array
    [encodeString "package", index (PackageCoordinate unit name sha)]
  where index coordinate = encodeWord (coordinates Map.! coordinate)

-- The producer's group inventory is only a suggestion. Rust compares every
-- emitted row with the original sidecar bytes before admitting any product.
encodeCertifiedProducts
  :: HscEnv -> WorkerExecutionSource -> Map.Map ModuleName ModIface -> FinalizedModuleArtifacts -> [ModuleCandidate] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)]
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String BS.ByteString)
encodeCertifiedProducts env sourceRecipe interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes =
  case reconcileOriginalProducts exact
      [(T.unpack unit,T.unpack name,map originalGroupFromProjected groups) | (unit,name,_,groups) <- fresh] of
    Left reason -> pure (Left reason)
    Right reconciled -> fmap (fmap fst) (encodeCertifiedProductsWithOriginals Map.empty Map.empty reconciled env sourceRecipe interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes)

encodeCertifiedProductsWithOriginals
  :: Map.Map (String,String) CanonicalInterfaceProof
  -> Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> ReconciledOriginalProducts
  -> HscEnv -> WorkerExecutionSource -> Map.Map ModuleName ModIface -> FinalizedModuleArtifacts -> [ModuleCandidate] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)]
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String (BS.ByteString, Map.Map (String,String) String))
encodeCertifiedProductsWithOriginals retained emittedSeals reconciled env sourceRecipe interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes =
  fmap (fmap (\(bytes,versions,_) -> (bytes,versions))) $
    encodeCertifiedOriginalProducts OrdinaryProductFacts retained emittedSeals reconciled env sourceRecipe
      interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes

encodeCertifiedOriginalProducts
  :: CertifiedProductKind
  -> Map.Map (String,String) CanonicalInterfaceProof
  -> Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> ReconciledOriginalProducts
  -> HscEnv -> WorkerExecutionSource -> Map.Map ModuleName ModIface -> FinalizedModuleArtifacts -> [ModuleCandidate] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)]
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String (BS.ByteString, Map.Map (String,String) String, TargetCertificationContext))
encodeCertifiedOriginalProducts kind retained emittedSeals reconciled env sourceRecipe interfaces finalized cached exact fresh targets evidence productBytes evidenceBytes =
  let targetNames = map (T.pack . fst) targets
      -- This DATA-only path owns no package code occurrences. Its actual targets
      -- still belong to the stage; PackageOwner witnesses must find a real link.
      packageIndex
        | Set.size (Set.fromList targetNames) /= length targetNames =
            Left "package projections repeat a target"
        | otherwise = issuePackageLinkIndex []
            (Map.fromList [(target,Map.empty) | target <- targetNames])
            (Map.fromList [(target,[]) | target <- targetNames])
            Map.empty Set.empty Map.empty Map.empty []
  in case packageIndex of
    Left reason -> pure (Left reason)
    Right issued -> do
      candidates <- forM cached $ \candidate -> do
        packages <- captureCandidatePackageEvidence candidate
        pure ((\roots -> (candidate,roots)) <$> packages)
      case sequence candidates of
        Left reason -> pure (Left reason)
        Right captured -> fmap (fmap (\(bytes,versions,context,_) -> (bytes,versions,context))) $
          encodeCertifiedOriginalProductsWithCapture Nothing Nothing Nothing [] Map.empty issued Map.empty kind retained emittedSeals reconciled env sourceRecipe
            interfaces finalized captured exact fresh targets DataOnlyTargets evidence productBytes evidenceBytes

-- Prior facts come from this same issuer's completed stage. They add imported
-- group coordinates and retained type seals, never newly emitted module rows.
encodeCertifiedOriginalProductsWithPrior
  :: FilePath -> Maybe TargetCertificationContext -> Maybe OriginalInterfaceArtifacts
  -> [(FilePath,BS.ByteString)] -> Map.Map (T.Text,T.Text) (FilePath,ArtifactBytes)
  -> PackageLinkIndex
  -> Map.Map SymbolIdentity WorldOriginalReference -> CertifiedProductKind
  -> Map.Map (String,String) CanonicalInterfaceProof
  -> Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> ReconciledOriginalProducts
  -> HscEnv -> WorkerExecutionSource -> Map.Map ModuleName ModIface -> FinalizedModuleArtifacts -> [(ModuleCandidate,PackageImportEvidence)] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)] -> [PreparedPackageTarget]
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String (BS.ByteString, Map.Map (String,String) String, TargetCertificationContext, [(FilePath,BS.ByteString)]))
encodeCertifiedOriginalProductsWithPrior outDir prior originalInterfaces packageProducts capturedPackages packageLinks currentReferences kind retained emittedSeals reconciled env sourceRecipe interfaces finalized cached exact fresh targets validationTargets =
  encodeCertifiedOriginalProductsWithCapture (Just outDir) prior originalInterfaces packageProducts capturedPackages packageLinks currentReferences kind retained emittedSeals reconciled env sourceRecipe interfaces finalized cached exact fresh targets (NativeTargets validationTargets)

-- Only the standalone DATA wrapper can select this mode. Native certification
-- always retains the opaque final package targets, independent of output paths.
data TargetValidation = DataOnlyTargets | NativeTargets [PreparedPackageTarget]

encodeCertifiedOriginalProductsWithCapture
  :: Maybe FilePath -> Maybe TargetCertificationContext -> Maybe OriginalInterfaceArtifacts
  -> [(FilePath,BS.ByteString)] -> Map.Map (T.Text,T.Text) (FilePath,ArtifactBytes)
  -> PackageLinkIndex
  -> Map.Map SymbolIdentity WorldOriginalReference -> CertifiedProductKind
  -> Map.Map (String,String) CanonicalInterfaceProof
  -> Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> ReconciledOriginalProducts
  -> HscEnv -> WorkerExecutionSource -> Map.Map ModuleName ModIface -> FinalizedModuleArtifacts -> [(ModuleCandidate,PackageImportEvidence)] -> Maybe ExactScope
  -> [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])]
  -> [(String, WireProgram)] -> TargetValidation
  -> DependencyEvidence -> BS.ByteString -> BS.ByteString
  -> IO (Either String (BS.ByteString, Map.Map (String,String) String, TargetCertificationContext, [(FilePath,BS.ByteString)]))
encodeCertifiedOriginalProductsWithCapture outputRoot prior originalInterfaces packageProducts capturedPackages packageLinks currentReferences kind retained emittedSeals reconciled env sourceRecipe interfaces finalized cachedWithPackages exact fresh targets validation evidence productBytes evidenceBytes = do
  let validationTargets = case validation of
        DataOnlyTargets -> []
        NativeTargets retainedTargets -> retainedTargets
  packageRef <- newIORef []
  timing <- readTimingEnabled
  (resolveGlobal, resolutionCounts) <- newPackageGlobalResolver timing env
  let resolvePackage identity = case Map.lookup (symbolUnit identity,symbolModule identity) capturedPackages of
        Just (path,body) -> pure (Right (PackageImportRoot
          (T.unpack (symbolUnit identity)) (T.unpack (symbolModule identity)) path (artifactSha256 body)))
        Nothing -> fmap (fmap snd) (resolveGlobal identity)
  let cached = map fst cachedWithPackages
      freshEvidenceSha = digest evidenceBytes
      freshProductSha = digest productBytes
      freshProductResults =
        [ do
            let owner = (unit,name)
                original = Map.lookup (T.unpack unit,T.unpack name) retained
            sourceSha <- case original of
              Nothing -> maybe (Left (MissingFreshSourceEvidence owner)) Right
                (readyNativeProductSourceSha256 evidence (T.unpack unit) (T.unpack name))
              Just proof -> Right (T.pack (canonicalSourceSha256 proof))
            groups <- traverse (freshGroup owner) projected
            pure Product
              { productOrigin = maybe FreshProduct (const RetainedCoreProduct) original, productUnit = unit, productModule = name
              , productVersion = Nothing, productSourceSha = sourceSha
              , productIfaceSha = digest iface
              , productBytesSha = freshProductSha
              , productEvidenceSha = maybe freshEvidenceSha (T.pack . canonicalCertificateSha256) original
              , productInterfaces = []
              , productGroups = groups
              }
        | (unit, name, iface, projected) <- fresh ]
      freshProducts = [product | Right product <- freshProductResults]
      cachedProducts =
        [ Product
          { productOrigin = CachedProduct
          , productUnit = T.pack (candidateUnit candidate)
          , productModule = T.pack (candidateModule candidate)
          , productVersion = Just (T.pack (candidateModuleVersion candidate))
          , productSourceSha = T.pack (candidateSourceSha256 candidate)
          , productIfaceSha = T.pack (candidateInterfaceSha256 candidate)
          , productBytesSha = T.pack (candidateProductSha256 candidate)
          , productEvidenceSha = T.pack (candidateEvidenceSha256 candidate)
          , productInterfaces = []
          , productGroups = candidateGroups candidate
          } | candidate <- cached ]
      unsealedProducts = freshProducts ++ cachedProducts
      interfaceSeals = Map.fromListWith Set.union
        ([(owner,Set.singleton sha) | (owner,sha) <- finalizedInterfaceSeals finalized]
        ++ [( (productUnit product, productModule product), Set.singleton (productIfaceSha product))
          | product <- unsealedProducts]
        ++ [((T.pack (exactUnit iface), T.pack (exactModule iface)), Set.singleton (T.pack (exactSha256 iface)))
           | scope <- maybe [] pure exact, (iface, _, _) <- scopeInterfaces scope]
        ++ [((T.pack (exactUnit iface), T.pack (exactModule iface)), Set.singleton (T.pack (exactSha256 iface)))
           | scope <- maybe [] pure exact, iface <- scopeValueInterfaces scope])
      previousSeals = maybe Map.empty targetInterfaceSeals prior
      selectedSeals key = case Map.lookup key interfaceSeals of
        Just seals -> Just seals
        Nothing -> Set.singleton <$> Map.lookup key previousSeals
  sealedProducts <- forM unsealedProducts $ \product -> do
    let owner = mkModule (stringToUnit (T.unpack (productUnit product))) (mkModuleName (T.unpack (productModule product)))
    selected <- case Map.lookup (moduleName owner) interfaces of
      Just iface | mi_module iface == owner -> pure (Right iface)
      _ -> fmap (either (const (Left "original interface usage inventory unavailable")) Right)
        (if toUnitId (moduleUnit owner) `Set.member` hsc_all_home_unit_ids env
          then readHomeInterface env owner
          else fmap (fmap fst) (readPackageInterface env owner))
    retainedSeals <- case selected of
      Left _ -> pure Map.empty
      Right iface -> fmap Map.fromList $ forM (homeInterfaceUsageOwners env iface) $ \(unit,name) -> do
        sha <- case originalInterfaces of
          Nothing -> pure Nothing
          Just artifacts -> originalInterfaceSha256 artifacts
            (mkModule (stringToUnit unit) (mkModuleName name))
        pure ((T.pack unit,T.pack name),T.pack <$> sha)
    pure $ do
      iface <- selected
      requirements <- forM (homeInterfaceUsageOwners env iface) $ \(unit, name) ->
        case selectedSeals (T.pack unit, T.pack name) of
          Just seals | [sha] <- Set.toAscList seals -> do
            case Map.lookup (T.pack unit,T.pack name) retainedSeals of
              Just (Just actual) | actual /= sha -> Left "captured original interface differs from its prior sealed owner"
              _ -> Right ()
            Right (T.pack unit, T.pack name, sha)
          Nothing | Just (Just sha) <- Map.lookup (T.pack unit,T.pack name) retainedSeals ->
            Right (T.pack unit,T.pack name,sha)
          missingSeals -> Left ("original interface usage leaves the exact sealed owner closure: "
            ++ show (productUnit product, productModule product)
            ++ " requires " ++ show (unit,name)
            ++ "; selected seals=" ++ show (maybe [] Set.toAscList missingSeals))
      Right product { productInterfaces = requirements }
  let products = [product | Right product <- sealedProducts]
      nativeObservations = fmap concat $ forM (maybe [] pure exact) $ \scope -> do
        observations <- scopeAvailableNativeGroupObservations scope
        forM_ observations $ \(product,_) -> unless
          (product `elem` unrecoveredExactProducts reconciled)
          (Left "native census observation leaves its reconciled exact product admission")
        forM [(product,group) | (product,groups) <- observations, group <- groups] $ \(product,group) -> do
          globals <- forM (zip [0..] (Native.nativeGroupGlobals group)) $ \(index,global) ->
            nativeGlobalWitness scope product (Native.nativeGroupOrdinal group) index global
          pure ((T.pack (originalUnit product), T.pack (originalModule product),
             Just (T.pack (originalVersion product)), Native.nativeGroupOrdinal group),
            Native.nativeGroupBinders group, globals)
      nativeObservationRows = either (const []) id nativeObservations
      admittedBinders =
        [ (binder, (T.pack (originalUnit product), T.pack (originalModule product),
                    Just (T.pack (originalVersion product)), originalOrdinal group))
        | product <- unrecoveredExactProducts reconciled
        , group <- originalGroups product, binder <- originalBinders group ]
      binders = admittedBinders ++
        [ (binder, (productUnit product, productModule product,
                    productVersion product, candidateGroupOrdinal group))
        | product <- products, group <- productGroups product
        , binder <- candidateGroupBinders group ]
      freshOwnerMap = Map.fromList binders
      previousReferences = maybe Map.empty targetOriginalReferences prior
      originalReferences = Map.union currentReferences previousReferences
      priorOwners = maybe Map.empty targetBinderOwners prior
      ownerMap = Map.union freshOwnerMap priorOwners
      nativeOwners = Set.fromList [(productUnit product,productModule product) | product <- products]
      homeModules = Set.union (maybe Set.empty targetHomeModules prior) $ Set.fromList (
        [ (T.pack (dependencyModuleUnit node), T.pack (dependencyModuleName node))
        | node <- dependencyModules evidence, not (dependencyModuleBoot node) ]
        ++ [(T.pack (exactUnit iface), T.pack (exactModule iface))
           | scope <- maybe [] pure exact, (iface, _, _) <- scopeInterfaces scope]
        ++ [(T.pack (exactUnit iface), T.pack (exactModule iface))
           | scope <- maybe [] pure exact, iface <- scopeValueInterfaces scope])
      allGlobals =
        [ (product, group) | product <- products, group <- productGroups product ]
      inventoryRefusals =
        [ConflictingInterfaceSeals owner (Set.size seals) (take 2 (Set.toAscList seals))
        | (owner,seals) <- Map.toAscList interfaceSeals
        , Set.size seals /= 1]
        ++ [PriorInterfaceSealConflict owner expected observed
           | (owner,seals) <- Map.toAscList interfaceSeals
           , Just expected <- [Map.lookup owner previousSeals]
           , observed : _ <- [Set.toAscList (Set.delete expected seals)]]
        ++ [reason | Left reason <- freshProductResults]
        ++ [reason | length binders /= Map.size freshOwnerMap
           , Just reason <- [duplicateOriginalBinder binders]]
        ++ [PriorBinderOwnerConflict binder expected observed
           | (binder,observed) <- Map.toAscList freshOwnerMap
           , Just expected <- [Map.lookup binder priorOwners], observed /= expected]
        ++ [PriorOriginalReferenceConflict binder expected observed
           | (binder,observed) <- Map.toAscList currentReferences
           , Just expected <- [Map.lookup binder previousReferences], observed /= expected]
        ++ [OriginalBinderReferenceConflict binder owner reference
           | (binder,owner) <- Map.toAscList freshOwnerMap
           , Just reference <- [Map.lookup binder originalReferences]]
        ++ [PriorNativeOwnerConflict owner
              (find (\(unit,name,_,_) -> (unit,name) == owner)
                (Map.keys (maybe Map.empty targetGroupWitnesses prior)))
              (case productGroups product of
                group : _ -> Just (productUnit product,productModule product,
                  productVersion product,candidateGroupOrdinal group)
                [] -> Nothing)
           | product <- products
           , let owner = (productUnit product,productModule product)
           , owner `Set.member` maybe Set.empty targetNativeOwners prior]
  case [reason | Left reason <- sealedProducts]
      ++ [reason | Left reason <- [nativeObservations]]
      ++ ["native census binder disagrees with its admitted exact coordinate: " ++ show (binder,coordinate)
         | (coordinate,groupBinders,_) <- nativeObservationRows, binder <- groupBinders
         , Map.lookup binder freshOwnerMap /= Just coordinate]
      ++ map renderOriginalInventoryRefusal inventoryRefusals of
    reason : _ -> pure (Left reason)
    [] -> do
      modules <- forM allGlobals $ \(product, group) -> do
        globals <- forM (candidateGroupGlobals group) $ \global ->
          encodeGlobalWitness env resolvePackage packageRef originalReferences ownerMap homeModules
            (candidateGlobalIdentity global)
            (candidateGlobalRep global)
            (candidateGlobalSignature global)
            (candidateGlobalEvaluated global)
            (fromIntegral <$> candidateGlobalGeneration global)
        pure (product, group, sequence globals)
      targetRows <- forM targets $ \(target, program) -> do
        globals <- forM (programGlobals program) $ \global ->
          encodeGlobalWitness env resolvePackage packageRef originalReferences ownerMap homeModules
            (globalIdentity global) (globalRep global)
            (globalEntrySignature global >>= \(SignatureId index) ->
              at (programSignatures program) (fromIntegral index))
            (globalRequiredEvaluated global) (globalRequiredGeneration global)
        pure (target, sequence globals)
      -- Validation-only segment targets must not add observations to the wire
      -- package table or interned witness rows.
      validationPackageRef <- newIORef []
      validationRows <- forM validationTargets $ \target -> do
        let program = packageTargetProgram target
        globals <- forM (programGlobals program) $ \global ->
          encodeGlobalWitness env resolvePackage validationPackageRef originalReferences ownerMap homeModules
            (globalIdentity global) (globalRep global)
            (globalEntrySignature global >>= \(SignatureId index) ->
              at (programSignatures program) (fromIntegral index))
            (globalRequiredEvaluated global) (globalRequiredGeneration global)
        pure (target, sequence globals)
      certifyLocalPackageExports env resolvePackage packageRef (map snd targets)
      observedPackages <- readIORef packageRef
      -- Canonical sidecars also own direct TYPE-only and reexport-facade
      -- imports. They need byte custody even when native groups never name
      -- them. Keep their sealed origin paths in the sidecar itself.
      retainedPackages <- forM (Map.elems retained) $ \proof -> do
        let (iface,path,seal) = canonicalInterfaceInput proof
        body <- canonicalProofOriginalBody proof path seal
        pure (decodeCapturedPackageImports iface (artifactBytes body))
      let sidecarPackages = map localFinalizedPackageEvidence
            (Map.elems (finalizedLocalAdmissions finalized))
            ++ [packages | Right packages <- retainedPackages]
            ++ map snd cachedWithPackages
          selectedPackages = observedPackages ++
            [(unit,name,path,T.pack (artifactSha256 body))
            | ((unit,name),(path,body)) <- Map.toAscList capturedPackages]
            ++ [(T.pack (packageUnit root),T.pack (packageModule root)
                ,packagePath root,T.pack (packageSha256 root))
               | packages <- sidecarPackages, root <- packageInterfaces packages]
      owned <- case outputRoot of
        Nothing -> pure (Right (selectedPackages,[]))
        Just root -> fmap (fmap (\(captures,writes) ->
          ([(unit,name,path,T.pack (artifactSha256 body))
            | ((unit,name),(path,body)) <- Map.toAscList captures],writes))) $
          captureSelectedPackageWitnesses root capturedPackages selectedPackages
      let (packageWitnesses,packageInterfaceWrites) = either (const ([],[])) id owned
          packages = Map.fromListWith Set.union
            [ ((unit, name), Set.singleton (path, sha))
            | (unit, name, path, sha) <- packageWitnesses ]
      (requests, loads, catalogs) <- resolutionCounts
      let references = Map.fromListWith (+)
            [((path, sha), 1) | (_, _, path, sha) <- packageWitnesses]
      validated <- case outputRoot of
        -- Production custody was checked against the actual token at capture.
        -- The DATA encoder retains its independent path validation boundary.
        Just _ -> pure []
        Nothing -> forM (Map.toAscList references) $ \((path, sha), count) -> do
          readBack <- try (BS.readFile path) :: IO (Either IOException BS.ByteString)
          pure $ case readBack of
            Left _ -> (False, 0, 0)
            Right bytes -> let size = fromIntegral (BS.length bytes)
              in (digest bytes == sha, size, size * count)
      let packagesValid = and [valid | (valid, _, _) <- validated]
      emitCount timing "certified_package_global_requests" requests
      emitCount timing "certified_package_owner_loads" loads
      emitCount timing "certified_package_catalog_builds" catalogs
      emitCount timing "certified_package_owner_revalidations" (fromIntegral (Map.size references))
      emitCount timing "certified_package_revalidation_bytes" (sum [size | (_, size, _) <- validated])
      emitCount timing "certified_package_reference_bytes" (sum [size | (_, _, size) <- validated])
      emitCount timing "certified_package_captured_interface_bytes"
        (sum [fromIntegral (BS.length bytes) | (_,bytes) <- packageInterfaceWrites])
      let failures = either (:[]) (const []) owned
            ++ [reason | Left reason <- retainedPackages]
            ++ [errorText | (_, _, Left errorText) <- modules]
            ++ [errorText | (_, Left errorText) <- targetRows]
            ++ [errorText | (_, Left errorText) <- validationRows]
            ++ ["package interface changed during certification" | not packagesValid]
            ++ ["package owner selected more than one interface" |
                any ((/= 1) . Set.size) (Map.elems packages)]
      case failures of
        first : _ -> pure (Left first)
        [] -> do
          let targetRowsReady =
                [(target, witnesses) | (target, Right witnesses) <- targetRows]
          targetRootsFromWitnesses <- Map.fromList <$> forM targetRowsReady (\(target,witnesses) -> do
            roots <- forM
              [identity | (identity,_,_,_,PackageOwner _ _ _) <- witnesses]
              (\identity -> maybe
                (fail ("target package import lacks its occurrence: " ++ show identity))
                pure
                (Map.lookup identity
                  (Map.findWithDefault Map.empty (T.pack target)
                    (packageLinkIndexTargetOwners packageLinks))))
            pure (T.pack target, roots))
          case kind of
            OrdinaryProductFacts -> either fail pure
              (validatePackageTargetRoots
                (packageLinkIndexTargetRoots packageLinks)
                targetRootsFromWitnesses)
            SegmentOriginalFacts -> pure ()
          let moduleRows =
                [(product, group, witnesses)
                | (product, group, Right witnesses) <- modules]
              allWitnessRows = map third moduleRows ++ map snd targetRowsReady
              (witnessIndices, referenceRows) = mapAccumL
                (mapAccumL internWitness) Map.empty allWitnessRows
              orderedWitnesses = Map.toAscList witnessIndices
              globalWitnesses = map fst orderedWitnesses
              coordinates = Set.toAscList (Set.fromList
                [coordinate | (_, _, _, _, owner) <- globalWitnesses
                , Just coordinate <- [ownerCoordinate owner]])
              coordinateIndices = Map.fromList (zip coordinates [0 ..])
              canonicalIndices = Map.fromList
                [(provisional, fromIntegral index)
                | (index, (_, provisional)) <- zip [0 :: Int ..] orderedWitnesses]
              encodeReference provisional = encodeWord
                (canonicalIndices Map.! provisional)
              (moduleReferenceRows, targetReferenceRows) =
                splitAt (length moduleRows) referenceRows
              byModule = Map.map reverse $ Map.fromListWith (++)
                [ ((productUnit product, productModule product),
                   [(candidateGroupOrdinal group, map encodeReference referenceIndices)])
                | ((product, group, _), referenceIndices) <- zip moduleRows moduleReferenceRows ]
              encodedModules =
                [ encodeModule product (Map.findWithDefault []
                    (productUnit product, productModule product) byModule)
                | product <- products ]
              encodedTargets =
                [ array [encodeString (T.pack target), list encodeReference referenceIndices]
                | ((target, _), referenceIndices) <- zip targetRowsReady targetReferenceRows ]
              encodedPackages =
                [ array [encodeString unit, encodeString name
                  , encodeString (T.pack path), encodeString sha]
                | ((unit, name), options) <- Map.toList packages
                , (path, sha) <- Set.toList options ]
              homeProductKeys = Set.fromList
                [ (productUnit product, productModule product, candidateGroupOrdinal group)
                | (product, group, _) <- moduleRows ]
                `Set.union` Set.fromList
                [ (T.pack (candidateUnit candidate),T.pack (candidateModule candidate),
                    candidateGroupOrdinal group)
                | candidate <- cached
                , group <- candidateGroups candidate ]
                `Set.union` Set.fromList
                [ (T.pack (originalUnit product),T.pack (originalModule product),originalOrdinal group)
                | product <- unrecoveredExactProducts reconciled
                , group <- originalGroups product ]
              expectedHomeRoots = packageLinkIndexExpectedHomeRoots packageLinks
              expectedProductCoordinates = Set.fromList
                [(unit,name,ordinal) | (_,unit,name,ordinal) <- Set.toList expectedHomeRoots]
              -- Each demanded target HOME group carries its issued package
              -- roots, whether its native product rows are current or cached.
              homeRoots = Map.restrictKeys
                (packageLinkIndexHomeRoots packageLinks) expectedHomeRoots
              targetRoots = case kind of
                OrdinaryProductFacts -> packageLinkIndexTargetRoots packageLinks
                SegmentOriginalFacts -> packageLinkIndexTargetRoots packageLinks
              encodedPackageLinks = encodePackageLinks
                (PackageLinks
                  (packageLinkIndexStageId packageLinks)
                  (packageLinkIndexOccurrences packageLinks)
                  targetRoots
                  homeRoots
                  (packageLinkIndexOriginalRoots packageLinks)
                  (packageLinkIndexProducts packageLinks))
              originalFields =
                [list id encodedModules, list id encodedPackages
                ,list (encodeWitness coordinateIndices) globalWitnesses
                ,encodeFinalizedModuleArtifacts finalized,encodeWorkerExecutionSource sourceRecipe
                ,list encodeCoordinate coordinates
                ,list (\(path,seal) -> array [encodeString (T.pack path),encodeBytes seal]) packageProducts]
              payload = case kind of
                OrdinaryProductFacts -> array (encodeString "ordinary" :
                  list id encodedModules : list id encodedTargets : drop 1 originalFields ++ [encodedPackageLinks])
                SegmentOriginalFacts -> array (encodeString "segment-originals" : originalFields ++ [encodedPackageLinks])
              certificateBytes = toStrictByteString $
                array [encodeString "TPCERT",encodeWord 13,payload]
              priorPackages = maybe Map.empty targetModulePackages prior
              currentPackages = Map.map Set.findMin packages
              packageConflicts = Map.intersectionWith (\(_,left) (_,right) -> left /= right) currentPackages priorPackages
              borrowedSeals = Map.fromList
                [( (unit,name),sha) | product <- products, (unit,name,sha) <- productInterfaces product]
              groupWitnesses = Map.fromList
                [((productUnit product,productModule product,productVersion product,candidateGroupOrdinal group),
                    CertifiedGroup (candidateGroupBinders group) witnesses)
                | (product,group,witnesses) <- moduleRows]
              makeContext completeGroups = TargetCertificationContext ownerMap originalReferences homeModules
                (Map.union currentPackages priorPackages)
                (Set.unions [Map.keysSet originalReferences,Set.fromList (map fst binders),maybe Set.empty targetBinderAvailability prior])
                (Set.union nativeOwners (maybe Set.empty targetNativeOwners prior))
                (Map.union borrowedSeals (Map.union (Map.map Set.findMin interfaceSeals) previousSeals))
                completeGroups
                Map.empty
                packageLinks
          pure $ do
            admittedGroups <- foldM (\groups (coordinate,groupBinders,witnesses) ->
                mergeCertifiedGroupObservations groups (Map.singleton coordinate (CertifiedGroup groupBinders witnesses)))
              Map.empty nativeObservationRows
            currentGroups <- mergeCertifiedGroupObservations groupWitnesses admittedGroups
            completeGroups <- mergeCertifiedGroupObservations currentGroups (maybe Map.empty targetGroupWitnesses prior)
            let context = makeContext completeGroups
            -- The native writer retains every final target, including targets
            -- with no Original or package uses. The standalone DATA encoder has
            -- no native target owner and keeps its separate empty-link contract.
            case validation of
              DataOnlyTargets -> do
                unless (Map.null originalReferences && null (packageLinkIndexOccurrences packageLinks)
                    && Map.null (packageLinkIndexOriginalRoots packageLinks)
                    && maybe True (const False) prior)
                  (Left "DATA-only certification received native Original or package authority")
                forM_ targetRowsReady $ \(name,witnesses) ->
                  validateTargetOriginalUses (T.pack name) context Set.empty witnesses Set.empty
              NativeTargets _ -> do
                let retainedNames = Set.fromList (map packageTargetName validationTargets)
                    requiredNames = Set.unions
                      [Map.keysSet (packageLinkIndexTargetRoots packageLinks)
                      ,Map.keysSet (packageLinkIndexTargetOwners packageLinks)
                      ,Set.fromList [name | (name,_,_,_) <- Map.keys (packageLinkIndexHomeRoots packageLinks)]
                      ,Map.keysSet (packageLinkIndexPriorHomeGroups packageLinks)
                      ,Set.fromList [name | (name,_,_) <- Map.keys (packageLinkIndexOriginalRoots packageLinks)]]
                unless (length validationTargets == Set.size retainedNames && retainedNames == requiredNames)
                  (Left "final validation target census differs from issued package targets")
                case kind of
                  SegmentOriginalFacts -> Right ()
                  OrdinaryProductFacts -> do
                    let actualPrograms = Map.fromList [(T.pack name,program) | (name,program) <- targets]
                        retainedPrograms = Map.fromList [(packageTargetName target,packageTargetProgram target)
                          | target <- validationTargets]
                    unless (length targets == Map.size actualPrograms && actualPrograms == retainedPrograms)
                      (Left "serialized ordinary targets differ from retained final targets")
            forM_ validationRows $ \(target,result) -> do
              witnesses <- result
              validateTargetOriginalUses (packageTargetName target) context (packageTargetLocalHomeBindings target)
                witnesses (Map.keysSet (packageTargetOriginalImports target))
              let name = packageTargetName target
                  declared = Map.fromList
                    [((reference,binder),map fst roots)
                    | ((targetName,reference,binder),roots) <- Map.toAscList (packageLinkIndexOriginalRoots packageLinks)
                    , targetName == name]
              unless (declared == packageTargetOriginalImports target)
                (Left "Original use package roots differ from final target")
              roots <- forM [identity | (identity,_,_,_,PackageOwner _ _ _) <- witnesses] $ \identity ->
                maybe (Left "validation target package import lacks its occurrence") Right
                  (Map.lookup identity (Map.findWithDefault Map.empty name (packageLinkIndexTargetOwners packageLinks)))
              validatePackageTargetRoots
                (Map.singleton name (Map.findWithDefault [] name (packageLinkIndexTargetRoots packageLinks)))
                (Map.singleton name roots)
            forM_ (Map.elems (packageLinkIndexPriorHomeGroups packageLinks)) $ \groups ->
              if Map.null groups then Right () else case prior of
                Nothing -> Left "prior HOME package root lacks its immediate predecessor"
                Just previous -> validatePriorHomeGroups previous groups
            let priorHomeKeys = Set.fromList
                  [(unit,name,ordinal)
                  | groups <- Map.elems (packageLinkIndexPriorHomeGroups packageLinks)
                  , ((unit,name,_,ordinal),_) <- Map.toAscList groups]
            if Map.size groupWitnesses /= length moduleRows
              then Left "duplicate certified native group coordinate"
              else Right ()
            if or (Map.elems packageConflicts)
              then Left "prior native package owner changed its captured interface"
              else Right ()
            if not (expectedProductCoordinates `Set.isSubsetOf` Set.union homeProductKeys priorHomeKeys)
              then Left ("HOME package root has no issued native product: products="
                  ++ show homeProductKeys
                  ++ " expected=" ++ show expectedHomeRoots)
              else Right ()
            if Map.keysSet homeRoots /= expectedHomeRoots
              then Left "fresh HOME package roots were not consumed exactly"
              else Right ()
            sourceSeals <- renderNativeSourceSealResult $ admittedNativeSourceSeals exact cached
              >>= mergeNativeSourceSeals (maybe Map.empty targetNativeSourceSeals prior)
            versions <- retainedNativeVersions sourceSeals emittedSeals packages products moduleRows
            finalizedSeals <- renderNativeSourceSealResult (finalizeNativeSourceSeals sourceSeals emittedSeals versions)
            Right (certificateBytes,versions,context { targetNativeSourceSeals = finalizedSeals },packageInterfaceWrites)
  where
    internWitness :: Map.Map GlobalWitness Word -> GlobalWitness
      -> (Map.Map GlobalWitness Word, Word)
    internWitness indices witness = case Map.lookup witness indices of
      Just index -> (indices, index)
      Nothing ->
        let index = fromIntegral (Map.size indices)
        in (Map.insert witness index indices, index)
    third (_, _, value) = value

-- Selection consumes the same witnesses that issue the item certificate.
-- Compiler-only names outside this executable closure create no value demand.
encodeCertifiedItemProductsWithInputs :: HscEnv -> TargetCertificationContext -> ItemPackageSelection
  -> IO (Either String (BS.ByteString,Set.Set (SymbolIdentity,Word64)))
encodeCertifiedItemProductsWithInputs env context selection = do
  certified <- certifyItemProducts env context selection
  pure $ do
    (bytes,witnesses) <- certified
    inputs <- retainedInputClosure context witnesses
    pure (bytes,inputs)

retainedInputClosure :: TargetCertificationContext -> [GlobalWitness]
  -> Either String (Set.Set (SymbolIdentity,Word64))
retainedInputClosure context = retainedInputClosureFromWitnesses (targetGroupWitnesses context)

retainedInputClosureFromWitnesses :: Map.Map BinderOwner (CertifiedGroupFor reference) -> [GlobalWitnessFor reference]
  -> Either String (Set.Set (SymbolIdentity,Word64))
retainedInputClosureFromWitnesses groups = visit Set.empty Set.empty
  where
    visit _ inputs [] = Right inputs
    visit seen inputs ((identity,_,_,_,owner):pending) = case owner of
      RetainedOwner generation -> visit seen (Set.insert (identity,generation) inputs) pending
      PackageOwner _ _ _ -> visit seen inputs pending
      -- Hidden retained capture requirements remain with Rust's exact selected
      -- native closure; Haskell does not infer an empty executable closure.
      OriginalBindingOwner _ -> visit seen inputs pending
      SourceOwner unit name version ordinal -> do
        let coordinate = (unit,name,version,ordinal)
        group <- maybe (Left ("executable input demand lacks its certified native group: " ++ show coordinate))
          Right (Map.lookup coordinate groups)
        unless (identity `elem` certifiedGroupBinders group)
          (Left ("source dependency is absent from its exact certified group: " ++ show (identity,coordinate)))
        if coordinate `Set.member` seen then visit seen inputs pending
        else visit (Set.insert coordinate seen) inputs (certifiedGroupGlobals group ++ pending)

certifyItemProducts :: HscEnv -> TargetCertificationContext -> ItemPackageSelection
  -> IO (Either String (BS.ByteString,[GlobalWitness]))
certifyItemProducts env context selection = do
  let program = itemSelectionProgram selection
  packageRef <- newIORef []
  timing <- readTimingEnabled
  (resolveGlobal,_) <- newPackageGlobalResolver timing env
  let resolvePackage identity = case Map.lookup (symbolUnit identity,symbolModule identity) (targetModulePackages context) of
        Just (path,seal) -> pure (Right (PackageImportRoot
          (T.unpack (symbolUnit identity)) (T.unpack (symbolModule identity)) path (T.unpack seal)))
        Nothing -> fmap (fmap snd) (resolveGlobal identity)
  globals <- forM (programGlobals program) $ \global ->
    encodeGlobalWitness env resolvePackage packageRef (targetOriginalReferences context) (targetBinderOwners context)
      (targetHomeModules context) (globalIdentity global) (globalRep global)
      (globalEntrySignature global >>= \(SignatureId index) ->
        at (programSignatures program) (fromIntegral index))
      (globalRequiredEvaluated global) (globalRequiredGeneration global)
  certifyLocalPackageExports env resolvePackage packageRef [program]
  packages <- Map.fromListWith Set.union . map
    (\(unit,name,path,seal) -> ((unit,name),Set.singleton (path,seal))) <$> readIORef packageRef
  validated <- forM (Map.toAscList packages) $ \(owner,choices) ->
    case Set.toAscList choices of
      [(path,seal)] -> do
        captured <- try (BS.readFile path) :: IO (Either IOException BS.ByteString)
        pure $ do
          bytes <- either (const (Left "item package interface unavailable")) Right captured
          if digest bytes /= seal then Left "item package interface changed" else pure ()
          case Map.lookup owner (targetModulePackages context) of
            Just previous | previous /= (path,seal) -> Left "item replaces an original package selection"
            Just _ -> Right Nothing
            Nothing -> Right (Just (owner,path,seal))
      _ -> pure (Left "item package owner selected more than one interface")
  pure $ do
    witnesses <- sequence globals
    additions <- catMaybes <$> sequence validated
    validateTargetOriginalUses (itemSelectionTarget selection) context (itemSelectionLocalHomeBindings selection) witnesses
      (Set.fromList [(reference,binder) | (_,reference,binder) <- Map.keys (itemSelectionOriginalRoots selection)])
    let bytes = toStrictByteString $ array [encodeString "TPCERT",encodeWord 13,
          array [encodeString "segment-item",list encodeFullWitness witnesses,
            list (\((unit,name),path,seal) -> array [encodeString unit,encodeString name,
              encodeString (T.pack path),encodeString seal]) additions,
            encodeItemPackageSelection selection]]
    Right (bytes,witnesses)

encodeItemPackageSelection :: ItemPackageSelection -> Encoding
encodeItemPackageSelection selection = array
  [ encodeWord 2
  , encodeBytes (itemSelectionStageSha256 selection)
  , encodeBytes (SHA256.hash (encodeWireProgram (itemSelectionProgram selection)))
  , encodeString (itemSelectionTarget selection)
  , list encodeLink (itemSelectionTargetRoots selection)
  , list encodeHomeRoot (Map.toAscList (itemSelectionHomeRoots selection))
  , list encodeOriginalUseRoot (Map.toAscList (itemSelectionOriginalRoots selection))
  ]
  where
    encodeHomeRoot ((target,unit,name,ordinal), roots) = array
      [array [encodeString target,encodeString unit,encodeString name,encodeWord ordinal]
      , list encodeLink roots]
    encodeLink occurrence =
      array [encodeString "local", encodeWord (fromIntegral occurrence)]

encodeFullWitness :: GlobalWitness -> Encoding
encodeFullWitness (identity,rep,signature,evaluated,owner) = array
  [encodeReceiptIdentity identity,encodeRep rep,maybe encodeNull encodeSignature signature,
   encodeBool evaluated,case owner of
    SourceOwner unit name version ordinal -> array [encodeString "source",encodeString unit,
      encodeString name,maybe encodeNull encodeString version,encodeWord ordinal,encodeReceiptIdentity identity]
    RetainedOwner generation -> array [encodeString "retained",encodeReceiptIdentity identity,encodeWord64 generation]
    OriginalBindingOwner reference -> encodeOriginalBinding reference
    PackageOwner unit name seal -> array [encodeString "package",encodeString unit,
      encodeString name,encodeString seal,encodeReceiptIdentity identity]]

-- Raw staged coordinates remain Nothing in the receipt. Their issuing context
-- associates that coordinate with the finalized native identity; historical
-- Just coordinates always select their own exact version.
type NativeSourceSeals = Map.Map (T.Text,T.Text,Maybe T.Text) (T.Text,T.Text,T.Text)

-- These pure comparisons validate observations; only certification constructs
-- the opaque context that carries them between stages.
data NativeSourceSealFailure
  = NativeSourceSealMissing (T.Text,T.Text,Maybe T.Text)
  | NativeSourceSealConflict (T.Text,T.Text,Maybe T.Text)
  | NativeSourceSealVersionMismatch (T.Text,T.Text,Maybe T.Text) T.Text
  deriving (Eq,Show)

renderNativeSourceSealResult :: Either NativeSourceSealFailure a -> Either String a
renderNativeSourceSealResult = either (Left . show) Right

lookupNativeSourceSeal :: NativeSourceSeals -> (T.Text,T.Text,Maybe T.Text)
  -> Either NativeSourceSealFailure (T.Text,T.Text,T.Text)
lookupNativeSourceSeal seals coordinate@(_,_,requested) = case Map.lookup coordinate seals of
  Nothing -> Left (NativeSourceSealMissing coordinate)
  Just seal@(version,_,_) -> case requested of
    Just expected | expected /= version -> Left (NativeSourceSealVersionMismatch coordinate version)
    _ -> Right seal

mergeNativeSourceSeals :: NativeSourceSeals -> NativeSourceSeals -> Either NativeSourceSealFailure NativeSourceSeals
mergeNativeSourceSeals left right = foldM insert left (Map.toAscList right)
  where
    insert known (coordinate,seal@(version,iface,native)) = version `seq` iface `seq` native `seq` case Map.lookup coordinate known of
      Just previous | previous /= seal -> Left (NativeSourceSealConflict coordinate)
      _ -> Right (Map.insert coordinate seal known)

admittedNativeSourceSeals :: Maybe ExactScope -> [ModuleCandidate] -> Either NativeSourceSealFailure NativeSourceSeals
admittedNativeSourceSeals exact cached = foldM add Map.empty
  ([((T.pack (candidateUnit candidate),T.pack (candidateModule candidate)),
      (T.pack (candidateModuleVersion candidate),T.pack (candidateInterfaceSha256 candidate),
       T.pack (candidateProductSha256 candidate))) | candidate <- cached]
   ++ [((T.pack (originalUnit product),T.pack (originalModule product)),
      (T.pack (originalVersion product),T.pack (originalIfaceSha256 product),
       T.pack (originalProductSha256 product)))
      | scope <- maybe [] pure exact, product <- scopeProducts scope])
  where
    add known ((unit,name),seal@(version,_,_)) =
      mergeNativeSourceSeals known (Map.singleton (unit,name,Just version) seal)

finalizeNativeSourceSeals :: NativeSourceSeals
  -> Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> Map.Map (String,String) String -> Either NativeSourceSealFailure NativeSourceSeals
finalizeNativeSourceSeals inherited emitted versions = foldM step inherited (Map.toAscList emitted)
  where
    step known ((unit,name),(provisional,iface,native,_)) = do
      let version = maybe provisional T.pack (Map.lookup (T.unpack unit,T.unpack name) versions)
      -- Non-exact fresh World products have no finalized version here. They
      -- cannot become external native graph evidence through a guessed seal.
      if T.null version then Right known else
        mergeNativeSourceSeals known (Map.fromList
          [((unit,name,Nothing),(version,iface,native)),((unit,name,Just version),(version,iface,native))])

-- Native identity binds the finite retained graph. Local promoted edges name
-- nodes rather than recursively derived versions, so cycles remain finite.
retainedNativeVersions
  :: NativeSourceSeals
  -> Map.Map (T.Text,T.Text) (T.Text,T.Text,T.Text,T.Text)
  -> Map.Map (T.Text,T.Text) (Set.Set (FilePath,T.Text))
  -> [Product] -> [(Product,CandidateGroup,[GlobalWitness])]
  -> Either String (Map.Map (String,String) String)
retainedNativeVersions inherited emitted packages products rows = do
  versions <- forM (Map.keys promoted) $ \root -> do
    reachable <- closure Set.empty [root]
    nodes <- traverse encodeNode (Set.toAscList reachable)
    let graph = toStrictByteString (list id nodes)
        (unit,name) = root
        framed bytes = BS.pack [fromIntegral ((fromIntegral (BS.length bytes) :: Word64) `shiftR` shift)
          | shift <- [56,48..0]] <> bytes
        version = digest (BS.concat (map framed
          ["retained-core-home-v1",TE.encodeUtf8 unit,TE.encodeUtf8 name,graph]))
    pure ((T.unpack unit,T.unpack name),T.unpack version)
  pure (Map.fromList versions)
  where
    key product = (productUnit product,productModule product)
    promoted = Map.fromList [(key product,product) | product <- products
      , productOrigin product == RetainedCoreProduct]
    groups = Map.union (Map.fromListWith Map.union
      [(key product,Map.singleton (candidateGroupOrdinal group) witnesses)
      | (product,group,witnesses) <- rows]) (Map.map (const Map.empty) promoted)
    lookupSeal coordinate@(unit,name,version) = renderNativeSourceSealResult $ do
      let current = case (version,Map.lookup (unit,name) emitted) of
            (Nothing,Just (final,iface,native,_)) | not (T.null final) ->
              Map.singleton coordinate (final,iface,native)
            _ -> Map.empty
      seals <- mergeNativeSourceSeals inherited current
      lookupNativeSourceSeal seals coordinate
    closure visited [] = Right visited
    closure visited (owner:pending)
      | owner `Set.member` visited = closure visited pending
      | otherwise = case Map.lookup owner groups of
          Nothing -> Left "native demand graph lacks a promoted group inventory"
          Just owned -> closure (Set.insert owner visited)
            ([dependency | witnesses <- Map.elems owned,(_,_,_,_,SourceOwner unit name Nothing _) <- witnesses
              , let dependency = (unit,name),Map.member dependency promoted] ++ pending)
    encodeNode owner@(unit,name) = do
      product <- maybe (Left "native demand graph lacks a promoted canonical proof") Right (Map.lookup owner promoted)
      (_,_,nativeSha,packageSha) <- maybe
        (Left ("native demand graph lacks emitted promoted seals at " ++ show owner)) Right (Map.lookup owner emitted)
      owned <- maybe (Left "native demand graph lacks retained groups") Right (Map.lookup owner groups)
      encodedGroups <- forM (Map.toAscList owned) $ \(ordinal,witnesses) -> do
        edges <- traverse encodeEdge witnesses
        pure (array [encodeWord ordinal,list id edges])
      pure (array [encodeString unit,encodeString name,encodeString (productEvidenceSha product),
        encodeString nativeSha,encodeString packageSha,list id encodedGroups])
    encodeEdge (identity,_,_,_,owner) = case owner of
      SourceOwner unit name version ordinal
        | version == Nothing && Map.member (unit,name) promoted -> Right (array
            [encodeString "local-source",encodeString unit,encodeString name,encodeWord ordinal,encodeReceiptIdentity identity])
        | otherwise -> do
            (finalVersion,ifaceSha,nativeSha) <- lookupSeal (unit,name,version)
            pure (array [encodeString "source",array (map encodeString [unit,name,finalVersion,ifaceSha,nativeSha]),
              encodeWord ordinal,encodeReceiptIdentity identity])
      RetainedOwner generation -> Right (array
        [encodeString "retained",encodeReceiptIdentity identity,encodeWord64 generation])
      OriginalBindingOwner reference -> Right (array
        [encodeString "original-binding",encodeOriginalBinding reference,encodeReceiptIdentity identity])
      PackageOwner unit name sha -> do
        path <- case Map.lookup (unit,name) packages of
          Just options | [(selected,selectedSha)] <- Set.toAscList options,selectedSha == sha -> Right selected
          _ -> Left "native demand graph lacks an exact package path seal"
        pure (array [encodeString "package",encodeString unit,encodeString name,
          encodeString sha,encodeReceiptIdentity identity,encodeString (T.pack path)])

-- A locally emitted package value may have no incoming global edge. Its exact
-- defining interface must still be retained before it is offered to later
-- turns. One canonical binder proves each module's selected interface;
-- ordinary package demands independently prove their own canonical binder.
-- Noncanonical compiler helpers remain internal and do not supply evidence.
certifyLocalPackageExports :: HscEnv -> PackageWitnessResolver
  -> IORef [PackageWitness] -> [WireProgram] -> IO ()
certifyLocalPackageExports env resolvePackage packageRef programs = do
  observed <- readIORef packageRef
  let witnessed = Set.fromList [(unit, name) | (unit, name, _, _) <- observed]
      candidates = Map.fromListWith (++)
        [ ((symbolUnit identity, symbolModule identity), [identity])
        | program <- programs, group <- programBindings program
        , TopBinding identity binding <- case group of
            NonRecursive top -> [top]
            Recursive tops -> tops
        , not (toUnitId (moduleUnit (symbolOwner identity)) `Set.member` hsc_all_home_unit_ids env)
        , symbolNamespace identity == "value"
        , eligible program (heapBindingRhs binding) ]
  mapM_ (select . Set.toAscList . Set.fromList . snd)
    (Map.toAscList (Map.withoutKeys candidates witnessed))
  where
    eligible _ (Bytes _) = False
    eligible program (Constructor (ConstructorId index) _) =
      maybe False ((== LiftedRefRep) . constructorResultRep)
        (at (programConstructors program) (fromIntegral index))
    eligible _ _ = True
    select [] = pure ()
    select (identity : rest) = resolvePackage identity >>= \case
      Left _ -> select rest
      Right witness -> modifyIORef' packageRef
        ((symbolUnit identity, symbolModule identity, packagePath witness,
          T.pack (packageSha256 witness)) :)

freshGroup :: (T.Text,T.Text) -> ProjectedGroup -> Either OriginalInventoryRefusal CandidateGroup
freshGroup owner group = do
  let body = projectedBody group
  globals <- traverse (\global -> do
    signature <- case globalEntrySignature global of
      Nothing -> Right Nothing
      Just (SignatureId index) -> fmap Just $ maybe
        (Left (MissingFreshSignature owner (fromIntegral (projectedOriginalOrdinal group))
          (globalIdentity global) index (length (projectedSignatures body)))) Right
        (at (projectedSignatures body) (fromIntegral index))
    pure CandidateGlobal
      { candidateGlobalIdentity = globalIdentity global
      , candidateGlobalRep = globalRep global
      , candidateGlobalSignature = signature
      , candidateGlobalEvaluated = globalRequiredEvaluated global
      , candidateGlobalGeneration = fromIntegral <$>
          globalRequiredGeneration global
      }) (projectedGlobals body)
  pure (CandidateGroup (fromIntegral (projectedOriginalOrdinal group))
    (projectedBinders group) globals)

at :: [a] -> Int -> Maybe a
at _ index | index < 0 = Nothing
at values index = case drop index values of
  value : _ -> Just value
  [] -> Nothing

-- Native publication additionally requires ProductReady. This is not a
-- predicate for fresh SOURCE selection, which precedes product admission.
readyNativeProductSourceSha256 :: DependencyEvidence -> String -> String -> Maybe T.Text
readyNativeProductSourceSha256 evidence unit name = do
  node <- find (\item -> dependencyModuleUnit item == unit
    && dependencyModuleName item == name
    && not (dependencyModuleBoot item)
    && dependencyModuleProduct item == ProductReady)
    (dependencyModules evidence)
  source <- find ((== dependencyModuleSource node) . dependencySourcePath)
    (dependencySources evidence)
  pure (T.pack (dependencySourceSha256 source))

encodeGlobalWitness
  :: HscEnv -> PackageWitnessResolver -> IORef [PackageWitness]
  -> Map.Map SymbolIdentity WorldOriginalReference -> Map.Map SymbolIdentity BinderOwner
  -> Set.Set (T.Text, T.Text)
  -> SymbolIdentity -> RuntimeRep -> Maybe Signature -> Bool -> Maybe Word64
  -> IO (Either String GlobalWitness)
encodeGlobalWitness env resolvePackage packageRef references binders homeModules identity rep signature evaluated generation = do
  selected <- case generation of
    Just wanted -> pure (Right (RetainedOwner wanted))
    Nothing | Just reference <- Map.lookup identity references -> pure (Right (OriginalBindingOwner reference))
    Nothing -> case Map.lookup identity binders of
      Just (unit, name, version, ordinal) -> pure (Right (SourceOwner unit name version ordinal))
      Nothing
        | (symbolUnit identity, symbolModule identity) `Set.member` homeModules
            || toUnitId (moduleUnit (symbolOwner identity)) `Set.member` hsc_all_home_unit_ids env ->
            pure (Left ("external home global has no certified source group: " ++ show identity))
        | otherwise -> packageOwner resolvePackage packageRef identity
  pure $ do
    owner <- selected
    Right (identity, rep, signature, evaluated, owner)

symbolOwner :: SymbolIdentity -> Module
symbolOwner identity = mkModule (stringToUnit (T.unpack (symbolUnit identity)))
  (mkModuleName (T.unpack (symbolModule identity)))

packageOwner :: PackageWitnessResolver -> IORef [PackageWitness] -> SymbolIdentity
  -> IO (Either String WitnessOwner)
packageOwner resolvePackage packageRef identity = do
  selected <- resolvePackage identity
  case selected of
    Left reason -> pure (Left reason)
    Right witness -> do
      let sha = T.pack (packageSha256 witness)
      modifyIORef' packageRef ((symbolUnit identity,
        symbolModule identity, packagePath witness, sha) :)
      pure (Right (PackageOwner
        (symbolUnit identity) (symbolModule identity) sha))

-- Capture only witnesses actually selected by this certification. Existing
-- tokens already own their bytes; digest-keyed sharing avoids rereading or
-- copying the same interface for multiple module references.
captureSelectedPackageWitnesses
  :: FilePath -> Map.Map (T.Text,T.Text) (FilePath,ArtifactBytes)
  -> [PackageWitness]
  -> IO (Either String (Map.Map (T.Text,T.Text) (FilePath,ArtifactBytes),[(FilePath,BS.ByteString)]))
captureSelectedPackageWitnesses outDir captured witnesses = do
  let selected = Map.fromListWith Set.union
        [((unit,name),Set.singleton (path,seal)) | (unit,name,path,seal) <- witnesses]
      initial = Map.fromList [(T.pack (artifactSha256 body),body) | (_,body) <- Map.elems captured]
      conflicts = Map.intersectionWith (\options (_,body) ->
        any ((/= T.pack (artifactSha256 body)) . snd) (Set.toList options)) selected captured
  if or (Map.elems conflicts) || any ((/= 1) . Set.size . Set.map snd) (Map.elems selected)
    then pure (Left "package owner selected more than one interface")
    else do
      result <- foldM capture (Right (Map.empty,initial)) (Map.toAscList selected)
      case result of
        Left reason -> pure (Left reason)
        Right (rows,bodies) -> do
          let path seal = outDir </> "package-interfaces" </> T.unpack seal ++ ".hi"
              used = Map.restrictKeys bodies (Set.fromList (Map.elems rows))
              writes = [(path seal,artifactBytes body) | (seal,body) <- Map.toAscList used]
          when (not (null writes)) $ createDirectoryIfMissing True (outDir </> "package-interfaces")
          mapM_ (uncurry BS.writeFile) writes
          pure (Right (Map.map (\seal -> (path seal,bodies Map.! seal)) rows,writes))
  where
    capture failure@(Left _) _ = pure failure
    capture (Right (rows,bodies)) (owner,options) = do
      let (source,seal) = Set.findMin options
      found <- case Map.lookup seal bodies of
        Just body -> pure (Right body)
        Nothing -> do
          bytes <- try (BS.readFile source) :: IO (Either IOException BS.ByteString)
          pure $ case bytes of
            Left _ -> Left "selected package interface is unavailable during capture"
            Right value -> let body = captureArtifactBytes value in
              if T.pack (artifactSha256 body) == seal then Right body
              else Left "selected package interface changed during capture"
      pure $ do
        body <- found
        Right (Map.insert owner seal rows,Map.insert seal body bodies)

type PackageGlobalResolver = SymbolIdentity -> IO (Either String (Id, PackageImportRoot))
type PackageWitnessResolver = SymbolIdentity -> IO (Either String PackageImportRoot)

captureCandidatePackageEvidence :: ModuleCandidate
  -> IO (Either String PackageImportEvidence)
captureCandidatePackageEvidence candidate = do
  let readPart path sha bound = do
        body <- case candidateInputCustody candidate of
          Just custody -> capturedRequestInputToken custody path sha
          Nothing -> captureArtifactBytes <$> readFileAtMost path (bound + 1)
        when (artifactLength body > bound)
          (fail "candidate package capture exceeds its consumer byte bound")
        pure body
  interfaceRead <- try (readPart (candidateInterface candidate)
    (candidateInterfaceSha256 candidate) (32 * 1024 * 1024))
    :: IO (Either IOException ArtifactBytes)
  packageRead <- try (readPart (candidatePackageImports candidate)
    (candidatePackageImportsSha256 candidate) (4 * 1024 * 1024))
    :: IO (Either IOException ArtifactBytes)
  pure $ do
    interfaceBody <- either
      (const (Left "candidate interface is unavailable during package capture"))
      Right interfaceRead
    packageBody <- either
      (const (Left "candidate package evidence is unavailable during package capture"))
      Right packageRead
    if artifactSha256 interfaceBody /= candidateInterfaceSha256 candidate
        || artifactSha256 packageBody /= candidatePackageImportsSha256 candidate
      then Left "candidate package evidence changed during package capture"
      else decodeCapturedPackageImports
        (ExactIfaceArtifact
          (candidateUnit candidate)
          (candidateModule candidate)
          (candidateInterface candidate)
          (candidateInterfaceSha256 candidate)
          [])
        (artifactBytes packageBody)

data PackageDeclarationCatalog = PackageDeclarationCatalog
  { catalogOwner :: Module
  , catalogParents :: Map.Map OccName [Name]
  , catalogWiredGlobals :: Map.Map OccName (Map.Map Name TyThing)
  }

-- The captured interface owns canonical parent Names and wired exports. Index
-- their occurrences once; loading a demanded parent still uses GHC's interface
-- loader and never reconstructs a Name from its spelling.
packageDeclarationCatalog :: Module -> ModIface -> PackageDeclarationCatalog
packageDeclarationCatalog owner iface = PackageDeclarationCatalog owner parents wired
  where
    owned name = nameModule_maybe name == Just owner
    parents = Map.fromListWith (flip (++))
      [ (occurrence, [ifName declaration])
      | (_, declaration) <- mi_decls iface
      , owned (ifName declaration)
      , occurrence <- Set.toAscList (Set.fromList
          (nameOccName (ifName declaration) : ifaceDeclImplicitBndrs declaration))
      , isVarOcc occurrence ]
    wired = Map.fromListWith Map.union
      [ (nameOccName name, Map.singleton name thing)
      | (name, thing) <- packageWiredGlobals owner iface ]

packageWiredGlobals :: Module -> ModIface -> [(Name, TyThing)]
packageWiredGlobals owner iface =
  [ (name, thing)
  | available <- mi_exports iface, exported <- availNames available
  , owned exported
  , parent <- maybe [] pure (wiredInNameTyThing_maybe exported)
  , thing@(AnId _) <- parent : implicitTyThings parent
  , let name = getName thing
  , owned name, isVarOcc (nameOccName name) ]
  where
    owned name = nameModule_maybe name == Just owner

type PackageGlobalLookup = OccName -> IO (MaybeErr () TyThing)

-- This capture belongs to one certification. A module's defining interface is
-- checked around its read; each demanded symbol is still resolved separately.
-- Successful witnesses are checked again before publication, and nothing is
-- retained across requests or changes to the compiler environment.
newPackageGlobalResolver :: Bool -> HscEnv -> IO (PackageGlobalResolver, IO (Integer, Integer, Integer))
newPackageGlobalResolver timing env = do
  state <- newIORef (0, Map.empty)
  let load owner = do
        (_, interfaces) <- readIORef state
        case Map.lookup owner interfaces of
          Just value -> pure value
          Nothing -> do
            loaded <- loadPackageInterface env owner
            value <- case loaded of
              Left reason -> pure (Left reason)
              Right (iface, witness) -> do
                unchanged <- validatePackageImportRoot env witness
                let catalog = packageDeclarationCatalog owner iface
                pure ((canonicalPackageGlobal env catalog, witness) <$ unchanged)
            modifyIORef' state (\(requests, selected) ->
              (requests, Map.insert owner value selected))
            pure value
      resolve identity = do
        when timing $ modifyIORef' state (\(requests, interfaces) ->
          let next = requests + 1 in next `seq` (next, interfaces))
        resolvePackageGlobalUsing load identity
      counts = do
        (requests, interfaces) <- readIORef state
        pure (requests, fromIntegral (Map.size interfaces),
          fromIntegral (length [() | Right _ <- Map.elems interfaces]))
  pure (resolve, counts)

-- Recovery and certification share one canonical package owner. The Id comes
-- from the exact interface's structural declaration, including implicit tops.
resolvePackageGlobal :: HscEnv -> PackageGlobalResolver
resolvePackageGlobal env identity = do
  let load owner = do
        found <- loadPackageInterface env owner
        pure $ fmap (\(iface, witness) ->
          (canonicalPackageGlobalOnce env owner iface, witness)) found
  selected <- resolvePackageGlobalUsing load identity
  case selected of
    Left reason -> pure (Left reason)
    Right (identifier, witness) -> do
      unchanged <- validatePackageImportRoot env witness
      pure $ case unchanged of
        Left reason -> Left (packageRefusal identity reason)
        Right () -> Right (identifier, witness)

loadPackageInterface :: HscEnv -> Module
  -> IO (Either String (ModIface, PackageImportRoot))
loadPackageInterface env owner = do
  found <- packageImportRoot env owner
  case found of
    Left reason -> pure (Left reason)
    Right witness -> do
      exact <- readPackageInterface env owner
      case exact of
        Right (iface, location) | ml_hi_file location == packagePath witness ->
          pure (Right (iface, witness))
        _ -> pure (Left "selected package interface is unavailable or changed")

resolvePackageGlobalUsing :: (Module -> IO (Either String (PackageGlobalLookup, PackageImportRoot)))
  -> PackageGlobalResolver
resolvePackageGlobalUsing load identity
  | symbolNamespace identity /= "value" =
      pure (Left (refusal "unsupported external global namespace"))
  | otherwise = do
      let owner = symbolOwner identity
      found <- load owner
      case found of
        Left reason -> pure (Left (refusal reason))
        Right (lookupGlobal, witness) -> do
          selected <- lookupGlobal (mkVarOcc (T.unpack (symbolOccurrence identity)))
          pure $ case selected of
            Succeeded (AnId identifier) -> Right (identifier, witness)
            _ -> Left (refusal "selected package global is absent from loaded interface")
  where
    refusal = packageRefusal identity

packageRefusal :: SymbolIdentity -> String -> String
packageRefusal identity reason = reason ++ ": " ++ T.unpack (symbolUnit identity) ++ ":"
  ++ T.unpack (symbolModule identity) ++ "." ++ T.unpack (symbolOccurrence identity)

-- The interface supplies canonical Names, including known-key bindings.
-- An implicit Id is authenticated by its defining parent declaration; wired
-- parents are authenticated by the exact interface's exported canonical Name.
-- Reconstructing a Name from Module/OccName can mint a different Unique.
canonicalPackageGlobal :: HscEnv -> PackageDeclarationCatalog -> OccName
  -> IO (MaybeErr () TyThing)
canonicalPackageGlobal env catalog wanted = canonicalPackageCandidates env
  (catalogOwner catalog)
  (Map.findWithDefault [] wanted (catalogParents catalog))
  (Map.findWithDefault Map.empty wanted (catalogWiredGlobals catalog)) wanted

-- A standalone recovery lookup scans only its demanded occurrence. It has no
-- certification owner to amortize construction of the complete catalog.
canonicalPackageGlobalOnce :: HscEnv -> Module -> ModIface -> PackageGlobalLookup
canonicalPackageGlobalOnce env owner iface wanted = canonicalPackageCandidates env owner
  [ ifName declaration
  | (_, declaration) <- mi_decls iface
  , nameModule_maybe (ifName declaration) == Just owner
  , nameOccName (ifName declaration) == wanted
      || wanted `elem` ifaceDeclImplicitBndrs declaration ]
  (Map.fromList [(name, thing) | (name, thing) <- packageWiredGlobals owner iface
    , nameOccName name == wanted]) wanted

canonicalPackageCandidates :: HscEnv -> Module -> [Name] -> Map.Map Name TyThing
  -> PackageGlobalLookup
canonicalPackageCandidates env owner declarations wired wanted = do
  parents <- mapM loadCanonical declarations
  let owned name = nameModule_maybe name == Just owner
      declared = Map.fromList
        [ (name, thing)
        | Succeeded parent <- parents
        , thing@(AnId _) <- parent : implicitTyThings parent
        , let name = getName thing
        , owned name, isVarOcc (nameOccName name), nameOccName name == wanted ]
      candidates = Map.union wired declared
  pure $ case Map.elems candidates of
    [thing] -> Succeeded thing
    _ -> Failed ()
  where
    loadCanonical :: Name -> IO (MaybeErr () TyThing)
    loadCanonical name = case wiredInNameTyThing_maybe name of
      Just thing -> pure (Succeeded thing)
      Nothing -> do
        result <- initIfaceLoad env (importDecl name)
        pure $ case result of
          Failed _ -> Failed ()
          Succeeded thing -> Succeeded thing

encodeModule :: Product -> [(Word, [Encoding])] -> Encoding
encodeModule product groups = array
  [ encodeString (renderProductOrigin (productOrigin product))
  , encodeString (productUnit product), encodeString (productModule product)
  , maybe encodeNull encodeString (productVersion product)
  , encodeString (productSourceSha product)
  , encodeString (productIfaceSha product)
  , encodeString (productBytesSha product)
  , encodeString (productEvidenceSha product)
  , list (\(ordinal, globals) -> array [encodeWord ordinal, list id globals]) groups
  , list (\(unit, name, sha) -> array [encodeString unit, encodeString name, encodeString sha])
      (productInterfaces product)
  ]

-- | Symbol identity in compiler receipts: an optional parent is null or text.
-- The execution schema has a distinct tagged-option grammar.
encodeReceiptIdentity :: SymbolIdentity -> Encoding
encodeReceiptIdentity identity = array
  [ encodeString (symbolUnit identity), encodeString (symbolModule identity)
  , encodeString (symbolNamespace identity), encodeString (symbolOccurrence identity)
  , maybe encodeNull encodeString (symbolRecordParent identity) ]

encodeRep :: RuntimeRep -> Encoding
encodeRep rep = case rep of
  VoidRep -> named "void" 0
  LiftedRefRep -> named "lifted" 0
  UnliftedRefRep -> named "unlifted" 0
  AddressRep -> named "address" 0
  IntRep bits -> named "int" (fromIntegral bits)
  WordRep bits -> named "word" (fromIntegral bits)
  FloatRep bits -> named "float" (fromIntegral bits)
  where named name width = array [encodeString name, encodeWord width]

encodeSignature :: Signature -> Encoding
encodeSignature signature = array
  [ list encodeRep (signatureArguments signature)
  , case signatureResults signature of
      Returns reps -> array [encodeString "returns", list encodeRep reps]
      NoSuccess -> array [encodeString "no_success", list encodeRep []]
      CallerResult -> array [encodeString "caller_result", list encodeRep []]
  ]

array :: [Encoding] -> Encoding
array fields = encodeListLen (fromIntegral (length fields)) <> fold fields

list :: (a -> Encoding) -> [a] -> Encoding
list encode values = encodeListLen (fromIntegral (length values)) <> foldMap encode values

digest :: BS.ByteString -> T.Text
digest bytes = T.pack (concatMap byteHex (BS.unpack (SHA256.hash bytes)))
  where byteHex byte = let rendered = showHex byte "" in
          replicate (2 - length rendered) '0' ++ rendered
