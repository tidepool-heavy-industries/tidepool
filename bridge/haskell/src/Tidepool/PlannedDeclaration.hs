-- | Compile the parser-owned declaration group under its reserved original
-- owner before the whole cell is checked. The checking module imports those
-- declarations; it never creates substitute Names for them.
module Tidepool.PlannedDeclaration
  ( PlannedDeclaration, plannedModule, plannedSource, plannedCheckPlan
  , preparePlannedDeclaration
  , PlannedDeclarationInventory, plannedExports, plannedInstances
  , certifyPlannedDeclaration
  , transformPlannedDeclarationImports
  ) where

import Control.Monad (forM, unless)
import Data.Char (isSpace)
import Data.List (intercalate, isPrefixOf, nub, tails)
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import GHC (ParsedModule(..), ModSummary(ms_hspp_opts))
import GHC.Data.Maybe (MaybeErr(..))
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_HUG, hscEPS, hsc_home_unit, hsc_unit_env)
import GHC.Driver.Session (xopt)
import GHC.Hs
import GHC.Iface.Load (loadInterface, WhereFrom(..))
import GHC.LanguageExtensions.Type (Extension(ImplicitPrelude))
import GHC.Rename.Names (renameRawPkgQual)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.Avail (AvailInfo, availName, availNames)
import GHC.Types.Name (Name, nameOccName)
import GHC.Types.Name.Reader (mkRdrUnqual, rdrNameOcc)
import GHC.Types.PkgQual (RawPkgQual(..))
import GHC.Types.SrcLoc (GenLocated(..), unLoc)
import GHC.Unit.Env (unitEnv_hpts)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt, lookupHpt)
import GHC.Unit.Module (Module, moduleName, moduleUnit)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Module.ModIface (mi_module, mi_src_hash, mi_iface_hash, mi_final_exts, mi_exports)
import GHC.Unit.Types (unitString)
import GHC.Utils.Fingerprint (Fingerprint, fingerprintByteString)
import GHC.Utils.Outputable (ppr)
import GHC.Types.Name.Occurrence
  ( isSymOcc, mkTcOcc, mkVarOcc, isTcOcc, isDataOcc, occNameString )
import Tidepool.Binders
  ( CellSourcePlan(..), CellAnalysisItem(..), CellSourceSpan(..)
  , StmtBinders(..), TurnKind(..), ExportItem(..), LocatedImport(..)
  , SourcePrologue(..), DeclarationSource(..), renderDeclarationForTemplate )
import Tidepool.DeclarationJoin
  ( DeclarationExport(..), DeclarationKind(..), ExportIdentity(..), ExportNamespace(..), exportIdentity
  , InstanceInventory(..), ClassInstanceEvidence(..), JoinDecision(..)
  , interfaceExports, interfaceInventory, validateRetainedFamilyInstances )
import Tidepool.Session
  ( SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString, scaffoldTargetName )

data PlannedDeclaration = PlannedDeclaration
  { plannedModule :: String
  , plannedSource :: String
  , plannedCheckPlan :: CellSourcePlan
  , authoredHeads :: [(DeclarationKind, String)]
  }

data PlannedDeclarationInventory = PlannedDeclarationInventory
  { plannedExports :: [DeclarationExport]
  , plannedInstances :: InstanceInventory
  , inventoryOwner :: Module
  , inventoryInterface :: Fingerprint
  } deriving (Eq)

instance Show PlannedDeclarationInventory where
  showsPrec precedence inventory = showsPrec precedence
    (unitString (moduleUnit (inventoryOwner inventory))
    ,moduleNameString (moduleName (inventoryOwner inventory))
    ,plannedExports inventory, plannedInstances inventory, inventoryInterface inventory)

-- The wrapper and module reservation are frozen by the caller's offer. The
-- plan comes from the existing cell parser; source ordinals remain intact in
-- the checking plan even though the original declarations live elsewhere.
preparePlannedDeclaration
  :: String -> String -> CellSourcePlan -> Either String PlannedDeclaration
preparePlannedDeclaration reserved wrapper plan = do
  owner <- maybe (Left "planned declaration has no canonical session owner") Right
    (parseSessionModule reserved)
  unless (smKind owner == LibMod && sessionModuleString owner == reserved)
    (Left "planned declaration requires the canonical reserved Lib owner")
  declaration <- case cellPlanItems plan of
    item : rest
      | sbKind (cellAnalysisVerdict item) == KDecl
      , not (cellAnalysisPrologueOnly item)
      , all ((/= KDecl) . sbKind . cellAnalysisVerdict) rest -> Right item
    _ -> Left "planned declaration requires one initial parser-owned declaration group"
  unless (occurrences "{{TURN}}" wrapper == 1)
    (Left "planned declaration wrapper requires one turn placeholder")
  let exports = nub (sbDeclItems (cellAnalysisVerdict declaration))
      heads = nub (map exportHead exports)
  unless (all ((/= scaffoldTargetName) . snd) heads)
    (Left "authored declaration uses the compiler result binder")
  let header = "module " ++ reserved ++ " (" ++ intercalate ", " (map renderExport exports) ++ ") where"
  originalWrapper <- replaceHeader header wrapper
  source <- renderDeclarationForTemplate originalWrapper
    (DeclarationSource (cellPlanPrologue plan) (cellAnalysisSource declaration))
  let cleared item
        | sbKind (cellAnalysisVerdict item) == KDecl = item {cellAnalysisSource = ""}
        | otherwise = item
      prologue = cellPlanPrologue plan
      check = plan
        { cellPlanItems = map cleared (cellPlanItems plan)
        , cellPlanPrologue = prologue
            { prologueImports = prologueImports prologue ++
                [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ reserved)] }
        , cellPlanDeclarationBase = ""
        , cellPlanGenericDeclarations = []
        , cellPlanDisplayDeclarations = ""
        , cellPlanDisplayTargets = []
        }
  pure (PlannedDeclaration reserved source check heads)
  where
    occurrences needle = length . filter (needle `isPrefixOf`) . tails
    replaceHeader header source = case
        [index | (index, line) <- zip [0 :: Int ..] (lines source)
          , "module " `isPrefixOf` dropWhile isSpace line] of
      [index] -> Right (unlines
        [if position == index then header else line
        | (position, line) <- zip [0 :: Int ..] (lines source)])
      _ -> Left "planned declaration wrapper requires one module header"
    exportHead (EValue name) = (ValueDeclaration, name)
    exportHead (EType name _) = (TypeDeclaration, name)
    exportHead (EClass name _) = (ClassDeclaration, name)
    renderExport (EValue name) = renderName mkVarOcc name
    renderExport (EType name children) = renderName mkTcOcc name
      ++ if null children then "" else "(..)"
    renderExport (EClass name _) = renderName mkTcOcc name ++ "(..)"
    renderName occurrence name
      | isSymOcc (occurrence name) = "(" ++ name ++ ")"
      | otherwise = name

-- Certification reads compiler identities and inventories from the original
-- interface. Rendered names are never evidence that two owners are equal.
certifyPlannedDeclaration
  :: PlannedDeclaration -> HscEnv -> IO (Either String PlannedDeclarationInventory)
certifyPlannedDeclaration planned env = case
    lookupHpt (hsc_HPT env) (mkModuleName (plannedModule planned)) of
  Nothing -> pure (Left "planned original declaration interface is absent")
  Just original -> do
    let iface = hm_iface original
        owner = mi_module iface
        unit = unitString (moduleUnit owner)
        owns identity = exportUnit identity == unit
          && exportModule identity == plannedModule planned
        sourceHash = fingerprintByteString (TE.encodeUtf8 (T.pack (plannedSource planned)))
    if moduleNameString (moduleName owner) /= plannedModule planned
        || moduleUnit owner /= homeUnitAsUnit (hsc_home_unit env)
        || mi_src_hash iface /= sourceHash
      then pure (Left "planned original declaration owner or source differs")
      else do
        exports <- interfaceExports env iface
        instances <- interfaceInventory env iface
        eps <- hscEPS env
        let families = concat
              [md_fam_insts (hm_details hmi)
              | hpt <- unitEnv_hpts (hsc_HUG env), hmi <- eltsHpt hpt]
            heads = nub [(exportKind item, exportOccurrence (exportHead item)) | item <- exports]
            exactExports = all (\item -> owns (exportHead item)
                && all owns (exportChildren item)) exports
            familyCheck = validateRetainedFamilyInstances (eps_fam_inst_env eps) families
        pure $ do
          unless (length heads == length (authoredHeads planned)
              && all (`elem` authoredHeads planned) heads && exactExports)
            (Left "planned original declaration exports differ from parser-owned heads")
          inventory <- instances
          unless (all (owns . instanceDfun) (inventoryClasses inventory)
              && all owns (inventoryFamilies inventory)
              && all (all owns . instanceSelectedAxioms) (inventoryClasses inventory))
            (Left "planned original instance inventory has a foreign owner")
          case familyCheck of
            JoinAccepted -> Right (PlannedDeclarationInventory exports inventory owner (mi_iface_hash (mi_final_exts iface)))
            JoinRejected _ diagnostic -> Left diagnostic

-- The checking recipe retains historical imports alongside the new original
-- owner. Give its certified authored names the declaration wrapper's lexical
-- precedence: subtract them from other unqualified selections, preserving each
-- unchanged selection in a qualified clone. Aliases, packages and SOURCE edges
-- remain intact; this does not make an invalid original declaration valid.
transformPlannedDeclarationImports
  :: PlannedDeclarationInventory -> HscEnv -> ParsedModule -> IO ParsedModule
transformPlannedDeclarationImports inventory env parsed = do
  let owner = inventoryOwner inventory
      ownerName = moduleName owner
      syntax = unLoc (pm_parsed_source parsed)
      originalImports = hsmodImports syntax
      direct imported = let declaration = unLoc imported in
        unLoc (ideclName declaration) == ownerName
          && ideclQualified declaration == NotQualified
          && ideclAs declaration == Nothing
          && (case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
          && ideclSource declaration == NotBoot
          && ideclImportList declaration == Nothing
      ownerInterface = lookupHpt (hsc_HPT env) ownerName
  unless (length (filter direct originalImports) == 1
      && maybe False (\hmi -> mi_module (hm_iface hmi) == owner
        && mi_iface_hash (mi_final_exts (hm_iface hmi)) == inventoryInterface inventory) ownerInterface) $
    fail "planned import refinement lacks its exact certified original interface"
  let flags = ms_hspp_opts (pm_mod_summary parsed)
      imports = if xopt ImplicitPrelude flags
          && not (any ((== mkModuleName "Prelude") . unLoc . ideclName . unLoc) originalImports)
        then noLocA (simpleImportDecl (mkModuleName "Prelude")) : originalImports
        else originalImports
      shadows = concatMap (\item -> exportHead item : exportChildren item) (plannedExports inventory)
      namespace TypeNamespace = TypeNamespace
      namespace ConstructorNamespace = ConstructorNamespace
      namespace _ = ValueNamespace
      key identity = (namespace (exportNamespace identity), exportOccurrence identity)
      shadowKeys = map key shadows
  refined <- fmap concat $ forM imports $ \located -> do
    let declaration = unLoc located
    if direct located || ideclQualified declaration /= NotQualified
      then pure [located]
      else do
        let importedName = unLoc (ideclName declaration)
            qualifier = renameRawPkgQual (hsc_unit_env env) importedName (ideclPkgQual declaration)
        resolved <- findImportedModule env importedName qualifier
        importedOwner <- case resolved of
          Found _ found -> pure found
          _ -> fail "planned import refinement cannot resolve an original import"
        iface <- initIfaceCheck (ppr importedName) env $
          loadInterface (ppr importedName) importedOwner (ImportByUser (ideclSource declaration))
        available <- case iface of
          Succeeded found -> pure (mi_exports found)
          Failed _ -> fail "planned import refinement cannot load an original import interface"
        selected <- either fail pure (selectedImportNames available (ideclImportList declaration))
        let retained = filter ((`notElem` shadowKeys) . key . exportIdentity) selected
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
