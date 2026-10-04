-- | Compile the parser-owned declaration group under its reserved original
-- owner before the whole cell is checked. The checking module imports those
-- declarations; it never creates substitute Names for them.
module Tidepool.PlannedDeclaration
  ( PlannedDeclaration, plannedModule, plannedSource, plannedCheckPlan
  , PlannedDeclarationRejection(..), preparePlannedDeclaration, replaceTemplateModuleHeader
  , PlannedDeclarationInventory, plannedExports, plannedInstances
  , plannedOriginalOwner, plannedInterfaceFingerprint, plannedSourceMatches, plannedFamilyClosure
  , renderPlannedDeclarationInventory
  , certifyPlannedDeclaration, hydratePlannedDeclarationInventory
  , transformPlannedDeclarationImports, transformPlannedDeclarationImportsWithCompleted
  , transformProgramDeclarationImports
  ) where

import Control.Monad (unless)
import Data.Char (isSpace)
import Data.List (intercalate, isPrefixOf, nub, sort, tails)
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import GHC (ParsedModule)
import GHC.Core.Coercion.Axiom (coAxiomName)
import GHC.Core.FamInstEnv (FamInst(..))
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_HUG, hscEPS, hsc_home_unit)
import GHC.Unit.Env (unitEnv_hpts)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt, lookupHpt)
import GHC.Unit.Module (Module, moduleName, moduleNameString, moduleUnit, mkModuleName)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Module.ModIface (mi_module, mi_src_hash, mi_iface_hash, mi_final_exts)
import GHC.Unit.Types (unitString)
import GHC.Utils.Fingerprint (Fingerprint, fingerprintByteString)
import GHC.Types.Name.Occurrence
  ( isSymOcc, mkTcOcc, mkVarOcc )
import Tidepool.CheckedPrefixImports
  ( CompletedValueImports, refineOriginalDeclarationImports
  , refineOriginalDeclarationImportsWithCompleted, refineProgramDeclarationImports )
import Tidepool.Binders
  ( CellSourcePlan(..), CellAnalysisItem(..), CellAnalysisSourceItem(..), CellSourceSpan(..)
  , StmtBinders(..), TurnKind(..), ExportItem(..), LocatedImport(..), ImportIntent(..)
  , SourcePrologue(..), DeclarationSource(..), renderDeclarationForTemplate )
import Tidepool.DeclarationJoin
  ( DeclarationExport(..), DeclarationKind(..), ExportIdentity(..), exportIdentity
  , InstanceInventory(..), ClassInstanceEvidence(..), JoinDecision(..)
  , interfaceExports, interfaceInventory, validateRetainedFamilyInstances
  , renderDeclarationSelection )
import Tidepool.Json (jsonString)
import Tidepool.Session
  ( SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString, scaffoldTargetName )

data PlannedDeclaration = PlannedDeclaration
  { plannedModule :: String
  , plannedSource :: String
  , plannedCheckPlan :: CellSourcePlan
  , authoredHeads :: [(DeclarationKind, String)]
  }

data PlannedDeclarationRejection
  = InvalidOriginalReservation
  | UnsupportedDeclarationOrder
  | InvalidTurnPlaceholder
  | ReservedResultDeclaration
  | InvalidOriginalHeader String
  | InvalidOriginalRendering String
  deriving (Eq, Show)

data PlannedDeclarationInventory = PlannedDeclarationInventory
  { plannedExports :: [DeclarationExport]
  , plannedInstances :: InstanceInventory
  , inventoryOwner :: Module
  , inventoryInterface :: Fingerprint
  , inventorySource :: Fingerprint
  , plannedFamilyClosure :: [ExportIdentity]
  } deriving (Eq)

instance Show PlannedDeclarationInventory where
  showsPrec precedence inventory = showsPrec precedence
    (unitString (moduleUnit (inventoryOwner inventory))
    ,moduleNameString (moduleName (inventoryOwner inventory))
    ,plannedExports inventory, plannedInstances inventory, inventoryInterface inventory)

plannedOriginalOwner :: PlannedDeclarationInventory -> (String, String)
plannedOriginalOwner inventory =
  (unitString (moduleUnit (inventoryOwner inventory))
  ,moduleNameString (moduleName (inventoryOwner inventory)))

plannedInterfaceFingerprint :: PlannedDeclarationInventory -> String
plannedInterfaceFingerprint = show . inventoryInterface

plannedSourceMatches :: PlannedDeclaration -> PlannedDeclarationInventory -> Bool
plannedSourceMatches planned inventory = plannedModule planned == snd (plannedOriginalOwner inventory)
  && inventorySource inventory == fingerprintByteString (TE.encodeUtf8 (T.pack (plannedSource planned)))

renderPlannedDeclarationInventory :: PlannedDeclarationInventory -> String
renderPlannedDeclarationInventory inventory = "{" ++ intercalate ","
  [ jsonString key ++ ":" ++ value
  | (key, value) <-
    [("original_unit", jsonString unit), ("original_module", jsonString owner)
    ,("interface_fingerprint", jsonString (plannedInterfaceFingerprint inventory))
    ,("selection", renderDeclarationSelection (plannedExports inventory)
      (plannedInstances inventory) (plannedFamilyClosure inventory))]
  ] ++ "}"
  where (unit, owner) = plannedOriginalOwner inventory

-- The wrapper and module reservation are frozen by the caller's offer. The
-- plan comes from the existing cell parser; source ordinals remain intact in
-- the checking plan even though the original declarations live elsewhere.
preparePlannedDeclaration
  :: String -> String -> CellSourcePlan -> Either PlannedDeclarationRejection PlannedDeclaration
preparePlannedDeclaration reserved wrapper plan = do
  owner <- maybe (Left InvalidOriginalReservation) Right
    (parseSessionModule reserved)
  unless (smKind owner == LibMod && sessionModuleString owner == reserved)
    (Left InvalidOriginalReservation)
  declaration <- case cellPlanItems plan of
    item : rest
      | sbKind (cellAnalysisVerdict item) == KDecl
      , all ((/= KDecl) . sbKind . cellAnalysisVerdict) rest
      , and [cellAnalysisSourceOrdinal declaration < cellAnalysisSourceOrdinal executable
            | declaration <- cellAnalysisSourceItems item
            , executable <- concatMap cellAnalysisSourceItems rest] -> Right item
    _ -> Left UnsupportedDeclarationOrder
  unless (occurrences "{{TURN}}" wrapper == 1)
    (Left InvalidTurnPlaceholder)
  let exports = nub (sbDeclItems (cellAnalysisVerdict declaration))
      heads = nub (map exportHead exports)
  unless (all ((/= scaffoldTargetName) . snd) heads)
    (Left ReservedResultDeclaration)
  let header = "module " ++ reserved ++ " (" ++ intercalate ", " (map renderExport exports) ++ ") where"
  originalWrapper <- either (Left . InvalidOriginalHeader) Right
    (replaceTemplateModuleHeader header wrapper)
  source <- either (Left . InvalidOriginalRendering) Right
    (renderDeclarationForTemplate originalWrapper
      (DeclarationSource (cellPlanPrologue plan) (cellAnalysisSource declaration)))
  let cleared item
        | sbKind (cellAnalysisVerdict item) == KDecl = item {cellAnalysisSource = ""}
        | otherwise = item
      prologue = cellPlanPrologue plan
      check = plan
        { cellPlanItems = map cleared (cellPlanItems plan)
        , cellPlanPrologue = prologue
            { prologueImports = prologueImports prologue ++
                [LocatedImport (CellSourceSpan 1 1 1 1) ("import " ++ reserved) RetainedGeneratedImport] }
        , cellPlanDeclarationBase = ""
        , cellPlanGenericDeclarations = []
        , cellPlanStructuralDisplayDeclarations = ""
        , cellPlanStructuralDisplayTargets = []
        }
  pure (PlannedDeclaration reserved source check heads)
  where
    occurrences needle = length . filter (needle `isPrefixOf`) . tails
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

-- Generated original and checking wrappers share one strict header renderer.
-- Their module owners are selected by the compiler, independently of the
-- source declarations whose GHC Names the original module certifies.
replaceTemplateModuleHeader :: String -> String -> Either String String
replaceTemplateModuleHeader header source = case
    [index | (index, line) <- zip [0 :: Int ..] (lines source)
      , "module " `isPrefixOf` dropWhile isSpace line] of
  [index] -> Right (unlines
    [if position == index then header else line
    | (position, line) <- zip [0 :: Int ..] (lines source)])
  _ -> Left "compiler wrapper requires one module header"

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
        sourceHash = fingerprintByteString (TE.encodeUtf8 (T.pack (plannedSource planned)))
    if moduleNameString (moduleName owner) /= plannedModule planned
        || moduleUnit owner /= homeUnitAsUnit (hsc_home_unit env)
        || mi_src_hash iface /= sourceHash
      then pure (Left "planned original declaration owner or source differs")
      else do
        result <- readPlannedDeclarationInventory env original
        pure $ do
          inventory <- result
          let heads = nub [(exportKind item, exportOccurrence (exportHead item))
                | item <- plannedExports inventory]
          unless (length heads == length (authoredHeads planned)
              && all (`elem` authoredHeads planned) heads)
            (Left "planned original declaration exports differ from parser-owned heads")
          Right inventory

-- A later target recipe receives the original owner and final interface
-- fingerprint from its closed declaration authorization. Read the same
-- already hydrated original; neither source replay nor rendered names can
-- stand in for its exact interface identity.
hydratePlannedDeclarationInventory
  :: (String, String) -> String -> HscEnv
  -> IO (Either String PlannedDeclarationInventory)
hydratePlannedDeclarationInventory expectedOwner expectedFingerprint env =
  case parseSessionModule (snd expectedOwner) of
    Just owner | smKind owner == LibMod && sessionModuleString owner == snd expectedOwner ->
      case lookupHpt (hsc_HPT env) (mkModuleName (snd expectedOwner)) of
        Nothing -> pure (Left "planned original declaration interface is absent")
        Just original -> do
          let iface = hm_iface original
              ownerModule = mi_module iface
              actualOwner = (unitString (moduleUnit ownerModule)
                ,moduleNameString (moduleName ownerModule))
          if actualOwner /= expectedOwner
              || moduleUnit ownerModule /= homeUnitAsUnit (hsc_home_unit env)
              || show (mi_iface_hash (mi_final_exts iface)) /= expectedFingerprint
            then pure (Left "planned original declaration owner or interface differs")
            else readPlannedDeclarationInventory env original
    _ -> pure (Left "planned declaration requires a canonical original Lib owner")

readPlannedDeclarationInventory
  :: HscEnv -> HomeModInfo -> IO (Either String PlannedDeclarationInventory)
readPlannedDeclarationInventory env original = do
  let iface = hm_iface original
      owner = mi_module iface
      unit = unitString (moduleUnit owner)
      ownerName = moduleNameString (moduleName owner)
      owns identity = exportUnit identity == unit && exportModule identity == ownerName
  exports <- interfaceExports env iface
  instances <- interfaceInventory env iface
  eps <- hscEPS env
  let families = concat
        [md_fam_insts (hm_details hmi)
        | hpt <- unitEnv_hpts (hsc_HUG env), hmi <- eltsHpt hpt]
      exactExports = all (\item -> owns (exportHead item)
        && exportOccurrence (exportHead item) /= scaffoldTargetName
        && all owns (exportChildren item)) exports
      familyCheck = validateRetainedFamilyInstances (eps_fam_inst_env eps) families
  pure $ do
    unless exactExports (Left "planned original declaration exports have a foreign or compiler owner")
    inventory <- instances
    unless (all (owns . instanceDfun) (inventoryClasses inventory)
        && all owns (inventoryFamilies inventory)
        && all (all owns . instanceSelectedAxioms) (inventoryClasses inventory))
      (Left "planned original instance inventory has a foreign owner")
    case familyCheck of
      JoinAccepted -> Right (PlannedDeclarationInventory exports inventory owner
        (mi_iface_hash (mi_final_exts iface)) (mi_src_hash iface)
        (sort (nub (map (exportIdentity . coAxiomName . fi_axiom) families))))
      JoinRejected _ diagnostic -> Left diagnostic

-- The checked recipe gives the certified original authored names lexical
-- precedence while preserving each historical qualified import selection.
transformPlannedDeclarationImports
  :: PlannedDeclarationInventory -> HscEnv -> ParsedModule -> IO ParsedModule
transformPlannedDeclarationImports inventory = refineOriginalDeclarationImports
  (inventoryOwner inventory) (inventoryInterface inventory)
  (concatMap (\item -> exportHead item : exportChildren item) (plannedExports inventory))

transformPlannedDeclarationImportsWithCompleted
  :: PlannedDeclarationInventory -> CompletedValueImports -> HscEnv -> ParsedModule -> IO ParsedModule
transformPlannedDeclarationImportsWithCompleted inventory = refineOriginalDeclarationImportsWithCompleted
  (inventoryOwner inventory) (inventoryInterface inventory)
  (concatMap (\item -> exportHead item : exportChildren item) (plannedExports inventory))

transformProgramDeclarationImports
  :: [PlannedDeclarationInventory] -> Maybe CompletedValueImports
  -> HscEnv -> ParsedModule -> IO ParsedModule
transformProgramDeclarationImports inventories = refineProgramDeclarationImports
  [(inventoryOwner inventory,inventoryInterface inventory,
      map (\item -> exportHead item : exportChildren item) (plannedExports inventory))
    | inventory <- inventories]
