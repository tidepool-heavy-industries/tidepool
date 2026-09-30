-- | Refine lexical imports only from the original declaration inventory or
-- completed-prefix value interfaces read and installed in this request.
module Tidepool.CheckedPrefixImports
  ( CompletedValueImport(..), CompletedValueImports
  , hydrateCompletedValueImports, transformCompletedValueImports
  , refineOriginalDeclarationImports
  ) where

import Control.Exception (evaluate)
import Control.Monad (forM, forM_, unless)
import Data.List (nub, sort)
import Data.Word (Word64)
import GHC (ParsedModule(..), ModSummary(ms_hspp_opts))
import GHC.Data.Maybe (MaybeErr(..))
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_unit_env)
import GHC.Driver.Session (xopt)
import GHC.Hs
import GHC.Iface.Load (loadInterface, WhereFrom(..))
import GHC.LanguageExtensions.Type (Extension(ImplicitPrelude))
import GHC.Rename.Names (renameRawPkgQual)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Types.Avail (AvailInfo, availName, availNames)
import GHC.Types.Name (Name, nameOccName)
import GHC.Types.Name.Occurrence (isTcOcc, isDataOcc, occNameString)
import GHC.Types.Name.Reader (mkRdrUnqual, rdrNameOcc)
import GHC.Types.PkgQual (RawPkgQual(..))
import GHC.Types.SrcLoc (GenLocated(..), unLoc)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), lookupHpt)
import GHC.Unit.Module (Module, moduleName, mkModuleName)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_iface_hash, mi_final_exts, mi_exports)
import GHC.Unit.Finder (FindResult(..), findImportedModule)
import GHC.Utils.Fingerprint (Fingerprint)
import GHC.Utils.Outputable (ppr)
import System.Mem.StableName (StableName, makeStableName)
import Tidepool.DeclarationJoin (ExportIdentity(..), ExportNamespace(..), exportIdentity)
import Tidepool.ExactHydration (ExactIfaceArtifact(..), readExactIfaceArtifacts, hydrateExactScope)
import Tidepool.Identity (stableVarId)
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule,
  sessionModuleString, scaffoldTargetName)

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
hydrateCompletedValueImports requested env
  | any invalidOwner requested = pure (Left "completed values require canonical Val owners and nonempty selections")
  | length winners /= length (nub winners) = pure (Left "completed value selections contain duplicate lexical winners")
  | otherwise = do
      decoded <- readExactIfaceArtifacts env artifacts
      case decoded of
        Left diagnostic -> pure (Left diagnostic)
        Right interfaces -> do
          case sequence (zipWith selectedNames requested interfaces) of
            Left diagnostic -> pure (Left diagnostic)
            Right selections -> do
              hydrated <- hydrateExactScope env interfaces
              owners <- forM (zip interfaces selections) $ \((_, iface), names) -> do
                forced <- evaluate iface
                stable <- makeStableName forced
                pure (RefinementOwner (mi_module forced) (mi_iface_hash (mi_final_exts forced))
                  (map exportIdentity names) (CompletedValues names) (Just (forced, stable)))
              pure (Right (hydrated, CompletedValueImports owners))
  where
    invalidOwner value = null (completedValueBinders value)
      || any ((== scaffoldTargetName) . fst) (completedValueBinders value)
      || case parseSessionModule (completedValueModule value) of
        Just owner -> smKind owner /= ValMod || sessionModuleString owner /= completedValueModule value
        Nothing -> True
    winners = concatMap (map fst . completedValueBinders) requested
    artifacts = [ExactIfaceArtifact (completedValueUnit value) (completedValueModule value)
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
transformCompletedValueImports (CompletedValueImports owners) = refineImports owners

-- Planned declaration certification supplies the full authored-only original
-- export inventory. Keep its direct wildcard import contract distinct from the
-- completed value owner's exact selected import while sharing one AST policy.
refineOriginalDeclarationImports
  :: Module -> Fingerprint -> [ExportIdentity] -> HscEnv -> ParsedModule -> IO ParsedModule
refineOriginalDeclarationImports owner fingerprint names env parsed = do
  original <- currentOwner env owner fingerprint
  unless (sort (nub names) == sort (nub (map exportIdentity
      (concatMap availNames (mi_exports (hm_iface original)))))) $
    fail "planned import refinement lacks its complete original export inventory"
  refineImports [RefinementOwner owner fingerprint names OriginalDeclarations Nothing] env parsed

currentOwner :: HscEnv -> Module -> Fingerprint -> IO HomeModInfo
currentOwner env owner fingerprint = case lookupHpt (hsc_HPT env) (moduleName owner) of
  Just original | mi_module (hm_iface original) == owner
      && mi_iface_hash (mi_final_exts (hm_iface original)) == fingerprint -> pure original
  _ -> fail "checked import refinement lacks its exact original interface"

refineImports :: [RefinementOwner] -> HscEnv -> ParsedModule -> IO ParsedModule
refineImports owners env parsed = do
  let syntax = unLoc (pm_parsed_source parsed)
      originalImports = hsmodImports syntax
      direct owner imported = let declaration = unLoc imported in
        unLoc (ideclName declaration) == moduleName (refinementOwner owner)
          && ideclQualified declaration == NotQualified
          && ideclAs declaration == Nothing
          && (case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
          && ideclSource declaration == NotBoot
  forM_ owners $ \owner -> do
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
  let flags = ms_hspp_opts (pm_mod_summary parsed)
      imports = if xopt ImplicitPrelude flags
          && not (any ((== mkModuleName "Prelude") . unLoc . ideclName . unLoc) originalImports)
        then noLocA (simpleImportDecl (mkModuleName "Prelude")) : originalImports
        else originalImports
      namespace TypeNamespace = TypeNamespace
      namespace ConstructorNamespace = ConstructorNamespace
      namespace _ = ValueNamespace
      key identity = (namespace (exportNamespace identity), exportOccurrence identity)
      shadowKeys = map key (concatMap refinementNames owners)
  refined <- fmap concat $ forM imports $ \located -> do
    let declaration = unLoc located
    if any (`direct` located) owners || ideclQualified declaration /= NotQualified
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
