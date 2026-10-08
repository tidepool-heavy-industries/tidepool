module TypedSegmentCases
  ( typedSegmentRewriteSemantics, typedSegmentNativePreparation
  , typedSegmentRecordMetadataProperty
  ) where

import Control.Exception (SomeException, bracket, evaluate, fromException, try)
import Control.Monad (forM, forM_, unless, void, when)
import Control.Monad.IO.Class (liftIO)
import Data.List (intercalate, isPrefixOf)
import Data.Maybe (isJust)
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map
import GHC
import GHC.Core (bindersOfBinds)
import GHC.Driver.Session (PackageDBFlag(..), PkgDbRef(..))
import GHC.Driver.Env.Types (hsc_unit_env)
import GHC.Data.FastString (fsLit)
import GHC.Unit.Env (ue_units)
import GHC.Unit.Info (PackageName(..))
import GHC.Unit.State (lookupPackageName)
import GHC.Unit.Module (moduleUnit)
import GHC.Unit.Types (unitIdString, unitString, GenUnit(RealUnit), Definite(Definite))
import GHC.Builtin.Types (intTy, doubleTy)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Types.Name (getOccString, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (OccName)
import GHC.Types.Avail (availNames)
import GHC.Types.TypeEnv (typeEnvIds)
import GHC.Types.Var (varName)
import GHC.Iface.Syntax (IfaceConDecl(..), IfaceConDecls(..), IfaceDecl(..))
import GHC.Types.FieldLabel (FieldSelectors(..), flHasFieldSelector, flSelector)
import GHC.Unit.Module.ModDetails (md_types)
import GHC.Unit.Module.ModIface (mi_decls, mi_exports, mi_module)
import GHC.Unit.Home.ModInfo (hm_details, hm_iface)
import qualified GHC.Stg.Syntax as Stg
import GHC.Types.SourceError (SourceError)
import GHC.Tc.Utils.TcType (tcSplitSigmaTy)
import GHC.Utils.Outputable (ppr, showSDocUnsafe)
import System.Directory
  ( createDirectory, createDirectoryIfMissing, getTemporaryDirectory, removeDirectoryRecursive, removeFile )
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import Tidepool.Binders
import Tidepool.ExtractUtil (getLibdir)
import Tidepool.FinalizedModule (finalizedHomeModInfo)
import Tidepool.DiagJson (diagsFromSourceError)
import Tidepool.GhcPipeline
import Tidepool.PreparedStg (pmModule, pmBindings, pmOriginalTopNames)
import Tidepool.PackageWitness (PackageImportEvidence(..), PackageImportRoot(..))
import Tidepool.SessionArtifacts
import Tidepool.Test.Runner (requiredInput)
import Tidepool.TurnSource (preambleDefaultDeclaration)
import qualified Tidepool.TypedSegment as TypedSegment
import Tidepool.TypedSegment.Source (rewriteParsedSegmentRoot)
import Unsafe.Coerce (unsafeCoerce)

-- The owning selector performs the actual whole-source frontend, typed
-- extraction, real thin-interface hydration, simplify and prepared STG.
-- Execution of those item roots belongs to the runtime semantic suite.
typedSegmentNativePreparation :: IO ()
typedSegmentNativePreparation = bracket temporary removeDirectoryRecursive $ \root -> do
  effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
  libdir <- getLibdir
  flags <- runGhc (Just libdir) getSessionDynFlags
  let shadowDirectory = root </> "Tidepool" </> "Internal"
  createDirectoryIfMissing True shadowDirectory
  -- A package-qualified compiler import must not inspect or compile this source.
  writeFile (shadowDirectory </> "Resume.hs") "invalid authored home source shadow\n"
  let fixtures = "test-cell-splitter/fixtures/typed-segment-native"
      includes = [root, fixtures, effects, prelude]
  withResidentPipelineSelectedRequests includes $ \runRequest ->
    forM_ (zip [0 :: Int ..] preparationCases) $ \(index, (name, expected)) -> do
      body <- readFile (fixtures </> name ++ ".hs")
      helpers <- if name == "template-helper-use"
        then readFile (fixtures </> "template-helper-definitions.hs")
        else pure ""
      let template = preparationTemplateWithHelpers ("TypedPrepared" ++ show index) helpers
      wholePlan <- analyzeOrderedCellWithFlags flags template body >>= either (fail . show) pure
      sourcePlan <- fixtureExecutableSegment name wholePlan
      let slots = [(ordinal, fromIntegral (index * 100 + ordinal + 1), observation ordinal item)
            | (ordinal, item) <- zip [0 ..] (cellPlanItems sourcePlan)]
          observation ordinal item = case sbKind (cellAnalysisVerdict item) of
            KExpr -> Just ("__typedObservation" ++ show ordinal)
            _ -> Nothing
      source <- either fail pure (prepareTypedSegmentSource template sourcePlan (replicate 64 'c') slots)
      let path = root </> "TypedPrepared" ++ show index ++ ".hs"
          plan = preparedTypedSegmentPlan source
          prepare environment admissions segment = do
            batch <- prepareTypedSegmentSessionBindings environment admissions segment
              (root </> "stage" ++ show index)
            pure (typedSegmentSessionEnvironment batch, typedSegmentSessionGlobals batch,
              typedSegmentSessionInterfaces batch)
      writeFile path (preparedTypedSegmentSource source)
      result <- try (runRequest (pure ()) $ \compiler -> compiler
        (WithTypedSegmentPreparation prepare (PreparedSegmentProducts plan Nothing))
        mempty (TypedSegmentCompile plan (preparedTypedSegmentOperations source) GeneralCompile) Nothing path includes Nothing)
          :: IO (Either SomeException PreparedSegmentProductsResult)
      case (expected, result) of
        (PrepareAccepted, Right products) -> do
          let prepared = preparedSegmentProducts products
              environment = prHscEnv (pprPipelineResult prepared)
          selected <- maybe (fail (name ++ ": pinned resume package is absent")) pure
            (lookupPackageName (ue_units (hsc_unit_env environment)) (PackageName (fsLit "tidepool-resume")))
          let roots = concatMap packageInterfaces (Map.elems (pprPackageImports prepared))
          unless (any (\root -> packageModule root == "Tidepool.Internal.Resume"
              && packageUnit root == unitIdString selected) roots
              && all (\modul -> moduleName (pmModule modul) /= mkModuleName "Tidepool.Internal.Resume"
                || moduleUnit (pmModule modul) == RealUnit (Definite selected)) (pprModules prepared))
            (fail (name ++ ": compiler support escaped its exact external package unit"))
          let segment = preparedSegmentCaptures products
              issued = bindersOfBinds (TypedSegment.typedSegmentRoots segment)
          owner <- case Set.toList (Set.fromList [owner
              | identifier <- issued, Just owner <- [nameModule_maybe (varName identifier)]]) of
            [owner] -> pure owner
            _ -> fail (name ++ ": issued item/ABI roots do not have one actual module owner")
          unless (all ((== Just owner) . nameModule_maybe . varName) issued)
            (fail (name ++ ": an issued item/ABI root lacks its actual module owner"))
          target <- case [modul | modul <- pprModules (preparedSegmentProducts products), pmModule modul == owner] of
            [modul] -> pure modul
            _ -> fail (name ++ ": actual prepared target module is absent or ambiguous")
          let emitted = Set.fromList [varName identifier
                | (Stg.StgTopLifted binding, _) <- pmBindings target
                , identifier <- case binding of
                    Stg.StgNonRec identifier _ -> [identifier]
                    Stg.StgRec bindings -> map fst bindings]
          forM_ issued $ \identifier -> unless
              (varName identifier `Set.member` pmOriginalTopNames target
                && varName identifier `Set.member` emitted)
            (fail (name ++ ": an actual issued item/ABI entry was lost during prepared lowering"))
          -- Generalization/defaulting controls look at the GHC-issued value
          -- type, never a type moved from the enclosing action's quantifiers.
          let captures = concatMap TypedSegment.typedItemCaptures
                (TypedSegment.typedSegmentItems (preparedSegmentCaptures products))
              capture binder = case [TypedSegment.typedCaptureType value | value <- captures
                  , getOccString (TypedSegment.typedCaptureIdentifier value) == binder] of
                [value] -> pure value
                _ -> fail (name ++ ": missing unique semantic witness " ++ binder)
          case name of
            "numeric-action-mr" -> capture "number" >>= assertInt name
            "numeric-action-nomr" -> capture "number" >>= assertInt name
            "read-later" -> capture "number" >>= assertInt name
            "template-helper-use" -> do
              capture "replyValue" >>= assertInt name
              capture "recursiveValue" >>= assertInt name
              capture "integerStep" >>= assertInt name
              double <- capture "doubleStep"
              unless (eqType double doubleTy) (fail "template helper lost its independent Double instantiation")
            "num-scalar-nomr" -> do
              ty <- capture "number"
              let (variables, predicates, _) = tcSplitSigmaTy ty
              unless (not (null variables) && not (null predicates) && isClosureType ty)
                (fail "numeric let lost its genuine dictionary-taking value sigma")
            _ -> pure ()
        (PrepareTypedRefusal, Left exception)
          | Just failure <- fromException exception, captureRefusal failure -> pure ()
        (PrepareSourceRefusal, Left exception)
          | Just sourceError <- fromException exception ->
              unless (not (null (diagsFromSourceError (sourceError :: SourceError))))
                (fail (name ++ ": GHC source refusal has no diagnostics"))
        (_, Left exception) -> fail (name ++ ": unexpected compiler failure " ++ show exception)
        (_, Right _) -> fail (name ++ ": expected compiler refusal was accepted")
  putStrLn ("native typed preparation: " ++ show (length preparationCases) ++ " fixture requests")
  where
    assertInt name ty = unless (eqType ty intTy) (fail (name ++ ": actual capture is not Int"))
    captureRefusal failure = case (failure :: TypedSegment.TypedSegmentFailure) of
      TypedSegment.UnresolvedItemType _ -> True
      TypedSegment.OpenCaptureType _ _ -> True
      TypedSegment.OpenItemCore _ _ -> True
      _ -> False

-- Exercise finalized record metadata through the same resident pipeline used
-- to prepare executable cells. Selectors are interface metadata even when the
-- cell never uses them; they must retain their exact Name and Id without
-- becoming lexical exports.
typedSegmentRecordMetadataProperty :: IO ()
typedSegmentRecordMetadataProperty = bracket temporary removeDirectoryRecursive $ \root -> do
  effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
  libdir <- getLibdir
  flags <- runGhc (Just libdir) getSessionDynFlags
  let fixtures = "test-cell-splitter/fixtures/typed-segment-native"
      includes = [root, fixtures, effects, prelude]
  withResidentPipelineSelectedRequests includes $ \runRequest ->
    forM_ recordMetadataCases $ \(index, shape, used, generic) -> do
      let owner = "TypedRecordMetadata" ++ show index
          (declarations, selectors, useExpression) = recordMetadataSource index shape generic
          template = metadataTemplate generic
            (metadataPragmas index shape generic (preparationTemplateWithHelpers owner declarations))
          body = if used
            then "selected <- pure (" ++ useExpression ++ ")\n"
            else "pure (0 :: Int)\n"
      wholePlan <- analyzeOrderedCellWithFlags flags template body >>= either (fail . show) pure
      sourcePlan <- fixtureExecutableSegment owner wholePlan
      let slots = [(ordinal, fromIntegral (index * 10 + ordinal + 1), observation ordinal item)
            | (ordinal, item) <- zip [0 ..] (cellPlanItems sourcePlan)]
          observation ordinal item = case sbKind (cellAnalysisVerdict item) of
            KExpr -> Just ("__metadataObservation" ++ show ordinal)
            _ -> Nothing
      source <- either fail pure
        (prepareTypedSegmentSource template sourcePlan (replicate 64 'a') slots)
      let path = root </> owner ++ ".hs"
          plan = preparedTypedSegmentPlan source
          prepare environment admissions segment = do
            batch <- prepareTypedSegmentSessionBindings environment admissions segment
              (root </> "metadata-stage" ++ show index)
            pure (typedSegmentSessionEnvironment batch, typedSegmentSessionGlobals batch,
              typedSegmentSessionInterfaces batch)
      writeFile path (preparedTypedSegmentSource source)
      products <- runRequest (pure ()) (\compiler -> compiler
        (WithTypedSegmentPreparation prepare (PreparedSegmentProducts plan Nothing))
        mempty (TypedSegmentCompile plan (preparedTypedSegmentOperations source) GeneralCompile)
        Nothing path includes Nothing)
      let prepared = preparedSegmentProducts products
          segment = preparedSegmentCaptures products
          issued = bindersOfBinds (TypedSegment.typedSegmentRoots segment)
      finalized <- maybe (fail (owner ++ ": actual finalized home module is absent")) pure
        (Map.lookup (mkModuleName owner) (pprFinalizedModules prepared))
      let home = finalizedHomeModInfo finalized
          interface = hm_iface home
          details = hm_details home
          moduleOwner = mi_module interface
          selectorIds = [identifier | identifier <- typeEnvIds (md_types details)
            , elem (getOccString (varName identifier)) selectors]
          selectorNames = map varName selectorIds
          ifaceSelectorNames = [ifName declaration
            | (_, declaration@IfaceId{}) <- mi_decls interface
            , elem (getOccString (ifName declaration)) selectors]
          ifaceIdNames = [ifName declaration
            | (_, declaration@IfaceId{}) <- mi_decls interface]
          patternSynSupportNames = concat
            [ fst (ifPatMatcher declaration)
                : maybe [] ((: []) . fst) (ifPatBuilder declaration)
            | (_, declaration@IfacePatSyn{}) <- mi_decls interface]
          ifacePatternFields = [getOccString (flSelector field)
            | (_, declaration@IfacePatSyn{}) <- mi_decls interface
            , field <- ifFieldLabels declaration]
          ifaceDataFields = concat
            [ map flSelector fields
            | (_, declaration@IfaceData{}) <- mi_decls interface
            , fields <- case ifCons declaration of
                IfAbstractTyCon -> []
                IfDataTyCon _ constructors -> map ifConFields constructors
                IfNewTyCon constructor -> [ifConFields constructor]
            ]
          noFieldSelectorFlags = [flHasFieldSelector field
            | (_, declaration@IfaceData{}) <- mi_decls interface
            , fields <- case ifCons declaration of
                IfAbstractTyCon -> []
                IfDataTyCon _ constructors -> map ifConFields constructors
                IfNewTyCon constructor -> [ifConFields constructor]
            , field <- fields
            , elem (getOccString (flSelector field)) selectors]
          actualFieldOwners =
            [ (getOccString (ifName declaration), getOccString (flSelector field))
            | (_, declaration@IfaceData{}) <- mi_decls interface
            , fields <- case ifCons declaration of
                IfAbstractTyCon -> []
                IfDataTyCon _ constructors -> map ifConFields constructors
                IfNewTyCon constructor -> [ifConFields constructor]
            , field <- fields]
            ++ [ (getOccString (ifName declaration), getOccString (flSelector field))
               | (_, declaration@IfacePatSyn{}) <- mi_decls interface
               , field <- ifFieldLabels declaration]
          ifaceRecordFields = ifaceDataFields
            ++ [flSelector field
              | (_, declaration@IfacePatSyn{}) <- mi_decls interface
              , field <- ifFieldLabels declaration]
          exportedNames = concatMap availNames (mi_exports interface)
          exportedIdentities = map publishedNameIdentity exportedNames
          issuedIdentities = map (publishedNameIdentity . varName) issued
          finalizedIdNames = map varName (typeEnvIds (md_types details))
          expectedCounts = Map.fromListWith (+) [(selector, 1 :: Int) | selector <- selectors]
          actualCounts = Map.fromListWith (+) [(getOccString name, 1 :: Int) | name <- selectorNames]
          expectedPatternFields = case shape of
            MetadataPatternSynonym -> selectors
            _ -> []
          actualPatternCounts = Map.fromListWith (+) [(field, 1 :: Int) | field <- ifacePatternFields]
          expectedPatternCounts = Map.fromListWith (+) [(field, 1 :: Int) | field <- expectedPatternFields]
          expectedFieldOwners = metadataFieldOwners index shape selectors
          actualFieldOwnerCounts = Map.fromListWith (+) [(identity, 1 :: Int) | identity <- actualFieldOwners]
          expectedFieldOwnerCounts = Map.fromListWith (+) [(identity, 1 :: Int) | identity <- expectedFieldOwners]
      unless (moduleNameString (moduleName moduleOwner) == owner)
        (fail (owner ++ ": finalized interface has another module identity"))
      unless (actualCounts == expectedCounts)
        (fail (owner ++ ": finalized type environment lost selector Ids: "
          ++ show (expectedCounts, actualCounts)))
      unless (Set.size (Set.fromList selectorNames) == length selectors)
        (fail (owner ++ ": distinct generated fields collapsed to one selector Name"))
      unless (Set.fromList ifaceSelectorNames == Set.fromList selectorNames)
        (fail (owner ++ ": finalized IfaceId declarations do not match selector Id Names"))
      unless (Set.fromList ifaceRecordFields == Set.fromList selectorNames)
        (fail (owner ++ ": finalized field metadata does not name the retained selector Ids"))
      unless (actualFieldOwnerCounts == expectedFieldOwnerCounts)
        (fail (owner ++ ": finalized fields have another generated parent type identity"))
      unless (actualPatternCounts == expectedPatternCounts)
        (fail (owner ++ ": finalized pattern-synonym field metadata differs from its source declaration"))
      when (shape == MetadataPatternSynonym) $
        unless (length patternSynSupportNames == 2
            && all (\name -> Set.member name (Set.fromList ifaceIdNames)) patternSynSupportNames
            && all ((== Just moduleOwner) . nameModule_maybe) patternSynSupportNames)
          (fail (owner ++ ": finalized pattern synonym lost its explicit matcher or builder Id"))
      when (shape == MetadataNoFieldSelectors) $
        unless (noFieldSelectorFlags == [NoFieldSelectors])
          (fail (owner ++ ": finalized field metadata lost the NoFieldSelectors source flag"))
      unless (all ((== Just moduleOwner) . nameModule_maybe) selectorNames)
        (fail (owner ++ ": a selector Id lost its generated module owner"))
      unless (all (\name -> notElem name exportedNames) selectorNames)
        (fail (owner ++ ": record selectors became lexical module exports"))
      unless (all (`elem` ifaceIdNames) exportedNames)
        (fail (owner ++ ": a finalized export lacks its exact finalized IfaceId Name"))
      unless (all (`elem` finalizedIdNames) exportedNames)
        (fail (owner ++ ": a finalized export lacks its exact finalized md_types Id Name"))
      unless (Set.size (Set.fromList exportedIdentities) == length exportedIdentities
            && Set.size (Set.fromList issuedIdentities) == length issuedIdentities
            && length exportedIdentities == length issuedIdentities
            && Set.fromList exportedIdentities == Set.fromList issuedIdentities)
        (fail (owner ++ ": finalized exports differ from actual issued typed-segment roots"
          ++ "\n  finalized owner: " ++ renderModuleIdentity moduleOwner
          ++ "\n  finalized exports:\n    "
          ++ intercalate "\n    " (map renderPublishedNameIdentity exportedNames)
          ++ "\n  issued root Ids:\n    "
          ++ intercalate "\n    " (map (renderPublishedNameIdentity . varName) issued)))
  putStrLn ("typed record metadata: " ++ show (length recordMetadataCases)
    ++ " serial generated requests; record/newtype/multi-constructor, used/unused, "
    ++ "Generic/no-Generic, NoFieldSelectors, existential fields, duplicate selector "
    ++ "occurrences, and used/unused record pattern synonyms passed")

data MetadataShape
  = MetadataRecord
  | MetadataNewtype
  | MetadataMultipleConstructors
  | MetadataNoFieldSelectors
  | MetadataExistential
  | MetadataPatternSynonym
  deriving (Eq)

recordMetadataCases :: [(Int, MetadataShape, Bool, Bool)]
recordMetadataCases =
  [ (index, shape, used, generic)
  | (index, (shape, used, generic)) <- zip [0 :: Int ..]
      ([(shape, used, generic)
       | shape <- [MetadataRecord, MetadataNewtype, MetadataMultipleConstructors]
       , used <- [False, True]
       , generic <- [False, True]]
       ++ [ (MetadataNoFieldSelectors, False, False)
          , (MetadataExistential, False, False)
          , (MetadataPatternSynonym, False, False)
          , (MetadataPatternSynonym, True, False)
          ]) ]

recordMetadataSource :: Int -> MetadataShape -> Bool -> (String, [String], String)
recordMetadataSource index shape generic =
  (declarations ++ duplicateOwners, selectors, firstUse)
  where
    suffix = show index
    typeName = "MetadataType" ++ suffix
    constructor = "MetadataConstructor" ++ suffix
    firstField = "metadataField" ++ suffix
    secondField = "metadataOtherField" ++ suffix
    patternBase = "MetadataPatternBase" ++ suffix
    patternName = "MetadataPattern" ++ suffix
    patternField = "metadataPatternField" ++ suffix
    derivingClause = if generic then " deriving (G.Generic)" else ""
    (declarations, coreSelectors, firstUse) = case shape of
      MetadataRecord ->
        ("data " ++ typeName ++ " = " ++ constructor ++ " { " ++ firstField
          ++ " :: Int }" ++ derivingClause ++ "\n", [firstField],
          firstField ++ " (" ++ constructor ++ " (1 :: Int))")
      MetadataNewtype ->
        ("newtype " ++ typeName ++ " = " ++ constructor ++ " { " ++ firstField
          ++ " :: Int }" ++ derivingClause ++ "\n", [firstField],
          firstField ++ " (" ++ constructor ++ " (1 :: Int))")
      MetadataMultipleConstructors ->
        ("data " ++ typeName ++ " = " ++ constructor ++ " { " ++ firstField
          ++ " :: Int } | MetadataOtherConstructor" ++ suffix ++ " { " ++ secondField
          ++ " :: Bool }" ++ derivingClause ++ "\n", [firstField, secondField],
          firstField ++ " (" ++ constructor ++ " (1 :: Int))")
      MetadataNoFieldSelectors ->
        ("data " ++ typeName ++ " = " ++ constructor ++ " { " ++ firstField
          ++ " :: Int }\n", [firstField], "")
      MetadataExistential ->
        ("data " ++ typeName ++ " where\n  " ++ constructor
          ++ " :: forall hidden. { " ++ firstField ++ " :: hidden } -> " ++ typeName ++ "\n",
          [firstField], "")
      MetadataPatternSynonym ->
        ("data " ++ patternBase ++ " = " ++ patternBase ++ " Int\n"
          ++ "pattern " ++ patternName ++ " { " ++ patternField ++ " } = "
          ++ patternBase ++ " " ++ patternField ++ "\n",
          [patternField], patternField ++ " (" ++ patternName ++ " (1 :: Int))")
    -- The final generated point checks that equal selector occurrences owned
    -- by separate record types remain two distinct interface Ids.
    duplicateOwners
      | index == 11 = unlines
          [ "data DuplicateMetadataLeft = DuplicateMetadataLeft { duplicateMetadataField :: Int }"
          , "data DuplicateMetadataRight = DuplicateMetadataRight { duplicateMetadataField :: Bool }"
          ]
      | otherwise = ""
    selectors
      | index == 11 = coreSelectors ++ ["duplicateMetadataField", "duplicateMetadataField"]
      | otherwise = coreSelectors

metadataPragmas :: Int -> MetadataShape -> Bool -> String -> String
metadataPragmas index shape generic template = case lines template of
  first : rest -> unlines (first : map pragma extensions ++ rest)
  [] -> template
  where
    extensions =
      (if index == 11 then ["DuplicateRecordFields"] else [])
      ++ (if generic then ["DeriveGeneric"] else [])
      ++ (case shape of
        MetadataNoFieldSelectors -> ["NoFieldSelectors"]
        MetadataExistential -> ["GADTs"]
        MetadataPatternSynonym -> ["PatternSynonyms"]
        _ -> [])
    pragma extension = "{-# LANGUAGE " ++ extension ++ " #-}"

metadataTemplate :: Bool -> String -> String
metadataTemplate generic template
  | not generic = template
  | otherwise = unlines (concatMap addGenericsImport (lines template))
  where
    addGenericsImport "{{CELL_IMPORTS}}" =
      ["import qualified GHC.Generics as G", "{{CELL_IMPORTS}}"]
    addGenericsImport line = [line]

metadataFieldOwners :: Int -> MetadataShape -> [String] -> [(String, String)]
metadataFieldOwners index shape selectors = case shape of
  MetadataPatternSynonym -> [("MetadataPattern" ++ show index, selector) | selector <- selectors]
  _ ->
    [("MetadataType" ++ show index, selector)
    | selector <- take (if shape == MetadataMultipleConstructors then 2 else 1) selectors]
    ++ if index == 11
      then [("DuplicateMetadataLeft", "duplicateMetadataField")
           ,("DuplicateMetadataRight", "duplicateMetadataField")]
      else []

renderModuleIdentity :: Module -> String
renderModuleIdentity owner = unitString (moduleUnit owner) ++ ":" ++ moduleNameString (moduleName owner)

-- Published roots use module and OccName; internal GHC Uniques are local to a compiler representation.
publishedNameIdentity :: Name -> (Maybe Module, OccName)
publishedNameIdentity name = (nameModule_maybe name, nameOccName name)

renderPublishedNameIdentity :: Name -> String
renderPublishedNameIdentity name =
  maybe "<no-module>" renderModuleIdentity (nameModule_maybe name)
    ++ "." ++ showSDocUnsafe (ppr (nameOccName name))

data PreparationExpected = PrepareAccepted | PrepareTypedRefusal | PrepareSourceRefusal

-- The parser retains pragmas/imports as a prologue-only declaration receipt.
-- Use its executable segment, as the production caller does, retaining that
-- prologue for rendering and inference. Authored declaration barriers would
-- need their own genuine compilation and are not part of these fixtures.
fixtureExecutableSegment :: String -> CellSourcePlan -> IO CellSourcePlan
fixtureExecutableSegment name whole = do
  unless (not (any (\item -> sbKind (cellAnalysisVerdict item) == KDecl
      && not (cellAnalysisPrologueOnly item)) (cellPlanItems whole)))
    (fail (name ++ ": semantic fixture contains an authored declaration barrier"))
  case [segment | segment <- cellInferenceSegments whole
      , any ((/= KDecl) . sbKind . cellAnalysisVerdict) (cellPlanItems segment)] of
    [segment] | cellPlanPrologue segment == cellPlanPrologue whole -> pure segment
    _ -> fail (name ++ ": semantic fixture needs one executable segment with its original prologue")

preparationCases :: [(String, PreparationExpected)]
preparationCases =
  [(name, PrepareAccepted) | name <-
    [ "let-poly", "num-mr", "num-nomr", "num-scalar-nomr"
    , "numeric-action-mr", "numeric-action-nomr", "read-later", "dependent-later"
    , "nondefaultable-let", "rank-n-value", "explicit-poly", "phantom-let"
    , "scoped-equality", "nested-existential", "patterns", "strict-patterns"
    , "lazy-nested-bottom", "applied-original", "substitution-shadow", "bare-bottom"
    , "constrained-open-let", "zero-let-lazy", "zero-let-bang", "zero-let-strict"
    , "strict-closure-let", "wildcard-action", "bang-wildcard-action"
    , "refutable-failure", "authored-helper-alias", "template-helper-use" ]]
  ++ [(name, PrepareTypedRefusal) | name <-
    [ "unresolved-action", "unresolved-dependent"
    , "unresolved-phantom-action", "open-let", "cross-item-existential" ]]
  ++ [(name, PrepareSourceRefusal) | name <- ["wrong-row", "wrong-observation-row", "unresolved-read"]]

preparationTemplate :: String -> String
preparationTemplate owner = preparationTemplateWithHelpers owner ""

preparationTemplateWithHelpers :: String -> String -> String
preparationTemplateWithHelpers owner helpers = unlines
  [ "{-# LANGUAGE GHC2024, NamedDefaults, ScopedTypeVariables, TypeApplications, BangPatterns, UndecidableInstances, ExtendedDefaultRules #-}"
  , "{{CELL_PRAGMAS}}"
  , "module " ++ owner ++ " where"
  , "import Control.Monad.Freer (Eff)"
  , "import Data.Proxy"
  , "import Data.Typeable"
  , "import Data.Text (Text)"
  , "import qualified GHC.TypeError as TidepoolWorkbenchTypeError"
  , "import TypedSegmentSupport"
  , "import Tidepool.Effects.Core ()"
  , "{{CELL_IMPORTS}}"
  , preambleDefaultDeclaration
  , "default Applicative (Eff '[])"
  , "default Monad (Eff '[])"
  , "class TidepoolCellPure value"
  , "instance {-# OVERLAPPABLE #-} TidepoolCellPure value"
  , "instance {-# OVERLAPPING #-} TidepoolWorkbenchTypeError.Unsatisfiable ('TidepoolWorkbenchTypeError.Text \"an Eff action must use the current workbench effect row\") => TidepoolCellPure (Eff effects value)"
  , "class TidepoolCellExpression value where { __tidepoolCellExpression :: value -> Eff '[] () }"
  , "instance {-# OVERLAPPING #-} (effects ~ '[]) => TidepoolCellExpression (Eff effects value) where { __tidepoolCellExpression action = action >> pure () }"
  , "instance {-# OVERLAPPABLE #-} TidepoolCellPure value => TidepoolCellExpression value where { __tidepoolCellExpression _ = pure () }"
  , "{{CELL_DECLS}}"
  , helpers
  , "__tidepool_cell_check :: Eff '[] ()"
  , "__tidepool_cell_check = do {"
  , "{{CELL_BODY}}"
  , "; pure () }"
  ]

-- This executes the production parsed-source transformation, separately
-- from item extraction and runtime publication. Both programs run with an
-- independent IO trace interpreter; fixed expected outcomes distinguish a
-- shared error in the original and rewritten execution from equivalence.
typedSegmentRewriteSemantics :: IO ()
typedSegmentRewriteSemantics = bracket temporary removeDirectoryRecursive $ \root -> do
  effects <- requiredInput "TIDEPOOL_TEST_EFFECTS_DIR"
  prelude <- requiredInput "TIDEPOOL_PRELUDE_DIR"
  libdir <- getLibdir
  let fixtures = "test-cell-splitter/fixtures/typed-segment-native"
      includes = [root, fixtures, "test-cell-splitter/fixtures/typed-segment-oracle", effects, prelude]
  runGhc (Just libdir) $ do
    flags <- getSessionDynFlags
    void (setSessionDynFlags flags
      { backend = interpreterBackend, ghcLink = LinkInMemory, importPaths = includes
      , packageDBFlags = [PackageDB GlobalPkgDb, ClearPackageDBs] })
    sources <- liftIO $ forM (zip [0 :: Int ..] differentialCases) $ \(index, control) -> do
      body <- readFile (fixtures </> differentialFixture control ++ ".hs")
      let ordinary = "TypedOrdinary" ++ show index
          rewritten = "TypedRewritten" ++ show index
          (pragmas, statements) = span (isPrefixOf "{-# LANGUAGE") (lines body)
          source = unlines pragmas ++ differentialBefore control
            ++ unlines statements ++ differentialAfter control
          template = oracleTemplate ordinary
      wholePlan <- analyzeOrderedCellWithFlags flags template source >>= either (fail . show) pure
      parsed <- fixtureExecutableSegment (differentialFixture control) wholePlan
      let slots = [(ordinal, fromIntegral (ordinal + 1), Nothing)
            | ordinal <- [0 .. length (cellPlanItems parsed) - 1]]
      prepared <- either fail pure
        (prepareTypedSegmentSource template parsed (replicate 64 'b') slots)
      let original = preparedTypedSegmentSource prepared
          sourcePath = root </> ordinary ++ ".hs"
      writeFile sourcePath (original ++ oracleEntry (preparedTypedSegmentPlan prepared))
      pure (control, ordinary, rewritten, sourcePath, prepared)
    originalTargets <- mapM (\(_, _, _, path, _) -> guessTarget path Nothing Nothing) sources
    setTargets originalTargets
    void (depanal [] False)
    rewrittenPaths <- forM sources $ \(_, ordinary, rewritten, _, prepared) -> do
      summary <- getModSummary (mkModuleName ordinary)
      parsed <- parseModule summary
      transformed <- liftIO (rewriteParsedSegmentRoot (preparedTypedSegmentOperations prepared)
        (preparedTypedSegmentPlan prepared) parsed)
      -- Preserve the actual source pragmas: ParsedModule's printed AST does
      -- not include the header's LANGUAGE directives. Only the module owner
      -- changes so both programs can be loaded in the same interpreter.
      let pragmas = unlines (takeWhile (not . isPrefixOf "module ")
            (lines (preparedTypedSegmentSource prepared)))
          rendered = unlines [if ("module " ++ ordinary ++ " ") `isPrefixOf` line
            then "module " ++ rewritten ++ " where" else line
            | line <- lines (showSDocUnsafe (ppr (pm_parsed_source transformed)))]
          path = root </> rewritten ++ ".hs"
      liftIO (writeFile path (pragmas ++ rendered))
      pure path
    rewrittenTargets <- mapM (\path -> guessTarget path Nothing Nothing) rewrittenPaths
    setTargets (originalTargets ++ rewrittenTargets)
    loaded <- load LoadAllTargets
    case loaded of Failed -> liftIO (fail "differential oracle programs failed to load"); Succeeded -> pure ()
    forM_ sources $ \(control, ordinary, rewritten, _, _) -> do
      original <- execute ordinary
      transformed <- execute rewritten
      liftIO $ do
        let expected = (differentialTrace control, differentialFailure control)
            outcome (values, exception) = (values, isJust exception)
        unless (outcome original == expected)
          (fail (differentialFixture control ++ ": ordinary GHC result " ++ show original ++ " /= " ++ show expected))
        unless (outcome transformed == expected)
          (fail (differentialFixture control ++ ": production rewrite result " ++ show transformed ++ " /= " ++ show expected))
        unless (original == transformed)
          (fail (differentialFixture control ++ ": production rewrite changed the complete language exception message/category"))
    liftIO (putStrLn ("production rewrite differential: " ++ show (length sources)
      ++ " histories, " ++ show (2 * length sources) ++ " executed programs"))
  where
    execute owner = do
      setContext [IIDecl (simpleImportDecl (mkModuleName owner))
        , IIDecl (simpleImportDecl (mkModuleName "Prelude"))]
      value <- compileExpr "(oracleRun :: IO ([Int], Maybe (Either String String)))"
      result <- liftIO (unsafeCoerce value :: IO ([Int], Maybe (Either String String)))
      -- Finish the shared base-type result before changing interpreter scope.
      void (liftIO (evaluate (length (show result))))
      pure result

data DifferentialCase = DifferentialCase
  { differentialFixture :: String
  , differentialBefore :: String
  , differentialAfter :: String
  , differentialTrace :: [Int]
  , differentialFailure :: Bool
  }

differentialCases :: [DifferentialCase]
differentialCases =
  [ success "let-poly" "_ <- record intValue\n_ <- record (if boolValue then 1 else 0)\n_ <- record constantValue\n" [3, 1, 7]
  , success "num-mr" "_ <- record intValue\n_ <- record doubleValue\n" [3, 4]
  , success "num-nomr" "_ <- record intValue\n_ <- record doubleValue\n" [3, 4]
  , success "num-scalar-nomr" "_ <- record intValue\n_ <- record doubleValue\n" [9, 11]
  , success "read-later" "_ <- record answer\n" [8]
  , success "dependent-later" "_ <- record answer\n" [9]
  , success "patterns" "_ <- record answer\n" [134]
  , success "substitution-shadow" "_ <- record first\n_ <- record second\n" [4, 13]
  , success "lazy-nested-bottom" "_ <- record answer\n" [7]
  , success "nested-existential" "_ <- record (if name == \"Int\" then 41 else 0)\n" [41]
  , success "cross-item-existential" "_ <- record (if name == \"Int\" then 41 else 0)\n" [41]
  , success "rank-n-value" "_ <- record intValue\n_ <- record doubleValue\n" [9, 11]
  , success "nondefaultable-let" "_ <- record intValue\n_ <- record doubleValue\n" [17, 20]
  , success "scoped-equality" "_ <- record answer\n" [12]
  , success "zero-let-lazy" "_ <- record 2\n" [1, 2]
  , failure "zero-let-bang"
  , failure "zero-let-strict"
  , failure "strict-closure-let"
  , success "wildcard-action" "_ <- record 2\n" [1, 2]
  , failure "bang-wildcard-action"
  , failure "refutable-failure"
  , DifferentialCase "applied-original" "_ <- record 1\n" "_ <- record answer\n" [1] True
  , success "authored-helper-alias" "_ <- record answer\n" [9]
  ]
  where
    success name after values = DifferentialCase name (before name) after values False
    failure name = DifferentialCase name "_ <- record 1\n" "_ <- record 2\n" [1] True
    before name | name `elem` ["zero-let-lazy", "wildcard-action"] = "_ <- record 1\n"
                | otherwise = ""

oracleTemplate :: String -> String
oracleTemplate owner = unlines
  [ "{-# LANGUAGE GHC2024, NamedDefaults, ScopedTypeVariables, TypeApplications, BangPatterns, ExtendedDefaultRules #-}"
  , "{{CELL_PRAGMAS}}"
  , "module " ++ owner ++ " where"
  , "import Control.Monad.Freer (Eff)"
  , "import Data.Proxy"
  , "import Data.Typeable"
  , "import Data.Text (Text)"
  , "import TypedSegmentSupport"
  , "import TypedSegmentOracleSupport"
  , "import Tidepool.Effects.Core ()"
  , "{{CELL_IMPORTS}}"
  , preambleDefaultDeclaration
  , "default Applicative (Eff '[IO])"
  , "default Monad (Eff '[IO])"
  , "{{CELL_DECLS}}"
  , "__tidepool_cell_check :: Eff '[IO] ()"
  , "__tidepool_cell_check = do {"
  , "{{CELL_BODY}}"
  , "; pure () }"
  ]

oracleEntry :: TypedSegment.TypedSegmentPlan -> String
oracleEntry plan = "\noracleRun :: IO ([Int], Maybe (Either String String))\noracleRun = check "
  ++ TypedSegment.typedSegmentPlanRoot plan ++ "\n"

temporary :: IO FilePath
temporary = do
  tmp <- getTemporaryDirectory
  (path, handle) <- openTempFile tmp "typed-segment-differential"
  hClose handle
  removeFile path
  createDirectory path
  pure path
