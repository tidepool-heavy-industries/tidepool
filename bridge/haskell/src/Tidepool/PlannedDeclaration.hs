-- | Compile the parser-owned declaration group under its reserved original
-- owner before the whole cell is checked. The checking module imports those
-- declarations; it never creates substitute Names for them.
module Tidepool.PlannedDeclaration
  ( PlannedDeclaration, plannedModule, plannedSource, plannedCheckPlan
  , preparePlannedDeclaration
  , PlannedDeclarationInventory, plannedExports, plannedInstances
  , certifyPlannedDeclaration
  ) where

import Control.Monad (unless)
import Data.Char (isSpace)
import Data.List (intercalate, isPrefixOf, nub, tails)
import Data.Text qualified as T
import Data.Text.Encoding qualified as TE
import GHC.Driver.Env (HscEnv, hsc_HPT, hsc_HUG, hscEPS, hsc_home_unit)
import GHC.Unit.Env (unitEnv_hpts)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Home.ModInfo (HomeModInfo(..), eltsHpt, lookupHpt)
import GHC.Unit.Module (mkModuleName, moduleNameString, moduleName, moduleUnit)
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Module.ModIface (mi_module, mi_src_hash)
import GHC.Unit.Types (unitString)
import GHC.Utils.Fingerprint (fingerprintByteString)
import GHC.Types.Name.Occurrence (isSymOcc, mkTcOcc, mkVarOcc)
import Tidepool.Binders
  ( CellSourcePlan(..), CellAnalysisItem(..), CellSourceSpan(..)
  , StmtBinders(..), TurnKind(..), ExportItem(..), LocatedImport(..)
  , SourcePrologue(..), DeclarationSource(..), renderDeclarationForTemplate )
import Tidepool.DeclarationJoin
  ( DeclarationExport(..), DeclarationKind(..), ExportIdentity(..)
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
  } deriving (Eq, Show)

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
            JoinAccepted -> Right (PlannedDeclarationInventory exports inventory)
            JoinRejected _ diagnostic -> Left diagnostic
