{-# LANGUAGE DataKinds #-}

-- | Refine declaration imports and install typed aliases from the current
-- World's captured value interfaces.
module Tidepool.CheckedPrefixImports
  ( refineProgramDeclarationImports
  , refineParsedDeclarationImports
  , selectedImportNames, parsedDeclarationOccurrences
  , CompletedValueAliases, completedValueAliasIds, hydrateCompletedValueAliases, transformCompletedValueAliases
  ) where

import Control.Exception (evaluate)
import Control.Monad (forM, forM_, unless)
import Data.List (nub, sort)
import Data.Maybe (isJust)
import qualified Data.IntMap.Strict as IntMap
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
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
import GHC.Types.Name.Occurrence (OccName, isTcOcc, isDataOcc, mkVarOcc, occNameString, occNameFS, varToRecFieldOcc, fieldOcc_maybe)
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
import Tidepool.DeclarationJoin (DeclarationExport(..), ExportIdentity(..), ExportNamespace, exportIdentity)
import Tidepool.ExactHydration
  ( ExactIfaceArtifact(..), HydratedExactInterfaces
  , hydratedExactEnvironment, hydratedInterfaceProduct
  , checkedValueOwner )
import Tidepool.Identity (nameSymbolIdentity)
import Tidepool.ExecutionSchema (SymbolIdentity)
import Tidepool.CheckedCell.Internal (NativeParsedModule(..))
import Tidepool.ImportSyntax (RefinedImportSyntax, refinedParsedModule)
import Tidepool.ImportSyntax.Internal
  ( ImportRefinement(..), mapRefinedImportsM, insertSyntheticPrelude, mapImportPreservingSource
  , ImportOccurrence, ImportOccurrenceRoot, importOccurrenceId, importOccurrenceRoot
  , refinedCapturedSyntax, capturedParsedSource, capturedImportOccurrences, refinedImportOccurrences )

-- This capability comes from exact installed interface allocations and their
-- actual exported Names. Runtime variable IDs never supply GHC binder identity.
data CompletedValueAliases = CompletedValueAliases [RefinementOwner] (Map.Map String Id)

completedValueAliasIds :: CompletedValueAliases -> Map.Map String Id
completedValueAliasIds (CompletedValueAliases _ aliases) = aliases

hydrateCompletedValueAliases
  :: [(String,SymbolIdentity,ExactIfaceArtifact)] -> HydratedExactInterfaces
  -> IO (Either String CompletedValueAliases)
hydrateCompletedValueAliases requested hydratedOwner
  | length requested /= Map.size (Map.fromList [(alias,()) | (alias,_,_) <- requested]) =
      pure (Left "completed value aliases have duplicate lexical winners")
  | any (not . checkedValueOwner . (\(_,_,artifact) -> artifact)) requested =
      pure (Left "completed value alias has another owner")
  | otherwise = do
      let env = hydratedExactEnvironment hydratedOwner
          capturedProducts = forM requested $ \(_,_,artifact) -> do
            (_,home) <- hydratedInterfaceProduct hydratedOwner artifact
            pure (artifact,hm_iface home)
      case capturedProducts of
        Left reason -> pure (Left reason)
        Right captured -> hydrateAliases env captured
  where
    hydrateAliases env captured = do
      let
          interfaces = Map.fromList [((exactUnit artifact,exactModule artifact),(artifact,iface,names iface)) | (artifact,iface) <- captured]
          names iface = Map.fromListWith (++)
            [(symbol,[name]) | name <- Set.toList (Set.fromList (concatMap availNames (mi_exports iface)))
              , Just symbol <- [nameSymbolIdentity "value" name]]
          select (alias,symbol,artifact) = do
            (actual,iface,exports) <- maybe (Left "completed value alias lacks its captured interface") Right
              (Map.lookup (exactUnit artifact,exactModule artifact) interfaces)
            -- Each receiving alias was checked against the selected owner
            -- above; different authenticated paths can share that owner.
            unless (sameAliasOwner actual artifact) (Left "completed value alias has another captured interface")
            name <- case Map.findWithDefault [] symbol exports of
              [name] -> Right name
              _ -> Left "completed value alias lacks its exact exported SymbolIdentity"
            pure (alias,iface,name)
      case mapM select requested of
        Left reason -> pure (Left reason)
        Right selected -> do
          let hydrated = env
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
              [] (Just (forced,stable)))
          pure ((\values -> CompletedValueAliases owners (Map.fromList values)) <$> sequence aliases)
    sameAliasOwner first second =
      (exactUnit first,exactModule first,exactSha256 first,exactRequirements first)
        == (exactUnit second,exactModule second,exactSha256 second,exactRequirements second)

-- The generated root's segment chain stays untouched. Explicit native-typed
-- top-level aliases let ordinary GHC renaming preserve authored local shadows
-- and the imported value's sigma type without textual source stand-ins.
transformCompletedValueAliases :: CompletedValueAliases -> HscEnv -> NativeParsedModule -> IO NativeParsedModule
transformCompletedValueAliases (CompletedValueAliases owners aliases) env (NativeParsedModule parsed usesBefore) = do
  forM_ owners $ \owner -> do
    original <- currentOwner env (refinementOwner owner) (refinementFingerprint owner)
    case refinementReadback owner of
      Just (retained,expected) -> do
        _ <- evaluate retained
        actual <- evaluate (hm_iface original) >>= makeStableName
        unless (actual == expected) (fail "completed value alias lost its exact installed interface allocation")
      Nothing -> fail "completed value alias lacks its installed interface allocation"
  let syntax = unLoc (pm_parsed_source (refinedParsedModule parsed))
      authored = concatMap parsedDeclarationOccurrences (hsmodDecls syntax)
      additions = concatMap aliasDeclarations (Map.toAscList aliases)
      uses = unionNameSets [extendNameSet
          (freeNamesIfDecl (IfaceId (idName identifier) (toIfaceType (idType identifier)) IfVanillaId []))
          (idName identifier) | identifier <- Map.elems aliases]
  unless (all ((`notElem` authored) . mkVarOcc) (Map.keys aliases))
    (fail "completed value alias conflicts with a generated top-level binder")
  next <- mapImportPreservingSource (\source -> pure (case source of
    L location _ -> L location syntax {hsmodDecls=additions ++ hsmodDecls syntax})) parsed
  pure (NativeParsedModule next (unionNameSets [usesBefore, uses]))
  where
    aliasDeclarations (alias,identifier) =
      let name = mkRdrUnqual (mkVarOcc alias)
          binding = mkHsVarBind noSrcSpan name (nlHsVar (Exact (idName identifier)))
          signature = TypeSig noAnn [noLocA name] (hsTypeToHsSigWcType (noLocA (XHsType (idType identifier))))
      in [noLocA (SigD noExtField signature),noLocA (ValD noExtField (unLoc binding))]

data RefinementOwner = RefinementOwner
  { refinementOwner :: Module
  , refinementFingerprint :: Fingerprint
  , refinementNames :: [ExportIdentity]
  , refinementReadback :: Maybe (ModIface, StableName ModIface)
  }

-- Only this issuer applies lineage-changing import operations to the held
-- bundle. Native type uses survive every import refinement unchanged.
mapPrefixImports
  :: (RefinedImportSyntax -> IO RefinedImportSyntax)
  -> NativeParsedModule -> IO NativeParsedModule
mapPrefixImports transform (NativeParsedModule parsed uses) = do
  next <- transform parsed
  pure (NativeParsedModule next uses)

-- Select the source-ordered declaration winners while keeping every original
-- owner available through its qualified import. World value aliases are applied
-- separately after this declaration refinement.
refineProgramDeclarationImports
  :: [(Module,Fingerprint,[DeclarationExport])] -> HscEnv -> NativeParsedModule -> IO NativeParsedModule
refineProgramDeclarationImports [] _ = pure
refineProgramDeclarationImports originals env = mapPrefixImports $ \parsed -> do
  unless (length originals == Set.size (Set.fromList [owner | (owner,_,_) <- originals]))
    (fail "program declaration owner is duplicated")
  owners <- mapM (\(owner,fingerprint,groups) -> originalRefinement env owner fingerprint
    (concatMap (\group -> exportHead group : exportChildren group) groups)) originals
  bindings <- bindCapturedImports owners parsed
  let headWinners = Map.fromList [(lexicalKey (exportHead group),owner)
        | (owner,_,groups) <- originals, group <- groups]
      originalWinners = [(lexicalKey name,owner)
        | (owner,_,groups) <- originals, group <- groups
        , Map.lookup (lexicalKey (exportHead group)) headWinners == Just owner
        , name <- exportHead group : exportChildren group]
      winners = Map.fromList originalWinners
      bound occurrence = [owner | (root,owner) <- bindings
        , root == importOccurrenceRoot (importOccurrenceId occurrence)]
      shadowKeys = Set.fromList (map lexicalKey (concatMap refinementNames owners))
  verified <- refineUnqualifiedImports (\occurrence _ -> not (null (bound occurrence)))
    ((`Set.notMember` shadowKeys) . lexicalKey . exportIdentity) env parsed
  let select occurrence located = case [owner | owner <- bound occurrence
          , ideclQualified (unLoc located) == NotQualified] of
        [] -> pure KeepImport
        [owner] -> do
          original <- currentOwner env (refinementOwner owner) (refinementFingerprint owner)
          selected <- either fail pure (selectedImportNames (mi_exports (hm_iface original))
            (ideclImportList (unLoc located)))
          let retained = [name | name <- selected
                , Map.lookup (lexicalKey (exportIdentity name)) winners == Just (refinementOwner owner)]
          pure $ if retained == selected then KeepImport else NarrowUnqualified (map importName retained)
        _ -> fail "program declaration owner is duplicated"
  mapRefinedImportsM select verified

lexicalKey :: ExportIdentity -> (ExportNamespace,String,Maybe String)
lexicalKey identity = (exportNamespace identity,exportOccurrence identity,exportRecordParent identity)

originalRefinement
  :: HscEnv -> Module -> Fingerprint -> [ExportIdentity] -> IO RefinementOwner
originalRefinement env owner fingerprint names = do
  original <- currentOwner env owner fingerprint
  unless (sort (nub names) == sort (nub (map exportIdentity
      (concatMap availNames (mi_exports (hm_iface original)))))) $
    fail "planned import refinement lacks its complete original export inventory"
  pure (RefinementOwner owner fingerprint names Nothing)

currentOwner :: HscEnv -> Module -> Fingerprint -> IO HomeModInfo
currentOwner env owner fingerprint = case lookupHpt (hsc_HPT env) (moduleName owner) of
  Just original | mi_module (hm_iface original) == owner
      && mi_iface_hash (mi_final_exts (hm_iface original)) == fingerprint -> pure original
  _ -> fail "checked import refinement lacks its exact original interface"

-- An original declaration has no certified inventory until after it checks.
-- Its existing GHC parse supplies local occurrences before renaming; imported
-- Names still come from their resolved interfaces and retain qualified access.
refineParsedDeclarationImports :: HscEnv -> NativeParsedModule -> IO NativeParsedModule
refineParsedDeclarationImports env = mapPrefixImports $ \parsed ->
  let locals = nub (concatMap parsedDeclarationOccurrences
        (hsmodDecls (unLoc (pm_parsed_source (refinedParsedModule parsed)))))
  in if null locals then pure parsed else refineUnqualifiedImports (\_ _ -> False)
      ((`notElem` locals) . nameOccName) env parsed

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

-- This syntax identifies the unique direct import whose exact HOME owner and
-- interface are checked by the composition. Aliased/package imports keep their
-- own selection and full-parent qualified clone from the ordinary pass.
directRefinementImport :: RefinementOwner -> LImportDecl GhcPs -> Bool
directRefinementImport owner imported = let declaration = unLoc imported in
  unLoc (ideclName declaration) == moduleName (refinementOwner owner)
    && ideclQualified declaration == NotQualified
    && ideclAs declaration == Nothing
    && (case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
    && ideclSource declaration == NotBoot

-- The wildcard premise belongs to immutable captured syntax. Subsequent passes
-- may narrow only its one unqualified descendant; another same-module import
-- cannot replace that occurrence.
bindCapturedImports
  :: [RefinementOwner] -> RefinedImportSyntax -> IO [(ImportOccurrenceRoot,RefinementOwner)]
bindCapturedImports owners parsed = forM owners $ \owner -> do
  let captured = refinedCapturedSyntax parsed
      capturedImports = zip (capturedImportOccurrences captured)
        (hsmodImports (unLoc (capturedParsedSource captured)))
      liveImports = zip (refinedImportOccurrences parsed)
        (hsmodImports (unLoc (pm_parsed_source (refinedParsedModule parsed))))
  occurrence <- case [(occurrence,imported) | (occurrence,imported) <- capturedImports
      , directRefinementImport owner imported] of
    [(occurrence,imported)] | ideclImportList (unLoc imported) == Nothing -> pure occurrence
    [_] -> fail "planned declaration requires one direct original wildcard import"
    _ -> fail "checked import refinement requires one direct original import"
  let root = importOccurrenceRoot (importOccurrenceId occurrence)
  case [imported | (live,imported) <- liveImports
      , importOccurrenceRoot (importOccurrenceId live) == root
      , ideclQualified (unLoc imported) == NotQualified] of
    [imported] | directRefinementImport owner imported -> pure (root,owner)
    _ -> fail "checked import refinement lost its direct original occurrence"

-- Narrow only the unqualified view. The qualified clone keeps the submitted
-- alias and import selection, so shadowing cannot widen historical access.
refineUnqualifiedImports
  :: (ImportOccurrence -> LImportDecl GhcPs -> Bool) -> (Name -> Bool)
  -> HscEnv -> RefinedImportSyntax -> IO RefinedImportSyntax
refineUnqualifiedImports protected retain env parsed = do
  let module_ = refinedParsedModule parsed
      syntax = unLoc (pm_parsed_source module_)
      originalImports = hsmodImports syntax
      flags = ms_hspp_opts (pm_mod_summary module_)
  withPrelude <- if xopt ImplicitPrelude flags
      && not (any ((== mkModuleName "Prelude") . unLoc . ideclName . unLoc) originalImports)
    then either fail pure (insertSyntheticPrelude parsed)
    else pure parsed
  mapRefinedImportsM (\occurrence located -> do
    let declaration = unLoc located
    if protected occurrence located || ideclQualified declaration /= NotQualified
      then pure KeepImport
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
          then pure KeepImport
          else pure (NarrowUnqualified (map importName retained))) withPrelude

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
       in if isJust (fieldOcc_maybe occurrence) then occurrence == actual
          else occNameString occurrence == occNameString actual
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
