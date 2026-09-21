module Tidepool.ExecutionProjection
  ( ProjectionContext(..)
  , ProjectionError(..)
  , projectPrepared
  , projectPreparedTarget
  , projectPreparedTargetWithConstructors
  , PreparedProjection
  , prepareProjection
  , projectSelected
  , preparedTopIdentities
  , preparedTargetReferences
  , ReferenceFact(..)
  , preparedModuleReferenceFacts
  , combinePreparedTargetReferences
  , preparedModuleReachFacts
  , preparedSeedUniques
  , PreparedReachability(..)
  , emptyPreparedReachability
  , admitReachFacts
  , topBinders
  , projectLiteralAtomForTest
  , assignTopIdentitySpellings
  , resolveTextPackageUnit
  , TextUnitAuthority(..)
  ) where

import Control.Monad (foldM, forM, forM_, unless)
import Control.Monad.State.Strict
import Data.Bits (shiftR)
import Data.ByteString qualified as BS
import Data.IntMap.Strict qualified as IntMap
import Data.List (find)
import Data.Maybe (fromMaybe, isJust, isNothing, listToMaybe)
import Tidepool.PreparedBuiltins
  ( DeferredFunction(..), deferredFunction, wiredInErrorKind )
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text (Text)
import Data.Text qualified as Text
import Data.Word (Word32, Word64, Word8)
import GHC.Builtin.PrimOps (PrimOp(..), PrimCall(..), primOpOcc)
import GHC.Builtin.Types (doubleDataCon, intDataCon)
import GHC.Core (AltCon(..))
import GHC.Core.DataCon
  ( DataCon, dataConName, dataConInstOrigArgTys, dataConRepArgTys, dataConRepArity, dataConWorkId
  , dataConTag, dataConTyCon, dataConOrigResTy, dataConImplBangs, HsImplBang(..)
  , isMarkedStrict, isUnboxedTupleDataCon )
import GHC.Core.TyCo.Rep (Scaled(..), Type(..))
import GHC.Core.TyCo.FVs (tyCoVarsOfType)
import GHC.Core.Type (splitFunTys, splitTyConApp_maybe)
import GHC.Core.TyCon qualified as GHC
import GHC.Data.FastString (fsLit, unpackFS)
import GHC.Driver.Env.Types (HscEnv, hsc_unit_env)
import GHC.Float (castDoubleToWord64, castFloatToWord32)
import GHC.Stg.Syntax
import GHC.Stg.Syntax qualified as Stg
import GHC.StgToCmm.Closure (importedIdLFInfo)
import GHC.StgToCmm.Types (LambdaFormInfo(..))
import GHC.Types.Demand (splitDmdSig)
import GHC.Types.Literal (LitNumType(..), Literal(..), literalType)
import GHC.Types.Id (idDmdSig, isDeadEndId, isDataConWorkId_maybe)
import GHC.Types.ForeignCall qualified as Foreign
import GHC.Types.Name (Name, isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (fieldOcc_maybe, occNameString)
import GHC.Types.RepType
  (typePrimRep_maybe, runtimeRepPrimRep_maybe, dataConRuntimeRepStrictness, unwrapType)
import GHC.Types.Unique.Set (UniqSet, addListToUniqSet, addOneToUniqSet, elementOfUniqSet, emptyUniqSet, mkUniqSet, nonDetEltsUniqSet)
import GHC.Types.Unique (Unique, getKey)
import GHC.Types.Unique.FM (UniqFM, addToUFM, emptyUFM, listToUFM, lookupUFM)
import GHC.Types.Var (Id, varName, varType, varUnique)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Types.Var.Set (dVarSetElems, isEmptyVarSet)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.Module (mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Types.PkgQual (PkgQual(OtherPkg))
import GHC.Unit.State (lookupPackageName)
import GHC.Unit.Types (Module, Unit, unitString)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import Tidepool.ExecutionIR (topBindingReferenceUniques, topBindingReferences)
import Tidepool.ExecutionSchema
import Tidepool.ExecutionSchema qualified as Schema
import Tidepool.PreparedFacts (PreparedFacts(..), extractPreparedFacts)
import Tidepool.Identity (varId)
import Tidepool.PreparedStg (PreparedModule(..), PreparedCoverage(..))
import Tidepool.PreparedSites (SiteRejection(..))
import Tidepool.PreparedSites (PreparedSite(..), requestReplyIndex, syntheticSiteId)
import Tidepool.EffectSchema qualified as Effect
import Tidepool.TypePolicy qualified as TypePolicy
import Tidepool.PreparedFormatting
  (FormattingAuthority, FormattingSpec(..), FormattingIntrinsic(..), classifyFormatting)
import Tidepool.PreparedTime (TimeAuthority, TimeSpec(..), classifyTime)

data ProjectionContext = ProjectionContext
  { projectionProfile :: Text
  , projectionToolchain :: Text
  , projectionTarget :: TargetDescriptor
  , projectionRetainedGenerations :: Map SymbolIdentity Word64
  , projectionEntry :: SymbolIdentity
  -- | Additional tops seeded into reachability beside 'projectionEntry'
  -- (a turn's resume entry). Optional: an absent root is not an error;
  -- the consumer checks the artifact for the entries it needs.
  , projectionAuxiliaryRoots :: [SymbolIdentity]
  , projectionFormattingAuthority :: Maybe FormattingAuthority
  , projectionTimeAuthority :: Maybe TimeAuthority
  -- | Missing authority rejects text's kernel, not unrelated projection.
  , projectionTextUnit :: Maybe TextUnitAuthority
  } deriving stock (Eq, Show)

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
  -- | A typed site in a reachable top cannot carry concrete evidence. This
  -- is a source error, reported with the compiler's own guidance.
  | RejectedTypedSite Text
  deriving stock (Eq, Show)

data PState = PState
  { nextValue :: Word32, nextJoin :: Word32
  , values :: VarEnv ValueId, joins :: VarEnv JoinId
  , entryArities :: VarEnv Int
  , topSymbols :: VarEnv SymbolIdentity, topValues :: Map SymbolIdentity ValueId
  , implicitTops :: [TopBinding]
  , implicitValues :: Map SymbolIdentity ValueId
  , globals :: VarEnv GlobalId, globalDecls :: [GlobalDecl]
  , constructors :: [(DataCon, ConstructorId)], constructorDecls :: [ConstructorDecl]
  , operations :: [(Schema.OperationIdentity, Signature, OperationId)]
  , operationDecls :: [OperationDecl]
  , signatures :: [(Signature, SignatureId)]
  , target :: TargetDescriptor
  , retainedGenerations :: Map SymbolIdentity Word64
  , homeModules :: Set (Text, Text)
  , formattingAuthority :: Maybe FormattingAuthority
  , timeAuthority :: Maybe TimeAuthority
  , textUnit :: Maybe TextUnitAuthority
  }

type P a = StateT PState (Either ProjectionError) a

-- | text's C kernels admitted as prepared intrinsics, with their exact ABI.
-- The Rust recognizer (`tidepool-codegen/src/prepared_program/text_search.rs`)
-- accepts exactly these signatures.
textKernels :: [(String, Signature)]
textKernels =
  [ ("_hs_text_memchr", Signature
      [UnliftedRefRep, WordRep 64, WordRep 64, WordRep 8, VoidRep] (Returns [IntRep 64]))
  , ("_hs_text_measure_off", Signature
      [UnliftedRefRep, WordRep 64, WordRep 64, WordRep 64, VoidRep] (Returns [IntRep 64]))
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
  (PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv emptyVarEnv Map.empty [] Map.empty emptyVarEnv [] [] [] [] [] [] machine Map.empty Set.empty Nothing Nothing Nothing)

projectPrepared :: ProjectionContext -> [PreparedModule] -> Either ProjectionError WireProgram
projectPrepared _ [] = Left (UnsupportedPreparedShape "execution program has no modules")
projectPrepared context modules =
  fst <$> projectPreparedWithTopSymbols context modules (buildTopIdentityMap modules)

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
projectPreparedWithTopSymbols context modules topIdentityMap = do
  let initial = PState 0 0 emptyVarEnv emptyVarEnv emptyVarEnv topIdentityMap Map.empty [] Map.empty
        emptyVarEnv [] [] [] [] [] [] (projectionTarget context)
        (projectionRetainedGenerations context) (Set.fromList
          [ (Text.pack (unitString (moduleUnit (pmModule prepared))),
             Text.pack (moduleNameString (moduleName (pmModule prepared))))
          | prepared <- modules, pmCoverage prepared == CompleteSourceModule ])
        (projectionFormattingAuthority context) (projectionTimeAuthority context)
        (projectionTextUnit context)
      -- An executable import's own top-level definition is never walked:
      -- 'homeModules'/'topIdentityMap' above still see the real, unfiltered
      -- module set (so a same-name internal identity cannot borrow home-module
      -- standing from the retained one), but nothing here recovers its body.
      projectable = map (dropRetainedTops context) modules
  ((bindingGroups, programTypes, programSites, programVerbSites), final) <- runStateT
    (do preallocate projectable
        groups <- concat <$> mapM projectModule projectable
        (types, sites, verbSites) <- lowerPreparedEvidence context projectable
        pure (groups, types, sites, verbSites)) initial
  entryTop <- maybe (Left (MissingPreparedEntry (projectionEntry context)))
    pure (findTop bindingGroups)
  let entry = topValue entryTop
      TopBinding _ entryBinding = entryTop
  case heapBindingRhs entryBinding of
    Function signature _ _ _
      | lookup signature [(identity, signatureResults contract)
          | (contract, identity) <- signatures final] == Just CallerResult ->
          Left (InvalidPreparedRepresentation "program entry requires a concrete result contract")
    _ -> pure ()
  let program = WireProgram
        { programEnvelope = ProgramEnvelope schemaVersion (projectionProfile context)
            (projectionToolchain context) executionAbiVersion (projectionTarget context)
        , programSignatures = map fst (signatures final)
        , programGlobals = globalDecls final
        , programConstructors = constructorDecls final
        , programOperations = operationDecls final
        , programBindings = map NonRecursive (reverse (implicitTops final)) ++ bindingGroups
        , programEntry = entry
        , programTypes = programTypes
        , programSites = programSites
        , programVerbSites = programVerbSites
        }
  pure (program, map fst (constructors final))
  where
    topValue (TopBinding _ binding) = heapBindingId binding
    findTop = foldr findGroup Nothing
    findGroup group found = case filter
      ((== projectionEntry context) . topSymbol) (groupItems group) of
      top : _ -> Just top
      [] -> found
    groupItems (NonRecursive top) = [top]
    groupItems (Recursive tops) = tops
    topSymbol (TopBinding symbol _) = symbol

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
-- Its binding lists are forced here so timing selection measures the complete
-- reachability walk, rather than charging its thunks to projection later.
data PreparedProjection = PreparedProjection ProjectionContext [PreparedModule] (VarEnv SymbolIdentity)

prepareProjection :: ProjectionContext -> [PreparedModule]
  -> Either ProjectionError PreparedProjection
prepareProjection _ [] = Left (UnsupportedPreparedShape "execution program has no modules")
prepareProjection context modules =
  let (identities, selected) = selectPreparedTarget context modules
      bindingCount = sum [length (pmBindings prepared) | prepared <- selected]
      reachable = mkUniqSet [ varUnique binder | prepared <- selected
        , (binding, _) <- pmBindings prepared, binder <- topBinders binding ]
  in bindingCount `seq` case [ srMessage rejection | prepared <- modules
            , rejection <- pmSiteRejections prepared
            , elementOfUniqSet (varUnique (srBinder rejection)) reachable
            , not (skippedFromRecovery context (srBinder rejection)) ] of
       message : _ -> Left (RejectedTypedSite (Text.pack message))
       [] -> Right (PreparedProjection context selected identities)

projectSelected :: PreparedProjection -> Either ProjectionError (WireProgram, [DataCon])
projectSelected (PreparedProjection context selected identities) =
  projectPreparedWithTopSymbols context selected identities

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
              (pmModule prepared) (pmTagSigs prepared) (recoveryReferences context binding))
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

registeredReplacement :: ProjectionContext -> Id -> Bool
registeredReplacement context binder =
  registeredFormatting context binder || registeredTime context binder
    || isJust (deferredFunction binder)

selectPreparedTarget :: ProjectionContext -> [PreparedModule]
  -> (VarEnv SymbolIdentity, [PreparedModule])
selectPreparedTarget context modules =
  (topIdentityMap, [ prepared { pmBindings = filter isReachable (pmBindings prepared) }
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
dropRetainedTops context prepared = prepared
  { pmBindings = filter keep (pmBindings prepared) }
  where
    keep (binding, _) = not (all (isJust . retainedGenerationOf context) (topBinders binding))

-- | Assign stable identities to internal tops before any target reachability
-- filtering.  Internal names may repeat (and a generated suffix may already
-- be an authored spelling), so reserve every original spelling first and claim
-- either that spelling or the first unused suffix in emission order. The
-- allocation is local to a symbol namespace; external names are retained
-- byte-for-byte while constraining generated suffixes around them.
buildTopIdentityMap :: [PreparedModule] -> VarEnv SymbolIdentity
buildTopIdentityMap modules = foldl insert emptyVarEnv (zip binders assigned)
  where
    binders =
      [ (pmModule prepared, binder)
      | prepared <- modules
      , (binding, _) <- pmBindings prepared
      , binder <- topBinders binding
      ]
    raw (fallback, binder) = idSymbolFor fallback (topIdentityNamespace binder) binder
    symbols = map raw binders
    assigned = assignTopIdentitySpellings
      (zip symbols (map (isExternalName . varName . snd) binders))
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
-- regressions. The Bool marks an externally named top, whose spelling is
-- retained exactly; all original spellings reserve suffixes for internal tops.
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
projectModule = mapM (projectTop . fst) . pmBindings

-- | Lower only evidence owned by the executable tops retained in each module.
-- Graph ids are module-local during elaboration; this pass compacts reachable
-- nodes in module/original order and rebases every edge into one program table.
-- An admitted auxiliary root's own result type follows the module evidence
-- ('lowerAuxiliaryRootEvidence'); synthetic reply sites for the program's
-- request constructors follow that ('lowerVerbEvidence').
lowerPreparedEvidence :: ProjectionContext -> [PreparedModule]
  -> P ([TypeNode], [SiteRow], [(ConstructorId, Word64)])
lowerPreparedEvidence context modules = do
  (moduleNodes, moduleSites) <- foldM lowerOne ([], []) modules
  auxNodes <- lowerAuxiliaryRootEvidence context modules (length moduleNodes)
  (verbNodes, verbRows, verbSites) <-
    lowerVerbEvidence (length moduleNodes + length auxNodes)
  let sites = moduleSites <> verbRows
      duplicates = Map.keys (Map.filter (> (1 :: Int))
        (Map.fromListWith (+) [(siteId site, 1) | site <- sites]))
  case duplicates of
    duplicate : _ -> failShape
      ("duplicate selected prepared site id " <> Text.pack (show duplicate))
    [] -> pure (moduleNodes <> auxNodes <> verbNodes, sites, verbSites)
 where
  lowerOne (priorNodes, priorSites) prepared = do
    let owners = mkUniqSet
          [ varUnique binder
          | (binding, _) <- pmBindings prepared
          , binder <- topBinders binding
          ]
        selected = filter
          (\site -> elementOfUniqSet (varUnique (psOwner site)) owners)
          (pmPreparedSites prepared)
        roots = concat
          [ psWireNode site : psInputNodes site | site <- selected ]
    (lowered, rebase) <- lowerTypeGraph (length priorNodes)
      (TypePolicy.tgNodes (pmTypeGraph prepared)) roots
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
    pure (priorNodes <> lowered, priorSites <> rows)

-- | Force-intern type evidence for every admitted auxiliary root's own
-- answer type, the same way a declared site's answer type is interned
-- ('lowerOne'/'siteWireType' in "Tidepool.PreparedSites"). An auxiliary root
-- (for example the turn's admitted decode entry, 'preparedDecodeTargetName')
-- is not itself a site: nothing about ordinary site traversal reaches its
-- answer type, so a program whose turns never independently construct or
-- observe that type (no 'httpGet', no rendered 'Left'/'Right') would
-- otherwise leave its constructors out of the program's evidence even though
-- the auxiliary root itself needs to read them back.
--
-- The answer type is read off the binder's own (pre-erasure) GHC 'Type' via
-- 'splitFunTys', never off its STG 'StgRhsClosure' result type: an
-- eta-unexpanded auxiliary root (@__decodeValue = Aeson.eitherDecodeValue@,
-- a zero-arity CAF) has an STG result type that is the whole function arrow
-- rather than its codomain, and 'TypePolicy.classifyType' refuses a function
-- type outright.
-- | An auxiliary root's answer type is skipped for evidence interning when
-- it still carries a free type variable after 'splitFunTys' (a genuinely
-- polymorphic root like 'Tidepool.Session.preparedApplyEntryTargetName'/
-- 'Tidepool.Session.preparedApplyValueTargetName', whose settled result is
-- whatever the applied closure returns, not one concrete turn's type).
-- 'TypePolicy.internType'/'classifyType' has no node for an unresolved type
-- variable, so interning one would fail the whole projection rather than
-- leaving the root's own evidence merely absent. Every OTHER auxiliary root
-- ('preparedResumeTargetName', 'preparedDecodeTargetName') is compiled
-- concretely per turn and is unaffected by this filter.
lowerAuxiliaryRootEvidence :: ProjectionContext -> [PreparedModule] -> Int -> P [TypeNode]
lowerAuxiliaryRootEvidence context modules base = do
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
      (graphRoots, builder) = runState
        (traverse TypePolicy.internType answerTypes)
        TypePolicy.emptyTypeGraphBuilder
  (lowered, _rebase) <- lowerTypeGraph base
    (TypePolicy.tgNodes (TypePolicy.finishTypeGraph builder)) graphRoots
  pure lowered

-- | One synthetic 'HostAnswer' row per interned constructor with a closed
-- reply index ('requestReplyIndex'), and the table naming it. Only the index
-- is interned; the row has no inputs, since the host answer is built from the
-- wire type alone. Membership in an effect row is deliberately not tested:
-- an unused row is inert, a missing one would refuse the request.
lowerVerbEvidence :: Int -> P ([TypeNode], [SiteRow], [(ConstructorId, Word64)])
lowerVerbEvidence base = do
  known <- gets constructors
  let candidates =
        [ (identity, qualified, index)
        | (constructor, identity) <- known
        , Just index <- [requestReplyIndex constructor]
        , let symbol = nameSymbol "constructor" (dataConName constructor)
              qualified = symbolModule symbol <> "." <> symbolOccurrence symbol
        ]
      (roots, builder) = runState
        (traverse (\(_, _, index) -> TypePolicy.internType index) candidates)
        TypePolicy.emptyTypeGraphBuilder
  (lowered, rebase) <- lowerTypeGraph base
    (TypePolicy.tgNodes (TypePolicy.finishTypeGraph builder)) roots
  entries <- traverse (\((identity, qualified, _), root) -> do
      wire <- rebase root
      let site = syntheticSiteId qualified
      pure ( SiteRow
               { siteId = site
               , siteOrigin = qualified
               , siteOrdinal = 0
               , siteDelivery = HostAnswer
               , siteWire = wire
               , siteInputs = []
               }
           , (identity, site) ))
    (zip candidates roots)
  pure (lowered, map fst entries, map snd entries)

-- | Lower the nodes of one elaboration-local graph reachable from @roots@,
-- in original order, as program nodes starting at @base@.
lowerTypeGraph :: Int -> [TypePolicy.TypeNodeG] -> [TypePolicy.TypeNodeId]
  -> P ([TypeNode], TypePolicy.TypeNodeId -> P TypeNodeId)
lowerTypeGraph base nodes roots = do
  let graphNodes = IntMap.fromAscList (zip [0 :: Int ..] nodes)
  reachable <- lift (reachableTypeNodes graphNodes roots)
  let ordered = [ TypePolicy.TypeNodeId (fromIntegral index)
                | index <- IntMap.keys graphNodes, Set.member index reachable ]
      mapping = Map.fromList
        [ (old, TypeNodeId (fromIntegral (base + offset)))
        | (offset, old) <- zip [0 :: Int ..] ordered ]
      rebase node = maybe
        (failShape "prepared type graph reachability omitted a referenced node")
        pure (Map.lookup node mapping)
  lowered <- traverse (lowerTypeNode graphNodes rebase) ordered
  pure (lowered, rebase)

reachableTypeNodes :: IntMap.IntMap TypePolicy.TypeNodeG -> [TypePolicy.TypeNodeId]
  -> Either ProjectionError (Set Int)
reachableTypeNodes nodes = go Set.empty
 where
  go visited [] = Right visited
  go visited (TypePolicy.TypeNodeId raw : pending)
    | index `Set.member` visited = go visited pending
    | otherwise = case IntMap.lookup index nodes of
        Just current -> go (Set.insert index visited) (typeNodeEdges current <> pending)
        Nothing -> Left (UnsupportedPreparedShape
          "prepared type graph contains an out-of-range node")
    where index = fromIntegral raw
  typeNodeEdges graphNode = case graphNode of
    TypePolicy.DataG _ _ arguments rows -> arguments <> concatMap snd rows
    _ -> []

lowerTypeNode
  :: IntMap.IntMap TypePolicy.TypeNodeG
  -> (TypePolicy.TypeNodeId -> P TypeNodeId)
  -> TypePolicy.TypeNodeId
  -> P TypeNode
lowerTypeNode nodes rebase (TypePolicy.TypeNodeId raw) = case IntMap.lookup (fromIntegral raw) nodes of
  Nothing -> failShape "prepared type graph contains an out-of-range node"
  Just node -> case node of
    TypePolicy.DataG ty tc arguments rows -> do
      attempted <- tryRepresentation (lowerDataNode ty tc arguments rows)
      case attempted of
        Right lowered -> pure lowered
        Left (InvalidPreparedLayout _) -> pure (refused "layout" ty)
        Left (InvalidPreparedRepresentation _) -> pure (refused "representation" ty)
        Left failure -> lift (Left failure)
    TypePolicy.TextG ty constructors -> lowerLeaf ty TypeText constructors
    TypePolicy.IntegerG ty constructors -> lowerLeaf ty TypeInteger constructors
    TypePolicy.NaturalG ty constructors -> lowerLeaf ty TypeNatural constructors
    TypePolicy.ScalarG _ rep -> TypeScalar <$> projectRep rep
    TypePolicy.UnconstructibleG reason rendered ->
      pure (TypeUnconstructible reason rendered)
    TypePolicy.ProjectionDefectG detail -> failRepresentation detail
 where
  refused reason ty = TypeUnconstructible reason (Text.pack (showSDocUnsafe (ppr ty)))
  lowerLeaf ty leaf constructors = do
    attempted <- tryRepresentation (mapM_ internConstructor constructors)
    case attempted of
      Right () -> pure leaf
      Left (InvalidPreparedRepresentation _) -> pure (refused "representation" ty)
      Left (InvalidPreparedLayout _) -> pure (refused "layout" ty)
      Left failure -> lift (Left failure)
  lowerDataNode ty tc arguments rows = do
    loweredRows <- traverse lowerRow rows
    loweredArguments <- traverse rebase arguments
    pure (TypeData (nameSymbol "type" (GHC.tyConName tc))
      loweredArguments loweredRows)
    where
      lowerRow (constructor, fields) = do
        sourceReps <- verifySourceLayout constructor
        identity@(ConstructorId index) <- internConstructor constructor
        -- The row is checked against the declaration it will name, not only
        -- the type graph's DataCon: the declaration interned first (from the
        -- program's own STG) is authoritative for the runtime layout, and a
        -- type reached through another DataCon object for the same
        -- constructor must not borrow it with a different field shape.
        declared <- gets (fmap constructorFieldReps . listToMaybe
          . drop (fromIntegral index) . constructorDecls)
        unless (declared == Just sourceReps)
          (failLayout "prepared type constructor declaration differs from its source fields")
        CtorRow identity <$> traverse rebase fields
      verifySourceLayout constructor = do
        let sourceFields = case splitTyConApp_maybe ty of
              Just (_, args) -> dataConInstOrigArgTys constructor args
              Nothing -> []
            unpacked = any isUnpacked (dataConImplBangs constructor)
        sourceReps <- traverse oneSourceRep sourceFields
        runtimeReps <- concat <$> traverse (\(Scaled _ fieldType) -> repsForType fieldType)
          (dataConRepArgTys constructor)
        resultReps <- repsForType (dataConOrigResTy constructor)
        unless (not unpacked && sourceReps == runtimeReps
          && length sourceFields == length runtimeReps
          && resultReps == [LiftedRefRep])
          (failLayout "prepared type constructor source/runtime layout is not one-to-one")
        pure sourceReps
      oneSourceRep (Scaled _ fieldType) = do
        reps <- repsForType fieldType
        case reps of
          [rep] -> pure rep
          _ -> failLayout "prepared type source field is void, flattened, or split"
      isUnpacked HsUnpack{} = True
      isUnpacked _ = False

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

projectTop :: CgStgTopBinding -> P (Group TopBinding)
projectTop (StgTopStringLit binder bytes) = do
  identity <- requireTopValue binder
  symbol <- topIdentity binder
  pure (NonRecursive (TopBinding symbol
    (HeapBinding identity (Bytes bytes))))
projectTop (StgTopLifted (StgNonRec binder rhs)) = NonRecursive <$> projectTopPair binder rhs
projectTop (StgTopLifted (StgRec pairs)) = Recursive <$> mapM (uncurry projectTopPair) pairs

projectTopPair :: Id -> CgStgRhs -> P TopBinding
projectTopPair binder rhs = do
  symbol <- topIdentity binder
  formatting <- formattingSpecFor binder
  time <- timeSpecFor binder
  let project = case deferredFunction binder of
        Just deferred -> projectDeferredRhs binder deferred rhs
        Nothing -> case time of
          Just spec -> projectTimeRhs spec rhs
          Nothing -> maybe (projectRhs binder rhs)
            (\spec -> projectFormattingRhs spec rhs) formatting
  TopBinding symbol <$> (HeapBinding <$> requireTopValue binder <*> project)

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
    Just join -> Jump join <$> mapM projectArg args
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
  modify' (\current -> current { joins = extendVarEnv (joins current) binder identity })
  pure (NonRecursive projected)
projectJoinGroup (StgRec pairs) = do
  mapM_ (uncurry registerRhsEntryArity) pairs
  identities <- mapM (bindJoin . fst) pairs
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
          let symbol = lookupVarEnv topNames binder
          case symbol of
            Just home -> case Map.lookup home tops of
              Just identity -> pure (Local identity)
              Nothing -> lift (Left (MissingPreparedTop home))
            Nothing -> case nullaryWorkerConstructor binder of
              Just con -> Local <$> internNullaryWorker binder con
              Nothing -> Global <$> internGlobal binder

deferredFunctionReference :: Id -> DeferredFunction -> P ValueId
deferredFunctionReference binder deferred = do
  topNames <- gets topSymbols
  tops <- gets topValues
  case lookupVarEnv topNames binder of
    Just symbol -> case Map.lookup symbol tops of
      Just identity -> pure identity
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

bindJoin :: Id -> P JoinId
bindJoin binder = do
  identity <- freshJoin
  modify' (\current -> current { joins = extendVarEnv (joins current) binder identity })
  pure identity

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
internGlobal binder
  | not (isExternalName (varName binder)) =
      lift (Left (UnboundPreparedInternal
        (Text.pack (occNameString (nameOccName (varName binder))))))
  | otherwise = case nameModule_maybe (varName binder) of
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
          existing <- gets globalDecls
          generations <- gets retainedGenerations
          let identity = GlobalId (fromIntegral (length existing))
              symbol = idSymbol "value" externalBinder
              retainedGeneration = Map.lookup symbol generations
              declaration = GlobalDecl symbol rep signature evaluated
                retainedGeneration
          modify' (\current -> current
            { globals = extendVarEnv (globals current) externalBinder identity
            , globalDecls = globalDecls current <> [declaration] })
          pure identity

internSignature :: Signature -> P SignatureId
internSignature signature = do
  known <- gets signatures
  case find ((== signature) . fst) known of
    Just (_, identity) -> pure identity
    Nothing -> do
      let identity = SignatureId (fromIntegral (length known))
      modify' (\current -> current { signatures = signatures current <> [(signature, identity)] })
      pure identity

internConstructor :: DataCon -> P ConstructorId
internConstructor con = do
  known <- gets constructors
  case lookup con known of
    Just identity -> pure identity
    Nothing -> do
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
      prior <- gets constructorDecls
      let identity = ConstructorId (fromIntegral (length prior))
          declaration = ConstructorDecl
            (nameSymbol "constructor" (dataConName con))
            (nameSymbol "type" (GHC.tyConName (dataConTyCon con)))
            resultRep reps fieldStrictness layout tag familySize
            (varId (dataConWorkId con))
      modify' (\current -> current
        { constructors = constructors current <> [(con, identity)]
        , constructorDecls = constructorDecls current <> [declaration] })
      pure identity
  where
    scaledThing (Scaled _ ty) = ty
    isUnboxed LiftedRefRep = False
    isUnboxed UnliftedRefRep = False
    isUnboxed _ = True

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
  operationSignature <- signatureForId signature
  known <- gets operations
  case find (matches operationIdentity operationSignature) known of
    Just (_, _, identity) -> pure identity
    Nothing -> do
      prior <- gets operationDecls
      let identity = OperationId (fromIntegral (length prior))
          declaration = OperationDecl operationIdentity signature
      modify' (\current -> current
        { operations = operations current <> [(operationIdentity, operationSignature, identity)]
        , operationDecls = operationDecls current <> [declaration] })
      pure identity
  where
    matches wantedIdentity wantedSignature (knownIdentity, knownSignature, _) =
      wantedIdentity == knownIdentity && wantedSignature == knownSignature

signatureForId :: SignatureId -> P Signature
signatureForId identity = do
  known <- gets signatures
  case find ((== identity) . snd) known of
    Just (signature, _) -> pure signature
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
nameSymbolWithFallback fallback namespace name = case nameModule_maybe name of
  Just modul -> SymbolIdentity (Text.pack (unitString (moduleUnit modul)))
    (Text.pack (moduleNameString (moduleName modul))) namespace
    (Text.pack (occNameString (nameOccName name)))
    (if isExternalName name
       then Text.pack . unpackFS <$> fieldOcc_maybe (nameOccName name)
       else Nothing)
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
