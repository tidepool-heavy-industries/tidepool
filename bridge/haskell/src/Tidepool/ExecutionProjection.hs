module Tidepool.ExecutionProjection
  ( ProjectionContext(..)
  , ProjectionError(..)
  , projectPrepared
  , projectPreparedTarget
  , projectPreparedTargetWithConstructors
  , ProjectedGroup(..), ProjectedGroupBody(..)
  , projectPreparedModuleGroups, projectPreparedModuleGroupsSelected
  , PreparedModuleProducts, OriginalGroupOmission(..), OriginalGroupOmissionReason(..)
  , projectPreparedModuleProducts, projectOriginalHomeModuleProducts
  , projectOriginalHomeModuleProductDemand
  , RawModuleProducts, projectRawOriginalHomeModuleProducts, forceRawModuleProducts
  , OriginalProjectionCache, newOriginalProjectionCache, copyOriginalProjectionCache, mergeOriginalProjectionCaches, selectOriginalProjectionCaches
  , evictOriginalProjectionMatching, projectCachedOriginalHomeModuleProducts
  , lookupCachedOriginalHomeModuleProducts
  , rawOriginalProductOwner, rawOriginalProductBinders, rawOriginalProductDemands
  , rawOriginalGroupEncodings
  , settleOriginalHomeModuleProducts, settleOriginalHomeModuleProductsWithoutOwners
  , preparedModuleProductOutcomes, preparedModuleProductOmissions, preparedModuleProductConstructors, preparedModuleProductYieldSites
  , closeUnavailableOriginalGroups, closeUnavailableOriginalModules
  , PreparedProjection
  , prepareProjection
  , prepareProjectionWithReachability
  , prepareComponentProjectionWithReachability
  , projectSelected
  , projectSelectedWithHostBindings
  , PreparedCandidate, projectSelectedCandidateWithHostBindings
  , candidateGlobals, finalizePreparedCandidate
  , preparedTopIdentities, preparedTopIdentityBindings
  , preparedTargetReferences
  , ReferenceFact(..)
  , preparedModuleReferenceFacts
  , combinePreparedTargetReferences
  , preparedModuleReachFacts
  , preparedSeedUniques
  , preparedRootIdentity
  , PreparedReachability(..)
  , emptyPreparedReachability
  , admitReachFacts
  , PreparedReachUpdate(..), updatePreparedReachability
  , PreparedReferenceWorklist, emptyPreparedReferenceWorklist
  , admitPreparedReferenceUnits, discoverPreparedReferences
  , topBinders
  , projectLiteralAtomForTest
  , assignTopIdentitySpellings
  , resolveTextPackageUnit
  , TextUnitAuthority(..)
  ) where

import Control.Exception (evaluate)
import Control.Concurrent.MVar (MVar, newMVar, readMVar, modifyMVar_)
import Control.Monad (foldM, forM, forM_, unless, when)
import Control.Monad.State.Strict
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.IntMap.Strict qualified as IntMap
import Data.Foldable (toList)
import Data.Sequence (Seq, (|>))
import Data.Sequence qualified as Seq
import Data.Maybe (fromMaybe, isJust, isNothing, listToMaybe, mapMaybe)
import Tidepool.PreparedBuiltins
  ( DeferredFunction(..), deferredFunction, wiredInErrorKind )
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text (Text)
import Data.Text qualified as Text
import Data.Text.Encoding qualified as TextEncoding
import Data.Word (Word32, Word64, Word8)
import GHC.Builtin.PrimOps (PrimOp(..), PrimCall(..), primOpOcc)
import GHC.Builtin.Types (doubleDataCon, intDataCon, intTy, promotedConsDataCon, promotedNilDataCon)
import GHC.Core (AltCon(..))
import GHC.Core.DataCon
  ( DataCon, dataConName, dataConTheta, dataConOrigArgTys, dataConRepArgTys, dataConRepArity, dataConWorkId
  , dataConTag, dataConTyCon, dataConOrigResTy, dataConImplBangs, HsImplBang(..)
  , isMarkedStrict, isUnboxedTupleDataCon )
import GHC.Core.TyCo.Rep (Scaled(..), Type(..))
import GHC.Core.TyCo.FVs (tyCoVarsOfType)
import GHC.Core.Type (splitFunTys, splitTyConApp_maybe)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCon qualified as GHC
import GHC.Data.FastString (fsLit, unpackFS)
import GHC.Driver.Env.Types (HscEnv, hsc_unit_env)
import GHC.Driver.Env (hsc_all_home_unit_ids)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import GHC.Stg.Syntax
import GHC.Stg.Syntax qualified as Stg
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Types.Demand (splitDmdSig)
import GHC.Types.Literal (LitNumType(..), Literal(..), literalType)
import GHC.Types.Id (idDmdSig, isDeadEndId, isDataConWorkId_maybe)
import GHC.Types.ForeignCall qualified as Foreign
import GHC.Types.Name (Name, isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (isDataOcc, occNameString)
import GHC.Types.RepType
  (typePrimRep_maybe, runtimeRepPrimRep_maybe, dataConRuntimeRepStrictness, unwrapType)
import GHC.Types.Unique.Set (UniqSet, addListToUniqSet, addOneToUniqSet, elementOfUniqSet, emptyUniqSet, mkUniqSet, nonDetEltsUniqSet)
import GHC.Types.Unique (Unique, getKey)
import GHC.Types.Unique.FM
  (UniqFM, addToUFM, emptyUFM, listToUFM, lookupUFM, nonDetEltsUFM)
import GHC.Types.Var (Id, varName, varType, varUnique)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Types.Var.Set (dVarSetElems, isEmptyVarSet)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.Module (ModuleName, mkModule, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.ModIface (ModIface, mi_module)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Types.PkgQual (PkgQual(OtherPkg))
import GHC.Unit.State (lookupPackageName)
import GHC.Unit.Types (Module, Unit, UnitId, stringToUnit, toUnitId, unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExecutionIR (topBindingReferenceUniques, topBindingReferences)
import Tidepool.ExecutionEncode
  (ProjectedGroupEncoding, prepareProjectedGroupEncoding, projectedGroupEncodingBytes)
import Tidepool.ExecutionSchema
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.PreparedFacts (PreparedFacts(..), extractPreparedFacts)
import Tidepool.Identity (nameSymbolIdentity, varId)
import Tidepool.FatIface (fatOriginalOwner)
import Tidepool.PreparedStg
  ( PreparedModule, PreparedCoverage(..), pmModule, pmCoverage, pmBindings
  , pmTypeGraph, pmPreparedSites, pmSiteRejections, pmRequestSiteTyCon, pmStableTopSpellings, pmOriginalTopNames
  , preparedBindingGroups, filterPreparedBindings, preparedRejectsIntrinsic, preparedExpectedEntry
  , PreparedComponents, preparedComponentModules, preparedComponentVersion )
import Tidepool.PreparedSites (SiteRejection(..))
import Tidepool.PreparedSites (PreparedSite(..), requestReplyIndex)
import Tidepool.EffectSchema qualified as Effect
import Tidepool.TypePolicy qualified as TypePolicy
import Tidepool.PreparedFormatting
  (FormattingAuthority, FormattingSpec(..), FormattingIntrinsic(..), classifyFormatting)
import Tidepool.PreparedTime (TimeAuthority, TimeSpec(..), classifyTime)
import Tidepool.HostBindingAuthority
  ( HostBindingRepresentation, hostBindingRepresentationConstructors
  , hostBindingRepresentationJsonAuthority )
import Tidepool.PreparedJson
  ( JsonAuthority, JsonSpec(..), classifyJson, jsonAuthorityLayout, jsonValueLayoutForType )
import System.Mem.StableName (StableName, makeStableName)

data ProjectionContext = ProjectionContext
  { projectionProfile :: Text
  , projectionToolchain :: Text
  , projectionTarget :: TargetDescriptor
  , projectionRetainedGenerations :: Map SymbolIdentity Word64
  -- Exact native originals admitted for this compiler batch; source imports
  -- carry no retained generation.
  , projectionCurrentOriginals :: Map Name SymbolIdentity
  , projectionEntry :: SymbolIdentity
  -- | Additional tops seeded into reachability beside 'projectionEntry'
  -- (a turn's resume entry). Optional: an absent root is not an error;
  -- the consumer checks the artifact for the entries it needs.
  , projectionAuxiliaryRoots :: [SymbolIdentity]
  , projectionFormattingAuthority :: Maybe FormattingAuthority
  , projectionTimeAuthority :: Maybe TimeAuthority
  , projectionJsonAuthority :: Maybe JsonAuthority
  -- | Missing authority rejects text's kernel, not unrelated projection.
  , projectionTextUnit :: Maybe TextUnitAuthority
  } deriving stock (Eq)

data ProjectionError
  = UnsupportedPreparedShape Text
  | InvalidPreparedIdentity Text
  | InvalidPreparedRepresentation Text
  | InvalidPreparedLayout Text
  | MissingPreparedEntry SymbolIdentity
  | MissingPreparedTop SymbolIdentity
  | UnboundPreparedInternal Text
  | DeferredFunctionSignatureMismatch SymbolIdentity Signature (Maybe Signature)
  | UnsupportedPrimitiveCall Text Signature
  | UnsupportedForeignCall Text Signature
  | UnavailableOriginalHomeDependencies [SymbolIdentity]
  | UnelaboratedCompilerIntrinsic SymbolIdentity
  | RecoveredEntryContractMismatch SymbolIdentity
      (Maybe Signature) Bool (Maybe Signature) Bool
  -- | A typed site in a reachable top cannot carry concrete evidence. This
  -- is a source error, reported with the compiler's own guidance.
  | RejectedTypedSite Text
  | TypeEvidenceIssuanceFailure TypePolicy.TypeGraphError
  deriving stock (Eq, Show)

data ProjectionPurpose
  = ExecutableTarget
  | OriginalHomeProduct (Module -> Bool)

data EntryContractAdmission = FinalEntryContracts | DeferredEntryContracts

data PState = PState
  { nextValue :: Word32, nextJoin :: Word32
  , values :: VarEnv ValueId, joins :: VarEnv (JoinId, [RuntimeRep])
  , entryArities :: VarEnv Int
  , topSymbols :: VarEnv SymbolIdentity, topValues :: Map SymbolIdentity ValueId
  , implicitTops :: [TopBinding]
  , implicitValues :: Map SymbolIdentity ValueId
  -- Tables retain first-encounter order; each ID is its declaration position.
  -- Lookup indexes and counters share the projection's representation rollback.
  , nextGlobal :: !Word32, nextConstructor :: !Word32
  , nextOperation :: !Word32, nextSignature :: !Word32
  , globals :: VarEnv GlobalId, globalDecls :: Seq GlobalDecl
  , constructors :: Seq (DataCon, ConstructorId), constructorDecls :: Seq ConstructorDecl
  , constructorIndex :: Map SymbolIdentity (ConstructorId, ConstructorDecl)
  , operations :: Map (Schema.OperationIdentity, SignatureId) OperationId
  , operationDecls :: Seq OperationDecl
  , signatures :: Seq Signature
  , signatureIndex :: Map ([RuntimeRep], ResultContract) SignatureId
  , target :: TargetDescriptor
  , retainedGenerations :: Map SymbolIdentity Word64
  , homeModules :: Set (Text, Text)
  , formattingAuthority :: Maybe FormattingAuthority
  , timeAuthority :: Maybe TimeAuthority
  , jsonAuthority :: Maybe JsonAuthority
  , textUnit :: Maybe TextUnitAuthority
  -- Tops outside the group currently being projected become explicit imports.
  , externalizedTops :: Set SymbolIdentity
  , projectionPurpose :: ProjectionPurpose
  , entryContractAdmission :: EntryContractAdmission
  , entryContractFailures :: [ProjectionError]
  }

type P a = StateT PState (Either ProjectionError) a

-- | text's C kernels admitted as prepared intrinsics, with their exact ABI.
-- The Rust recognizer (`tidepool/codegen/src/prepared_program/text_search.rs`)
-- accepts exactly these signatures.
textKernels :: [(String, Signature)]
textKernels =
  [ ("_hs_text_memchr", Signature
      [UnliftedRefRep, WordRep 64, WordRep 64, WordRep 8, VoidRep] (Returns [IntRep 64]))
  , ("_hs_text_measure_off", Signature
      [UnliftedRefRep, WordRep 64, WordRep 64, WordRep 64, VoidRep] (Returns [IntRep 64]))
  , ("_hs_text_reverse", Signature
      [UnliftedRefRep, UnliftedRefRep, WordRep 64, WordRep 64, VoidRep] (Returns []))
  ]

-- | Authority is a compiler-resolved unit, never a package-name prefix.
newtype TextUnitAuthority = TextUnitAuthority Unit deriving stock (Eq)

instance Show TextUnitAuthority where
  show (TextUnitAuthority unit) = unitString unit

-- | Resolve the text package selected by GHC's unit database, then ask its
-- module finder for the kernel's provider in that exact package. The explicit
-- package qualifier excludes home-module shadows. Failure grants no authority.
resolveTextPackageUnit :: HscEnv -> IO (Maybe TextUnitAuthority)
resolveTextPackageUnit hscEnv =
  case lookupPackageName (ue_units (hsc_unit_env hscEnv)) (PackageName (fsLit "text")) of
    Nothing -> pure Nothing
    Just selected -> do
      found <- findImportedModule hscEnv (mkModuleName "Data.Text.Internal.Search")
        (OtherPkg selected)
      pure $ case found of
        Found _ owner -> Just (TextUnitAuthority (moduleUnit owner))
        _ -> Nothing

-- | Narrow test seam for GHC literals which cannot be written in source Haskell.
projectLiteralAtomForTest :: TargetDescriptor -> Literal -> Either ProjectionError Atom
projectLiteralAtomForTest machine literal = evalStateT (projectLiteralAtom literal)
  (PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv emptyVarEnv Map.empty [] Map.empty
    0 0 0 0 emptyVarEnv Seq.empty Seq.empty Seq.empty Map.empty Map.empty Seq.empty Seq.empty Map.empty
    machine Map.empty Set.empty Nothing Nothing Nothing Nothing Set.empty ExecutableTarget FinalEntryContracts [])

projectPrepared :: ProjectionContext -> [PreparedModule] -> Either ProjectionError WireProgram
projectPrepared _ [] = Left (UnsupportedPreparedShape "execution program has no modules")
projectPrepared context modules =
  fst <$> projectPreparedWithTopSymbols context modules (buildTopIdentityMap modules)

-- Preserve the original STG position before retained tops are removed. A
-- recursive group is projected as one indivisible unit, with its own tables.
projectPreparedModuleGroups :: ProjectionContext -> PreparedModule
  -> Either ProjectionError [ProjectedGroup]
projectPreparedModuleGroups context prepared =
  projectPreparedModuleGroupsSelected context prepared Nothing

-- One compilation owns these outcomes for package closure and original-product
-- publication. Original groups retain their own projection result and ordinal;
-- executable-target products preserve the all-or-nothing result.
data OriginalGroupOmissionReason
  = ProjectionFailed ProjectionError
  | DependsOnUnavailable [SymbolIdentity]
  deriving stock (Eq, Show)

data OriginalGroupOmission = OriginalGroupOmission
  { omittedOriginalOrdinal :: Word32
  , omittedOriginalBinders :: [SymbolIdentity]
  , omittedOriginalReason :: OriginalGroupOmissionReason
  } deriving stock (Eq, Show)

-- The wire arena and its compiler provenance are one lowering result. Neither
-- caching nor dependency settlement can retain one without the other.
data ProjectedGroupEvidence = ProjectedGroupEvidence
  { projectedGroupWire :: ProjectedGroup
  , projectedGroupConstructors :: [DataCon]
  , projectedGroupYieldSites :: [Effect.YieldSite]
  }

newtype PreparedModuleProducts = PreparedModuleProducts
  [(Module, Either ProjectionError [ProjectedGroupEvidence], [OriginalGroupOmission])]

projectPreparedModuleProducts :: ProjectionContext -> [PreparedModule] -> PreparedModuleProducts
projectPreparedModuleProducts context modules = PreparedModuleProducts
  [(pmModule prepared, traverse (\(_,_,result) -> result)
      (projectPreparedModuleGroupResultsFor ExecutableTarget context prepared Nothing), [])
    | prepared <- modules]

-- The compiler's actual home unit, complete source coverage and current module
-- own original issuance. Package globals are sealed later against their exact
-- canonical defining interfaces; executable targets keep their live imports.
projectOriginalHomeModuleProducts :: HscEnv -> Map ModuleName ModIface
  -> ProjectionContext -> Set SymbolIdentity -> [PreparedModule]
  -> PreparedModuleProducts
projectOriginalHomeModuleProducts env interfaces context externalBinders modules =
  fst (projectOriginalHomeModuleProductDemand env interfaces context externalBinders modules)

-- Actual projected home globals request defining code. Canonical interfaces,
-- lexical imports and type requirements do not supply original native groups.
projectOriginalHomeModuleProductDemand :: HscEnv -> Map ModuleName ModIface
  -> ProjectionContext -> Set SymbolIdentity -> [PreparedModule]
  -> (PreparedModuleProducts, Set SymbolIdentity)
projectOriginalHomeModuleProductDemand env interfaces context externalBinders modules =
  settleOriginalHomeModuleProducts env externalBinders
    (map (projectRawOriginalHomeModuleProducts env interfaces context) modules)

-- Independent lowering retains each group's original ordinal and failure.
-- Missing owners remain demands until the compiler worklist has settled.
data RawModuleProducts = RawModuleProducts
  { rawOriginalProductOwner :: Module
  , rawOriginalProductGroups :: Maybe [(Word32, [SymbolIdentity], Either ProjectionError ProjectedGroupEvidence)]
  , rawExecutableProduct :: Either ProjectionError [ProjectedGroupEvidence]
  , rawOriginalGroupEncodings :: Map Word32 ProjectedGroupEncoding
  }

-- Raw facts retain exact prepared compiler objects and projection authority.
-- Home-unit classification and interface ownership also affect projection;
-- neither a nominal owner nor a request identifier admits a hit.
data OriginalProjectionEntry = OriginalProjectionEntry
  (StableName PreparedModule) (Set Name) (Set SymbolIdentity) (Set SymbolIdentity)
  ProjectionContext (Set UnitId) Bool RawModuleProducts

newtype OriginalProjectionCache = OriginalProjectionCache
  (MVar (Map Module [OriginalProjectionEntry]))

newOriginalProjectionCache :: IO OriginalProjectionCache
newOriginalProjectionCache = OriginalProjectionCache <$> newMVar Map.empty

copyOriginalProjectionCache :: OriginalProjectionCache -> IO OriginalProjectionCache
copyOriginalProjectionCache (OriginalProjectionCache entries) =
  OriginalProjectionCache <$> (readMVar entries >>= newMVar)

-- | Keep all selected immutable projection alternatives. Entries from earlier
-- sources precede later ones, preserving first matching-entry priority.
mergeOriginalProjectionCaches :: [(OriginalProjectionCache, Module -> Bool)] -> IO OriginalProjectionCache
mergeOriginalProjectionCaches sources = do
  selected <- mapM (\(OriginalProjectionCache ref, keep) ->
    Map.filterWithKey (\owner _ -> keep owner) <$> readMVar ref) sources
  OriginalProjectionCache <$> newMVar (Map.unionsWith (++) selected)

selectOriginalProjectionCaches :: [(OriginalProjectionCache, Set Module)] -> IO OriginalProjectionCache
selectOriginalProjectionCaches sources = do
  selected <- mapM (\(OriginalProjectionCache ref, owners) -> do
    entries <- readMVar ref
    pure (Map.fromAscList [(owner, alternatives) | owner <- Set.toAscList owners
      , Just alternatives <- [Map.lookup owner entries]])) sources
  OriginalProjectionCache <$> newMVar (Map.unionsWith (++) selected)

evictOriginalProjectionMatching :: OriginalProjectionCache -> (Module -> Bool) -> IO ()
evictOriginalProjectionMatching (OriginalProjectionCache entries) stale =
  modifyMVar_ entries (pure . Map.filterWithKey (\owner _ -> not (stale owner)))

normalizeOriginalProjectionContext :: Set Name -> Set SymbolIdentity -> Set SymbolIdentity
  -> ProjectionContext -> ProjectionContext
normalizeOriginalProjectionContext names symbols tops context = context
  { projectionEntry = SymbolIdentity "" "" "value" "" Nothing
  , projectionRetainedGenerations = Map.restrictKeys (projectionRetainedGenerations context) symbols
  , projectionCurrentOriginals = Map.restrictKeys (projectionCurrentOriginals context) names
  , projectionAuxiliaryRoots = Set.toAscList (Set.fromList (projectionAuxiliaryRoots context)
      `Set.intersection` tops) }

lookupCachedOriginalHomeModuleProducts :: OriginalProjectionCache -> HscEnv
  -> Map ModuleName ModIface -> ProjectionContext -> PreparedModule -> IO (Maybe RawModuleProducts)
lookupCachedOriginalHomeModuleProducts (OriginalProjectionCache entries) env interfaces context prepared = do
  identity <- evaluate prepared >>= makeStableName
  let owner = pmModule prepared
      homes = hsc_all_home_unit_ids env
      hasInterface = maybe False ((== owner) . mi_module) (Map.lookup (moduleName owner) interfaces)
      matches (OriginalProjectionEntry old names symbols tops oldContext oldHomes oldInterface _) =
        identity == old && homes == oldHomes && hasInterface == oldInterface
          && normalizeOriginalProjectionContext names symbols tops context == oldContext
  known <- Map.findWithDefault [] owner <$> readMVar entries
  pure (listToMaybe [raw | entry@(OriginalProjectionEntry _ _ _ _ _ _ _ raw) <- known, matches entry])

projectCachedOriginalHomeModuleProducts :: OriginalProjectionCache -> HscEnv
  -> Map ModuleName ModIface -> ProjectionContext -> PreparedModule -> IO (Bool,RawModuleProducts)
projectCachedOriginalHomeModuleProducts cache@(OriginalProjectionCache entries) env interfaces context prepared = do
  hit <- lookupCachedOriginalHomeModuleProducts cache env interfaces context prepared
  case hit of
    Just raw -> pure (True,raw)
    Nothing -> project
 where
  project = do
    identity <- evaluate prepared >>= makeStableName
    let owner = pmModule prepared
        homes = hsc_all_home_unit_ids env
        hasInterface = maybe False ((== owner) . mi_module) (Map.lookup (moduleName owner) interfaces)
        identities = preparedTopIdentityBindings [prepared]
        facts = extractPreparedFacts owner (map fst (pmBindings prepared))
        identifiers = preparedReferencedIds facts
          ++ concatMap snd (preparedClosureCaptures facts)
          ++ [binder | (binding,_) <- pmBindings prepared, binder <- topBinders binding]
        names = Set.fromList (map varName identifiers)
        symbols = Set.fromList (Map.elems identities ++ map (idSymbol "value") identifiers)
        tops = Set.fromList (Map.elems identities)
        normalized = normalizeOriginalProjectionContext names symbols tops context
    raw <- forceRawModuleProducts (projectRawOriginalHomeModuleProducts env interfaces context prepared)
    modifyMVar_ entries (pure . Map.insertWith (++) owner
      [OriginalProjectionEntry identity names symbols tops normalized homes hasInterface raw])
    pure (False,raw)

projectRawOriginalHomeModuleProducts :: HscEnv -> Map ModuleName ModIface
  -> ProjectionContext -> PreparedModule -> RawModuleProducts
projectRawOriginalHomeModuleProducts env interfaces context prepared =
  RawModuleProducts owner original executable encodings
  where
    owner = pmModule prepared
    isHome modul = toUnitId (moduleUnit modul) `Set.member` hsc_all_home_unit_ids env
    original = case Map.lookup (moduleName owner) interfaces of
      Just interface | pmCoverage prepared == CompleteSourceModule && isHome owner
        && mi_module interface == owner -> Just
          (projectPreparedModuleGroupResultsFor (OriginalHomeProduct isHome) context prepared Nothing)
      _ -> Nothing
    executable = traverse (\(_,_,result) -> result)
      (projectPreparedModuleGroupResultsFor ExecutableTarget context prepared Nothing)
    encodings = Map.fromList [(ordinal,prepareProjectedGroupEncoding (projectedGroupWire group))
      | outcomes <- maybe [] pure original, (ordinal,_,Right group) <- outcomes]

-- Force the local lowering on its executor worker, before incorporation.
-- Failed groups remain values; only infrastructure exceptions abort the batch.
forceRawModuleProducts :: RawModuleProducts -> IO RawModuleProducts
forceRawModuleProducts raw = do
  selected <- case rawOriginalProductGroups raw of
    Just outcomes -> do
      _ <- evaluate (Set.size (rawOriginalProductDemands raw)
        + Set.size (rawOriginalProductBinders raw)
        + sum (map (BS.length . projectedGroupEncodingBytes)
            (Map.elems (rawOriginalGroupEncodings raw))))
      pure [group | (_,_,Right group) <- outcomes]
    Nothing -> do
      outcome <- evaluate (rawExecutableProduct raw)
      case outcome of
        Left _ -> pure []
        Right groups -> pure groups
  -- Consume the census spine and exact compiler identities on the worker,
  -- releasing the lazy traversal of the completed projection state. DataCons
  -- themselves remain paired with their original compiler environment lifetime.
  forM_ selected $ \group -> do
    forM_ (projectedGroupConstructors group) $ \con -> do
      let identity = nameSymbol "constructor" (dataConName con)
      _ <- evaluate (varId (dataConWorkId con))
      _ <- evaluate (dataConTag con)
      _ <- evaluate (Text.length (symbolUnit identity) + Text.length (symbolModule identity)
        + Text.length (symbolNamespace identity) + Text.length (symbolOccurrence identity)
        + maybe 0 Text.length (symbolRecordParent identity))
      pure ()
    forM_ (projectedGroupYieldSites group) $ \site -> do
      _ <- evaluate (Effect.ysSite site)
      _ <- evaluate (Effect.ysOrdinal site)
      _ <- evaluate (Text.length (Effect.ysOrigin site)
        + siteTypeSize (Effect.ysAnswer site)
        + sum (map siteTypeSize (Effect.ysInputs site))
        + length (Effect.ysInputTypeWitnesses site)
        + maybe 0 Text.length (Effect.ysReplyDeclaration site))
      pure ()
  pure raw
 where
  siteTypeSize siteType = Text.length (Effect.stType siteType)
    + sum (map Text.length (Effect.stModules siteType))
    + sum [Text.length unit + Text.length owner + Text.length occurrence
          | Effect.NominalHead unit owner occurrence <- Effect.stHeads siteType]

rawOriginalProductBinders :: RawModuleProducts -> Set SymbolIdentity
rawOriginalProductBinders raw = Set.fromList
  [symbol | outcomes <- maybe [] pure (rawOriginalProductGroups raw)
    , (_, symbols, _) <- outcomes, symbol <- symbols]

rawOriginalProductDemands :: RawModuleProducts -> Set SymbolIdentity
rawOriginalProductDemands raw = Set.fromList
  [globalIdentity global | outcomes <- maybe [] pure (rawOriginalProductGroups raw)
    , (_, _, Right evidence) <- outcomes
    , global <- projectedGlobals (projectedBody (projectedGroupWire evidence))
    , globalRequiredGeneration global == Nothing]

-- Settlement runs only after queued/running work and completion-driven demand
-- are empty. Native cycles are ordinary identity edges, never scheduling gates.
settleOriginalHomeModuleProducts :: HscEnv -> Set SymbolIdentity
  -> [RawModuleProducts] -> (PreparedModuleProducts, Set SymbolIdentity)
settleOriginalHomeModuleProducts env externalBinders =
  settleOriginalHomeModuleProductsWithoutOwners env externalBinders Set.empty

-- Canonical admission may withdraw an owner with no captured native Core.
-- Every consumer of a sibling binder sees the same atomic owner refusal.
settleOriginalHomeModuleProductsWithoutOwners :: HscEnv -> Set SymbolIdentity
  -> Set Module -> [RawModuleProducts] -> (PreparedModuleProducts, Set SymbolIdentity)
settleOriginalHomeModuleProductsWithoutOwners env externalBinders withdrawn rows =
  let isHome owner = toUnitId (moduleUnit owner) `Set.member` hsc_all_home_unit_ids env
      originalRows = [(rawOriginalProductOwner raw, outcomes)
        | raw <- rows, rawOriginalProductOwner raw `Set.notMember` withdrawn
        , Just outcomes <- [rawOriginalProductGroups raw]]
      rowsByOwner = Map.fromList originalRows
      groupOwners = Map.fromList
        [(symbol, (owner, ordinal)) | (owner, outcomes) <- originalRows
          , (ordinal, symbols, _) <- outcomes, symbol <- symbols]
      failedBinders = Set.fromList
        [symbol | (_, outcomes) <- originalRows
          , (_, symbols, Left _) <- outcomes, symbol <- symbols]
      knownBinders = Set.union externalBinders (Map.keysSet groupOwners)
      dependencies = Map.fromList
        [((owner, projectedOriginalOrdinal projected), Set.fromList
            [globalIdentity global | global <- projectedGlobals (projectedBody projected)
              , globalRequiredGeneration global == Nothing])
          | (owner, outcomes) <- originalRows, (_, _, Right evidence) <- outcomes
          , let projected = projectedGroupWire evidence]
      unavailableHomeReferences = Set.filter
        (\identity -> isHome (symbolOwner identity) && identity `Set.notMember` knownBinders)
        (Set.unions [rawOriginalProductDemands raw | raw <- rows
          , rawOriginalProductOwner raw `Set.notMember` withdrawn])
      blockedHome = closeUnavailableOriginalGroups dependencies groupOwners unavailableHomeReferences
      blocked = closeUnavailableOriginalGroups dependencies groupOwners
        (Set.union failedBinders unavailableHomeReferences)
      initialUnavailableOwners = Set.fromList
        [owner | (owner, _) <- Set.toList blockedHome, Map.member owner rowsByOwner]
      unavailableOwners = closeUnavailableOriginalModules dependencies groupOwners
        (Set.fromList [(owner, ordinal) | owner <- Set.toList initialUnavailableOwners
          , (_, (owner', ordinal)) <- Map.toList groupOwners, owner' == owner])
      finish raw | rawOriginalProductOwner raw `Set.member` withdrawn =
        (rawOriginalProductOwner raw, Left (UnavailableOriginalHomeDependencies
          (Set.toAscList (rawOriginalProductBinders raw))), [])
      finish raw = case rawOriginalProductGroups raw of
        Nothing -> (rawOriginalProductOwner raw, rawExecutableProduct raw, [])
        Just _ | rawOriginalProductOwner raw `Set.member` unavailableOwners ->
          (rawOriginalProductOwner raw,
            Left (UnavailableOriginalHomeDependencies (Set.toAscList unavailableHomeReferences)), [])
        Just outcomes ->
          let owner = rawOriginalProductOwner raw
              failed = [OriginalGroupOmission ordinal symbols (ProjectionFailed reason)
                | (ordinal, symbols, Left reason) <- outcomes]
              retained = [projected | (ordinal, _, Right projected) <- outcomes
                , (owner, ordinal) `Set.notMember` blocked]
              unavailableDependencies projected = Set.fromList
                [globalIdentity global | global <- projectedGlobals (projectedBody projected)
                  , globalRequiredGeneration global == Nothing
                  , maybe False (`Set.member` blocked)
                      (Map.lookup (globalIdentity global) groupOwners)]
              dependent = [OriginalGroupOmission ordinal (projectedBinders projected)
                    (DependsOnUnavailable (Set.toAscList (unavailableDependencies projected)))
                | (ordinal, _, Right evidence) <- outcomes, (owner, ordinal) `Set.member` blocked
                , let projected = projectedGroupWire evidence]
          in (owner, Right retained, failed ++ dependent)
  in (PreparedModuleProducts (map finish rows), unavailableHomeReferences)

symbolOwner :: SymbolIdentity -> Module
symbolOwner identity = mkModule
  (stringToUnit (Text.unpack (symbolUnit identity)))
  (mkModuleName (Text.unpack (symbolModule identity)))

preparedModuleProductOutcomes :: PreparedModuleProducts
  -> [(Module, Either ProjectionError [ProjectedGroup])]
preparedModuleProductOutcomes (PreparedModuleProducts outcomes) =
  [(owner, map projectedGroupWire <$> outcome) | (owner, outcome, _) <- outcomes]

preparedModuleProductOmissions :: PreparedModuleProducts
  -> [(Module, [OriginalGroupOmission])]
preparedModuleProductOmissions (PreparedModuleProducts outcomes) =
  [(owner, omissions) | (owner, _, omissions) <- outcomes]

-- | Successful published groups retain the compiler constructors from their
-- own lowering. Mismatched compiler evidence is an admission failure, including
-- on a warm original-projection hit. Unavailable groups contribute no metadata.
preparedModuleProductConstructors :: PreparedModuleProducts -> Either ProjectionError [DataCon]
preparedModuleProductConstructors (PreparedModuleProducts outcomes) = do
  selected <- fmap concat $ forM outcomes $ \(_,outcome,_) -> case outcome of
    Left _ -> pure []
    Right groups -> fmap concat $ forM groups $ \evidence -> do
      let group = projectedGroupWire evidence
          cons = projectedGroupConstructors evidence
          declarations = projectedConstructors (projectedBody group)
      unless (length declarations == length cons && and (zipWith agrees declarations cons)) $
        Left (InvalidPreparedIdentity "published original constructor differs from its compiler provenance")
      pure cons
  _ <- foldM admit Map.empty selected
  pure selected
 where
  agrees declaration con = constructorIdentity declaration == nameSymbol "constructor" (dataConName con)
    && constructorHostId declaration == varId (dataConWorkId con)
    && constructorTag declaration == fromIntegral (dataConTag con)
  admit known con =
    let hostId = varId (dataConWorkId con)
        identity = nameSymbol "constructor" (dataConName con)
    in case Map.lookup hostId known of
      Just previous | previous /= identity ->
        Left (InvalidPreparedIdentity "distinct original constructor identities share one runtime host id")
      _ -> Right (Map.insert hostId identity known)

-- | Native site metadata and the wire site/type graph come from the same
-- selected tops. Withdrawing a group withdraws both, including on cache hits.
preparedModuleProductYieldSites :: PreparedModuleProducts -> Either ProjectionError [Effect.YieldSite]
preparedModuleProductYieldSites (PreparedModuleProducts outcomes) = do
  selected <- fmap concat $ forM outcomes $ \(_,outcome,_) -> case outcome of
    Left _ -> pure []
    Right groups -> fmap concat $ forM groups $ \evidence -> do
      let rows = projectedSites (projectedBody (projectedGroupWire evidence))
          sites = projectedGroupYieldSites evidence
          agrees row site = siteId row == Effect.ysSite site
            && siteOrigin row == Effect.ysOrigin site
            && siteOrdinal row == Effect.ysOrdinal site
            && length (siteInputs row) == length (Effect.ysInputs site)
      unless (length rows == length sites && and (zipWith agrees rows sites)) $
        Left (InvalidPreparedIdentity "published original site differs from its compiler provenance")
      pure sites
  either (Left . InvalidPreparedIdentity . ("conflicting original site metadata: " <>) . Text.pack . show)
    Right (Effect.mergeYieldSites selected)

-- | Find every projected original group that transitively imports an
-- unavailable original binder. Dependency edges are identities emitted by
-- actual 'GlobalDecl's; retained-generation imports are removed by the caller.
closeUnavailableOriginalGroups
  :: Ord key => Map key (Set SymbolIdentity) -> Map SymbolIdentity key
  -> Set SymbolIdentity -> Set key
closeUnavailableOriginalGroups dependencies owners unavailable = go (Set.toList initial) initial
  where
    dependants = Map.fromListWith Set.union
      [ (symbol, Set.singleton group)
      | (group, symbols) <- Map.toList dependencies
      , symbol <- Set.toList symbols ]
    symbolsByGroup = Map.fromListWith Set.union
      [(group, Set.singleton symbol) | (symbol, group) <- Map.toList owners]
    initial = Set.union
      (Set.fromList (mapMaybe (`Map.lookup` owners) (Set.toList unavailable)))
      (Set.fromList
        [group | (group, symbols) <- Map.toList dependencies
        , not (Set.null (symbols `Set.intersection` unavailable))])
    go [] blocked = blocked
    go (group : pending) blocked =
      let referenced = Map.findWithDefault Set.empty group symbolsByGroup
          next = Set.unions
            [Map.findWithDefault Set.empty symbol dependants | symbol <- Set.toList referenced]
          fresh = next `Set.difference` blocked
      in go (Set.toList fresh ++ pending) (blocked `Set.union` fresh)

-- A module-level product is atomic. Once one of its groups makes the module
-- unavailable, consumers of any sibling binder must also miss that owner.
closeUnavailableOriginalModules
  :: (Ord owner, Ord ordinal)
  => Map (owner, ordinal) (Set SymbolIdentity)
  -> Map SymbolIdentity (owner, ordinal)
  -> Set (owner, ordinal)
  -> Set owner
closeUnavailableOriginalModules dependencies owners initial = go initialModules
  where
    initialModules = Set.fromList [owner | (owner, _) <- Set.toList initial]
    bindersByModule = Map.fromListWith Set.union
      [(owner, Set.singleton symbol) | (symbol, (owner, _)) <- Map.toList owners]
    go modules =
      let unavailable = Set.unions
            [Map.findWithDefault Set.empty owner bindersByModule | owner <- Set.toList modules]
          blocked = closeUnavailableOriginalGroups dependencies owners unavailable
          dependentModules = Set.fromList [owner | (owner, _) <- Set.toList blocked]
          expanded = modules `Set.union` dependentModules
      in if expanded == modules then modules else go expanded

projectPreparedModuleGroupsSelected :: ProjectionContext -> PreparedModule
  -> Maybe (Set Word32) -> Either ProjectionError [ProjectedGroup]
projectPreparedModuleGroupsSelected = projectPreparedModuleGroupsFor ExecutableTarget

projectPreparedModuleGroupsFor :: ProjectionPurpose -> ProjectionContext -> PreparedModule
  -> Maybe (Set Word32) -> Either ProjectionError [ProjectedGroup]
projectPreparedModuleGroupsFor purpose context prepared selection =
  traverse (\(_, _, result) -> result)
    (projectPreparedModuleGroupOutcomesFor purpose context prepared selection)

projectPreparedModuleGroupOutcomesFor :: ProjectionPurpose -> ProjectionContext -> PreparedModule
  -> Maybe (Set Word32) -> [(Word32, [SymbolIdentity], Either ProjectionError ProjectedGroup)]
projectPreparedModuleGroupOutcomesFor purpose context prepared selection =
  [(ordinal,symbols,projectedGroupWire <$> result)
  | (ordinal,symbols,result) <- projectPreparedModuleGroupResultsFor purpose context prepared selection]

projectPreparedModuleGroupResultsFor :: ProjectionPurpose -> ProjectionContext -> PreparedModule
  -> Maybe (Set Word32) -> [(Word32, [SymbolIdentity], Either ProjectionError ProjectedGroupEvidence)]
projectPreparedModuleGroupResultsFor purpose context prepared selection =
  [(ordinal, groupBinderSymbols item, projectOne ordinal item onlyGroup)
  | (ordinal, item, onlyGroup) <- surviving]
  where
    identities = buildTopIdentityMap [prepared]
    originals = pmBindings prepared
    evidenceIndex = indexPreparedEvidence prepared
    surviving =
      [ (ordinal, item, onlyGroup)
      | (ordinal, onlyGroup) <- preparedBindingGroups prepared
      , item@(binding, _) <- pmBindings onlyGroup
      , not (null (topBinders binding))
      , keepPreparedTop context binding
      , maybe True (Set.member ordinal) selection ]
    allSymbols = Set.fromList
      [ symbol
      | (binding, _) <- originals, binder <- topBinders binding
      , Just symbol <- [lookupVarEnv identities binder] ]
    groupBinderSymbols (binding, _) =
      mapMaybe (lookupVarEnv identities) (topBinders binding)
    owner = (Text.pack (unitString (moduleUnit (pmModule prepared))),
             Text.pack (moduleNameString (moduleName (pmModule prepared))))
    projectOne ordinal (binding, _) onlyGroup = do
      refuseUnelaboratedIntrinsics [prepared] [onlyGroup]
      case [ srMessage rejection
           | rejection <- selectOwnedEvidence (topBinders binding)
               (evidenceRejectionsByOwner evidenceIndex)
           , not (skippedFromRecovery context (srBinder rejection)) ] of
        message : _ -> Left (RejectedTypedSite (Text.pack message))
        [] -> pure ()
      binders <- traverse (\binder -> maybe
        (Left (UnsupportedPreparedShape "prepared top has no identity")) Right
        (lookupVarEnv identities binder)) (topBinders binding)
      let outside = allSymbols `Set.difference` Set.fromList binders
          initial = PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv identities
            Map.empty [] Map.empty 0 0 0 0 emptyVarEnv Seq.empty Seq.empty Seq.empty Map.empty Map.empty Seq.empty Seq.empty Map.empty
            (projectionTarget context) (projectionRetainedGenerations context)
            (if pmCoverage prepared == CompleteSourceModule
              then Set.singleton owner else Set.empty)
            (projectionFormattingAuthority context) (projectionTimeAuthority context)
            (projectionJsonAuthority context) (projectionTextUnit context) outside purpose FinalEntryContracts []
      evidence <- selectPreparedEvidence evidenceIndex (topBinders binding)
      ((groups, types, sites, verbSites, jsonLayout), final) <- runStateT
        (do validatePreparedEvidence context [onlyGroup] [evidence] []
            preallocate [onlyGroup]
            groups <- projectModule onlyGroup
            (types, sites, verbSites) <- lowerPreparedEvidence context [onlyGroup] [evidence]
            jsonLayout <- lowerJsonLayout [onlyGroup]
            pure (groups, types, sites, verbSites, jsonLayout)) initial
      pure (ProjectedGroupEvidence (ProjectedGroup
        { projectedOriginalOrdinal = ordinal
        , projectedBinders = binders
        , projectedBody = ProjectedGroupBody
            { projectedEnvelope = ProgramEnvelope schemaVersion
                (projectionProfile context) (projectionToolchain context)
                executionAbiVersion (projectionTarget context)
            , projectedSignatures = toList (signatures final)
            , projectedGlobals = toList (globalDecls final)
            , projectedConstructors = toList (constructorDecls final)
            , projectedOperations = toList (operationDecls final)
            , projectedBindings = map NonRecursive (reverse (implicitTops final)) ++ groups
            , projectedTypes = types
            , projectedSites = sites
            , projectedConstructorReplies = verbSites
            , projectedJsonLayout = jsonLayout
            }
        }) (map fst (toList (constructors final))) (map psSite (selectedEvidenceSites evidence)))

-- The exact compiler Names travel with their stable full-owner identities.
-- Filtering groups before assigning private spellings would change collisions.
preparedTopIdentityBindings :: [PreparedModule] -> Map Name SymbolIdentity
preparedTopIdentityBindings modules = Map.fromList
  [(varName binder, symbol) | prepared <- modules
    , (binding, _) <- pmBindings prepared, binder <- topBinders binding
    , Just symbol <- [lookupVarEnv identities binder]]
  where identities = buildTopIdentityMap modules

-- | Corpus tooling enumerates the same identities that projection resolves,
-- before any target filtering. Preserve module/binding emission order and never
-- infer STG names from artifact filenames.
preparedTopIdentities :: [PreparedModule] -> Either ProjectionError [SymbolIdentity]
preparedTopIdentities modules = traverse identityOf
  [ binder
  | prepared <- modules
  , (binding, _) <- pmBindings prepared
  , binder <- topBinders binding
  ]
  where
    identities = buildTopIdentityMap modules
    identityOf binder = maybe
      (Left (UnsupportedPreparedShape "top binder missing from complete identity map"))
      Right (lookupVarEnv identities binder)

projectPreparedWithTopSymbols :: ProjectionContext -> [PreparedModule]
  -> VarEnv SymbolIdentity -> Either ProjectionError (WireProgram, [DataCon])
projectPreparedWithTopSymbols = projectPreparedWithHostBindings []

-- Only the executable target retains bound host representations. Ordinary
-- original module products keep their own executable constructor inventory.
projectPreparedWithHostBindings :: [HostBindingRepresentation] -> ProjectionContext
  -> [PreparedModule] -> VarEnv SymbolIdentity
  -> Either ProjectionError (WireProgram, [DataCon])
projectPreparedWithHostBindings hostBindings context modules topIdentityMap =
  projectPreparedCandidateWithHostBindings hostBindings context modules topIdentityMap
    >>= finalizePreparedCandidate

-- The candidate keeps its entry checks paired with the exact emitted program.
-- Package recovery may inspect globals, but cannot publish an unchecked body.
data PreparedCandidate = PreparedCandidate WireProgram [DataCon] [ProjectionError]

candidateGlobals :: PreparedCandidate -> [GlobalDecl]
candidateGlobals (PreparedCandidate program _ _) = programGlobals program

finalizePreparedCandidate :: PreparedCandidate -> Either ProjectionError (WireProgram, [DataCon])
finalizePreparedCandidate (PreparedCandidate program constructors failures) = case failures of
  failure : _ -> Left failure
  [] -> Right (program, constructors)

projectPreparedCandidateWithHostBindings :: [HostBindingRepresentation] -> ProjectionContext
  -> [PreparedModule] -> VarEnv SymbolIdentity -> Either ProjectionError PreparedCandidate
projectPreparedCandidateWithHostBindings hostBindings context modules topIdentityMap = do
  boundJsonAuthority <- foldM admitJsonAuthority (projectionJsonAuthority context)
    (mapMaybe hostBindingRepresentationJsonAuthority hostBindings)
  let initial = PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv topIdentityMap Map.empty [] Map.empty
        0 0 0 0 emptyVarEnv Seq.empty Seq.empty Seq.empty Map.empty Map.empty Seq.empty Seq.empty Map.empty (projectionTarget context)
        (projectionRetainedGenerations context) (Set.fromList
          [ (Text.pack (unitString (moduleUnit (pmModule prepared))),
             Text.pack (moduleNameString (moduleName (pmModule prepared))))
          | prepared <- modules, pmCoverage prepared == CompleteSourceModule ])
        (projectionFormattingAuthority context) (projectionTimeAuthority context)
        boundJsonAuthority
        (projectionTextUnit context)
        (Set.fromList (Map.elems (projectionCurrentOriginals context)))
        ExecutableTarget DeferredEntryContracts []
      -- An executable import's own top-level definition is never walked:
      -- 'homeModules'/'topIdentityMap' above still see the real, unfiltered
      -- module set (so a same-name internal identity cannot borrow home-module
      -- standing from the retained one), but nothing here recovers its body.
      projectable = map (dropRetainedTops context) modules
  refuseUnelaboratedIntrinsics modules projectable
  ((bindingGroups, programTypes, programSites, programConstructorReplies, programJsonLayout), final) <- runStateT
    (do evidence <- lift (traverse preparedEvidence projectable)
        validatePreparedEvidence context projectable evidence
          (concatMap hostBindingRepresentationConstructors hostBindings)
        preallocate projectable
        groups <- concat <$> mapM projectModule projectable
        (types, sites, verbSites) <- lowerPreparedEvidence context projectable evidence
        hostJsonLayout <- lowerHostBindings hostBindings
        jsonLayout <- case hostJsonLayout of
          Just layout -> pure (Just layout)
          Nothing -> lowerJsonLayout projectable
        pure (groups, types, sites, verbSites, jsonLayout)) initial
  entryTop <- maybe (Left (MissingPreparedEntry (projectionEntry context)))
    pure (findTop bindingGroups)
  let entry = topValue entryTop
      TopBinding _ entryBinding = entryTop
  case heapBindingRhs entryBinding of
    Function signature _ _ _
      | SignatureId index <- signature
      , (signatureResults <$> Seq.lookup (fromIntegral index) (signatures final))
          == Just CallerResult ->
          Left (InvalidPreparedRepresentation "program entry requires a concrete result contract")
    _ -> pure ()
  let program = WireProgram
        { programEnvelope = ProgramEnvelope schemaVersion (projectionProfile context)
            (projectionToolchain context) executionAbiVersion (projectionTarget context)
        , programSignatures = toList (signatures final)
        , programGlobals = toList (globalDecls final)
        , programConstructors = toList (constructorDecls final)
        , programOperations = toList (operationDecls final)
        , programBindings = map NonRecursive (reverse (implicitTops final)) ++ bindingGroups
        , programEntry = entry
        , programTypes = programTypes
        , programSites = programSites
        , programConstructorReplies = programConstructorReplies
        , programJsonLayout = programJsonLayout
        }
  pure (PreparedCandidate program (map fst (toList (constructors final)))
    (reverse (entryContractFailures final)))
  where
    admitJsonAuthority Nothing authority = pure (Just authority)
    admitJsonAuthority (Just selected) authority
      | selected == authority = pure (Just selected)
      | otherwise = Left (InvalidPreparedRepresentation "bound JSON authority differs from target projection")
    topValue (TopBinding _ binding) = heapBindingId binding
    findTop = foldr findGroup Nothing
    findGroup group found = case filter
      ((== projectionEntry context) . topSymbol) (groupItems group) of
      top : _ -> Just top
      [] -> found
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops
    topSymbol (TopBinding symbol _) = symbol

-- Complete host representations do not depend on executable constructor
-- reachability. JSON roles come directly from that same admitted authority.
lowerHostBindings :: [HostBindingRepresentation] -> P (Maybe (JsonLayout ConstructorId))
lowerHostBindings = foldM lower Nothing
 where
  lower prior representation = case hostBindingRepresentationJsonAuthority representation of
    Just authority -> do
      layout <- traverse internConstructor (jsonAuthorityLayout authority)
      case prior of
        Just selected | selected /= layout ->
          failShape "bound JSON representations have conflicting constructor roles"
        _ -> pure (Just layout)
    Nothing -> do
      mapM_ internConstructor (hostBindingRepresentationConstructors representation)
      pure prior

-- JSON operations, structural answers and host mounts consume authenticated
-- roles. A host carrier can have a Value root without constructing a Value or
-- declaring a site, so its binder type must also retain the layout. Unrelated
-- original groups need neither the roles nor their constructor declarations.
lowerJsonLayout :: [PreparedModule] -> P (Maybe (JsonLayout ConstructorId))
lowerJsonLayout modules = do
  authority <- gets jsonAuthority
  case authority of
    Nothing -> pure Nothing
    Just owner -> do
      emittedOperations <- gets operationDecls
      admittedConstructors <- gets constructors
      let layout = jsonAuthorityLayout owner
          needsOperations = any (isJsonOperation . operationIdentity) emittedOperations
          needsConstructors = any
            (isJust . jsonValueLayoutForType owner . dataConOrigResTy . fst)
            admittedConstructors
          needsHostRoot = any (isValueResult owner)
            [ binder | prepared <- modules, (binding, _) <- pmBindings prepared
                     , binder <- topBinders binding ]
      if needsOperations || needsConstructors || needsHostRoot
        then Just <$> traverse internConstructor layout
        else pure Nothing
 where
  isJsonOperation Schema.JsonDecodeIdentity{} = True
  isJsonOperation Schema.JsonEncodeIdentity = True
  isJsonOperation _ = False
  isValueResult authority binder =
    let (_, _, body) = tcSplitSigmaTy (varType binder)
        result = snd (splitFunTys body)
    in isJust (jsonValueLayoutForType authority (unwrapType result))

-- | Project only the supplied top-level closure reachable from the selected
-- entry. Package imports remain explicit globals for atomic linking. This
-- avoids rejecting unrelated polymorphic bindings while retaining every
-- supplied top-level dependency of the entry.
projectPreparedTarget :: ProjectionContext -> [PreparedModule] -> Either ProjectionError WireProgram
projectPreparedTarget context modules =
  fst <$> projectPreparedTargetWithConstructors context modules

-- | The shared artifact writer needs the exact GHC constructors admitted by
-- projection, including site-only evidence and generated settlement values.
-- Return them from the same transaction that produced the wire declarations.
projectPreparedTargetWithConstructors :: ProjectionContext -> [PreparedModule]
  -> Either ProjectionError (WireProgram, [DataCon])
projectPreparedTargetWithConstructors context modules =
  prepareProjection context modules >>= projectSelected

-- | Selection retains the exact context and identity map that authorized it.
-- Its binding lists are forced here so selection work is not charged to
-- lowering later.
data PreparedProjection = PreparedProjection ProjectionContext [PreparedModule] (VarEnv SymbolIdentity)

prepareProjection :: ProjectionContext -> [PreparedModule]
  -> Either ProjectionError PreparedProjection
prepareProjection _ [] = Left (UnsupportedPreparedShape "execution program has no modules")
prepareProjection context modules =
  let (identities, selected) = selectPreparedTarget context modules
  in finishProjection context modules identities selected

-- | Recovery already closed these exact modules under the target's seeds.
-- Keep identity assignment over every live top: filtering first would change
-- collision suffixes and the standing of retained definitions.
prepareProjectionWithReachability :: ProjectionContext -> [PreparedModule]
  -> PreparedReachability -> Either ProjectionError PreparedProjection
prepareProjectionWithReachability _ [] _ =
  Left (UnsupportedPreparedShape "execution program has no modules")
prepareProjectionWithReachability context modules reach =
  finishProjection context modules (buildTopIdentityMap modules)
    [ filterPreparedBindings isReachable prepared
    | prepared <- modules
    , any isReachable (pmBindings prepared)
    ]
  where
    isReachable (binding, _) = any
      ((`elementOfUniqSet` reachedUniques reach) . varUnique)
      (topBinders binding)

-- Component-backed package owners retain their one declaring context and one
-- site arena. Projection walks their units directly, in canonical owner/group
-- order; no growing prepared module aggregate is required.
prepareComponentProjectionWithReachability :: ProjectionContext -> [PreparedModule]
  -> [PreparedComponents] -> PreparedReachability -> Either ProjectionError PreparedProjection
prepareComponentProjectionWithReachability context home components reach
  | length owners /= Set.size (Set.fromList owners) =
      Left (InvalidPreparedRepresentation "component projection repeats a defining owner")
  | otherwise = prepareProjectionWithReachability context
      (home ++ concatMap preparedComponentModules components) reach
  where owners = map (fatOriginalOwner . preparedComponentVersion) components

finishProjection :: ProjectionContext -> [PreparedModule] -> VarEnv SymbolIdentity
  -> [PreparedModule] -> Either ProjectionError PreparedProjection
finishProjection context modules identities selected =
  let bindingCount = sum [length (pmBindings prepared) | prepared <- selected]
      identityCount = length (nonDetEltsUFM identities)
      reachable = mkUniqSet [ varUnique binder | prepared <- selected
        , (binding, _) <- pmBindings prepared, binder <- topBinders binding ]
  in bindingCount `seq` identityCount `seq` case [ srMessage rejection | prepared <- modules
            , rejection <- pmSiteRejections prepared
            , elementOfUniqSet (varUnique (srBinder rejection)) reachable
            , not (skippedFromRecovery context (srBinder rejection)) ] of
       message : _ -> Left (RejectedTypedSite (Text.pack message))
       [] -> Right (PreparedProjection context selected identities)

projectSelected :: PreparedProjection -> Either ProjectionError (WireProgram, [DataCon])
projectSelected = projectSelectedWithHostBindings []

-- The opaque representations were admitted from the same stabilized binding
-- types used to publish the session interface and host-authority tags.
projectSelectedWithHostBindings :: [HostBindingRepresentation] -> PreparedProjection
  -> Either ProjectionError (WireProgram, [DataCon])
projectSelectedWithHostBindings bindings (PreparedProjection context selected identities) =
  projectPreparedWithHostBindings bindings context selected identities

-- Original native groups can add package roots after executable projection.
-- Only finalization of the candidate admits the captured entry contracts.
projectSelectedCandidateWithHostBindings :: [HostBindingRepresentation] -> PreparedProjection
  -> Either ProjectionError PreparedCandidate
projectSelectedCandidateWithHostBindings bindings (PreparedProjection context selected identities) =
  projectPreparedCandidateWithHostBindings bindings context selected identities

-- | The per-module, round-invariant part of 'preparedTargetReferences'.
--
-- For EVERY one of a module's own top-level binding groups -- unfiltered by
-- reachability, since 'selectPreparedTarget''s reachable set can grow round
-- to round as more of the closure is discovered, but a binding group's OWN
-- contributed references never do for a fixed 'context' -- this computes the
-- external value Ids that group's body refers to, via the same
-- 'recoveryReferences' + 'extractPreparedFacts' pipeline
-- 'preparedTargetReferences' used inline. Only the 'preparedReferencedIds'
-- field is ever read from 'extractPreparedFacts' downstream (by
-- 'combinePreparedTargetReferences'), and that field's 'Monoid' instance is
-- plain list append over the traversal, so computing it one binding group at
-- a time and concatenating in the module's own binding order reproduces
-- exactly what computing it over the whole (possibly reachability-filtered)
-- list would -- this is what lets 'combinePreparedTargetReferences' apply
-- 'selectPreparedTarget''s filter AFTER this lookup instead of before it.
--
-- Keyed by the 'Unique' key of each group's first top binder (a module's
-- groups have disjoint binders). The result depends only on 'context' and the
-- module's own 'pmBindings', so a caller may memoize it until that
-- 'PreparedModule' is replaced.
preparedModuleReferenceFacts :: ProjectionContext -> PreparedModule
  -> Map Word64 [ReferenceFact]
preparedModuleReferenceFacts context prepared = Map.fromList
  [ (getKey (varUnique firstBinder), entryReferences)
  | (binding, _) <- pmBindings prepared
  , firstBinder : _ <- [topBinders binding]
  , let entryReferences =
          [ ReferenceFact binder (idSymbol "value" binder)
          | binder <- preparedReferencedIds (extractPreparedFacts
              (pmModule prepared) (recoveryReferences context binding))
          , isExternalName (varName binder)
          , isNothing (nullaryWorkerConstructor binder) ]
  ]

-- | One candidate external reference contributed by a binding group, together
-- with the identity it is retained and deduplicated by.
--
-- Both fields are functions of the 'Id' alone, so they are round-invariant in
-- the same sense the rest of 'preparedModuleReferenceFacts' is. The identity
-- stays lazy: a recovery round only forces it for the references that survive
-- the closure's own @defined@ filter, and the memoized fact then holds the
-- forced identity for every later round instead of rebuilding it.
data ReferenceFact = ReferenceFact
  { referenceBinder :: !Id
  , referenceSymbol :: SymbolIdentity
  }

-- Compiler surface definitions are placeholders. Only the elaborator's
-- admitted sites may execute; a surviving surface Name is never ordinary code.
refuseUnelaboratedIntrinsics
  :: [PreparedModule] -> [PreparedModule]
  -> Either ProjectionError ()
refuseUnelaboratedIntrinsics admitted selected = case
    [ binder | prepared <- selected
      , (binding,_) <- pmBindings prepared
      , binder <- topBinders binding ++ preparedReferencedIds
          (extractPreparedFacts (pmModule prepared) [binding])
      , any (\owner -> preparedRejectsIntrinsic owner binder) admitted ] of
  binder : _ -> Left (UnelaboratedCompilerIntrinsic (preparedRootIdentity binder))
  [] -> Right ()

-- | Cross-module combination for 'preparedTargetReferences': the external
-- value references of every binding group @kept@ selects, minus the ones
-- @defined@ says this closure already supplies. Each module is paired with its
-- own 'preparedModuleReferenceFacts' (possibly memoized by the caller);
-- pairing by position keeps two prepared copies of one owner distinct. Groups
-- are walked in module and binding order, so the result equals recomputing the
-- facts over the kept bindings.
--
-- @defined@ is the caller's, for the same reason @kept@ is: recovery carries a
-- monotone set of admitted tops across its rounds
-- ('PreparedReachability') rather than rebuilding it per round.
combinePreparedTargetReferences :: ProjectionContext -> UniqSet Unique
  -> (CgStgTopBinding -> Bool) -> [(PreparedModule, Map Word64 [ReferenceFact])]
  -> [Id]
combinePreparedTargetReferences context defined kept entries =
  let referenced = [ fact | (prepared, facts) <- entries
        , (binding, _) <- pmBindings prepared
        , kept binding
        , firstBinder : _ <- [topBinders binding]
        , fact <- Map.findWithDefault [] (getKey (varUnique firstBinder)) facts
        , not (elementOfUniqSet (varUnique (referenceBinder fact)) defined)
        -- An executable import is resolved by generation, never by pulling
        -- its defining module's source into this program's recovery closure.
        , isNothing (Map.lookup (referenceSymbol fact)
            (projectionRetainedGenerations context)) ]
  in Map.elems (Map.fromList
       [(referenceSymbol fact, referenceBinder fact) | fact <- referenced])

-- Immutable group facts are indexed once as units arrive. Target discovery
-- examines only newly reached tops and groups whose definitions just arrived.
data PreparedReferenceWorklist = PreparedReferenceWorklist
  { referenceGroupOf :: Map Word64 Word64
  , referenceGroupTops :: Map Word64 (Set.Set Word64)
  , referenceGroupBodies :: Map Word64 [ReferenceFact]
  , referenceNewGroups :: Set.Set Word64
  , referenceVisitedGroups :: Set.Set Word64
  , referenceReached :: Set.Set Word64
  }

emptyPreparedReferenceWorklist :: PreparedReferenceWorklist
emptyPreparedReferenceWorklist = PreparedReferenceWorklist Map.empty Map.empty Map.empty Set.empty Set.empty Set.empty

admitPreparedReferenceUnits :: [(PreparedModule,Map Word64 [ReferenceFact])]
  -> PreparedReferenceWorklist -> PreparedReferenceWorklist
admitPreparedReferenceUnits units initial = foldl' addUnit initial units
  where
    addUnit known (prepared,facts) = foldl' (addGroup facts) known (pmBindings prepared)
    addGroup facts known (binding,_) = case topBinders binding of
      [] -> known
      first:rest ->
        let key = getKey (varUnique first)
            tops = Set.fromList (map (getKey . varUnique) (first:rest))
        in known
          { referenceGroupOf = foldl' (\index top -> Map.insert top key index)
              (referenceGroupOf known) (Set.toList tops)
          , referenceGroupTops = Map.insert key tops (referenceGroupTops known)
          , referenceGroupBodies = Map.insert key (Map.findWithDefault [] key facts) (referenceGroupBodies known)
          , referenceNewGroups = Set.insert key (referenceNewGroups known) }

-- The complete reached set is carried by the reachability owner. This frontier
-- retains no source or compiler authority and never supplies definitions.
discoverPreparedReferences :: ProjectionContext -> PreparedReachability
  -> PreparedReferenceWorklist -> ([Id],PreparedReferenceWorklist)
discoverPreparedReferences context reach known =
  let reached = Set.fromList (map getKey (nonDetEltsUniqSet (reachedUniques reach)))
      changed = reached `Set.difference` referenceReached known
      groups = (Set.fromList [group | top <- Set.toList changed
                 , Just group <- [Map.lookup top (referenceGroupOf known)]]
          `Set.union` referenceNewGroups known) `Set.difference` referenceVisitedGroups known
      selected = Set.filter (\group -> not (Set.null
          (Map.findWithDefault Set.empty group (referenceGroupTops known) `Set.intersection` reached))) groups
      references = Map.elems (Map.fromList
        [(referenceSymbol fact,referenceBinder fact) | group <- Set.toAscList selected
        , fact <- Map.findWithDefault [] group (referenceGroupBodies known)
        , not (elementOfUniqSet (varUnique (referenceBinder fact)) (admittedTops reach))
        , Map.notMember (referenceSymbol fact) (projectionRetainedGenerations context)])
  in (references,known
      { referenceReached=reached,referenceNewGroups=Set.empty
      , referenceVisitedGroups=referenceVisitedGroups known `Set.union` selected })

-- | Exact external value references of the selected top closure. The identity
-- map is always computed before filtering. Recovery uses Ids, never occurrence
-- strings or the imported-only annotations returned by stg2stg.
preparedTargetReferences :: ProjectionContext -> [PreparedModule] -> [Id]
preparedTargetReferences context modules =
  combinePreparedTargetReferences context defined keep
    [(prepared, preparedModuleReferenceFacts context prepared) | prepared <- modules]
  where
    (_, selected) = selectPreparedTarget context modules
    defined = mkUniqSet [varUnique binder | prepared <- modules
      , (binding, _) <- pmBindings prepared, binder <- topBinders binding]
    reachable = mkUniqSet [varUnique binder | prepared <- selected
      , (binding, _) <- pmBindings prepared, binder <- topBinders binding]
    keep binding = any ((`elementOfUniqSet` reachable) . varUnique) (topBinders binding)

-- | Per-module, round-invariant reachability input for recovery: every
-- individual top in binding order with the raw uniques its body mentions
-- (none for a top 'skippedFromRecovery' leaves unrecovered, as in
-- 'selectPreparedTarget').
preparedModuleReachFacts :: ProjectionContext -> PreparedModule -> [(Id, [Unique])]
preparedModuleReachFacts context prepared =
  [ (binder, if skippedFromRecovery context binder then []
      else topBindingReferenceUniques (pmModule prepared) single)
  | (binding, _) <- pmBindings prepared
  , (binder, single) <- individualTops binding
  ]

-- | Tops of the home modules whose identity is the entry or an auxiliary
-- root. Recovered modules never share a home module's identity namespace, so
-- home spellings, and therefore these seeds, do not change as recovery adds
-- modules.
preparedSeedUniques :: ProjectionContext -> [PreparedModule] -> UniqSet Unique
preparedSeedUniques context home = mkUniqSet
  [ varUnique binder
  | prepared <- home
  , (binding, _) <- pmBindings prepared
  , binder <- topBinders binding
  , Just symbol <- [lookupVarEnv identities binder]
  , symbol == projectionEntry context || symbol `elem` projectionAuxiliaryRoots context
  ]
  where
    identities = buildTopIdentityMap home

-- | 'selectPreparedTarget''s reachable closure over top uniques instead of
-- assigned identities (tops and identities correspond one to one within a
-- closure), carried across recovery's rounds instead of rebuilt by each one.
--
-- A round only ADMITS binding groups: it adds a defining module, or replaces
-- one with a preparation of a strictly larger exact body set that still names
-- every top its interface named. The dependency relation and its closure are
-- therefore monotone, and a round can cost what it admits rather than what the
-- closure already holds.
--
-- Two fields deliberately hold more than their name promises, because nothing
-- a caller asks can tell the difference:
--
-- * 'reachedUniques' is the RAW closure, so it also holds references that are
--   not tops of any admitted module. A reference that is not a top carries no
--   dependencies, so it never extends the walk, and its unique can never equal
--   a live top binder's. Pre-filtering every reference list against the tops
--   instead would cost one membership test per reference of every top,
--   reachable or not, in every round.
-- * 'admittedTops' keeps the tops of a replaced preparation. Only that
--   preparation's own bindings could name its preparation-local tops, and
--   those bindings are exactly what the replacement dropped; an external
--   reference names the interface Id, whose unique the replacement preserves.
data PreparedReachability = PreparedReachability
  { admittedTops :: !(UniqSet Unique)
  , reachedUniques :: !(UniqSet Unique)
  , topDependencies :: !(UniqFM Unique [Unique])
  }

emptyPreparedReachability :: PreparedReachability
emptyPreparedReachability =
  PreparedReachability emptyUniqSet emptyUniqSet emptyUFM

-- | Admit the reach facts of newly prepared modules and re-close from @seeds@
-- and from every admitted top the walk had already reached. Re-expanding those
-- is what makes the incremental closure equal the whole-closure one: a top
-- reached as a bare reference before its defining module arrived, and a top
-- whose dependencies a re-preparation just replaced, both have dependencies
-- the previous round could not follow.
admitReachFacts :: [Unique] -> [[(Id, [Unique])]] -> PreparedReachability
  -> PreparedReachability
admitReachFacts seeds admitted carried = PreparedReachability
  { admittedTops = addListToUniqSet (admittedTops carried) (map fst entries)
  , reachedUniques =
      close (reachedUniques carried) (seeds ++ concatMap reexpanded entries)
  , topDependencies = dependencies
  }
  where
    entries = [ (varUnique binder, references)
              | facts <- admitted, (binder, references) <- facts ]
    dependencies = foldl' (\deps (unique, references) -> addToUFM deps unique references)
      (topDependencies carried) entries
    reexpanded (unique, references)
      | unique `elementOfUniqSet` reachedUniques carried = references
      | otherwise = []
    close visited [] = visited
    close visited (unique : pending)
      | unique `elementOfUniqSet` visited = close visited pending
      | otherwise = close (addOneToUniqSet visited unique)
          (fromMaybe [] (lookupUFM dependencies unique) <> pending)

-- Site arenas are replaceable; pure component facts are only admitted.
-- A replacement closes from actual roots against the current complete facts.
data PreparedReachUpdate
  = AdmitPreparedFacts [[(Id,[Unique])]]
  | ReplacePreparedFacts [[(Id,[Unique])]]

updatePreparedReachability :: [Unique] -> PreparedReachUpdate -> PreparedReachability -> PreparedReachability
updatePreparedReachability seeds update carried = case update of
  AdmitPreparedFacts additions -> admitReachFacts seeds additions carried
  ReplacePreparedFacts current -> admitReachFacts seeds current emptyPreparedReachability

-- A registered replacement has no source-body dependencies. Split recursive
-- groups for this fact query so unrelated siblings retain their own references.
-- A retained-generation import is the same shape: its own body is never
-- recovered, so its internal references are moot for this query too.
recoveryReferences :: ProjectionContext -> CgStgTopBinding -> [CgStgTopBinding]
recoveryReferences context (StgTopLifted (StgRec pairs)) =
  [ StgTopLifted (StgNonRec binder rhs)
  | (binder, rhs) <- pairs, not (skippedFromRecovery context binder) ]
recoveryReferences context binding
  | any (skippedFromRecovery context) (topBinders binding) = []
  | otherwise = [binding]

-- | Retention is looked up by external identity only (namespace "value"),
-- never inferred from module membership: a symbol present in the caller's
-- retained-generation map is an executable import regardless of whether its
-- defining module happens to be compiled alongside the referencing program.
retainedGenerationOf :: ProjectionContext -> Id -> Maybe Word64
retainedGenerationOf context binder =
  Map.lookup (idSymbol "value" binder) (projectionRetainedGenerations context)

skippedFromRecovery :: ProjectionContext -> Id -> Bool
skippedFromRecovery context binder =
  registeredReplacement context binder || isJust (retainedGenerationOf context binder)
    || Map.member (varName binder) (projectionCurrentOriginals context)

formattingSpec :: ProjectionContext -> Id -> Either ProjectionError (Maybe FormattingSpec)
formattingSpec context binder = case projectionFormattingAuthority context of
  Nothing -> Right Nothing
  Just authority -> case classifyFormatting authority binder of
    Left failure -> Left (UnsupportedPreparedShape (Text.pack (show failure)))
    Right spec -> Right spec

registeredFormatting :: ProjectionContext -> Id -> Bool
registeredFormatting context binder = case formattingSpec context binder of
  Right (Just _) -> True
  _ -> False

timeSpec :: ProjectionContext -> Id -> Either ProjectionError (Maybe TimeSpec)
timeSpec context binder = case projectionTimeAuthority context of
  Nothing -> Right Nothing
  Just authority -> case classifyTime authority binder of
    Left failure -> Left (UnsupportedPreparedShape (Text.pack (show failure)))
    Right spec -> Right spec

registeredTime :: ProjectionContext -> Id -> Bool
registeredTime context binder = case timeSpec context binder of
  Right (Just _) -> True
  _ -> False

jsonSpec :: ProjectionContext -> Id -> Either ProjectionError (Maybe JsonSpec)
jsonSpec context binder = case projectionJsonAuthority context of
  Nothing -> Right Nothing
  Just authority -> case classifyJson authority binder of
    Left failure -> Left (UnsupportedPreparedShape (Text.pack (show failure)))
    Right spec -> Right spec

registeredJson :: ProjectionContext -> Id -> Bool
registeredJson context binder = case jsonSpec context binder of
  Right (Just _) -> True
  _ -> False

registeredReplacement :: ProjectionContext -> Id -> Bool
registeredReplacement context binder =
  registeredFormatting context binder || registeredTime context binder
    || registeredJson context binder
    || isJust (deferredFunction binder)

selectPreparedTarget :: ProjectionContext -> [PreparedModule]
  -> (VarEnv SymbolIdentity, [PreparedModule])
selectPreparedTarget context modules =
  (topIdentityMap, [ filterPreparedBindings isReachable prepared
    | prepared <- modules
    , any isReachable (pmBindings prepared)
    ])
  where
    topIdentityMap = buildTopIdentityMap modules
    topUniqueIdentityMap = buildTopUniqueIdentityMap topIdentityMap modules
    allBindings =
      [ (pmModule prepared, binding)
      | prepared <- modules
      , (binding, _) <- pmBindings prepared
      ]
    topLevel = mkUniqSet
      [ varUnique binder
      | (_, binding) <- allBindings
      , binder <- topBinders binding
      ]
    entry = projectionEntry context
    auxiliaryRoots = projectionAuxiliaryRoots context
    seedSymbols =
      [ symbol
      | (_, binding) <- allBindings
      , binder <- topBinders binding
      , let symbol = mappedTopIdentity binder
      , symbol == entry || symbol `elem` auxiliaryRoots
      ]
    -- A top whose body is never recovered -- a registered replacement or a
    -- retained-generation import -- is a closure boundary: its own
    -- references must not make its floated sub-bindings reachable. The same
    -- 'skippedFromRecovery' predicate governs 'recoveryReferences', so
    -- reachability and recovery cannot disagree about which bodies exist.
    dependencies = Map.fromListWith (<>)
      [ (mappedTopIdentity binder, Set.fromList
          [ symbol
          | unique <- if skippedFromRecovery context binder then [] else
              nonDetEltsUniqSet (topBindingReferences modul topLevel single)
          , Just symbol <- [lookupUFM topUniqueIdentityMap unique]
          ])
      | (modul, binding) <- allBindings
      , (binder, single) <- individualTops binding
      ]
    reachableSymbols = close Set.empty seedSymbols
    isReachable (binding, _) = any
      (\binder -> mappedTopIdentity binder `Set.member` reachableSymbols)
      (topBinders binding)
    close :: Set SymbolIdentity -> [SymbolIdentity] -> Set SymbolIdentity
    close visited [] = visited
    close visited (symbol : pending)
      | symbol `Set.member` visited = close visited pending
      | otherwise = close (Set.insert symbol visited)
          (maybe pending (\next -> Set.toList next <> pending)
            (Map.lookup symbol dependencies))

    mappedTopIdentity binder = lookupVarEnv topIdentityMap binder
      `orElse` idSymbol (topIdentityNamespace binder) binder

    orElse (Just value) _ = value
    orElse Nothing fallback = fallback

individualTops :: CgStgTopBinding -> [(Id, CgStgTopBinding)]
individualTops (StgTopStringLit binder bytes) =
  [(binder, StgTopStringLit binder bytes)]
individualTops (StgTopLifted (StgNonRec binder rhs)) =
  [(binder, StgTopLifted (StgNonRec binder rhs))]
individualTops (StgTopLifted (StgRec pairs)) =
  [(binder, StgTopLifted (StgNonRec binder rhs)) | (binder, rhs) <- pairs]

topBinders :: CgStgTopBinding -> [Id]
topBinders (StgTopStringLit binder _) = [binder]
topBinders (StgTopLifted binding) = bindingBinders binding

-- | Drop a module's own definitions of its retained-generation imports. A
-- group is dropped only when every one of its binders is retained, so an
-- ordinary sibling recursive with a retained import still gets a body.
-- References to the dropped binder still resolve (as a 'Global'):
-- 'topSymbols'/'homeModules' are built from the unfiltered module list
-- upstream of this filter, never from this one.
dropRetainedTops :: ProjectionContext -> PreparedModule -> PreparedModule
dropRetainedTops context = filterPreparedBindings (keepPreparedTop context . fst)

keepPreparedTop :: ProjectionContext -> CgStgTopBinding -> Bool
keepPreparedTop context binding =
  not (all (\binder -> isJust (retainedGenerationOf context binder)
    || Map.member (varName binder) (projectionCurrentOriginals context)) (topBinders binding))

-- | Assign stable identities to internal tops before any target reachability
-- filtering.  Internal names may repeat (and a generated suffix may already
-- be an authored spelling), so reserve every original spelling first and claim
-- either that spelling or the first unused suffix in emission order. The
-- allocation is local to a symbol namespace; external names are retained
-- byte-for-byte while constraining generated suffixes around them. Canonical
-- preparation also issues reserved internal spellings from its complete owner
-- census; those remain fixed as independently prepared components accumulate.
buildTopIdentityMap :: [PreparedModule] -> VarEnv SymbolIdentity
buildTopIdentityMap modules = foldl insert emptyVarEnv (zip binders assigned)
  where
    binders =
      [ (pmModule prepared, binder)
      | prepared <- modules
      , (binding, _) <- pmBindings prepared
      , binder <- topBinders binding
      ]
    stableSpellings = Map.unions (map pmStableTopSpellings modules)
    fixed binder = isExternalName (varName binder)
      || Map.member (varName binder) stableSpellings
    raw (fallback, binder) =
      let symbol = idSymbolFor fallback (topIdentityNamespace binder) binder
      in if isExternalName (varName binder) then symbol else
        maybe symbol (\spelling -> symbol {symbolOccurrence = spelling})
          (Map.lookup (varName binder) stableSpellings)
    symbols = map raw binders
    assigned = assignTopIdentitySpellings
      (zip symbols (map (fixed . snd) binders))
    insert mappings ((_, binder), symbol) = extendVarEnv mappings binder symbol

buildTopUniqueIdentityMap :: VarEnv SymbolIdentity -> [PreparedModule]
  -> UniqFM Unique SymbolIdentity
buildTopUniqueIdentityMap topIdentityMap modules = listToUFM
  [ (varUnique binder, symbol)
  | prepared <- modules
  , (binding, _) <- pmBindings prepared
  , binder <- topBinders binding
  , Just symbol <- [lookupVarEnv topIdentityMap binder]
  ]

-- | Deterministic identity allocation shared by projection and collision
-- regressions. The Bool marks a fixed spelling (external or preparation-issued),
-- retained exactly; all original spellings reserve suffixes for other internal tops.
assignTopIdentitySpellings
  :: [(SymbolIdentity, Bool)] -> [SymbolIdentity]
assignTopIdentitySpellings entries = reverse (snd (foldl' allocateOne
  (externalClaims, []) entries))
  where
    -- The accumulator holds assigned spellings newest first; recovery calls
    -- this once per round over the whole closure, so an append per entry
    -- made each call quadratic in the number of tops.
    reserved :: Map (Text, Text, Text) (Set Text)
    reserved = Map.fromListWith Set.union
      [ (namespaceKey symbol, Set.singleton (symbolOccurrence symbol))
      | (symbol, _) <- entries
      ]
    externalClaims :: Map (Text, Text, Text) (Set Text)
    externalClaims = Map.fromListWith Set.union
      [ (namespaceKey symbol, Set.singleton (symbolOccurrence symbol))
      | (symbol, external) <- entries
      , external
      ]
    allocateOne (claimedByNamespace, assigned) (symbol, external)
      | external = (claimedByNamespace, symbol : assigned)
      | otherwise =
          let key = namespaceKey symbol
              claimed = Map.findWithDefault Set.empty key claimedByNamespace
              reservedNames = Map.findWithDefault Set.empty key reserved
              occurrence = chooseOccurrence (symbolOccurrence symbol)
                claimed reservedNames
              nextClaimed = Set.insert occurrence claimed
          in (Map.insert key nextClaimed claimedByNamespace,
              symbol { symbolOccurrence = occurrence } : assigned)

    chooseOccurrence :: Text -> Set Text -> Set Text -> Text
    chooseOccurrence original claimed reservedNames
      | original `Set.notMember` claimed = original
      | otherwise = case listToMaybe
          [ candidate | n <- [1 :: Int ..]
          , let candidate = original <> "." <> Text.pack (show n)
          , candidate `Set.notMember` blocked
          ] of
          Just value -> value
          Nothing -> original
      where
        blocked = claimed `Set.union` reservedNames

    namespaceKey symbol =
      (symbolUnit symbol, symbolModule symbol, symbolNamespace symbol)

preallocate :: [PreparedModule] -> P ()
preallocate modules = do
  mapM_ (mapM_ (registerBindingArities . fst) . pmBindings) modules
  mapM_ (mapM_ allocateTop . pmBindings) modules
  where
    allocateTop (StgTopStringLit binder _, _) = allocateTopValue binder >> pure ()
    allocateTop (StgTopLifted binding, _) = mapM_ allocateTopValue (bindingBinders binding) >> pure ()

projectModule :: PreparedModule -> P [Group TopBinding]
projectModule prepared = mapM (projectTop prepared . fst) (pmBindings prepared)

-- One module projection owns this immutable index. Its graph stays lazy so
-- groups without typed sites do not force an otherwise unused full graph.
data PreparedEvidenceIndex = PreparedEvidenceIndex
  { evidenceGraph :: TypePolicy.TypeGraph
  , evidenceSitesByOwner :: Map Word64 [(Int, PreparedSite)]
  , evidenceRejectionsByOwner :: Map Word64 [(Int, SiteRejection)]
  }

data SelectedPreparedEvidence = SelectedPreparedEvidence
  { selectedEvidenceGraph :: TypePolicy.TypeGraph
  , selectedEvidenceSites :: [PreparedSite]
  }

indexPreparedEvidence :: PreparedModule -> PreparedEvidenceIndex
indexPreparedEvidence prepared = PreparedEvidenceIndex
  { evidenceGraph = pmTypeGraph prepared
  , evidenceSitesByOwner = Map.fromListWith (<>)
      [ (owner, [(ordinal, site)])
      | (ordinal, site) <- zip [0 :: Int ..] (pmPreparedSites prepared)
      , owner <- Set.toList (ownedTops (psOwner site)) ]
  , evidenceRejectionsByOwner = Map.fromListWith (<>)
      [ (getKey (varUnique (srBinder rejection)), [(ordinal, rejection)])
      | (ordinal, rejection) <- zip [0 :: Int ..] (pmSiteRejections prepared) ]
  }
 where
  -- CorePrep/STG can float a site's computation into a separate original
  -- top. The exact source owner's introduced-top dependency closure owns its site
  -- evidence too: a captured floated closure must not depend on the source
  -- owner's native image remaining code-reachable. No scalar literal or
  -- rendered binder spelling is used to recover this relationship. Exact
  -- pre-CorePrep top Names keep authored helpers independent even when they
  -- have internal linkage; recursive peers remain indivisible.
  bindings = map fst (pmBindings prepared)
  introducedTops = mkUniqSet [varUnique binder | binding <- bindings, binder <- topBinders binding
    , varName binder `Set.notMember` pmOriginalTopNames prepared]
  dependencies = Map.fromList
    [ (getKey (varUnique binder), Set.fromList (map getKey (nonDetEltsUniqSet
        (topBindingReferences (pmModule prepared) introducedTops binding)))
        `Set.union` Set.fromList (map (getKey . varUnique) (topBinders binding)))
    | binding <- bindings, binder <- topBinders binding ]
  ownedTops binder = Map.findWithDefault Set.empty (getKey (varUnique binder)) closures
  closures = Map.fromSet (\owner -> go [owner] Set.empty) (Set.fromList
    (map (getKey . varUnique . psOwner) (pmPreparedSites prepared)))
  go [] visited = visited
  go (owner:pending) visited
    | owner `Set.member` visited = go pending visited
    | otherwise = go (Set.toList (Map.findWithDefault Set.empty owner dependencies) ++ pending)
        (Set.insert owner visited)

selectPreparedEvidence :: PreparedEvidenceIndex -> [Id]
  -> Either ProjectionError SelectedPreparedEvidence
selectPreparedEvidence index binders = do
  let sites = selectOwnedEvidence binders (evidenceSitesByOwner index)
      roots = concat [ psWireNode site : psInputNodes site | site <- sites ]
  graph <- selectTypeGraph (evidenceGraph index) roots
  pure (SelectedPreparedEvidence graph sites)

-- Original ordinals recover module row order even for recursive groups whose
-- binders are encountered in a different order.
selectOwnedEvidence :: [Id] -> Map Word64 [(Int, a)] -> [a]
selectOwnedEvidence binders rowsByOwner = IntMap.elems (IntMap.fromList
  [ row
  | owner <- Set.toList (Set.fromList (map (getKey . varUnique) binders))
  , row <- Map.findWithDefault [] owner rowsByOwner ])

preparedEvidence :: PreparedModule -> Either ProjectionError SelectedPreparedEvidence
preparedEvidence prepared = selectPreparedEvidence (indexPreparedEvidence prepared)
  [ binder | (binding, _) <- pmBindings prepared, binder <- topBinders binding ]

-- | Lower only evidence owned by the executable tops retained in each module.
-- Graph ids are module-local during elaboration; this pass compacts reachable
-- nodes in module/original order and rebases every edge into one program table.
-- An admitted auxiliary root's own result type follows the module evidence
-- ('lowerAuxiliaryRootEvidence'); intrinsic constructor reply graphs follow.
lowerPreparedEvidence :: ProjectionContext -> [PreparedModule]
  -> [SelectedPreparedEvidence] -> P (TypeGraph, [SiteRow], [(ConstructorId, ConstructorReply)])
lowerPreparedEvidence context modules evidence = do
  (moduleNodes, moduleSites) <- foldM lowerOne (emptyProjectedTypeGraph, []) evidence
  auxNodes <- lowerAuxiliaryRootEvidence context modules (graphNodeCount moduleNodes)
  (verbNodes, verbSites) <-
    lowerConstructorReplies (mapMaybe pmRequestSiteTyCon modules)
      (graphNodeCount moduleNodes + graphNodeCount auxNodes)
  let sites = moduleSites
      duplicates = Map.keys (Map.filter (> (1 :: Int))
        (Map.fromListWith (+) [(siteId site, 1) | site <- sites]))
  case duplicates of
    duplicate : _ -> failShape
      ("duplicate selected prepared site id " <> Text.pack (show duplicate))
    [] -> do
      let combined = unionTypeGraphs (unionTypeGraphs moduleNodes auxNodes) verbNodes
      (graph, rebase) <- lift (assembleTypeGraph combined)
      sites' <- traverse (\site -> do
        wire <- lift (rebase (siteWire site))
        inputs <- traverse (lift . rebase) (siteInputs site)
        pure site { siteWire = wire, siteInputs = inputs }) sites
      replies <- traverse (\(constructor, reply) -> (constructor,) <$> case reply of
        StaticReply root -> StaticReply <$> lift (rebase root)
        ReplyAtSite -> pure ReplyAtSite
        StaticReplyWithSite root field payload capture ->
          (\root' -> StaticReplyWithSite root' field payload capture) <$> lift (rebase root)) verbSites
      pure (graph, sites', replies)
 where
  lowerOne (priorNodes, priorSites) selectedEvidence = do
    let selected = selectedEvidenceSites selectedEvidence
    (lowered, rebase) <- lowerSelectedTypeGraph (graphNodeCount priorNodes)
      (selectedEvidenceGraph selectedEvidence)
    rows <- traverse (\site -> do
          wire <- rebase (psWireNode site)
          inputs <- traverse rebase (psInputNodes site)
          pure SiteRow
              { siteId = Effect.ysSite (psSite site)
              , siteOrigin = Effect.ysOrigin (psSite site)
              , siteOrdinal = Effect.ysOrdinal (psSite site)
              , siteDelivery = lowerDelivery (psDelivery site)
              , siteWire = wire
              , siteInputs = inputs
              }) selected
    pure (unionTypeGraphs priorNodes lowered, priorSites <> rows)

-- Compare all reachable constructor evidence before projecting bindings or
-- lowering any one graph. Representation recovery may roll one graph's local
-- state back; a conflict in that graph must still reject the complete program
-- in either encounter order. Publication remains owned by 'internConstructor'.
validatePreparedEvidence :: ProjectionContext -> [PreparedModule]
  -> [SelectedPreparedEvidence] -> [DataCon] -> P ()
validatePreparedEvidence context modules evidence hostConstructors = do
  let moduleEvidence = concatMap
        (constructorsForSelectedTypeGraph . selectedEvidenceGraph) evidence
  (auxiliaryNodes, auxiliaryRoots) <- auxiliaryRootTypeGraph context modules
  auxiliaryEvidence <- lift (constructorsForTypeGraph auxiliaryNodes auxiliaryRoots)
  validateConstructorEvidence (moduleEvidence <> auxiliaryEvidence <> hostConstructors)

-- | Force-intern type evidence for every admitted auxiliary root's own
-- answer type, the same way a declared site's answer type is interned
-- ('lowerOne'/'siteWireType' in "Tidepool.PreparedSites"). An auxiliary root
-- is not itself a site: nothing about ordinary site traversal reaches its
-- answer type, so a program whose turns never independently construct or
-- observe that type (no 'httpGet', no rendered 'Left'/'Right') would
-- otherwise leave its constructors out of the program's evidence even though
-- the auxiliary root itself needs to read them back.
--
-- The answer type is read off the binder's own pre-erasure GHC 'Type' via
-- 'splitFunTys', never off its STG closure result type: an eta-unexpanded
-- zero-arity root can retain the whole function arrow there.
--
-- An auxiliary root's answer type is skipped for evidence interning when
-- it still carries a free type variable after 'splitFunTys' (a genuinely
-- polymorphic root like 'Tidepool.Session.preparedApplyEntryTargetName'/
-- 'Tidepool.Session.preparedApplyValueTargetName', whose settled result is
-- whatever the applied closure returns, not one concrete turn's type).
-- 'TypePolicy.internType'/'classifyType' has no node for an unresolved type
-- variable, so interning one would fail the whole projection rather than
-- leaving the root's own evidence merely absent. Concrete auxiliary roots
-- are unaffected by this filter.
lowerAuxiliaryRootEvidence :: ProjectionContext -> [PreparedModule] -> Int -> P TypeGraph
lowerAuxiliaryRootEvidence context modules base = do
  (nodes, graphRoots) <- auxiliaryRootTypeGraph context modules
  (lowered, _rebase) <- lowerTypeGraph base nodes graphRoots
  pure lowered

auxiliaryRootTypeGraph :: ProjectionContext -> [PreparedModule]
  -> P (TypePolicy.TypeGraph, [TypePolicy.TypeNodeId])
auxiliaryRootTypeGraph context modules = do
  let roots = Set.fromList (projectionAuxiliaryRoots context)
  topSymbolMap <- gets topSymbols
  let answerTypes =
        [ answerType
        | prepared <- modules
        , (binding, _) <- pmBindings prepared
        , binder <- topBinders binding
        , Just symbol <- [lookupVarEnv topSymbolMap binder]
        , symbol `Set.member` roots
        , let answerType = snd (splitFunTys (varType binder))
        , isEmptyVarSet (tyCoVarsOfType answerType)
        ]
  (graphRoots, builder) <- lift $ either (Left . TypeEvidenceIssuanceFailure) Right
    (runStateT (traverse TypePolicy.internType answerTypes) TypePolicy.emptyTypeGraphBuilder)
  graph <- lift $ either (Left . TypeEvidenceIssuanceFailure) Right (TypePolicy.finishTypeGraph builder)
  pure (graph, graphRoots)

-- | A genuine saturated GHC constructor owns its result graph independently
-- of effect rows. Only the exact compiler-issued carrier selects dynamic sites.
lowerConstructorReplies :: [GHC.TyCon] -> Int -> P (TypeGraph, [(ConstructorId, ConstructorReply)])
lowerConstructorReplies carriers base = do
  known <- gets (toList . constructors)
  let candidates =
        [ (constructor, identity, index, atSite constructor index)
        | (constructor, identity) <- known
        , Just index <- [requestReplyIndex constructor]
        ]
      static = [(constructor, identity, index)
        | (constructor, identity, index, False) <- candidates]
  (roots, builder) <- lift $ either (Left . TypeEvidenceIssuanceFailure) Right
    (runStateT (traverse (\(constructor, _, reply) ->
      TypePolicy.internConstructorType constructor reply) static) TypePolicy.emptyTypeGraphBuilder)
  graph <- lift $ either (Left . TypeEvidenceIssuanceFailure) Right (TypePolicy.finishTypeGraph builder)
  (lowered, rebase) <- lowerTypeGraph base graph roots
  entries <- traverse (\((constructor, identity, _), root) -> do
      node <- rebase root
      pure (identity, case inputSite constructor of
        Just (field, payload, capture) -> StaticReplyWithSite node field payload capture
        Nothing -> StaticReply node)) (zip static roots)
  let replies = Map.fromList (entries <> [(identity, ReplyAtSite) | (_, identity, _, True) <- candidates])
  pure (lowered, [(identity, replies Map.! identity) | (_, identity, _, _) <- candidates])
 where
  -- Only the original nominal carrier authorizes an erased field. Closed
  -- replies remain independent of this site's input and eventual result.
  inputSite constructor
    | not (null (dataConTheta constructor)) = Nothing
    | length originals /= length runtime = Nothing
    | otherwise = case
        [ (fromIntegral ordinal, fromIntegral payload,
            fmap (fromIntegral . fst) (lastInput inputs >>= \(index, ty) ->
              if eqType ty (originals !! payload) then Just (index, ty) else Nothing))
        | (ordinal, (original, representation)) <- zip [0 :: Int ..] (zip originals runtime)
        , ordinal > 0
        , Just (carrier, [inputs, _]) <- [splitTyConApp_maybe original]
        , carrier `elem` carriers, eqType (unwrapType representation) intTy
        , let payload = if ordinal + 1 < length originals then ordinal + 1 else ordinal - 1
        , typePrimRep_maybe (runtime !! payload) == Just [GHC.BoxedRep (Just GHC.Lifted)] ] of
          [evidence] -> Just evidence
          _ -> Nothing
    where
      originals = [ty | Scaled _ ty <- dataConOrigArgTys constructor]
      runtime = [ty | Scaled _ ty <- dataConRepArgTys constructor]
  lastInput inputs = go 0 inputs
    where
      go index ty = case splitTyConApp_maybe ty of
        Just (cons, [_, input, rest]) | cons == promotedConsDataCon ->
          case splitTyConApp_maybe rest of
            Just (nil, [_]) | nil == promotedNilDataCon -> Just (index, input)
            _ -> go (index + 1) rest
        _ -> Nothing
  atSite constructor reply = case (dataConOrigArgTys constructor, dataConRepArgTys constructor) of
    (Scaled _ first : _, Scaled _ runtimeFirst : _) ->
      case splitTyConApp_maybe first of
        Just (carrier, [_, carrierReply]) ->
          null (dataConTheta constructor)
            && carrier `elem` carriers && eqType carrierReply reply
            && eqType (unwrapType runtimeFirst) intTy
        _ -> False
    _ -> False

-- Scope and declaration edges remain finite through selection and lowering.
-- Only final publication interns them into one output arena.
lowerTypeGraph :: Int -> TypePolicy.TypeGraph -> [TypeNodeId]
  -> P (TypeGraph, TypeNodeId -> P TypeNodeId)
lowerTypeGraph base graph roots = do
  selected <- lift (selectTypeGraph graph roots)
  lowerSelectedTypeGraph base selected

selectTypeGraph :: TypePolicy.TypeGraph -> [TypeNodeId]
  -> Either ProjectionError TypePolicy.TypeGraph
selectTypeGraph _ [] = Right TypePolicy.emptyTypeGraph
selectTypeGraph graph roots = do
  reachable <- reachableTypeNodes graph roots
  -- Reachability already authenticated every node and edge. Select those
  -- original ascending keys without scanning unrelated module evidence.
  nodes <- traverse (\index -> case IntMap.lookup index (TypePolicy.tgNodes graph) of
      Just node -> Right (index, node)
      Nothing -> Left (UnsupportedPreparedShape
        "finite type graph contains an out-of-range node")) (Set.toAscList reachable)
  pure (TypeGraph (IntMap.fromDistinctAscList nodes)
    (IntMap.fromDistinctAscList
      [(index, edges) | index <- Set.toAscList reachable
      , Just edges <- [IntMap.lookup index (TypePolicy.tgEdges graph)]]))

lowerSelectedTypeGraph :: Int -> TypePolicy.TypeGraph
  -> P (TypeGraph, TypeNodeId -> P TypeNodeId)
lowerSelectedTypeGraph base original = do
  rewritten <- traverseWithKey rewriteDeclaration (TypePolicy.tgNodes original)
  let roots = [TypeNodeId (fromIntegral index)
        | (index, TypeRoot{}) <- IntMap.toAscList rewritten]
      outgoing = IntMap.mapWithKey (\index edges -> case IntMap.lookup index rewritten of
        Just (TypeDeclaration _ _ (OpaqueDeclaration _ _) _) ->
          [(role, target) | (role, target) <- edges, not (isConstructorEdge role)]
        _ -> edges) (TypePolicy.tgEdges original)
      rewrittenGraph = TypeGraph rewritten outgoing
  selected <- lift (selectTypeGraph rewrittenGraph roots)
  let ordered = IntMap.keys (TypePolicy.tgNodes selected)
      mapping = IntMap.fromAscList
        [(old, TypeNodeId (fromIntegral (base + offset))) | (offset, old) <- zip [0 :: Int ..] ordered]
      rebase (TypeNodeId raw) = maybe
        (failShape "finite type graph selection omitted a referenced node") pure
        (IntMap.lookup (fromIntegral raw) mapping)
  nodes <- traverse (\(index, node) -> do
      converted <- lowerNode node
      target <- rebase (TypeNodeId (fromIntegral index))
      pure (typeNodeIndex target, converted)) (IntMap.toAscList (TypePolicy.tgNodes selected))
  edges <- traverse (\(index, outgoing') -> do
      source <- rebase (TypeNodeId (fromIntegral index))
      converted <- traverse (\(role, target) -> (,) <$> lowerRole role <*> rebase target) outgoing'
      pure (typeNodeIndex source, converted)) (IntMap.toAscList (TypePolicy.tgEdges selected))
  pure (TypeGraph (IntMap.fromAscList nodes) (IntMap.fromAscList edges), rebase)
 where
  traverseWithKey action = fmap IntMap.fromAscList . traverse (\(index, node) ->
    (index,) <$> action index node) . IntMap.toAscList
  isConstructorEdge TypeConstructor{} = True
  isConstructorEdge _ = False
  rewriteDeclaration index node = case node of
    TypeDeclaration constructor flags form restriction -> do
      attempted <- tryRepresentation $ case form of
        DataDeclaration -> do
          mapM_ verifyTemplate
            [target | (TypeConstructor _, target) <- graphOutgoing original index]
          pure form
        TextDeclaration -> do
          authority <- gets textUnit
          case (authority, nameModule_maybe (GHC.tyConName constructor)) of
            (Just (TextUnitAuthority expected), Just owner) | moduleUnit owner == expected ->
              mapM_ internConstructor (GHC.tyConDataCons constructor) >> pure form
            _ -> lift (Left (InvalidPreparedIdentity
              "Text graph candidate lacks its selected compiler package identity"))
        IntegerDeclaration -> mapM_ internConstructor (GHC.tyConDataCons constructor) >> pure form
        NaturalDeclaration -> mapM_ internConstructor (GHC.tyConDataCons constructor) >> pure form
        _ -> pure form
      converted <- case attempted of
        Right value -> pure value
        Left (InvalidPreparedLayout _) -> pure (OpaqueDeclaration NominalConstructor "layout")
        Left (InvalidPreparedRepresentation _) -> pure (OpaqueDeclaration NominalConstructor "representation")
        Left failure -> lift (Left failure)
      pure (TypeDeclaration constructor flags converted restriction)
    _ -> pure node
  verifyTemplate target = case IntMap.lookup (typeNodeIndex target) (TypePolicy.tgNodes original) of
    Just (TypeConstructorTemplate constructor) -> do
      let sourceFields = dataConOrigArgTys constructor
          unpacked = any isUnpacked (dataConImplBangs constructor)
      sourceReps <- traverse oneSourceRep sourceFields
      issued <- traverse (projectRep . snd)
        [(ordinal, rep) | (TypeField ordinal rep, _) <- graphOutgoing original (typeNodeIndex target)]
      runtimeReps <- concat <$> traverse (\(Scaled _ fieldType) -> repsForType fieldType)
        (dataConRepArgTys constructor)
      resultReps <- repsForType (dataConOrigResTy constructor)
      unless (not unpacked && sourceReps == issued && sourceReps == runtimeReps
          && length sourceFields == length runtimeReps && resultReps == [LiftedRefRep])
        (failLayout "original constructor field template is not one-to-one with its runtime layout")
      ConstructorId identity <- internConstructor constructor
      declared <- gets (fmap constructorFieldReps . Seq.lookup (fromIntegral identity) . constructorDecls)
      unless (declared == Just sourceReps)
        (failLayout "original field template differs from its physical constructor declaration")
    _ -> failShape "finite type declaration names a non-constructor template"
  oneSourceRep (Scaled _ fieldType) = do
    reps <- repsForType fieldType
    case reps of
      [rep] -> pure rep
      _ -> failLayout "original source field is void, flattened, or split"
  isUnpacked HsUnpack{} = True
  isUnpacked _ = False
  lowerNode node = case node of
    TypeRoot domain flags rendered -> pure (TypeRoot domain flags rendered)
    TypeDeclaration constructor flags form restriction -> do
      converted <- case form of
        DataDeclaration -> pure DataDeclaration
        NewtypeDeclaration arity -> pure (NewtypeDeclaration arity)
        TextDeclaration -> pure TextDeclaration
        IntegerDeclaration -> pure IntegerDeclaration
        NaturalDeclaration -> pure NaturalDeclaration
        ScalarDeclaration rep -> ScalarDeclaration <$> projectRep rep
        OpaqueDeclaration headKind reason -> pure (OpaqueDeclaration headKind reason)
      let name = GHC.tyConName constructor
          namespace = if isDataOcc (nameOccName name) then "data" else "type"
      pure (TypeDeclaration (nameSymbol namespace name) flags converted restriction)
    TypeConstructorTemplate constructor -> TypeConstructorTemplate <$> internConstructor constructor
    TypeBound index -> pure (TypeBound index)
    TypeNominalApplication -> pure TypeNominalApplication
    TypeApplication -> pure TypeApplication
    TypeFunction flag -> pure (TypeFunction flag)
    TypeForAll flag -> pure (TypeForAll flag)
    TypeLiteral literal -> pure (TypeLiteral literal)
  lowerRole role = case role of
    TypeBinderKind index -> pure (TypeBinderKind index)
    TypeBody -> pure TypeBody
    TypeHead -> pure TypeHead
    TypeArgument index -> pure (TypeArgument index)
    TypeFunctionEdge -> pure TypeFunctionEdge
    TypeApplyArgument -> pure TypeApplyArgument
    TypeMultiplicity -> pure TypeMultiplicity
    TypeDomain -> pure TypeDomain
    TypeCodomain -> pure TypeCodomain
    TypeKind -> pure TypeKind
    TypeConstructor tag -> pure (TypeConstructor tag)
    TypeField index rep -> TypeField index <$> projectRep rep
    TypeAliasRhs -> pure TypeAliasRhs

constructorsForTypeGraph :: TypePolicy.TypeGraph -> [TypeNodeId]
  -> Either ProjectionError [DataCon]
constructorsForTypeGraph graph roots = constructorsForSelectedTypeGraph <$> selectTypeGraph graph roots

constructorsForSelectedTypeGraph :: TypePolicy.TypeGraph -> [DataCon]
constructorsForSelectedTypeGraph = concatMap nodeConstructors . IntMap.elems . TypePolicy.tgNodes
 where
  nodeConstructors (TypeConstructorTemplate constructor) = [constructor]
  nodeConstructors (TypeDeclaration constructor _ form _) = case form of
    TextDeclaration -> GHC.tyConDataCons constructor
    IntegerDeclaration -> GHC.tyConDataCons constructor
    NaturalDeclaration -> GHC.tyConDataCons constructor
    _ -> []
  nodeConstructors _ = []

reachableTypeNodes :: TypePolicy.TypeGraph -> [TypeNodeId]
  -> Either ProjectionError (Set Int)
reachableTypeNodes graph = go Set.empty
 where
  go visited [] = Right visited
  go visited (node : pending)
    | index `Set.member` visited = go visited pending
    | IntMap.member index (TypePolicy.tgNodes graph) =
        go (Set.insert index visited) (map snd (graphOutgoing graph index) <> pending)
    | otherwise = Left (UnsupportedPreparedShape "finite type graph contains an out-of-range node")
   where index = typeNodeIndex node

graphOutgoing :: TypeGraphF constructor identity rep -> Int -> [(TypeEdgeRoleF rep, TypeNodeId)]
graphOutgoing graph index = IntMap.findWithDefault [] index (typeGraphEdges graph)

typeNodeIndex :: TypeNodeId -> Int
typeNodeIndex (TypeNodeId raw) = fromIntegral raw

graphNodeCount :: TypeGraphF constructor identity rep -> Int
graphNodeCount = IntMap.size . typeGraphNodes

emptyProjectedTypeGraph :: TypeGraph
emptyProjectedTypeGraph = TypeGraph IntMap.empty IntMap.empty

unionTypeGraphs :: TypeGraph -> TypeGraph -> TypeGraph
unionTypeGraphs first second = TypeGraph
  (IntMap.union (typeGraphNodes first) (typeGraphNodes second))
  (IntMap.union (typeGraphEdges first) (typeGraphEdges second))

-- One final arena coalesces equal declarations and scoped syntax from all
-- prepared inputs. Nominal collisions compare complete formal outgoing edges.
data TypeArena = TypeArena
  { arenaNodes :: !(IntMap.IntMap (Maybe TypeNode))
  , arenaEdges :: !(IntMap.IntMap [(TypeEdgeRoleF RuntimeRep, TypeNodeId)])
  , arenaDeclarations :: !(Map SymbolIdentity TypeNodeId)
  , arenaExpressions :: !(Map (TypeNode, [(TypeEdgeRoleF RuntimeRep, TypeNodeId)]) TypeNodeId)
  , arenaOriginals :: !(IntMap.IntMap TypeNodeId)
  , arenaNext :: !Word32
  , arenaWork :: !Int
  , arenaBytes :: !Int
  }

type Assemble = StateT TypeArena (Either ProjectionError)

assembleTypeGraph :: TypeGraph -> Either ProjectionError (TypeGraph, TypeNodeId -> Either ProjectionError TypeNodeId)
assembleTypeGraph original = do
  let initial = TypeArena IntMap.empty IntMap.empty Map.empty Map.empty IntMap.empty 0 0 0
  (_, final) <- runStateT (do
    -- Reserve every nominal owner before following expressions. Expression
    -- edges form a DAG; declaration/template links can be mutually recursive.
    mapM_ reserveDeclaration declarations
    mapM_ completeDeclaration declarations
    mapM_ intern [TypeNodeId (fromIntegral index)
      | (index, TypeRoot{}) <- IntMap.toAscList (typeGraphNodes original)]) initial
  nodes <- traverse completed (IntMap.toAscList (arenaNodes final))
  let graph = TypeGraph (IntMap.fromAscList nodes) (arenaEdges final)
      rebase identity = maybe (Left (UnsupportedPreparedShape "final type arena omitted a selected root")) Right
        (IntMap.lookup (typeNodeIndex identity) (arenaOriginals final))
  pure (graph, rebase)
 where
  failure :: Text -> Assemble a
  failure detail = lift (Left (UnsupportedPreparedShape detail))
  completed (index, Just node) = Right (index, node)
  completed (_, Nothing) = Left (UnsupportedPreparedShape "final type arena has an incomplete reservation")
  intern :: TypeNodeId -> Assemble TypeNodeId
  intern old = do
    work <- gets ((+ 1) . arenaWork)
    if work > 16777216 then lift (Left (TypeEvidenceIssuanceFailure TypePolicy.TypeGraphWorkLimit))
      else modify' (\arena -> arena { arenaWork = work })
    known <- gets (IntMap.lookup (typeNodeIndex old) . arenaOriginals)
    case known of
      Just identity -> pure identity
      Nothing -> case IntMap.lookup (typeNodeIndex old) (typeGraphNodes original) of
        Nothing -> failure "final type arena input contains an invalid reference"
        Just node@TypeDeclaration{} -> internDeclaration old node
        Just node -> do
          edges <- convertedEdges old
          let key = (semanticWeight node, edges)
          duplicate <- gets (Map.lookup key . arenaExpressions)
          identity <- case duplicate of
            Just existing -> pure existing
            Nothing -> do
              reserved <- reserve
              publish reserved node edges
              modify' (\arena -> arena
                { arenaExpressions = Map.insert key reserved (arenaExpressions arena) })
              pure reserved
          remember old identity
          pure identity
  declarations = [(TypeNodeId (fromIntegral index), node)
    | (index, node@TypeDeclaration{}) <- IntMap.toAscList (typeGraphNodes original)]
  reserveDeclaration :: (TypeNodeId, TypeNode) -> Assemble ()
  reserveDeclaration (old, TypeDeclaration symbol _ _ _) = do
    duplicate <- gets (Map.lookup symbol . arenaDeclarations)
    identity <- case duplicate of
      Just existing -> pure existing
      Nothing -> do
        reserved <- reserve
        modify' (\arena -> arena
          { arenaDeclarations = Map.insert symbol reserved (arenaDeclarations arena) })
        pure reserved
    remember old identity
  reserveDeclaration _ = failure "final type arena declaration category mismatch"
  completeDeclaration :: (TypeNodeId, TypeNode) -> Assemble ()
  completeDeclaration (old, node) = do
    known <- gets (IntMap.lookup (typeNodeIndex old) . arenaOriginals)
    identity <- maybe (failure "final type arena declaration has no owner") pure known
    edges <- convertedEdges old
    previous <- gets (IntMap.lookup (typeNodeIndex identity) . arenaNodes)
    case previous of
      Just Nothing -> publish identity node edges
      Just (Just existing) -> do
        existingEdges <- gets (IntMap.findWithDefault [] (typeNodeIndex identity) . arenaEdges)
        unless (semanticWeight existing == semanticWeight node && existingEdges == edges)
          (lift (Left (TypeEvidenceIssuanceFailure TypePolicy.TypeGraphOriginalDeclarationMismatch)))
      _ -> failure "final type arena declaration has no reservation"
  internDeclaration old _ = do
    known <- gets (IntMap.lookup (typeNodeIndex old) . arenaOriginals)
    maybe (failure "final type arena declaration was not reserved") pure known
  convertedEdges old = traverse (\(role, target) -> (role,) <$> intern target)
    (graphOutgoing original (typeNodeIndex old))
  remember old identity = modify' (\arena -> arena
    { arenaOriginals = IntMap.insert (typeNodeIndex old) identity (arenaOriginals arena) })
  reserve :: Assemble TypeNodeId
  reserve = do
    next <- gets arenaNext
    when (next >= 65535) (lift (Left (TypeEvidenceIssuanceFailure TypePolicy.TypeGraphNodeLimit)))
    let identity = TypeNodeId next
    modify' (\arena -> arena
      { arenaNodes = IntMap.insert (fromIntegral next) Nothing (arenaNodes arena)
      , arenaNext = next + 1 })
    pure identity
  publish :: TypeNodeId -> TypeNode -> [(TypeEdgeRoleF RuntimeRep, TypeNodeId)] -> Assemble ()
  publish identity node edges = do
    let cost = 32 + nodeTextBytes node + length edges * 32
    bytes <- gets ((+ cost) . arenaBytes)
    when (bytes > 16777216) (lift (Left (TypeEvidenceIssuanceFailure TypePolicy.TypeGraphByteLimit)))
    work <- gets ((+ cost) . arenaWork)
    when (work > 16777216) (lift (Left (TypeEvidenceIssuanceFailure TypePolicy.TypeGraphWorkLimit)))
    modify' (\arena -> arena
      { arenaWork = work
      , arenaNodes = IntMap.insert (typeNodeIndex identity) (Just node) (arenaNodes arena)
      , arenaEdges = IntMap.insert (typeNodeIndex identity) edges (arenaEdges arena)
      , arenaBytes = bytes })
  semanticWeight (TypeRoot domain flags _) = TypeRoot domain flags ""
  semanticWeight (TypeDeclaration symbol flags (OpaqueDeclaration headKind _) restriction) =
    TypeDeclaration symbol flags (OpaqueDeclaration headKind "") restriction
  semanticWeight node = node
  nodeTextBytes node = case node of
    TypeRoot _ flags rendered -> length flags + utf8Bytes rendered
    TypeDeclaration symbol flags form _ -> length flags
      + sum (map utf8Bytes [symbolUnit symbol, symbolModule symbol, symbolNamespace symbol, symbolOccurrence symbol])
      + case form of OpaqueDeclaration _ reason -> utf8Bytes reason; _ -> 0
    TypeLiteral (NaturalTypeLiteral value) -> utf8Bytes value
    TypeLiteral (SymbolTypeLiteral value) -> utf8Bytes value
    _ -> 0
  utf8Bytes = BS.length . TextEncoding.encodeUtf8

tryRepresentation :: P a -> P (Either ProjectionError a)
tryRepresentation action = StateT $ \machineState -> case runStateT action machineState of
  Left failure -> Right (Left failure, machineState)
  Right (value, next) -> Right (Right value, next)

lowerDelivery :: Effect.SiteDelivery -> SiteDelivery
lowerDelivery delivery = case delivery of
  Effect.DeliverHostAnswer -> HostAnswer
  Effect.DeliverLiveReentry -> LiveReentry
  Effect.DeliverExitCellFill -> ExitCellFill
  Effect.DeliverTerminalCapture -> TerminalCapture

registerBindingArities :: CgStgTopBinding -> P ()
registerBindingArities (StgTopStringLit _ _) = pure ()
registerBindingArities (StgTopLifted binding) = registerBindingEntryArities binding

registerBindingEntryArities :: CgStgBinding -> P ()
registerBindingEntryArities (StgNonRec binder rhs) = registerRhsEntryArity binder rhs
registerBindingEntryArities (StgRec pairs) =
  mapM_ (uncurry registerRhsEntryArity) pairs

registerRhsEntryArity :: Id -> CgStgRhs -> P ()
registerRhsEntryArity binder (StgRhsClosure _ _ _ parameters _ _) =
  modify' (\current -> current
    { entryArities = extendVarEnv (entryArities current) binder (length parameters) })
registerRhsEntryArity _ _ = pure ()

projectTop :: PreparedModule -> CgStgTopBinding -> P (Group TopBinding)
projectTop prepared (StgTopStringLit binder bytes) = do
  identity <- requireTopValue binder
  symbol <- topIdentity binder
  validateExpectedEntry prepared binder (Bytes bytes)
  pure (NonRecursive (TopBinding symbol
    (HeapBinding identity (Bytes bytes))))
projectTop prepared (StgTopLifted (StgNonRec binder rhs)) =
  NonRecursive <$> projectTopPair prepared binder rhs
projectTop prepared (StgTopLifted (StgRec pairs)) =
  Recursive <$> mapM (uncurry (projectTopPair prepared)) pairs

projectTopPair :: PreparedModule -> Id -> CgStgRhs -> P TopBinding
projectTopPair prepared binder rhs = do
  symbol <- topIdentity binder
  formatting <- formattingSpecFor binder
  time <- timeSpecFor binder
  json <- jsonSpecFor binder
  let project = case deferredFunction binder of
        Just deferred -> projectDeferredRhs binder deferred rhs
        Nothing -> case json of
          Just spec -> projectJsonRhs spec rhs
          Nothing -> case time of
            Just spec -> projectTimeRhs spec rhs
            Nothing -> maybe (projectRhs binder rhs)
              (\spec -> projectFormattingRhs spec rhs) formatting
  projected <- project
  validateExpectedEntry prepared binder projected
  TopBinding symbol <$> (HeapBinding <$> requireTopValue binder <*> pure projected)

-- Recovery can prepare a provisional subset before its same-owner siblings
-- are available. Only selected emitted tops must satisfy the defining entry;
-- expected interface facts never substitute for the body's actual ABI.
validateExpectedEntry :: PreparedModule -> Id -> HeapRhs -> P ()
validateExpectedEntry prepared binder rhs = do
  purpose <- gets projectionPurpose
  case purpose of
    OriginalHomeProduct _ -> pure ()
    ExecutableTarget -> validate
 where
  validate = case preparedExpectedEntry prepared binder of
    Nothing -> pure ()
    Just original -> do
      (required, requiresEvaluated) <- importedEntry original
      (offered, evaluated) <- case rhs of
        Function signature _ _ _ -> (\entry -> (Just entry, True)) <$> signatureForId signature
        Thunk signature _ _ _ -> (\entry -> (Just entry, False)) <$> signatureForId signature
        Constructor{} -> pure (Nothing, True)
        Bytes{} -> pure (Nothing, True)
      let entryMatches = case importedIdLFInfo original of
            LFUnknown{} -> True
            _ -> offered == required
      unless (entryMatches
          && (not requiresEvaluated || evaluated)) $ do
        symbol <- topIdentity binder
        let failure = RecoveredEntryContractMismatch symbol
              required requiresEvaluated offered evaluated
        admission <- gets entryContractAdmission
        case admission of
          FinalEntryContracts -> lift (Left failure)
          DeferredEntryContracts -> modify' (\current -> current
            { entryContractFailures = failure : entryContractFailures current })

formattingSpecFor :: Id -> P (Maybe FormattingSpec)
formattingSpecFor binder = do
  authority <- gets formattingAuthority
  case authority of
    Nothing -> pure Nothing
    Just owner -> case classifyFormatting owner binder of
      Left failure -> failShape (Text.pack (show failure))
      Right result -> pure result

timeSpecFor :: Id -> P (Maybe TimeSpec)
timeSpecFor binder = do
  authority <- gets timeAuthority
  case authority of
    Nothing -> pure Nothing
    Just owner -> case classifyTime owner binder of
      Left failure -> failShape (Text.pack (show failure))
      Right result -> pure result

jsonSpecFor :: Id -> P (Maybe JsonSpec)
jsonSpecFor binder = do
  authority <- gets jsonAuthority
  case authority of
    Nothing -> pure Nothing
    Just owner -> case classifyJson owner binder of
      Left failure -> failShape (Text.pack (show failure))
      Right result -> pure result

projectJsonRhs :: JsonSpec -> CgStgRhs -> P HeapRhs
projectJsonRhs (DecodeJson textDataCon left right)
    (StgRhsClosure _ _ ReEntrant parameters _ resultType) = withScope $ do
  actual <- concat <$> mapM (argumentRepsForType . varType) parameters
  result <- repsForType resultType
  unless (actual == [LiftedRefRep] && result == [LiftedRefRep])
    (failRepresentation "registered JSON parser has unexpected prepared entry reps")
  textConstructor <- internConstructor textDataCon
  left' <- internConstructor left
  right' <- internConstructor right
  textFields <- concat <$> mapM (repsForType . scaledThing) (dataConRepArgTys textDataCon)
  unless (textFields == [UnliftedRefRep, IntRep 64, IntRep 64])
    (failRepresentation "JSON parser Text constructor must contain byte array, offset, length")
  parameters' <- mapM bindValue parameters
  signature <- internSignature (Signature [LiftedRefRep] (Returns [LiftedRefRep]))
  body <- case parameters' of
    [input] -> jsonDecodeBody textDataCon textConstructor left' right' input
    _ -> failShape "registered JSON parser has unexpected prepared arity"
  pure (Function signature parameters' [] body)
 where scaledThing (Scaled _ ty) = ty
projectJsonRhs (EncodeJson textDataCon)
    (StgRhsClosure _ _ ReEntrant parameters _ resultType) = withScope $ do
  actual <- concat <$> mapM (argumentRepsForType . varType) parameters
  result <- repsForType resultType
  unless (actual == [LiftedRefRep] && result == [LiftedRefRep])
    (failRepresentation "registered JSON encoder has unexpected prepared entry reps")
  textFields <- concat <$> mapM (repsForType . scaledThing) (dataConRepArgTys textDataCon)
  unless (textFields == [UnliftedRefRep, IntRep 64, IntRep 64])
    (failRepresentation "JSON encoder Text constructor must contain byte array, offset, length")
  parameters' <- mapM bindValue parameters
  signature <- internSignature (Signature [LiftedRefRep] (Returns [LiftedRefRep]))
  encodeSignature <- internSignature (Signature [LiftedRefRep] (Returns [LiftedRefRep]))
  encode <- internSyntheticOperation Schema.JsonEncodeIdentity encodeSignature
  body <- case parameters' of
    [input] -> pure (Operation encode [Ref (Local input)])
    _ -> failShape "registered JSON encoder has unexpected prepared arity"
  pure (Function signature parameters' [] body)
 where scaledThing (Scaled _ ty) = ty
projectJsonRhs _ _ = failShape "registered JSON anchor is not a reentrant closure"

jsonDecodeBody :: DataCon -> ConstructorId -> ConstructorId -> ConstructorId -> ValueId -> P Expr
jsonDecodeBody textDataCon textConstructor left right input = do
  enter <- internSignature (Signature [] (Returns [LiftedRefRep]))
  parseSignature <- internSignature (Signature
    [UnliftedRefRep, IntRep 64, IntRep 64] (Returns [LiftedRefRep]))
  parse <- internSyntheticOperation (Schema.JsonDecodeIdentity left right) parseSignature
  inputCase <- freshValue
  rawBytes <- freshValue
  rawOffset <- freshValue
  rawLength <- freshValue
  let textFamily = AlgebraicCase (nameSymbol "type"
        (GHC.tyConName (dataConTyCon textDataCon)))
      parsed = Operation parse
        [Ref (Local rawBytes), Ref (Local rawOffset), Ref (Local rawLength)]
  pure (Case (Enter (Ref (Local input)) enter) inputCase (Returns [LiftedRefRep]) textFamily
    [Alternative (ConstructorPattern textConstructor) [rawBytes, rawOffset, rawLength] parsed])

-- A registered wrapper is a normal function top. The source body has already
-- established non-bottoming demand facts in GHC; only its dependencies and
-- executable body are replaced at this projection boundary.
projectFormattingRhs :: FormattingSpec -> CgStgRhs -> P HeapRhs
projectFormattingRhs spec (StgRhsClosure _ _ ReEntrant parameters _ resultType) = withScope $ do
  let expected = case formattingKind spec of
        RenderDouble -> [LiftedRefRep]
        RenderDoublePrec -> [LiftedRefRep, LiftedRefRep]
  actual <- concat <$> mapM (argumentRepsForType . varType) parameters
  result <- repsForType resultType
  unless (actual == expected && result == [LiftedRefRep])
    (failRepresentation "registered formatting wrapper has unexpected prepared entry reps")
  textConstructor <- internConstructor (formattingTextConstructor spec)
  textFields <- concat <$> mapM (repsForType . scaledThing)
    (dataConRepArgTys (formattingTextConstructor spec))
  unless (textFields == [UnliftedRefRep, IntRep 64, IntRep 64])
    (failRepresentation "Text constructor must contain byte array, offset, length")
  parameters' <- mapM bindValue parameters
  signature <- internSignature (Signature expected (Returns [LiftedRefRep]))
  body <- formattingBody spec textConstructor parameters'
  pure (Function signature parameters' [] body)
  where scaledThing (Scaled _ ty) = ty
projectFormattingRhs _ _ = failShape "registered formatting wrapper is not a reentrant closure"

-- The shipped definition is deliberately opaque and returning so GHC may
-- learn strictness without learning a false result constructor. Projection
-- replaces its complete body, including higher-order uses of the top.
projectTimeRhs :: TimeSpec -> CgStgRhs -> P HeapRhs
projectTimeRhs spec (StgRhsClosure _ _ ReEntrant parameters _ resultType) = withScope $ do
  actual <- concat <$> mapM (argumentRepsForType . varType) parameters
  result <- repsForType resultType
  unless (actual == [LiftedRefRep] && result == [LiftedRefRep])
    (failRepresentation "registered time parser has unexpected prepared entry reps")
  let textDataCon = timeTextConstructor spec
  textConstructor <- internConstructor textDataCon
  textFields <- concat <$> mapM (repsForType . scaledThing) (dataConRepArgTys textDataCon)
  unless (textFields == [UnliftedRefRep, IntRep 64, IntRep 64])
    (failRepresentation "time parser Text constructor must contain byte array, offset, length")
  leftConstructor <- internConstructor (timeLeftConstructor spec)
  rightConstructor <- internConstructor (timeRightConstructor spec)
  parameters' <- mapM bindValue parameters
  signature <- internSignature (Signature [LiftedRefRep] (Returns [LiftedRefRep]))
  body <- case parameters' of
    [input] -> timeBody spec textConstructor leftConstructor rightConstructor input
    _ -> failShape "registered time parser has unexpected prepared arity"
  pure (Function signature parameters' [] body)
  where scaledThing (Scaled _ ty) = ty
projectTimeRhs _ _ = failShape "registered time parser is not a reentrant closure"

timeBody :: TimeSpec -> ConstructorId -> ConstructorId -> ConstructorId -> ValueId -> P Expr
timeBody spec textConstructor leftConstructor rightConstructor input = do
  enter <- internSignature (Signature [] (Returns [LiftedRefRep]))
  parseSignature <- internSignature (Signature
    [UnliftedRefRep, IntRep 64, IntRep 64]
    (Returns [IntRep 64, IntRep 64, UnliftedRefRep]))
  parse <- internSyntheticOperation
    (Schema.IntrinsicIdentity "prepared_parse_iso8601" Schema.CCall) parseSignature
  sizeSignature <- internSignature (Signature [UnliftedRefRep] (Returns [IntRep 64]))
  size <- internSyntheticOperation (Schema.PrimOpIdentity "sizeofByteArray#") sizeSignature
  intConstructor <- internConstructor intDataCon
  inputCase <- freshValue
  rawBytes <- freshValue
  rawOffset <- freshValue
  rawLength <- freshValue
  parseCase <- freshValue
  succeeded <- freshValue
  decisionCase <- freshValue
  millis <- freshValue
  errorBytes <- freshValue
  errorLengthCase <- freshValue
  errorLength <- freshValue
  errorText <- freshValue
  boxedMillis <- freshValue
  let textFamily = AlgebraicCase (nameSymbol "type"
        (GHC.tyConName (dataConTyCon (timeTextConstructor spec))))
      intFamily = AlgebraicCase (nameSymbol "type" (GHC.tyConName (dataConTyCon intDataCon)))
      failure = Case (Operation size [Ref (Local errorBytes)]) errorLengthCase
        (Returns [IntRep 64]) MultiValueCase
        [Alternative DefaultPattern [errorLength]
          (Case (Construct textConstructor
              [ Ref (Local errorBytes)
              , Scalar (IntLiteral 64 (BS.replicate 8 0))
              , Ref (Local errorLength)
              ]) errorText (Returns [LiftedRefRep]) PolymorphicCase
            [Alternative DefaultPattern []
              (Construct leftConstructor [Ref (Local errorText)])])]
      success = Case (Construct intConstructor [Ref (Local millis)]) boxedMillis
        (Returns [LiftedRefRep]) intFamily
        [Alternative DefaultPattern []
          (Construct rightConstructor [Ref (Local boxedMillis)])]
      decide = Case (Return [Ref (Local succeeded)]) decisionCase
        (Returns [IntRep 64]) (PrimitiveCase (IntRep 64))
        [ Alternative (LiteralPattern (IntLiteral 64 (BS.replicate 8 0))) [] failure
        , Alternative DefaultPattern [] success
        ]
      parsed = Case (Operation parse
          [Ref (Local rawBytes), Ref (Local rawOffset), Ref (Local rawLength)])
        parseCase (Returns [IntRep 64, IntRep 64, UnliftedRefRep]) MultiValueCase
        [Alternative DefaultPattern [succeeded, millis, errorBytes] decide]
  pure (Case (Enter (Ref (Local input)) enter) inputCase (Returns [LiftedRefRep]) textFamily
    [Alternative (ConstructorPattern textConstructor) [rawBytes, rawOffset, rawLength] parsed])

formattingBody :: FormattingSpec -> ConstructorId -> [ValueId] -> P Expr
formattingBody spec textConstructor parameters = do
  (boxedDouble, boxedPrecedence) <- case (formattingKind spec, parameters) of
    (RenderDouble, [value]) -> pure (value, Nothing)
    (RenderDoublePrec, [precedence, value]) -> pure (value, Just precedence)
    _ -> failShape "registered formatting wrapper has unexpected prepared arity"
  enter <- internSignature (Signature [] (Returns [LiftedRefRep]))
  doubleConstructor <- internConstructor doubleDataCon
  doubleCase <- freshValue
  rawDouble <- freshValue
  let doubleAtom = Ref (Local rawDouble)
      doubleFamily = AlgebraicCase (nameSymbol "type"
        (GHC.tyConName (dataConTyCon doubleDataCon)))
  rendered <- case formattingKind spec of
    RenderDouble -> renderText textConstructor RenderDouble [doubleAtom]
    RenderDoublePrec -> do
      precedence <- maybe
        (failShape "precedence wrapper omitted its boxed Int") pure boxedPrecedence
      needSignature <- internSignature (Signature [FloatRep 64] (Returns [IntRep 64]))
      need <- internSyntheticOperation
        (Schema.IntrinsicIdentity "prepared_double_needs_precedence" Schema.CCall)
        needSignature
      decision <- freshValue
      plain <- renderText textConstructor RenderDouble [doubleAtom]
      intConstructor <- internConstructor intDataCon
      intCase <- freshValue
      rawInt <- freshValue
      negative <- renderText textConstructor RenderDoublePrec
        [Ref (Local rawInt), doubleAtom]
      let intFamily = AlgebraicCase (nameSymbol "type"
            (GHC.tyConName (dataConTyCon intDataCon)))
          forcePrecedence = Case (Enter (Ref (Local precedence)) enter)
            intCase (Returns [LiftedRefRep]) intFamily
            [Alternative (ConstructorPattern intConstructor) [rawInt] negative]
      pure (Case (Operation need [doubleAtom]) decision (Returns [IntRep 64])
        (PrimitiveCase (IntRep 64))
        [ Alternative (LiteralPattern (IntLiteral 64 (BS.replicate 8 0))) [] plain
        , Alternative DefaultPattern [] forcePrecedence ])
  pure (Case (Enter (Ref (Local boxedDouble)) enter) doubleCase
    (Returns [LiftedRefRep]) doubleFamily
    [Alternative (ConstructorPattern doubleConstructor) [rawDouble] rendered])

renderText :: ConstructorId -> FormattingIntrinsic -> [Atom] -> P Expr
renderText textConstructor kind arguments = do
  let (label, argumentReps) = case kind of
        RenderDouble -> ("prepared_render_double_bytes", [FloatRep 64])
        RenderDoublePrec -> ("prepared_render_double_prec_bytes", [IntRep 64, FloatRep 64])
  renderSignature <- internSignature (Signature argumentReps (Returns [UnliftedRefRep]))
  render <- internSyntheticOperation (Schema.IntrinsicIdentity label Schema.CCall) renderSignature
  sizeSignature <- internSignature (Signature [UnliftedRefRep] (Returns [IntRep 64]))
  size <- internSyntheticOperation (Schema.PrimOpIdentity "sizeofByteArray#") sizeSignature
  bytesCase <- freshValue
  bytesValue <- freshValue
  lengthCase <- freshValue
  lengthValue <- freshValue
  let bytes = Ref (Local bytesValue)
      text = Construct textConstructor
        [bytes, Scalar (IntLiteral 64 (BS.replicate 8 0)), Ref (Local lengthValue)]
  pure (Case (Operation render arguments) bytesCase (Returns [UnliftedRefRep])
    MultiValueCase [Alternative DefaultPattern [bytesValue]
      (Case (Operation size [bytes]) lengthCase (Returns [IntRep 64])
        MultiValueCase [Alternative DefaultPattern [lengthValue] text])])

projectRhs :: Id -> CgStgRhs -> P HeapRhs
projectRhs binder (StgRhsClosure captures _ update parameters body resultType) = withScope $ do
  captureRefs <- mapM projectReference (dVarSetElems captures)
  parameterIds <- mapM bindValue parameters
  let callableArity = case update of
        ReEntrant -> length parameters
        _ -> 0
  resultContract <- resultContractFor binder callableArity resultType
  projectedBody <- projectBody resultContract resultType body
  case update of
    ReEntrant -> Function <$> (internSignature =<< signatureFor parameters resultContract)
      <*> pure parameterIds <*> pure captureRefs <*> pure projectedBody
    Updatable -> do
      signature <- internSignature (Signature [] resultContract)
      pure (Thunk signature Memoize captureRefs projectedBody)
    Stg.SingleEntry -> do
      signature <- internSignature (Signature [] resultContract)
      pure (Thunk signature Schema.SingleEntry captureRefs projectedBody)
    JumpedTo -> failShape ("heap binding marked JumpedTo: " <> symbolText (idSymbol "value" binder))
projectRhs _ (StgRhsCon _ con _ _ args _) = Constructor <$> internConstructor con <*> mapM projectArg args

-- | A bottoming enclosing binding need not call a statically bottoming callee
-- (a function parameter is the ordinary counterexample). Preserve that callee's
-- concrete result convention, then discharge the enclosing no-success promise
-- with an empty case. A returned value takes the typed integrity path, never a
-- fabricated successful result. Runtime-polymorphic dead ends still require
-- their own callee evidence; no representation is guessed for them.
projectBody :: ResultContract -> Type -> CgStgExpr -> P Expr
projectBody NoSuccess resultType body
  | Just _ <- typePrimRep_maybe resultType = do
      results <- Returns <$> repsForType resultType
      expression <- projectExpr results body
      binder <- freshValue
      pure (Case expression binder results MultiValueCase [])
projectBody expected _ body = projectExpr expected body

projectExpr :: ResultContract -> CgStgExpr -> P Expr
projectExpr expected (StgApp function args) = do
  knownJoins <- gets joins
  case lookupVarEnv knownJoins function of
    Just (join, parameterReps) -> do
      unless (length args == length parameterReps) $
        failShape "join argument count differs from its prepared parameters"
      Jump join <$> sequence (zipWith projectJoinArg parameterReps args)
    Nothing -> case args of
      [] -> do
        deadEnd <- deadEndApplicationSaturated function args
        if deadEnd
          then do
            callee <- Ref <$> projectReference function
            signature <- internSignature (Signature [] NoSuccess)
            pure (Enter callee signature)
          else do
            reps <- repsForType (varType function)
            case reps of
              [] -> pure (Return [])
              [LiftedRefRep] -> do
                callee <- Ref <$> projectReference function
                signature <- internSignature =<< signatureForApplication [] expected
                pure (Enter callee signature)
              [_] -> Return . pure . Ref <$> projectReference function
              _ -> failRepresentation "zero-argument STG application retains a multi-component variable"
      _ -> do
        projectedArgs <- mapM projectArg args
        callee <- Ref <$> projectReference function
        deadEnd <- deadEndApplicationSaturated function args
        signature <- if deadEnd
          then internSignature =<< signatureForArgsNoSuccess args
          else internSignature =<< signatureForApplication args expected
        pure (Call callee signature projectedArgs)
projectExpr _ (StgLit literal) = Return . pure <$> projectLiteralAtom literal
projectExpr _ (StgConApp con _ args _)
  | isUnboxedTupleDataCon con = Return <$> mapM projectArg args
  | otherwise = Construct <$> internConstructor con <*> mapM projectArg args
projectExpr _ (StgOpApp (StgPrimOp TagToEnumOp) args resultType) =
  projectTagToEnum args resultType
projectExpr _ (StgOpApp (StgPrimOp primop) args _)
  | primop `elem` [RaiseOp, RaiseDivZeroOp, RaiseUnderflowOp] = do
    signature <- internSignature =<< signatureForArgsNoSuccess args
    Operation <$> internOperation (StgPrimOp primop) signature <*> mapM projectArg args
projectExpr _ (StgOpApp op args resultType) = do
  signature <- internSignature =<< signatureForArgs args resultType
  Operation <$> internOperation op signature <*> mapM projectArg args
projectExpr expected (StgCase scrutinee binder altType alts) = do
  scrutineeResults <- case alts of
    [] | typePrimRep_maybe (varType binder) == Nothing -> pure NoSuccess
    _ -> Returns <$> repsForType (varType binder)
  projectedScrutinee <- projectExpr scrutineeResults scrutinee
  kind <- reconcileIntegerCase scrutineeResults <$> projectCaseKind altType
  (identity, alternatives) <- withScope $ do
    identity <- bindValue binder
    alternatives <- mapM (projectAlt expected altType) alts
    pure (identity, alternatives)
  pure (Case projectedScrutinee identity scrutineeResults kind
    (map (retagIntegerPattern kind) alternatives))
projectExpr expected (StgLet _ binding body) = withScope $
  Let <$> projectLocalGroup binding <*> projectExpr expected body
projectExpr expected (StgLetNoEscape _ binding body) = withScope $
  LetJoins <$> projectJoinGroup binding <*> projectExpr expected body
projectExpr expected (StgTick _ body) = projectExpr expected body

-- | A case binder of type @Word#@ may be scrutinized with @Int#@ alternatives
-- (and the reverse) once casts are erased. Same-width signed and unsigned
-- integers share one bit pattern, so the case takes the scrutinee's
-- representation and its literal patterns keep their bytes.
reconcileIntegerCase :: ResultContract -> CaseKind -> CaseKind
reconcileIntegerCase (Returns [actual]) (PrimitiveCase declared)
  | sameWidthInteger actual declared = PrimitiveCase actual
  where
    sameWidthInteger (IntRep a) (WordRep b) = a == b
    sameWidthInteger (WordRep a) (IntRep b) = a == b
    sameWidthInteger _ _ = False
reconcileIntegerCase _ kind = kind

retagIntegerPattern :: CaseKind -> Alternative -> Alternative
retagIntegerPattern (PrimitiveCase (WordRep bits))
  (Alternative (LiteralPattern (IntLiteral width bytes)) binders body)
  | width == bits = Alternative (LiteralPattern (WordLiteral width bytes)) binders body
retagIntegerPattern (PrimitiveCase (IntRep bits))
  (Alternative (LiteralPattern (WordLiteral width bytes)) binders body)
  | width == bits = Alternative (LiteralPattern (IntLiteral width bytes)) binders body
retagIntegerPattern _ alternative = alternative

-- | GHC supplies the complete enumeration through the result type. Lower its
-- zero-based tag to ordinary classified cases, never reconstruct a family from
-- constructors encountered elsewhere. An invalid tag takes the typed case-failure
-- path; there is no fabricated default constructor.
projectTagToEnum :: [StgArg] -> Type -> P Expr
projectTagToEnum [argument] resultType = do
  family <- case splitTyConApp_maybe resultType of
    Just (tycon, _) | GHC.isEnumerationTyCon tycon -> pure tycon
    _ -> failRepresentation "tagToEnum# requires GHC enumeration result evidence"
  bits <- gets (targetWordWidth . target)
  actual <- argumentRepsForType $ case argument of
    StgVarArg value -> varType value
    StgLitArg literal -> literalType literal
  unless (actual == [IntRep bits])
    (failRepresentation "tagToEnum# requires a machine Int argument")
  atom <- projectArg argument
  binder <- freshValue
  alternatives <- forM (GHC.tyConDataCons family) $ \constructor -> do
    identity <- internConstructor constructor
    let tag = toInteger (dataConTag constructor) - 1
    pure (Alternative (LiteralPattern (IntLiteral bits (integerBytes bits tag)))
      [] (Construct identity []))
  pure (Case (Return [atom]) binder (Returns [IntRep bits])
    (PrimitiveCase (IntRep bits)) alternatives)
projectTagToEnum _ _ = failRepresentation "tagToEnum# requires exactly one argument"

projectCaseKind :: AltType -> P CaseKind
projectCaseKind (AlgAlt tycon) = do
  -- Prepared dispatch recognizes a scrutinee by the descriptors its program
  -- declares for the family, so declare the whole family: a constructor the
  -- alternatives do not name (a host answer's, another program's) must reach
  -- the default rather than look like a foreign object. A constructor with
  -- no prepared layout cannot be built anywhere and stays undeclared.
  forM_ (GHC.tyConDataCons tycon) $ \constructor -> do
    attempted <- tryRepresentation (internConstructor constructor)
    case attempted of
      Right _ -> pure ()
      Left (InvalidPreparedRepresentation _) -> pure ()
      Left (InvalidPreparedLayout _) -> pure ()
      Left failure -> lift (Left failure)
  pure (AlgebraicCase (nameSymbol "type" (GHC.tyConName tycon)))
projectCaseKind (PrimAlt rep) = PrimitiveCase <$> projectRep rep
projectCaseKind (MultiValAlt _) = pure MultiValueCase
projectCaseKind PolyAlt = pure PolymorphicCase

projectAlt :: ResultContract -> AltType -> CgStgAlt -> P Alternative
projectAlt expected (MultiValAlt _) (GenStgAlt (DataAlt con) binders body)
  | isUnboxedTupleDataCon con = withScope $ Alternative DefaultPattern
      <$> mapM bindValue binders <*> projectExpr expected body
projectAlt expected _ (GenStgAlt con binders body) = withScope $ Alternative <$> projectPattern con
  <*> mapM bindValue binders <*> projectExpr expected body

projectPattern :: AltCon -> P AlternativePattern
projectPattern DEFAULT = pure DefaultPattern
projectPattern (DataAlt con) = ConstructorPattern <$> internConstructor con
projectPattern (LitAlt literal) = LiteralPattern <$> projectLiteral literal

projectLocalGroup :: CgStgBinding -> P (Group HeapBinding)
projectLocalGroup (StgNonRec binder rhs) = do
  registerRhsEntryArity binder rhs
  projectedRhs <- projectRhs binder rhs
  identity <- bindValue binder
  pure (NonRecursive (HeapBinding identity projectedRhs))
projectLocalGroup (StgRec pairs) = do
  mapM_ (uncurry registerRhsEntryArity) pairs
  identities <- mapM (bindValue . fst) pairs
  Recursive <$> forM (zip identities pairs) (\(identity, (binder, rhs)) ->
    HeapBinding identity <$> projectRhs binder rhs)

projectJoinGroup :: CgStgBinding -> P (Group JoinBinding)
projectJoinGroup (StgNonRec binder rhs) = do
  registerRhsEntryArity binder rhs
  identity <- freshJoin
  projected <- projectJoin identity binder rhs
  parameterReps <- joinArgumentReps binder rhs
  modify' (\current -> current { joins = extendVarEnv (joins current) binder (identity, parameterReps) })
  pure (NonRecursive projected)
projectJoinGroup (StgRec pairs) = do
  mapM_ (uncurry registerRhsEntryArity) pairs
  identities <- mapM (uncurry bindJoin) pairs
  Recursive <$> forM (zip identities pairs) (\(identity, (binder, rhs)) ->
    projectJoin identity binder rhs)

projectJoin :: JoinId -> Id -> CgStgRhs -> P JoinBinding
projectJoin identity binder (StgRhsClosure _ _ JumpedTo parameters body resultType) = withScope $ do
  resultContract <- resultContractFor binder (length parameters) resultType
  JoinBinding identity <$> (internSignature =<< signatureFor parameters resultContract)
    <*> mapM bindValue parameters
    <*> projectBody resultContract resultType body
projectJoin _ binder _ = failShape
  ("let-no-escape binding lacks JumpedTo form: " <> symbolText (idSymbol "join" binder))

projectArg :: StgArg -> P Atom
projectArg (StgVarArg binder) = do
  reps <- repsForType (varType binder)
  if null reps then pure Void else Ref <$> projectReference binder
projectArg (StgLitArg literal) = projectLiteralAtom literal

-- GHC unarises sum tags as signed literals while join parameters use
-- the sum's unsigned slot convention. The actual prepared join determines
-- that convention; only same-width integer literals change their wire tag.
-- Variable references and other representations keep their typed identity.
projectJoinArg :: RuntimeRep -> StgArg -> P Atom
projectJoinArg expected argument = do
  actual <- argumentRepsForType $ case argument of
    StgVarArg binder -> varType binder
    StgLitArg literal -> literalType literal
  atom <- projectArg argument
  case (expected, atom) of
    (WordRep bits, Scalar (IntLiteral width bytes))
      | bits == width -> pure (Scalar (WordLiteral width bytes))
    (IntRep bits, Scalar (WordLiteral width bytes))
      | bits == width -> pure (Scalar (IntLiteral width bytes))
    _ | actual == [expected] -> pure atom
      | otherwise -> failRepresentation "join argument differs from its prepared parameter representation"

projectReference :: Id -> P ValueRef
projectReference binder | Just kind <- wiredInErrorKind binder =
  Local <$> internWiredInError binder kind
projectReference binder | Just deferred <- deferredFunction binder =
  Local <$> deferredFunctionReference binder deferred
projectReference binder = do
  known <- gets values
  case lookupVarEnv known binder of
    Just identity -> pure (Local identity)
    Nothing -> do
      generations <- gets retainedGenerations
      -- An executable import always resolves as a Global, regardless of
      -- whether its defining module is also being compiled alongside this
      -- one: retention is never inferred from module membership.
      if Map.member (idSymbol "value" binder) generations
        then Global <$> internGlobal binder
        else do
          topNames <- gets topSymbols
          tops <- gets topValues
          offGroup <- gets externalizedTops
          let symbol = lookupVarEnv topNames binder
          case symbol of
            Just home -> case Map.lookup home tops of
              Just identity -> pure (Local identity)
              Nothing | home `Set.member` offGroup -> Global <$> internGlobal binder
              Nothing -> lift (Left (MissingPreparedTop home))
            Nothing -> case nullaryWorkerConstructor binder of
              Just con -> Local <$> internNullaryWorker binder con
              Nothing -> Global <$> internGlobal binder

deferredFunctionReference :: Id -> DeferredFunction -> P ValueId
deferredFunctionReference binder deferred = do
  topNames <- gets topSymbols
  tops <- gets topValues
  offGroup <- gets externalizedTops
  case lookupVarEnv topNames binder of
    Just symbol -> case Map.lookup symbol tops of
      Just identity -> pure identity
      Nothing | symbol `Set.member` offGroup -> internDeferredFunction binder deferred
      Nothing -> lift (Left (MissingPreparedTop symbol))
    Nothing -> internDeferredFunction binder deferred

projectDeferredRhs :: Id -> DeferredFunction -> CgStgRhs -> P HeapRhs
projectDeferredRhs binder deferred
    (StgRhsClosure _ _ ReEntrant parameters _ resultType) = withScope $ do
  result <- resultContractFor binder (length parameters) resultType
  actual <- signatureFor parameters result
  requireDeferredSignature binder deferred (Just actual)
  parameterIds <- mapM bindValue parameters
  deferredFunctionRhs deferred parameterIds
projectDeferredRhs binder deferred _ = do
  requireDeferredSignature binder deferred Nothing
  failShape "unreachable deferred function signature check"

internDeferredFunction :: Id -> DeferredFunction -> P ValueId
internDeferredFunction binder deferred = do
  let symbol = idSymbol "value" binder
  existing <- gets (Map.lookup symbol . implicitValues)
  case existing of
    Just identity -> pure identity
    Nothing -> do
      (actual, _) <- importedEntry binder
      requireDeferredSignature binder deferred actual
      identity <- freshValue
      parameters <- mapM (const freshValue)
        (signatureArguments (deferredSignature deferred))
      rhs <- deferredFunctionRhs deferred parameters
      modify' (\current -> current
        { implicitValues = Map.insert symbol identity (implicitValues current)
        , implicitTops = TopBinding symbol (HeapBinding identity rhs) : implicitTops current
        })
      pure identity

requireDeferredSignature
  :: Id -> DeferredFunction -> Maybe Signature -> P ()
requireDeferredSignature binder deferred actual =
  unless (actual == Just (deferredSignature deferred))
    (lift (Left (DeferredFunctionSignatureMismatch (idSymbol "value" binder)
      (deferredSignature deferred) actual)))

deferredFunctionRhs :: DeferredFunction -> [ValueId] -> P HeapRhs
deferredFunctionRhs deferred parameters = do
  let signatureValue = deferredSignature deferred
      arguments = zipWith deferredArgument
        (signatureArguments signatureValue) parameters
  signature <- internSignature signatureValue
  operation <- internSyntheticOperation
    (Schema.CapabilityIdentity (deferredCapability deferred)) signature
  pure (Function signature parameters [] (Operation operation arguments))

deferredArgument :: RuntimeRep -> ValueId -> Atom
deferredArgument VoidRep _ = Void
deferredArgument _ identity = Ref (Local identity)

-- | Synthesized functions preserve bare references and partial application;
-- failure occurs only upon saturation, through an ordinary operation body.
internWiredInError :: Id -> Schema.WiredInErrorKind -> P ValueId
internWiredInError binder kind = do
  let symbol = idSymbol "value" binder
  existing <- gets (Map.lookup symbol . implicitValues)
  case existing of
    Just identity -> pure identity
    Nothing -> do
      identity <- freshValue
      parameters <- if kind == Schema.WiredAbsentSumField then pure []
        else pure <$> freshValue
      signature <- internSignature (Signature
        (map (const AddressRep) parameters) NoSuccess)
      operation <- internSyntheticOperation (Schema.WiredInErrorIdentity kind) signature
      let rhs = Function signature parameters []
            (Operation operation (map (Ref . Local) parameters))
      modify' (\current -> current
        { implicitValues = Map.insert symbol identity (implicitValues current)
        , implicitTops = TopBinding symbol (HeapBinding identity rhs) : implicitTops current
        })
      pure identity

-- | A genuinely nullary data-con worker denotes an evaluated object, not an
-- executable import. Requiring no representation arguments also excludes
-- workers whose logical Void arguments still require application.
nullaryWorkerConstructor :: Id -> Maybe DataCon
nullaryWorkerConstructor binder = do
  con <- isDataConWorkId_maybe binder
  if dataConRepArity con == 0 && null (dataConRepArgTys con)
      && not (isUnboxedTupleDataCon con)
    then Just con
    else Nothing

-- | Materialize one ordinary constructor top per authoritative worker identity.
-- These field-free objects precede source tops and need no body recovery.
internNullaryWorker :: Id -> DataCon -> P ValueId
internNullaryWorker binder con = do
  let symbol = idSymbol "value" binder
  existing <- gets (Map.lookup symbol . implicitValues)
  case existing of
    Just identity -> pure identity
    Nothing -> do
      collision <- gets (Map.member symbol . topValues)
      if collision
        then failIdentity ("constructor worker collides with prepared top: " <> symbolText symbol)
        else pure ()
      constructor <- internConstructor con
      identity <- freshValue
      modify' (\current -> current
        { implicitValues = Map.insert symbol identity (implicitValues current)
        , implicitTops = TopBinding symbol (HeapBinding identity (Constructor constructor []))
            : implicitTops current
        })
      pure identity

bindingBinders :: CgStgBinding -> [Id]
bindingBinders (StgNonRec binder _) = [binder]
bindingBinders (StgRec pairs) = map fst pairs

allocateTopValue :: Id -> P ValueId
allocateTopValue binder = do
  symbol <- topIdentity binder
  known <- gets topValues
  case Map.lookup symbol known of
    Just _ -> failIdentity ("duplicate top-level value: " <> symbolText symbol)
    Nothing -> do
      identity <- freshValue
      modify' (\current -> current { topValues = Map.insert symbol identity (topValues current) })
      pure identity

requireTopValue :: Id -> P ValueId
requireTopValue binder = do
  symbol <- topIdentity binder
  gets (Map.lookup symbol . topValues) >>= maybe
    (failIdentity ("missing top-level value allocation: " <> symbolText symbol)) pure

topIdentity :: Id -> P SymbolIdentity
topIdentity binder = gets (\st -> lookupVarEnv (topSymbols st) binder) >>= maybe
  (pure (idSymbol (topIdentityNamespace binder) binder)) pure

-- GHC uniques can be reused by binders in disjoint RHS scopes. Each lexical
-- binder gets a fresh wire ID, while the VarEnv tracks only the current scope.
bindValue :: Id -> P ValueId
bindValue binder = do
  identity <- freshValue
  modify' (\current -> current { values = extendVarEnv (values current) binder identity })
  pure identity

freshValue :: P ValueId
freshValue = do
  identity <- ValueId <$> gets nextValue
  modify' (\current -> current { nextValue = nextValue current + 1 })
  pure identity

bindJoin :: Id -> CgStgRhs -> P JoinId
bindJoin binder rhs = do
  parameterReps <- joinArgumentReps binder rhs
  identity <- freshJoin
  modify' (\current -> current { joins = extendVarEnv (joins current) binder (identity, parameterReps) })
  pure identity

joinArgumentReps :: Id -> CgStgRhs -> P [RuntimeRep]
joinArgumentReps _ (StgRhsClosure _ _ JumpedTo parameters _ _) =
  concat <$> mapM (argumentRepsForType . varType) parameters
joinArgumentReps binder _ = failShape
  ("let-no-escape binding lacks JumpedTo form: " <> symbolText (idSymbol "join" binder))

freshJoin :: P JoinId
freshJoin = do
  identity <- JoinId <$> gets nextJoin
  modify' (\current -> current { nextJoin = nextJoin current + 1 })
  pure identity

withScope :: P a -> P a
withScope action = do
  savedValues <- gets values
  savedJoins <- gets joins
  savedEntryArities <- gets entryArities
  result <- action
  modify' (\current -> current
    { values = savedValues, joins = savedJoins
    , entryArities = savedEntryArities })
  pure result

internGlobal :: Id -> P GlobalId
internGlobal binder = do
  names <- gets topSymbols
  offGroup <- gets externalizedTops
  let mapped = lookupVarEnv names binder
      groupImport = maybe False (`Set.member` offGroup) mapped
  if groupImport then internExternalGlobal binder
  else if not (isExternalName (varName binder)) then
    lift (Left (UnboundPreparedInternal
      (Text.pack (occNameString (nameOccName (varName binder))))))
  else case nameModule_maybe (varName binder) of
      Nothing -> lift (Left (InvalidPreparedIdentity
        ("global has no defining module: "
          <> Text.pack (occNameString (nameOccName (varName binder))))))
      Just module_ -> do
        let symbol = idSymbol "value" binder
        generations <- gets retainedGenerations
        -- A retained-generation symbol is an executable import even when its
        -- defining module is compiled alongside this one as a home module:
        -- the retained check comes first, and is never inferred from module
        -- membership.
        if Map.member symbol generations
          then internExternalGlobal binder
          else do
            homes <- gets homeModules
            if homeKey module_ `Set.member` homes
              then lift (Left (MissingPreparedTop symbol))
              else internExternalGlobal binder
  where
    homeKey module_ =
      (Text.pack (unitString (moduleUnit module_)),
       Text.pack (moduleNameString (moduleName module_)))
    internExternalGlobal externalBinder = do
      known <- gets globals
      case lookupVarEnv known externalBinder of
        Just identity -> pure identity
        Nothing -> do
          reps <- case (typePrimRep_maybe (varType externalBinder), importedIdLFInfo externalBinder) of
            (Nothing, LFThunk{}) -> do
              bottomingThunk <- deadEndApplicationSaturated externalBinder []
              if bottomingThunk
                then pure [LiftedRefRep]
                else repsForType (varType externalBinder)
            _ -> repsForType (varType externalBinder)
          rep <- case reps of
            [] -> pure VoidRep
            [single] -> pure single
            _ -> failRepresentation "global value has more than one representation component"
          (entry, evaluated) <- importedEntry externalBinder
          signature <- traverse internSignature entry
          next <- gets nextGlobal
          generations <- gets retainedGenerations
          names <- gets topSymbols
          purpose <- gets projectionPurpose
          let identity = GlobalId next
              symbol = fromMaybe (idSymbol "value" externalBinder)
                (lookupVarEnv names externalBinder)
              retainedGeneration = case purpose of
                OriginalHomeProduct isHome
                  | Just owner <- nameModule_maybe (varName externalBinder)
                  , not (isHome owner) -> Nothing
                _ -> Map.lookup symbol generations
              declaration = GlobalDecl symbol rep signature evaluated
                retainedGeneration
          modify' (\current -> current
            { globals = extendVarEnv (globals current) externalBinder identity
            , nextGlobal = next + 1
            , globalDecls = globalDecls current |> declaration })
          pure identity

internSignature :: Signature -> P SignatureId
internSignature signature = do
  let key = (signatureArguments signature, signatureResults signature)
  known <- gets signatureIndex
  case Map.lookup key known of
    Just identity -> pure identity
    Nothing -> do
      next <- gets nextSignature
      let identity = SignatureId next
      modify' (\current -> current
        { nextSignature = next + 1
        , signatures = signatures current |> signature
        , signatureIndex = Map.insert key identity (signatureIndex current) })
      pure identity

constructorDeclaration :: DataCon -> P ConstructorDecl
constructorDeclaration con = do
  reps <- concat <$> mapM (repsForType . scaledThing) (dataConRepArgTys con)
  -- GHC expands strictness along with representation arguments: a strict
  -- unboxed tuple does not make its lifted components strict. Resolve all
  -- representations first, before calling the fixed-representation helper.
  let marks = map isMarkedStrict (dataConRuntimeRepStrictness con)
  if length marks /= length reps
    then failRepresentation "constructor runtime strictness/representation arity mismatch"
    else pure ()
  let fieldStrictness = zipWith (\strict rep -> strict || isUnboxed rep) marks reps
  resultReps <- repsForType (dataConOrigResTy con)
  resultRep <- case resultReps of
    [rep@LiftedRefRep] -> pure rep
    [rep@UnliftedRefRep] -> pure rep
    _ -> failRepresentation "heap constructor lacks a managed result representation"
  layout <- layoutFor reps
  tag <- checkedWord32 "constructor tag" (dataConTag con)
  familySize <- checkedWord32 "constructor family size" (GHC.tyConFamilySize (dataConTyCon con))
  pure (ConstructorDecl
        (nameSymbol "constructor" (dataConName con))
        (nameSymbol "type" (GHC.tyConName (dataConTyCon con)))
        resultRep reps fieldStrictness layout tag familySize
        (varId (dataConWorkId con)))
 where
  scaledThing (Scaled _ ty) = ty
  isUnboxed LiftedRefRep = False
  isUnboxed UnliftedRefRep = False
  isUnboxed _ = True

validateConstructorEvidence :: [DataCon] -> P ()
validateConstructorEvidence = mapM_ validateOne
 where
  validateOne constructor = do
    attempted <- tryRepresentation (internConstructor constructor)
    case attempted of
      Right _ -> pure ()
      Left (InvalidPreparedLayout _) -> pure ()
      Left (InvalidPreparedRepresentation _) -> pure ()
      Left failure -> lift (Left failure)

internConstructor :: DataCon -> P ConstructorId
internConstructor con = do
  -- Validate each incoming GHC declaration before nominal reuse: equal names
  -- and uniques do not prove equal physical declarations or layout authority.
  declaration <- constructorDeclaration con
  known <- gets constructorIndex
  let nominal = constructorIdentity declaration
  case Map.lookup nominal known of
    Nothing -> do
      next <- gets nextConstructor
      let identity = ConstructorId next
      modify' (\current -> current
        { nextConstructor = next + 1
        , constructors = constructors current |> (con, identity)
        , constructorDecls = constructorDecls current |> declaration
        , constructorIndex = Map.insert nominal (identity, declaration) (constructorIndex current) })
      pure identity
    Just (identity, existing)
      | existing == declaration -> pure identity
      | otherwise -> failConstructorConflict existing declaration

failConstructorConflict :: ConstructorDecl -> ConstructorDecl -> P a
failConstructorConflict existing incoming =
  lift . Left . InvalidPreparedIdentity $
    "distinct GHC provenance for nominal constructor "
      <> symbolText (constructorIdentity incoming)
      <> " has conflicting physical declarations before publication; existing="
      <> Text.pack (show existing)
      <> ", incoming=" <> Text.pack (show incoming)

checkedWord32 :: Text -> Int -> P Word32
checkedWord32 label value
  | value < 0 = failRepresentation (label <> " is negative")
  | toInteger value > toInteger (maxBound :: Word32) =
      failRepresentation (label <> " exceeds u32")
  | otherwise = pure (fromIntegral value)

internOperation :: StgOp -> SignatureId -> P OperationId
internOperation op signature = do
  operationSignature <- signatureForId signature
  pinnedTextUnit <- gets textUnit
  operationIdentity <- case op of
    StgPrimOp GetCurrentCCSOp
      | operationSignature == Signature [LiftedRefRep, VoidRep] (Returns [AddressRep]) ->
          pure (Schema.CapabilityIdentity "ghc:getCurrentCCS")
    StgPrimOp primop -> pure (Schema.PrimOpIdentity
      (Text.pack (occNameString (primOpOcc primop))))
    -- ghc-internal's rounding helper is an external C implementation, not an
    -- interface body. Preserve its exact target and convention; native
    -- admission independently checks the same signature before lowering it.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label _ _) Foreign.CCallConv _)) _
      | unpackFS label == "rintDouble"
      , operationSignature == Signature [FloatRep 64] (Returns [FloatRep 64])
          || operationSignature == Signature [FloatRep 64, VoidRep]
              (Returns [FloatRep 64]) ->
          pure (Schema.IntrinsicIdentity "rintDouble" Schema.CCall)
    -- GHC.CString's c_strlen is a ghc-prim foreign import with no Haskell body.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlayRisky)) _
      | unpackFS label == "strlen"
      , unitString unit == "ghc-prim"
      , operationSignature == Signature [AddressRep, VoidRep] (Returns [IntRep 64]) ->
          pure (Schema.IntrinsicIdentity "strlen" Schema.CCall)
    -- ghc-bignum keeps these final integer-to-Double conversions in C. Their
    -- ABI is fixed by the pinned GHC, and native lowering delegates to the
    -- existing tidepool-bignum implementation rather than loading package code.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlayRisky)) _
      | unitString unit == "ghc-bignum"
      , Just arguments <- lookup (unpackFS label)
          [ ("__int_encodeDouble", [IntRep 64, IntRep 64, VoidRep])
          , ("__word_encodeDouble", [WordRep 64, IntRep 64, VoidRep])
          ]
      , operationSignature == Signature arguments (Returns [FloatRep 64]) ->
          pure (Schema.IntrinsicIdentity (Text.pack (unpackFS label)) Schema.CCall)
    -- ghc-internal's Float/Double classifiers are exact C leaves. Keep the
    -- package and ABI evidence in the catalog so same-named user calls cannot
    -- acquire native lowering by occurrence-name coincidence.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlayRisky)) _
      | unitString unit == "ghc-internal"
      , Just arguments <- lookup (unpackFS label)
          [ ("isFloatNaN", [FloatRep 32, VoidRep])
          , ("isFloatInfinite", [FloatRep 32, VoidRep])
          , ("isFloatNegativeZero", [FloatRep 32, VoidRep])
          , ("isDoubleNaN", [FloatRep 64, VoidRep])
          , ("isDoubleInfinite", [FloatRep 64, VoidRep])
          , ("isDoubleNegativeZero", [FloatRep 64, VoidRep])
          , ("isFloatDenormalized", [FloatRep 32, VoidRep])
          , ("isFloatFinite", [FloatRep 32, VoidRep])
          , ("isDoubleDenormalized", [FloatRep 64, VoidRep])
          , ("isDoubleFinite", [FloatRep 64, VoidRep])
          ]
      , operationSignature == Signature arguments (Returns [IntRep 64]) ->
          pure (Schema.IntrinsicIdentity (Text.pack (unpackFS label)) Schema.CCall)
    -- text's byte kernels are C implementations with no Haskell body. The
    -- compiler-resolved provider unit and each kernel's exact ABI jointly
    -- authorize it; the table is the whole admitted set.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlayRisky)) _
      | pinnedTextUnit == Just (TextUnitAuthority unit)
      , Just expected <- lookup (unpackFS label) textKernels
      , operationSignature == expected ->
          pure (Schema.IntrinsicIdentity (Text.pack (unpackFS label)) Schema.CCall)
    -- Fingerprinting is on the ordinary exception/Typeable path. These C
    -- leaves retain their pinned ABI and execute against authenticated byte
    -- storage; they are not deferred stack capabilities.
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlayRisky)) _
      | unitString unit == "ghc-internal"
      , Just arguments <- lookup (unpackFS label)
          [ ("__hsbase_MD5Init", [AddressRep, VoidRep])
          , ("__hsbase_MD5Update", [AddressRep, AddressRep, IntRep 32, VoidRep])
          , ("__hsbase_MD5Final", [AddressRep, AddressRep, VoidRep])
          ]
      , operationSignature == Signature arguments (Returns []) ->
          pure (Schema.IntrinsicIdentity (Text.pack (unpackFS label)) Schema.CCall)
    StgPrimCallOp (PrimCall label unit)
      | unitString unit == "ghc-internal"
      , unpackFS label == "stg_cloneMyStackzh"
      , operationSignature == Signature [VoidRep] (Returns [UnliftedRefRep]) ->
          pure (Schema.CapabilityIdentity "ghc:cloneMyStack")
      | unitString unit == "ghc-internal"
      , unpackFS label == "stg_decodeStackzh"
      , operationSignature == Signature [UnliftedRefRep, VoidRep] (Returns [UnliftedRefRep]) ->
          pure (Schema.CapabilityIdentity "ghc:decodeStack")
    StgFCallOp (Foreign.CCall (Foreign.CCallSpec
      (Foreign.StaticTarget _ label (Just unit) _) Foreign.CCallConv Foreign.PlaySafe)) _
      | unitString unit == "ghc-internal"
      , unpackFS label == "lookupIPE"
      , operationSignature == Signature [AddressRep, AddressRep, VoidRep] (Returns [WordRep 8]) ->
          pure (Schema.CapabilityIdentity "ghc:lookupIPE")
    StgPrimCallOp call -> lift . Left $
      UnsupportedPrimitiveCall (Text.pack (showSDocUnsafe (ppr call))) operationSignature
    StgFCallOp call _ -> lift . Left $
      UnsupportedForeignCall (Text.pack (showSDocUnsafe (ppr call))) operationSignature
  internSyntheticOperation operationIdentity signature

internSyntheticOperation :: Schema.OperationIdentity -> SignatureId -> P OperationId
internSyntheticOperation operationIdentity signature = do
  -- Interned signature IDs are canonical; validate the reference before reuse.
  _ <- signatureForId signature
  known <- gets operations
  let key = (operationIdentity, signature)
  case Map.lookup key known of
    Just identity -> pure identity
    Nothing -> do
      next <- gets nextOperation
      let identity = OperationId next
          declaration = OperationDecl operationIdentity signature
      modify' (\current -> current
        { nextOperation = next + 1
        , operations = Map.insert key identity (operations current)
        , operationDecls = operationDecls current |> declaration })
      pure identity

signatureForId :: SignatureId -> P Signature
signatureForId (SignatureId index) = do
  known <- gets signatures
  case Seq.lookup (fromIntegral index) known of
    Just signature -> pure signature
    Nothing -> failIdentity "operation refers to an unknown signature"

signatureFor :: [Id] -> ResultContract -> P Signature
signatureFor args result = Signature <$> (concat <$> mapM (argumentRepsForType . varType) args) <*> pure result

-- Imported LF information is authoritative. In its absence GHC uses positive
-- representation arity as function evidence, but never guesses a thunk from
-- zero arity. A CAF returning a function has a zero-argument entry, not all the
-- arrows in the returned function's type.
--
-- `importedIdLFInfo` is partial for GHC's wired-in unused-argument descriptor.
-- Such a zero-width argument is projected directly as Void and never reaches
-- internGlobal, so this query remains restricted to genuine imported entries.
importedEntry :: Id -> P (Maybe Signature, Bool)
importedEntry binder = case importedIdLFInfo binder of
  LFReEntrant _ arity _ _ -> do
    (arguments, result) <- splitRepArguments arity (varType binder)
    signature <- Signature arguments <$> resultContractFor binder arity result
    pure (Just signature, True)
  LFThunk{} -> do
    signature <- Signature [] <$> resultContractFor binder 0 (varType binder)
    pure (Just signature, False)
  LFCon{} -> pure (Nothing, True)
  LFUnlifted -> pure (Nothing, True)
  LFUnknown{} -> pure (Nothing, False)
  LFLetNoEscape -> failShape "imported join has no heap/global entry"

signatureForArgs :: [StgArg] -> Type -> P Signature
signatureForArgs args result = Signature <$> (concat <$> mapM argReps args) <*> (Returns <$> repsForType result)
  where
    argReps (StgVarArg binder) = argumentRepsForType (varType binder)
    argReps (StgLitArg literal) = argumentRepsForType (literalType literal)

-- The STG context, not the callee's source type, says what this application
-- must produce. Arguments retain their actual unarised representations while
-- the enclosing RHS, join, or case supplies the demanded result group.
signatureForApplication :: [StgArg] -> ResultContract -> P Signature
signatureForApplication args demandedResult = Signature
  <$> (concat <$> mapM argReps args) <*> demandedResultForApplication
  where
    demandedResultForApplication = case demandedResult of
      Returns reps -> pure (Returns reps)
      CallerResult -> pure CallerResult
      NoSuccess -> failShape "demanded NoSuccess lacks callee evidence"
    argReps (StgVarArg binder) = argumentRepsForType (varType binder)
    argReps (StgLitArg literal) = argumentRepsForType (literalType literal)

-- Imported LF arity is expressed in GHC's callable representation view. Only
-- imported entries need this source-type traversal; STG application sites use
-- their actual arguments plus an already-threaded demanded result instead.
splitRepArguments :: Int -> Type -> P ([RuntimeRep], Type)
splitRepArguments 0 ty = pure ([], ty)
splitRepArguments supplied ty = case unwrapType ty of
  FunTy _ _ argument result -> do
    reps <- argumentRepsForType argument
    if supplied < length reps
      then failRepresentation "application splits an unarised source argument"
      else do
        (remaining, finalResult) <- splitRepArguments (supplied - length reps) result
        pure (reps <> remaining, finalResult)
  _ -> failRepresentation "application exceeds its GHC function type"

-- Void positions count toward semantic saturation even though they have no
-- register or payload component after unarisation.
argumentRepsForType :: Type -> P [RuntimeRep]
argumentRepsForType ty = do
  reps <- repsForType ty
  pure (if null reps then [VoidRep] else reps)

repsForType :: Type -> P [RuntimeRep]
repsForType ty = maybe (failRepresentation "runtime-polymorphic representation")
  (mapM projectRep) (typePrimRep_maybe ty)

resultContractFor :: Id -> Int -> Type -> P ResultContract
resultContractFor binder actual ty
  | isDeadEndId binder = do
      threshold <- demandRepThreshold binder
      if actual >= threshold then pure NoSuccess else returning
  | otherwise = returning
  where
    -- A callable body can inherit the caller's concrete result convention.
    -- Zero-arity closures have no call boundary at which to instantiate it.
    returning
      | actual > 0, Nothing <- typePrimRep_maybe ty = pure CallerResult
      | otherwise = Returns <$> repsForType ty

signatureForArgsNoSuccess :: [StgArg] -> P Signature
signatureForArgsNoSuccess args = Signature <$> (concat <$> mapM argReps args) <*> pure NoSuccess
  where
    argReps (StgVarArg binder) = argumentRepsForType (varType binder)
    argReps (StgLitArg literal) = argumentRepsForType (literalType literal)

-- Demand signatures count source arguments. Convert those arguments through
-- the binder type so an unboxed tuple contributes all payload reps and a
-- zero-width argument contributes one Void slot.
demandRepThreshold :: Id -> P Int
demandRepThreshold binder = sourceArgumentRepSlots sourceArity (varType binder)
  where
    sourceArity = length (fst (splitDmdSig (idDmdSig binder)))

sourceArgumentRepSlots :: Int -> Type -> P Int
sourceArgumentRepSlots 0 _ = pure 0
sourceArgumentRepSlots remaining ty = case unwrapType ty of
  FunTy _ _ argument result -> do
    reps <- argumentRepsForType argument
    rest <- sourceArgumentRepSlots (remaining - 1) result
    pure (length reps + rest)
  _ -> failRepresentation "demand signature exceeds function type"

-- Bottoming evidence belongs to an entered call, not to a partial application
-- of a bottoming function. Local entries come from the actual prepared-STG
-- closure parameter list; only external entries use their imported LF arity.
deadEndApplicationSaturated :: Id -> [StgArg] -> P Bool
deadEndApplicationSaturated function args
  | not (isDeadEndId function) = pure False
  | otherwise = do
      threshold <- demandRepThreshold function
      actual <- knownEntryArity function
      pure (maybe False (\entry -> entry >= threshold && length args >= entry) actual)

knownEntryArity :: Id -> P (Maybe Int)
knownEntryArity function = do
  local <- gets (\st -> lookupVarEnv (entryArities st) function)
  case local of
    Just arity -> pure (Just arity)
    Nothing
      | isExternalName (varName function) -> pure (importedEntryArity function)
      | otherwise -> pure Nothing

importedEntryArity :: Id -> Maybe Int
importedEntryArity binder = case importedIdLFInfo binder of
  LFReEntrant _ arity _ _ -> Just arity
  LFThunk{} -> Just 0
  LFCon{} -> Nothing
  LFUnlifted -> Nothing
  LFUnknown{} -> Nothing
  LFLetNoEscape -> Nothing

projectRep :: GHC.PrimRep -> P RuntimeRep
projectRep (GHC.BoxedRep (Just GHC.Lifted)) = pure LiftedRefRep
projectRep (GHC.BoxedRep (Just GHC.Unlifted)) = pure UnliftedRefRep
projectRep (GHC.BoxedRep Nothing) = failRepresentation "runtime-polymorphic boxed representation"
projectRep GHC.AddrRep = pure AddressRep
projectRep GHC.IntRep = targetWidth IntRep
projectRep GHC.WordRep = targetWidth WordRep
projectRep GHC.Int8Rep = pure (IntRep 8)
projectRep GHC.Word8Rep = pure (WordRep 8)
projectRep GHC.Int16Rep = pure (IntRep 16)
projectRep GHC.Word16Rep = pure (WordRep 16)
projectRep GHC.Int32Rep = pure (IntRep 32)
projectRep GHC.Word32Rep = pure (WordRep 32)
projectRep GHC.Int64Rep = pure (IntRep 64)
projectRep GHC.Word64Rep = pure (WordRep 64)
projectRep GHC.FloatRep = pure (FloatRep 32)
projectRep GHC.DoubleRep = pure (FloatRep 64)
projectRep GHC.VecRep{} = failRepresentation "vector representation"

targetWidth :: (Word8 -> RuntimeRep) -> P RuntimeRep
targetWidth constructor = constructor . targetWordWidth <$> gets target

layoutFor :: [RuntimeRep] -> P CheckedLayout
layoutFor reps = do
  machine <- gets target
  let stored = filter (/= VoidRep) reps
  (fields, end, alignment) <- foldM (place machine) ([], 0, 1) stored
  pure (CheckedLayout fields alignment (alignUp end alignment) (map isRoot stored))
  where
    place machine (fields, cursor, greatest) rep = do
      size <- repBytes machine rep
      let alignment = max 1 size; offset = alignUp cursor alignment
      pure (fields <> [FieldLayout rep offset], offset + size, max greatest alignment)
    isRoot LiftedRefRep = True
    isRoot UnliftedRefRep = True
    isRoot _ = False

repBytes :: TargetDescriptor -> RuntimeRep -> P Word32
repBytes machine rep = width $ case rep of
  VoidRep -> 0
  LiftedRefRep -> targetPointerWidth machine
  UnliftedRefRep -> targetPointerWidth machine
  AddressRep -> targetPointerWidth machine
  IntRep bits -> bits
  WordRep bits -> bits
  FloatRep bits -> bits
  where
    width 0 = pure 0
    width bits | bits `mod` 8 == 0 = pure (fromIntegral bits `div` 8)
    width _ = failLayout "non-byte runtime width"

alignUp :: Word32 -> Word32 -> Word32
alignUp value alignment = ((value + alignment - 1) `div` alignment) * alignment

-- | Unarise splits multi-representation rubbish and removes zero-width rubbish.
-- Both TYPE and CONSTRAINT use the same resolved physical representation; no
-- GHC kind/type needs to cross the execution boundary.
projectLiteralAtom :: Literal -> P Atom
projectLiteralAtom (LitRubbish _ runtimeRep) =
  case runtimeRepPrimRep_maybe runtimeRep of
    Just [rep] -> Rubbish <$> projectRep rep
    Just _ -> failRepresentation "rubbish literal was not unarised to one component"
    Nothing -> failRepresentation "runtime-polymorphic rubbish literal"
projectLiteralAtom literal = Scalar <$> projectLiteral literal

projectLiteral :: Literal -> P ScalarLiteral
projectLiteral literal = case literal of
  LitChar character -> do
    machine <- gets target
    let bits = targetWordWidth machine
    pure (WordLiteral bits (integerBytes bits (fromIntegral (fromEnum character))))
  LitString bytes -> pure (BytesLiteral bytes)
  LitNumber kind value -> numeric kind value
  LitFloat value -> pure (FloatLiteral 32 (wordBytes 4 (fromIntegral (castFloatToWord32 (fromRational value)))))
  LitDouble value -> pure (FloatLiteral 64 (wordBytes 8 (castDoubleToWord64 (fromRational value))))
  LitNullAddr -> pure NullAddressLiteral
  LitRubbish{} -> failShape "rubbish literal cannot be an alternative pattern"
  LitLabel{} -> failShape "relocatable label literal"
  where
    numeric LitNumBigNat _ = failShape "BigNat literal"
    numeric kind value = do
      machine <- gets target
      let (signed, bits) = case kind of
            LitNumInt -> (True, targetWordWidth machine)
            LitNumInt8 -> (True, 8); LitNumInt16 -> (True, 16)
            LitNumInt32 -> (True, 32); LitNumInt64 -> (True, 64)
            LitNumWord -> (False, targetWordWidth machine)
            LitNumWord8 -> (False, 8); LitNumWord16 -> (False, 16)
            LitNumWord32 -> (False, 32); LitNumWord64 -> (False, 64)
          bytes = integerBytes bits value
      pure (if signed then IntLiteral bits bytes else WordLiteral bits bytes)

integerBytes :: Word8 -> Integer -> BS.ByteString
integerBytes bits value = BS.pack
  [ fromIntegral (normalized `shiftR` (byte * 8))
  | byte <- reverse [0 .. fromIntegral bits `div` 8 - 1] ]
  where normalized = value `mod` (2 ^ bits)

wordBytes :: Int -> Word64 -> BS.ByteString
wordBytes count value = BS.pack
  [ fromIntegral (value `shiftR` (byte * 8)) | byte <- reverse [0 .. count - 1] ]

preparedRootIdentity :: Id -> SymbolIdentity
preparedRootIdentity = idSymbol "value"

idSymbol :: Text -> Id -> SymbolIdentity
idSymbol namespace = nameSymbol namespace . varName

idSymbolFor :: Module -> Text -> Id -> SymbolIdentity
idSymbolFor fallback namespace binder = nameSymbolFor fallback namespace (varName binder)

topIdentityNamespace :: Id -> Text
topIdentityNamespace binder
  | isExternalName (varName binder) = "value"
  | otherwise = "local"

nameSymbol :: Text -> Name -> SymbolIdentity
nameSymbol namespace = nameSymbolWithFallback Nothing namespace

nameSymbolFor :: Module -> Text -> Name -> SymbolIdentity
nameSymbolFor fallback namespace = nameSymbolWithFallback (Just fallback) namespace

nameSymbolWithFallback :: Maybe Module -> Text -> Name -> SymbolIdentity
nameSymbolWithFallback fallback namespace name = case nameSymbolIdentity namespace name of
  Just identity -> identity
  Nothing -> case fallback of
    Just modul -> SymbolIdentity (Text.pack (unitString (moduleUnit modul)))
      (Text.pack (moduleNameString (moduleName modul))) namespace
      (Text.pack (occNameString (nameOccName name))) Nothing
    Nothing -> SymbolIdentity "<interactive>" "<local>" namespace
      (Text.pack (occNameString (nameOccName name))) Nothing

symbolText :: SymbolIdentity -> Text
symbolText symbol = case symbolRecordParent symbol of
  Nothing -> symbolUnit symbol <> ":" <> symbolModule symbol <> ":"
    <> symbolNamespace symbol <> ":" <> symbolOccurrence symbol
  Just parent -> symbolUnit symbol <> ":" <> symbolModule symbol <> ":"
    <> symbolNamespace symbol <> ":" <> parent <> ":" <> symbolOccurrence symbol

failShape :: Text -> P a
failShape = lift . Left . UnsupportedPreparedShape
failIdentity :: Text -> P a
failIdentity = lift . Left . InvalidPreparedIdentity
failRepresentation :: Text -> P a
failRepresentation = lift . Left . InvalidPreparedRepresentation
failLayout :: Text -> P a
failLayout = lift . Left . InvalidPreparedLayout
