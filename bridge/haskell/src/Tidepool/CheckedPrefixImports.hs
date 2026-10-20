{-# LANGUAGE DataKinds #-}

-- | Refine lexical imports from parsed local declarations, original declaration
-- inventories, or completed-prefix interfaces installed in this request.
module Tidepool.CheckedPrefixImports
  ( CompletedValueImport(..), CompletedValueImports
  , hydrateCompletedValueImports, hydrateCompletedValueImportsWithDependencies
  , hydrateCompletedValueImportsWithVerifiedDependencies
  , transformCompletedValueImports
  , refineOriginalDeclarationImports, refineOriginalDeclarationImportsWithCompleted
  , refineProgramDeclarationImports
  , refineParsedDeclarationImports
  , selectedImportNames, parsedDeclarationOccurrences
  , CompletedValueAliases, completedValueAliasIds, hydrateCompletedValueAliases, transformCompletedValueAliases
  ) where

import Control.Exception (evaluate)
import Control.Monad (forM, forM_, unless)
import Data.List (nub, sort)
import qualified Data.IntMap.Strict as IntMap
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import Data.Word (Word64)
import GHC (ParsedModule(..), ModSummary(ms_hspp_opts))
import GHC.Data.Maybe (MaybeErr(..))
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_unit_env, hsc_home_unit, lookupType)
import GHC.Driver.Session (xopt)
import GHC.Hs
import GHC.Iface.Load (loadInterface, importDecl, WhereFrom(..))
import GHC.LanguageExtensions.Type (Extension(ImplicitPrelude))
import GHC.Rename.Names (renameRawPkgQual)
import GHC.Tc.Utils.Monad (initIfaceCheck, initIfaceLoad)
import GHC.Types.Avail (AvailInfo, availName, availNames)
import GHC.Types.Name (Name, nameOccName, nameModule_maybe)
import GHC.Types.Name.Occurrence (OccName, isTcOcc, isDataOcc, mkVarOcc, occNameString, occNameFS, varToRecFieldOcc)
import GHC.Types.Name.Reader (RdrName(Exact), mkRdrUnqual, rdrNameOcc)
import GHC.Types.Name.Set (extendNameSet, unionNameSets)
import GHC.Types.Id (Id, idName, idType)
import GHC.Types.TyThing (TyThing(AnId))
import GHC.Iface.Syntax (IfaceDecl(..), IfaceIdDetails(..), freeNamesIfDecl)
import GHC.CoreToIface (toIfaceType)
import GHC.Types.PkgQual (RawPkgQual(..))
import GHC.Types.SrcLoc (GenLocated(..), unLoc, noSrcSpan)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module (Module, moduleName, moduleUnit)
import GHC.Unit.Home (isHomeUnit)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_iface_hash, mi_final_exts, mi_exports)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Utils.Fingerprint (Fingerprint)
import GHC.Utils.Outputable (ppr)
import System.Mem.StableName (StableName, makeStableName)
import Tidepool.DeclarationJoin (ExportIdentity(..), ExportNamespace(..), exportIdentity)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), VerifiedExactIfaceClosure, readExactIfaceArtifacts
  , selectVerifiedValueInterfaces, hydrateExactScope, checkedValueOwner )
import Tidepool.Identity (stableVarId, nameSymbolIdentity)
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.CheckedCell (NativeParsedModule(..))
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule,
  sessionModuleString, scaffoldTargetName, registerSessionInterfaceLocation)

-- These fields come from the closed completed-prefix authorization. They are
-- advisory until the exact captured bytes and exported native Names validate.
data CompletedValueImport = CompletedValueImport
  { completedValueUnit :: String
  , completedValueModule :: String
  , completedValueIfacePath :: FilePath
  , completedValueIfaceSha256 :: String
  , completedValueBinders :: [(String, Word64)]
  } deriving (Eq, Show)

newtype CompletedValueImports = CompletedValueImports [RefinementOwner]

-- This capability comes from exact installed interface allocations and their
-- actual exported Names. Runtime variable IDs never supply GHC binder identity.
data CompletedValueAliases = CompletedValueAliases [RefinementOwner] (Map.Map String Id)

completedValueAliasIds :: CompletedValueAliases -> Map.Map String Id
completedValueAliasIds (CompletedValueAliases _ aliases) = aliases

hydrateCompletedValueAliases
  :: [(String,SymbolIdentity,ExactIfaceArtifact)] -> [(ExactIfaceArtifact,ModIface)] -> HscEnv
  -> IO (Either String (HscEnv,CompletedValueAliases))
hydrateCompletedValueAliases requested captured env
  | length requested /= Map.size (Map.fromList [(alias,()) | (alias,_,_) <- requested]) =
      pure (Left "completed value aliases have duplicate lexical winners")
  | any (not . checkedValueOwner . fst) captured = pure (Left "completed value alias has another owner")
  | length captured /= Map.size (Map.fromList
      [((exactUnit artifact,exactModule artifact),()) | (artifact,_) <- captured]) =
      pure (Left "completed value aliases contain duplicate interface owners")
  | otherwise = do
      let interfaces = Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,iface,names iface)) | (artifact,iface) <- captured]
          names iface = Map.fromListWith (++)
            [(symbol,[name]) | name <- Set.toList (Set.fromList (concatMap availNames (mi_exports iface)))
              , Just symbol <- [nameSymbolIdentity "value" name]]
          select (alias,symbol,artifact) = do
            (actual,iface,exports) <- maybe (Left "completed value alias lacks its captured interface") Right
              (Map.lookup (exactUnit artifact,exactModule artifact) interfaces)
            unless (actual == artifact) (Left "completed value alias has another captured interface")
            name <- case Map.findWithDefault [] symbol exports of
              [name] -> Right name
              _ -> Left "completed value alias lacks its exact exported SymbolIdentity"
            pure (alias,iface,name)
      case mapM select requested of
        Left reason -> pure (Left reason)
        Right selected -> do
          installed <- forM captured $ \(_,iface) -> case lookupHpt (hsc_HPT env) (moduleName (mi_module iface)) of
            Just home | mi_module (hm_iface home) == mi_module iface -> do
              expected <- evaluate iface >>= makeStableName
              actual <- evaluate (hm_iface home) >>= makeStableName
              pure (actual == expected)
            _ -> pure False
          hydrated <- if and installed then pure env else hydrateExactScope env captured
          aliases <- forM selected $ \(alias,iface,name) -> do
            _ <- currentOwner hydrated (mi_module iface) (mi_iface_hash (mi_final_exts iface))
            known <- lookupType hydrated name
            resolved <- case known of
              Just thing -> pure (Just thing)
              Nothing | Just owner <- nameModule_maybe name
                  , not (isHomeUnit (hsc_home_unit hydrated) (moduleUnit owner)) -> do
                    loaded <- initIfaceLoad hydrated (importDecl name)
                    pure $ case loaded of Succeeded thing -> Just thing; Failed _ -> Nothing
              _ -> pure Nothing
            case resolved of
              Just (AnId identifier) | idName identifier == name -> pure (Right (alias,identifier))
              _ -> pure (Left "completed value alias has no hydrated value identifier")
          owners <- forM captured $ \(_,iface) -> do
            forced <- evaluate iface
            stable <- makeStableName forced
            pure (RefinementOwner (mi_module forced) (mi_iface_hash (mi_final_exts forced))
              [] (CompletedValues []) (Just (forced,stable)))
          pure ((\values -> (hydrated,CompletedValueAliases owners (Map.fromList values))) <$> sequence aliases)

-- The generated root's segment chain stays untouched. Explicit native-typed
-- top-level aliases let ordinary GHC renaming preserve authored local shadows
-- and the imported value's sigma type without textual source stand-ins.
transformCompletedValueAliases :: CompletedValueAliases -> HscEnv -> ParsedModule -> IO NativeParsedModule
transformCompletedValueAliases (CompletedValueAliases owners aliases) env parsed = do
  forM_ owners $ \owner -> do
    original <- currentOwner env (refinementOwner owner) (refinementFingerprint owner)
    case refinementReadback owner of
      Just (retained,expected) -> do
        _ <- evaluate retained
        actual <- evaluate (hm_iface original) >>= makeStableName
        unless (actual == expected) (fail "completed value alias lost its exact installed interface allocation")
      Nothing -> fail "completed value alias lacks its installed interface allocation"
  let syntax = unLoc (pm_parsed_source parsed)
      authored = concatMap parsedDeclarationOccurrences (hsmodDecls syntax)
      additions = concatMap aliasDeclarations (Map.toAscList aliases)
      uses = unionNameSets [extendNameSet
          (freeNamesIfDecl (IfaceId (idName identifier) (toIfaceType (idType identifier)) IfVanillaId []))
          (idName identifier) | identifier <- Map.elems aliases]
  unless (all ((`notElem` authored) . mkVarOcc) (Map.keys aliases))
    (fail "completed value alias conflicts with a generated top-level binder")
  pure (NativeParsedModule (parsed {pm_parsed_source=case pm_parsed_source parsed of
    L location _ -> L location syntax {hsmodDecls=additions ++ hsmodDecls syntax}}) uses)
  where
    aliasDeclarations (alias,identifier) =
      let name = mkRdrUnqual (mkVarOcc alias)
          binding = mkHsVarBind noSrcSpan name (nlHsVar (Exact (idName identifier)))
          signature = TypeSig noAnn [noLocA name] (hsTypeToHsSigWcType (noLocA (XHsType (idType identifier))))
      in [noLocA (SigD noExtField signature),noLocA (ValD noExtField (unLoc binding))]

data DirectSelection = OriginalDeclarations | CompletedValues [Name]
data RefinementOwner = RefinementOwner
  { refinementOwner :: Module
  , refinementFingerprint :: Fingerprint
  , refinementNames :: [ExportIdentity]
  , refinementSelection :: DirectSelection
  , refinementReadback :: Maybe (ModIface, StableName ModIface)
  }

-- Thin value interfaces have fingerprint0. Decode captured SHA-verified bytes,
-- install those exact interfaces together, then retain their actual allocations.
-- A zero GHC fingerprint never substitutes for that read-and-install proof.
hydrateCompletedValueImports
  :: [CompletedValueImport] -> HscEnv
  -> IO (Either String (HscEnv, CompletedValueImports))
hydrateCompletedValueImports = hydrateCompletedValueImportsWithDependencies []

-- Read dependencies and completed owners once, installing one HPT knot. The
-- completed cap retains exactly the interface allocations installed by this
-- batch, including when a selected value's type depends on another input.
hydrateCompletedValueImportsWithDependencies
  :: [ExactIfaceArtifact] -> [CompletedValueImport] -> HscEnv
  -> IO (Either String (HscEnv, CompletedValueImports))
hydrateCompletedValueImportsWithDependencies = hydrateCompletedValueInputs readExactIfaceArtifacts

hydrateCompletedValueImportsWithVerifiedDependencies
  :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact] -> [CompletedValueImport] -> HscEnv
  -> IO (Either String (HscEnv, CompletedValueImports))
hydrateCompletedValueImportsWithVerifiedDependencies closure =
  hydrateCompletedValueInputs (\_ artifacts -> pure (selectVerifiedValueInterfaces closure artifacts))

hydrateCompletedValueInputs
  :: (HscEnv -> [ExactIfaceArtifact] -> IO (Either String [(ExactIfaceArtifact,ModIface)]))
  -> [ExactIfaceArtifact] -> [CompletedValueImport] -> HscEnv
  -> IO (Either String (HscEnv, CompletedValueImports))
hydrateCompletedValueInputs readInputs dependencies requested env
  | null artifacts = pure (Right (env, CompletedValueImports []))
  | any invalidOwner requested = pure (Left "completed values require canonical Val owners and nonempty selections")
  | any invalidDependency dependencies = pure (Left "completed value dependencies require canonical Val owners")
  | length winners /= length (nub winners) = pure (Left "completed value selections contain duplicate lexical winners")
  | length owners /= length (nub owners) = pure (Left "completed value hydration contains duplicate interface owners")
  | otherwise = do
      decoded <- readInputs env artifacts
      case decoded of
        Left diagnostic -> pure (Left diagnostic)
        Right interfaces -> do
          let completedInterfaces = drop (length dependencies) interfaces
          case sequence (zipWith selectedNames requested completedInterfaces) of
            Left diagnostic -> pure (Left diagnostic)
            Right selections -> do
              hydrated <- hydrateExactScope env interfaces
              forM_ artifacts $ \artifact -> case parseSessionModule (exactModule artifact) of
                Just owner -> registerSessionInterfaceLocation (exactPath artifact) owner hydrated
                Nothing -> fail "completed value owner ceased to be canonical"
              refinements <- forM (zip completedInterfaces selections) $ \((_, iface), names) -> do
                forced <- evaluate iface
                stable <- makeStableName forced
                pure (RefinementOwner (mi_module forced) (mi_iface_hash (mi_final_exts forced))
                  (map exportIdentity names) (CompletedValues names) (Just (forced, stable)))
              pure (Right (hydrated, CompletedValueImports refinements))
  where
    invalidOwner value = null (completedValueBinders value)
      || any ((== scaffoldTargetName) . fst) (completedValueBinders value)
      || case parseSessionModule (completedValueModule value) of
        Just owner -> smKind owner /= ValMod || sessionModuleString owner /= completedValueModule value
        Nothing -> True
    winners = concatMap (map fst . completedValueBinders) requested
    invalidDependency artifact = case parseSessionModule (exactModule artifact) of
      Just owner -> smKind owner /= ValMod || sessionModuleString owner /= exactModule artifact
      Nothing -> True
    owners = map exactModule artifacts
    artifacts = dependencies ++ [ExactIfaceArtifact (completedValueUnit value) (completedValueModule value)
      (completedValueIfacePath value) (completedValueIfaceSha256 value) [] | value <- requested]
    selectedNames value (_, iface) = mapM select (completedValueBinders value)
      where
        select (occurrence, identity) = case
          [name | name <- nub (concatMap availNames (mi_exports iface))
            , let exported = exportIdentity name
            , exportUnit exported == completedValueUnit value
            , exportModule exported == completedValueModule value
            , exportNamespace exported == ValueNamespace
            , exportOccurrence exported == occurrence
            , stableVarId name == identity] of
          [name] -> Right name
          _ -> Left "completed value selection lacks its exact exported native identity"

transformCompletedValueImports
  :: CompletedValueImports -> HscEnv -> ParsedModule -> IO ParsedModule
transformCompletedValueImports (CompletedValueImports owners) = refineImports owners []

-- Planned declaration certification supplies the full authored-only original
-- export inventory. Keep its direct wildcard import contract distinct from the
-- completed value owner's exact selected import while sharing one AST policy.
refineOriginalDeclarationImports
  :: Module -> Fingerprint -> [ExportIdentity] -> HscEnv -> ParsedModule -> IO ParsedModule
refineOriginalDeclarationImports owner fingerprint names env parsed = do
  original <- originalRefinement env owner fingerprint names
  refineImports [original] [] env parsed

-- A completed value may replace a Lib value. Protect only the completed cap's
-- verified exact direct selections during the Lib pass, then give the value
-- winners precedence over that Lib and every historical unqualified import.
refineOriginalDeclarationImportsWithCompleted
  :: Module -> Fingerprint -> [ExportIdentity] -> CompletedValueImports
  -> HscEnv -> ParsedModule -> IO ParsedModule
refineOriginalDeclarationImportsWithCompleted owner fingerprint names
    (CompletedValueImports completed) env parsed = do
  original <- originalRefinement env owner fingerprint names
  intermediate <- refineImports [original] completed env parsed
  refineImports completed [] env intermediate

-- Select the source-ordered declaration winners while keeping every original
-- owner available through its qualified import. Completed Val selections have
-- already been pruned by declaration shadowing before they enter this pass.
refineProgramDeclarationImports
  :: [(Module,Fingerprint,[[ExportIdentity]])] -> Maybe CompletedValueImports
  -> HscEnv -> ParsedModule -> IO ParsedModule
refineProgramDeclarationImports originals captured env parsed = do
  owners <- mapM (\(owner,fingerprint,groups) -> originalRefinement env owner fingerprint (concat groups)) originals
  let completed = case captured of Just (CompletedValueImports values) -> values; Nothing -> []
      namespace TypeNamespace = TypeNamespace
      namespace ConstructorNamespace = ConstructorNamespace
      namespace FieldNamespace = FieldNamespace
      namespace _ = ValueNamespace
      key identity = (namespace (exportNamespace identity),exportOccurrence identity,exportRecordParent identity)
      headWinners = Map.fromList [(key headName,owner)
        | (owner,_,groups) <- originals, headName:_ <- groups]
      originalWinners = [(key name,owner)
        | (owner,_,groups) <- originals, group@(headName:_) <- groups
        , Map.lookup (key headName) headWinners == Just owner, name <- group]
      winners = Map.fromList (originalWinners ++ [(key name,refinementOwner owner)
        | owner <- completed, name <- refinementNames owner])
  verified <- refineImports owners completed env parsed
  let syntax = unLoc (pm_parsed_source verified)
      select located = case [owner | owner <- owners
          , unLoc (ideclName (unLoc located)) == moduleName (refinementOwner owner)
          , ideclQualified (unLoc located) == NotQualified] of
        [] -> pure [located]
        [owner] -> do
          original <- currentOwner env (refinementOwner owner) (refinementFingerprint owner)
          selected <- either fail pure (selectedImportNames (mi_exports (hm_iface original)) Nothing)
          let retained = [name | name <- selected
                , Map.lookup (key (exportIdentity name)) winners == Just (refinementOwner owner)]
              declaration = unLoc located
              replace value = case located of L location _ -> L location value
          pure [replace declaration {ideclImportList = Just (Exactly,noLocA (map importName retained))}
            , replace declaration {ideclQualified = QualifiedPre}]
        _ -> fail "program declaration owner is duplicated"
  imports <- concat <$> mapM select (hsmodImports syntax)
  let selected = verified {pm_parsed_source = case pm_parsed_source verified of
        L location _ -> L location syntax {hsmodImports = imports}}
  if null completed then pure selected else refineImports completed [] env selected

originalRefinement
  :: HscEnv -> Module -> Fingerprint -> [ExportIdentity] -> IO RefinementOwner
originalRefinement env owner fingerprint names = do
  original <- currentOwner env owner fingerprint
  unless (sort (nub names) == sort (nub (map exportIdentity
      (concatMap availNames (mi_exports (hm_iface original)))))) $
    fail "planned import refinement lacks its complete original export inventory"
  pure (RefinementOwner owner fingerprint names OriginalDeclarations Nothing)

currentOwner :: HscEnv -> Module -> Fingerprint -> IO HomeModInfo
currentOwner env owner fingerprint = case lookupHpt (hsc_HPT env) (moduleName owner) of
  Just original | mi_module (hm_iface original) == owner
      && mi_iface_hash (mi_final_exts (hm_iface original)) == fingerprint -> pure original
  _ -> fail "checked import refinement lacks its exact original interface"

-- An original declaration has no certified inventory until after it checks.
-- Its existing GHC parse supplies local occurrences before renaming; imported
-- Names still come from their resolved interfaces and retain qualified access.
refineParsedDeclarationImports :: HscEnv -> ParsedModule -> IO ParsedModule
refineParsedDeclarationImports env parsed
  | null locals = pure parsed
  | otherwise = refineUnqualifiedImports (const False)
      ((`notElem` locals) . nameOccName) env parsed
  where
    locals = nub (concatMap parsedDeclarationOccurrences
      (hsmodDecls (unLoc (pm_parsed_source parsed))))

parsedDeclarationOccurrences :: LHsDecl GhcPs -> [OccName]
parsedDeclarationOccurrences (L _ declaration) = case declaration of
  ValD _ binding -> map rdrNameOcc (collectHsBindBinders CollNoDictBinders binding)
    ++ case binding of
      PatSynBind _ PSB {psb_id = name, psb_args = RecCon fields} ->
        [varToRecFieldOcc (occNameFS (rdrNameOcc (unLoc name)))
          (rdrNameOcc (unLoc (foLabel (recordPatSynField field)))) | field <- fields]
      _ -> []
  TyClD _ declaration' ->
    let binders = hsLTyClDeclBinders (noLocA declaration')
     in map (rdrNameOcc . unLoc)
          (fst (tyDeclMainBinder binders) : map fst (tyDeclATs binders) ++ tyDeclOpSigs binders)
        ++ parsedConstructorOccurrences (tyDeclConsWithFields binders)
  InstD _ (DataFamInstD _ declaration') ->
    parsedConstructorOccurrences (hsDataFamInstBinders declaration')
  InstD _ (ClsInstD _ declaration') -> concatMap
    (parsedConstructorOccurrences . hsDataFamInstBinders . unLoc) (cid_datafam_insts declaration')
  ForD _ declaration' -> map (rdrNameOcc . unLoc)
    (hsForeignDeclsBinders [noLocA declaration'])
  _ -> []

-- GHC namespaces every record field by the first constructor of its datatype,
-- including fields declared by later constructors. Keep that native parent;
-- a same-spelled field of a different datatype is a different occurrence.
parsedConstructorOccurrences :: LConsWithFields 'Parsed -> [OccName]
parsedConstructorOccurrences (LConsWithFields constructors fields) =
  map (rdrNameOcc . unLoc . fst) constructors ++ case constructors of
    (first, _) : _ ->
      [varToRecFieldOcc (occNameFS (rdrNameOcc (unLoc first)))
        (rdrNameOcc (unLoc (foLabel (unLoc field)))) | field <- IntMap.elems fields]
    [] -> []

refineImports :: [RefinementOwner] -> [RefinementOwner] -> HscEnv -> ParsedModule -> IO ParsedModule
refineImports [] [] _ parsed = pure parsed
refineImports owners protected env parsed = do
  let syntax = unLoc (pm_parsed_source parsed)
      originalImports = hsmodImports syntax
      direct owner imported = let declaration = unLoc imported in
        unLoc (ideclName declaration) == moduleName (refinementOwner owner)
          && ideclQualified declaration == NotQualified
          && ideclAs declaration == Nothing
          && (case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
          && ideclSource declaration == NotBoot
  forM_ (owners ++ protected) $ \owner -> do
    original <- currentOwner env (refinementOwner owner) (refinementFingerprint owner)
    case refinementReadback owner of
      Nothing -> pure ()
      Just (retained, expected) -> do
        _ <- evaluate retained
        actual <- evaluate (hm_iface original) >>= makeStableName
        unless (actual == expected) $
          fail "completed value import lost its exact installed interface allocation"
    case filter (direct owner) originalImports of
      [imported] -> case refinementSelection owner of
        OriginalDeclarations -> unless (ideclImportList (unLoc imported) == Nothing) $
          fail "planned declaration requires one direct original wildcard import"
        CompletedValues expected -> do
          let selection = ideclImportList (unLoc imported)
          unless (case selection of Just (Exactly, _) -> True; _ -> False) $
            fail "completed value import requires its exact explicit selection"
          selected <- either fail pure (selectedImportNames (mi_exports (hm_iface original)) selection)
          unless (sort (map exportIdentity selected) == sort (map exportIdentity expected)) $
            fail "completed value import differs from its certified prefix winners"
      _ -> fail "checked import refinement requires one direct original import"
  let namespace TypeNamespace = TypeNamespace
      namespace ConstructorNamespace = ConstructorNamespace
      namespace _ = ValueNamespace
      key identity = (namespace (exportNamespace identity), exportOccurrence identity)
      shadowKeys = map key (concatMap refinementNames owners)
  refineUnqualifiedImports (\located -> any (`direct` located) (owners ++ protected))
    ((`notElem` shadowKeys) . key . exportIdentity) env parsed

-- Narrow only the unqualified view. The qualified clone keeps the submitted
-- alias and import selection, so shadowing cannot widen historical access.
refineUnqualifiedImports
  :: (LImportDecl GhcPs -> Bool) -> (Name -> Bool)
  -> HscEnv -> ParsedModule -> IO ParsedModule
refineUnqualifiedImports protected retain env parsed = do
  let syntax = unLoc (pm_parsed_source parsed)
      originalImports = hsmodImports syntax
      flags = ms_hspp_opts (pm_mod_summary parsed)
      imports = if xopt ImplicitPrelude flags
          && not (any ((== mkModuleName "Prelude") . unLoc . ideclName . unLoc) originalImports)
        then noLocA (simpleImportDecl (mkModuleName "Prelude")) : originalImports
        else originalImports
  refined <- fmap concat $ forM imports $ \located -> do
    let declaration = unLoc located
    if protected located || ideclQualified declaration /= NotQualified
      then pure [located]
      else do
        let importedName = unLoc (ideclName declaration)
            qualifier = renameRawPkgQual (hsc_unit_env env) importedName (ideclPkgQual declaration)
        resolved <- findImportedModule env importedName qualifier
        importedOwner <- case resolved of
          Found _ found -> pure found
          _ -> fail "checked import refinement cannot resolve an original import"
        iface <- initIfaceCheck (ppr importedName) env $
          loadInterface (ppr importedName) importedOwner (ImportByUser (ideclSource declaration))
        available <- case iface of
          Succeeded found -> pure (mi_exports found)
          Failed _ -> fail "checked import refinement cannot load an original import interface"
        selected <- either fail pure (selectedImportNames available (ideclImportList declaration))
        let retained = filter retain selected
        if length retained == length selected
          then pure [located]
          else do
            let qualified = declaration {ideclQualified = QualifiedPre}
                narrowed = declaration
                  {ideclImportList = Just (Exactly, noLocA (map importName retained))}
                replace value = case located of L location _ -> L location value
            pure [replace narrowed, replace qualified]
  pure parsed {pm_parsed_source = case pm_parsed_source parsed of
    L location _ -> L location syntax {hsmodImports = refined}}

selectedImportNames
  :: [AvailInfo] -> Maybe (ImportListInterpretation, LocatedLI [LIE GhcPs])
  -> Either String [Name]
selectedImportNames available selection = do
  chosen <- case selection of
    Nothing -> Right allNames
    Just (interpretation, entries) -> do
      names <- nub . concat <$> mapM (select . unLoc) (unLoc entries)
      pure $ case interpretation of
        Exactly -> names
        EverythingBut -> filter (`notElem` names) allNames
  pure (nub chosen)
  where
    allNames = nub (concatMap availNames available)
    matches wrapped name =
      let occurrence = rdrNameOcc (ieWrappedName (unLoc wrapped))
          actual = nameOccName name
       in occNameString occurrence == occNameString actual
          && case unLoc wrapped of
            IEType {} -> isTcOcc actual
            IEPattern {} -> isDataOcc actual
            _ | isTcOcc occurrence -> isTcOcc actual
              | isDataOcc occurrence -> isDataOcc actual
              | otherwise -> not (isTcOcc actual || isDataOcc actual)
    select entry = case entry of
      IEVar _ wrapped _ -> Right (filter (matches wrapped) allNames)
      IEThingAbs _ wrapped _ -> Right (filter (matches wrapped) allNames)
      IEThingAll _ wrapped _ -> Right (concatMap availNames (filter (matches wrapped . availName) available))
      IEThingWith _ wrapped wildcard children _ -> Right
        [name | avail <- available, matches wrapped (availName avail), name <- availNames avail
          , name == availName avail || wildcard /= NoIEWildcard || any (`matches` name) children]
      _ -> Left "planned import refinement encountered a non-name import selection"

-- Keep the GHC occurrence namespace, including a record field's parent. This
-- avoids broadening T(field) to every same-spelled field exported by a module.
importName :: Name -> LIE GhcPs
importName name
  | isTcOcc occurrence = noLocA (IEThingAbs Nothing wrapped Nothing)
  | isDataOcc occurrence = noLocA (IEVar Nothing
      (noLocA (IEPattern noAnn (noLocA (mkRdrUnqual occurrence)))) Nothing)
  | otherwise = noLocA (IEVar Nothing wrapped Nothing)
  where
    occurrence = nameOccName name
    wrapped = noLocA (IEName noExtField (noLocA (mkRdrUnqual occurrence)))
