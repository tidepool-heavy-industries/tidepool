{-# LANGUAGE PatternSynonyms #-}
{-# LANGUAGE TypeApplications #-}

module Tidepool.ExactHydration
  ( ExactIfaceArtifact(..)
  , freshExactState
  , readExactIfaceArtifacts
  , hydrateExactScope
  , CheckedValueImportAuthority
  , noCheckedValueImports
  , readCheckedValueImportAuthority
  , VerifiedExactIfaceClosure
  , readVerifiedExactIfaceClosure
  , readVerifiedExactIfaceClosureWithCheckedValues
  , selectVerifiedExactInterfaces
  , selectVerifiedValueInterfaces
  , checkedValueImportAuthorityFromVerified
  , GeneratedScaffoldRecipe, generatedScaffoldRecipe, captureGeneratedScaffoldTarget
  , GeneratedScaffoldImportAuthority, noGeneratedScaffoldImports, readGeneratedScaffoldImportAuthority
  , installExactLexicalGraphWithScaffold
  , installExactLexicalGraph
  ) where

import Tidepool.Timing (readTimingEnabled, emitCount)
import Tidepool.Session (SessionModule(..), SessionModuleKind(..), parseSessionModule, sessionModuleString)
import Control.Monad (forM, forM_, unless)
import Control.Exception
  ( IOException, SomeException, SomeAsyncException, bracket, try, fromException, throwIO )
import Data.Char (isHexDigit, toLower)
import Data.Maybe (isJust)
import qualified Data.ByteString as BS
import qualified Crypto.Hash.SHA256 as SHA256
import GHC.Driver.Env
  ( HscEnv(..), hscUpdateHPT_lazy, hsc_home_unit, hsc_HPT, discardIC )
import qualified GHC.Linker.Loader as Linker
import GHC.Unit.Env (UnitEnv(..), HomeUnitEnv(..))
import GHC.Unit.External (initExternalUnitCache)
import GHC.Unit.Finder (initFinderCache)
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Driver.Env.KnotVars (emptyKnotVars)
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), emptyHomeModInfoLinkable, emptyHomePackageTable, addToHpt
  , lookupHpt )
import GHC.Iface.Load (readIface)
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Unit.Module (Module, moduleName, moduleUnit, moduleNameString, mkModule, mkModuleName)
import GHC.Unit.Module.Graph
  ( ModuleGraph, ModuleGraphNode(..), NodeKey(..), ModNodeKeyWithUid(..)
  , mgModSummaries', mkModuleGraph )
import GHC.Unit.Module.Location
  ( pattern ModLocation
  , ml_hs_file, ml_hi_file, ml_dyn_hi_file, ml_obj_file, ml_dyn_obj_file, ml_hie_file )
import GHC.Unit.Module.ModSummary (ModSummary(..))
import GHC.Types.SourceFile (HscSource(..))
import GHC.Types.PkgQual (PkgQual(..), RawPkgQual(..))
import GHC.Types.SrcLoc (unLoc, getLoc, SrcSpan(..), srcSpanStartLine)
import GHC.Types.Avail (availNames)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import Tidepool.ExecutionSource (ExecutionSourceIdentity(..))
import Tidepool.DependencyEvidence (sourceEvidenceWithFingerprint, dependencySourceSha256)
import qualified Data.Text as Text
import qualified Data.Text.Encoding as TextEncoding
import GHC (ParsedModule(..))
import GHC.Parser.Annotation (getLocA)
import Language.Haskell.Syntax (HsModule(..))
import GHC.Hs (ImportDecl(..), ImportDeclQualifiedStyle(..))
import GHC.Unit.Module.Deps (dep_orphs, dep_finsts)
import GHC.Unit.Home (homeUnitId, isHomeUnit)
import GHC.Unit.Types (GenWithIsBoot(..))
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import GHC.Utils.Fingerprint (fingerprintByteString)
import GHC.Unit.Module.ModIface (ModIface, mi_module, mi_extra_decls, mi_exports, mi_insts, mi_fam_insts, mi_deps)
import GHC.Unit.Types (unitString, stringToUnit)
import qualified GHC.Data.Maybe as MErr
import GHC.Utils.Outputable (text)
import Numeric (showHex)
import System.Directory (getTemporaryDirectory, removeFile, canonicalizePath)
import GHC.Fingerprint.Type (Fingerprint)
import System.IO (fixIO)
import System.IO (hClose, hIsClosed, openBinaryTempFile)
import qualified Data.Set as Set
import qualified Data.Map.Strict as Map

-- An artifact is advisory until its bytes and GHC module identity have been
-- checked in the transaction that consumes it. Requirements cover the exact
-- implementation closure and are checked by the caller before hydration.
data ExactIfaceArtifact = ExactIfaceArtifact
  { exactUnit :: String
  , exactModule :: String
  , exactPath :: FilePath
  , exactSha256 :: String
  , exactRequirements :: [(String, String)]
  } deriving (Eq, Show)

-- Checked value modules are injected in dependency order after the lexical
-- graph is installed. Their verified identities authorize source imports at
-- that earlier boundary without exposing implementation-only originals.
newtype CheckedValueImportAuthority = CheckedValueImportAuthority (Set.Set (String, String))

newtype VerifiedExactIfaceClosure = VerifiedExactIfaceClosure
  (Map.Map (String, String) (ExactIfaceArtifact, ModIface, [ExactIfaceArtifact]))

readVerifiedExactIfaceClosure
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosure env artifacts = fmap (VerifiedExactIfaceClosure . Map.fromList
  . map (\(artifact,iface) -> ((exactUnit artifact,exactModule artifact),(artifact,iface,[artifact]))))
  <$> readExactIfaceArtifacts env artifacts

readVerifiedExactIfaceClosureWithCheckedValues
  :: HscEnv -> [ExactIfaceArtifact] -> [ExactIfaceArtifact]
  -> IO (Either String VerifiedExactIfaceClosure)
readVerifiedExactIfaceClosureWithCheckedValues env originals values
  | any (not . checkedValueOwner) values = pure (Left "checked value import has another owner")
  | length (map key values) /= Set.size (Set.fromList (map key values)) =
      pure (Left "duplicate checked value owner")
  | otherwise = do
      let originalKeys = Set.fromList (map key originals)
          inputs = originals ++ [value | value <- values, key value `Set.notMember` originalKeys]
      verified <- readVerifiedExactIfaceClosure env inputs
      case verified of
        Left reason -> pure (Left reason)
        Right (VerifiedExactIfaceClosure captured) -> do
          aliases <- forM values $ \value -> case Map.lookup (key value) captured of
            Just (original,iface,known)
              | value `elem` known -> pure (Right (key value,(original,iface,known)))
              | exactSha256 value == exactSha256 original
              , exactRequirements value == [] || exactRequirements value == exactRequirements original -> do
                  -- The closed original supplies the type dependency proof.
                  -- Verify this protected value's separate capture before its
                  -- path becomes an alias for those exact original bytes.
                  alias <- readOne env value
                  pure ((\_ -> (key value,(original,iface,value:known))) <$> alias)
            _ -> pure (Left "checked value alias conflicts with its captured original")
          pure $ do
            additions <- sequence aliases
            Right (VerifiedExactIfaceClosure (Map.union (Map.fromList additions) captured))
  where key artifact = (exactUnit artifact,exactModule artifact)

-- Requirements are validated in the complete captured closure. A later
-- dependency-ordered injection may select one value whose type mentions
-- another original without rereading or weakening that original proof.
selectVerifiedExactInterfaces
  :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact]
  -> Either String [(ExactIfaceArtifact, ModIface)]
selectVerifiedExactInterfaces (VerifiedExactIfaceClosure captured) artifacts =
  mapM select artifacts
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (original,iface,_) | original == artifact -> Right (original,iface)
      _ -> Left "requested exact interface differs from its verified closure"

-- Completed-value wire inputs bind bytes and selected exported Names. Their
-- type requirements come from this complete verified closure, including
-- worker-issued values created earlier in the same compiled cell.
selectVerifiedValueInterfaces
  :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact]
  -> Either String [(ExactIfaceArtifact, ModIface)]
selectVerifiedValueInterfaces (VerifiedExactIfaceClosure captured) artifacts = mapM select artifacts
  where
    select artifact = case Map.lookup (exactUnit artifact,exactModule artifact) captured of
      Just (original,iface,aliases)
        | checkedValueOwner artifact
        , any (\alias -> exactPath alias == exactPath artifact
            && exactSha256 alias == exactSha256 artifact) aliases
        , exactRequirements artifact == [] || exactRequirements artifact == exactRequirements original ->
            Right (original,iface)
      _ -> Left "completed value input differs from its verified closure"

checkedValueImportAuthorityFromVerified
  :: VerifiedExactIfaceClosure -> [ExactIfaceArtifact]
  -> Either String CheckedValueImportAuthority
checkedValueImportAuthorityFromVerified closure artifacts
  | any (not . checkedValueOwner) artifacts = Left "checked value import has another owner"
  | otherwise = CheckedValueImportAuthority . Set.fromList
      . map (\(artifact, _) -> (exactUnit artifact, exactModule artifact))
      <$> selectVerifiedValueInterfaces closure artifacts

checkedValueOwner :: ExactIfaceArtifact -> Bool
checkedValueOwner artifact = exactUnit artifact == "main" && case parseSessionModule (exactModule artifact) of
  Just owner -> smKind owner == ValMod && sessionModuleString owner == exactModule artifact
  Nothing -> False

noCheckedValueImports :: CheckedValueImportAuthority
noCheckedValueImports = CheckedValueImportAuthority Set.empty

-- A protected turn producer records the compiler's import before authored
-- bytes are inserted. The resulting capability belongs to one rendered
-- target, not to the support module's name or to the surrounding scope.
data GeneratedScaffoldRecipe = GeneratedScaffoldRecipe FilePath String BS.ByteString Int
  deriving (Eq)

instance Show GeneratedScaffoldRecipe where
  show (GeneratedScaffoldRecipe path name _ line) =
    "GeneratedScaffoldRecipe " ++ show (path,name,line)

generatedScaffoldRecipe :: String -> String -> FilePath -> String
  -> IO (Either String GeneratedScaffoldRecipe)
generatedScaffoldRecipe protectedTemplate rendered path name = do
  canonical <- canonicalizePath path
  let compilerImport = "import qualified Tidepool.Internal.Resume as TidepoolResume"
      occurrences text' = [line | (line,textLine) <- zip [1..] (lines text'), textLine == compilerImport]
      bytes = TextEncoding.encodeUtf8 (Text.pack rendered)
  pure $ case (occurrences protectedTemplate,occurrences rendered) of
    ([_],[line]) -> Right (GeneratedScaffoldRecipe canonical name bytes line)
    _ -> Left "generated scaffold import is missing or duplicated"

captureGeneratedScaffoldTarget :: GeneratedScaffoldRecipe -> FilePath -> IO (Either String BS.ByteString)
captureGeneratedScaffoldTarget (GeneratedScaffoldRecipe path _ expected _) requested = do
  canonical <- canonicalizePath requested
  actual <- BS.readFile canonical
  pure $ if canonical == path && actual == expected then Right expected
    else Left "generated scaffold target differs from its protected recipe"

data GeneratedScaffoldImportAuthority = GeneratedScaffoldImportAuthority
  [(Module,Fingerprint,ExecutionSourceIdentity,SrcSpan,ExactIfaceArtifact)]

noGeneratedScaffoldImports :: GeneratedScaffoldImportAuthority
noGeneratedScaffoldImports = GeneratedScaffoldImportAuthority []

readGeneratedScaffoldImportAuthority :: VerifiedExactIfaceClosure -> [ExecutionSourceIdentity]
  -> GeneratedScaffoldRecipe -> ParsedModule -> ModuleGraph -> HscEnv
  -> IO (Either String GeneratedScaffoldImportAuthority)
readGeneratedScaffoldImportAuthority (VerifiedExactIfaceClosure captured) nativeOwners
    recipe@(GeneratedScaffoldRecipe path target expected line) parsed sourceGraph env = do
  checked <- captureGeneratedScaffoldTarget recipe path
  (source,fingerprint) <- sourceEvidenceWithFingerprint path
  targetPaths <- forM [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph
      , moduleNameString (moduleName (ms_mod summary)) == target] $ \summary ->
    traverse canonicalizePath (ml_hs_file (ms_location summary))
  pure $ do
    _ <- checked
    unless (dependencySourceSha256 source == hexBytes (SHA256.hash expected))
      (Left "generated scaffold source changed during admission")
    summary <- case [summary | ModuleNode _ summary <- mgModSummaries' sourceGraph
        , moduleNameString (moduleName (ms_mod summary)) == target] of
      [summary] | ms_hs_hash summary == fingerprint && targetPaths == [Just path]
          && ms_mod summary == mkModule (stringToUnit (unitString (homeUnitId (hsc_home_unit env)))) (mkModuleName target)
          && ms_mod (pm_mod_summary parsed) == ms_mod summary
          && ms_hs_hash (pm_mod_summary parsed) == fingerprint -> Right summary
      _ -> Left "generated scaffold target summary differs from its protected source"
    let support = mkModule (stringToUnit (unitString (homeUnitId (hsc_home_unit env))))
          (mkModuleName "Tidepool.Internal.Resume")
        key = (unitString (moduleUnit support),moduleNameString (moduleName support))
    if any (\case ModuleNode _ loaded -> ms_mod loaded == support; _ -> False)
        (mgModSummaries' sourceGraph)
      then Right noGeneratedScaffoldImports
      else do
        (artifact,iface,_) <- maybe (Left "generated scaffold lacks its verified support interface") Right
          (Map.lookup key captured)
        native <- case [owner | owner <- nativeOwners,
            (executionUnit owner,executionModule owner) == key
            && executionIfaceSha256 owner == exactSha256 artifact] of
          [owner] -> Right owner
          _ -> Left "generated scaffold lacks one paired original native owner"
        unless (mi_module iface == support)
          (Left "generated scaffold lacks a paired original native owner")
        let hiddenHomeWitnesses = filter (isHomeUnit (hsc_home_unit env) . moduleUnit)
              (dep_orphs (mi_deps iface) ++ dep_finsts (mi_deps iface))
        unless (null (exactRequirements artifact) && null (mi_insts iface) && null (mi_fam_insts iface)
            && null hiddenHomeWitnesses)
          (Left "generated scaffold support introduces home instance or family dependencies")
        let exports = concatMap availNames (mi_exports iface)
        unless (all (\occurrence -> any (\name -> nameModule_maybe name == Just support
            && occNameString (nameOccName name) == occurrence) exports) ["settle","resumeLifted"])
          (Left "generated scaffold support has another export owner")
        imported <- case [name | (NoPkgQual,name) <- ms_textual_imps summary
            , unLoc name == moduleName support
            , case getLoc name of RealSrcSpan span' _ -> srcSpanStartLine span' == line; _ -> False] of
          [name] -> Right name
          _ -> Left "generated scaffold import occurrence differs from its protected recipe"
        declaration <- case [unLoc located | located <- hsmodImports (unLoc (pm_parsed_source parsed))
            , getLocA (ideclName (unLoc located)) == getLoc imported] of
          [declaration] -> Right declaration
          _ -> Left "generated scaffold parsed import differs from its captured occurrence"
        unless (ideclQualified declaration == QualifiedPre
            && fmap unLoc (ideclAs declaration) == Just (mkModuleName "TidepoolResume")
            && ideclSource declaration == NotBoot && ideclImportList declaration == Nothing
            && case ideclPkgQual declaration of NoRawPkgQual -> True; _ -> False)
          (Left "generated scaffold parsed import differs from its protected shape")
        pure (GeneratedScaffoldImportAuthority [(ms_mod summary,fingerprint,native,getLoc imported,artifact)])

readCheckedValueImportAuthority
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String CheckedValueImportAuthority)
readCheckedValueImportAuthority env artifacts
  | any (not . checkedValueOwner) artifacts = pure (Left "checked value import has another owner")
  | otherwise = do
      verified <- readVerifiedExactIfaceClosure env artifacts
      pure (verified >>= (`checkedValueImportAuthorityFromVerified` artifacts))

-- The resident GHC process survives requests, but its mutable package and
-- home interface tables must not carry visibility across lexical scopes.
freshExactState :: HscEnv -> IO HscEnv
freshExactState env = do
  -- Extraction uses NoLink, so GHC's make driver does not unload splice
  -- executables. Reset those home symbols with the same loader protocol
  -- before another lexical scope can supply the same module identity.
  let cleared = discardIC env
  forM_ (hsc_interp cleared) $ \interp -> Linker.unload interp cleared []
  eps <- initExternalUnitCache
  finder <- initFinderCache
  let units = hsc_unit_env env
      homes = fmap (\home -> home { homeUnitEnv_hpt = emptyHomePackageTable })
        (ue_home_unit_graph units)
  pure cleared
    { hsc_FC = finder
    , hsc_targets = []
    , hsc_mod_graph = mkModuleGraph []
    , hsc_type_env_vars = emptyKnotVars
    , hsc_unit_env = units { ue_eps = eps, ue_home_unit_graph = homes }
    }

-- Reading every interface before installing any of them prevents a corrupt
-- member of an implementation SCC from partially mutating the HPT.
readExactIfaceArtifacts
  :: HscEnv -> [ExactIfaceArtifact] -> IO (Either String [(ExactIfaceArtifact, ModIface)])
readExactIfaceArtifacts env artifacts
  | Set.size moduleNames /= length artifacts = pure (Left "duplicate exact interface owner")
  | any (\artifact -> length (exactSha256 artifact) /= 64
      || not (all isHexDigit (exactSha256 artifact))) artifacts =
      pure (Left "invalid exact interface digest")
  | any (\artifact -> any (`Set.notMember` owners) (exactRequirements artifact)) artifacts =
      pure (Left "incomplete exact interface dependency closure")
  | otherwise = sequence <$> forM artifacts (readOne env)
  where
    moduleNames = Set.fromList (map exactModule artifacts)
    owners = Set.fromList [(exactUnit artifact, exactModule artifact) | artifact <- artifacts]

readOne :: HscEnv -> ExactIfaceArtifact -> IO (Either String (ExactIfaceArtifact, ModIface))
readOne env artifact = do
  timing <- readTimingEnabled
  emitCount timing ("exact_iface_read_calls." ++ exactModule artifact) 1
  readResult <- try (BS.readFile (exactPath artifact)) :: IO (Either IOException BS.ByteString)
  case readResult of
    Left _ -> pure (Left ("interface unavailable: " ++ exactModule artifact))
    Right bytes -> do
      emitCount timing ("exact_iface_read_bytes." ++ exactModule artifact) (fromIntegral (BS.length bytes))
      readVerified timing bytes
  where
   readVerified timing bytes =
    if hexBytes (SHA256.hash bytes) /= map toLower (exactSha256 artifact)
    then pure (Left ("interface digest mismatch: " ++ exactModule artifact))
    else do
      -- Decode the exact bytes that passed the digest check. Reading the
      -- candidate path again would admit a different interface between the
      -- hash and GHC's decoder, even if a later revalidation observed the
      -- original bytes restored.
      let owner = mkModule (stringToUnit (exactUnit artifact))
            (mkModuleName (exactModule artifact))
      decoded <- try @SomeException (withCapturedIface timing (exactModule artifact) bytes $ \path -> do
        emitCount timing ("exact_iface_decode_reads." ++ exactModule artifact) 1
        readIface (hsc_dflags env) (hsc_NC env) owner path)
      case decoded of
        Left failure -> case fromException failure :: Maybe SomeAsyncException of
          Just async -> throwIO async
          Nothing -> pure (Left ("interface read failed: " ++ exactModule artifact))
        Right result -> pure $ case result of
          MErr.Failed _ -> Left ("interface read failed: " ++ exactModule artifact)
          MErr.Succeeded iface
            | unitString (moduleUnit (mi_module iface)) /= exactUnit artifact
                || moduleNameString (moduleName (mi_module iface)) /= exactModule artifact ->
                Left ("interface owner mismatch: " ++ exactModule artifact)
            | isJust (mi_extra_decls iface) ->
                Left ("interface contains defining Core: " ++ exactModule artifact)
            | otherwise -> Right (artifact, iface)

withCapturedIface :: Bool -> String -> BS.ByteString -> (FilePath -> IO a) -> IO a
withCapturedIface timing owner bytes consume = do
  directory <- getTemporaryDirectory
  bracket (openBinaryTempFile directory "tidepool-exact-iface.hi")
    (\(path, handle) -> do
      closed <- hIsClosed handle
      unless closed (hClose handle)
      removeFile path)
    (\(path, handle) -> do
      BS.hPut handle bytes
      emitCount timing ("exact_iface_capture_write_bytes." ++ owner) (fromIntegral (BS.length bytes))
      hClose handle
      consume path)

-- GHC's home-interface knot allows mutually recursive source/boot modules to
-- resolve each other's original Names while typechecking their details.
hydrateExactScope
  :: HscEnv -> [(ExactIfaceArtifact, ModIface)] -> IO HscEnv
hydrateExactScope env loaded = do
  details <- fixIO $ \recursiveDetails -> do
    let knotted = withDetails recursiveDetails
    forM loaded $ \(_, iface) ->
      initIfaceCheck (text "tidepool exact hydration") knotted (typecheckIface iface)
  pure (withDetails details)
  where
    withDetails details = hscUpdateHPT_lazy (\hpt -> foldr
      (\(iface, detail) table -> addToHpt table (moduleName (mi_module iface))
        (HomeModInfo iface detail emptyHomeModInfoLinkable))
      hpt (zipDetails loaded details)) env
    -- The loaded interface spine is available before fixIO returns. Ordinary
    -- zip would demand the recursive detail spine while building the HPT;
    -- sharing a deferred head/tail split keeps the knot lazy and traversal linear.
    zipDetails [] _ = []
    zipDetails ((_, iface) : rest) remaining =
      let ~(detail, tailDetails) = splitDetails remaining
      in (iface, detail) : zipDetails rest tailDetails
    splitDetails (detail : rest) = (detail, rest)
    splitDetails [] = error "exact hydration detail arity mismatch"

-- A lexical interface contributes its chosen instance/family environment to
-- GHC's graph traversal. Implementation-only HMIs remain installed but never
-- appear in this graph; their original Names can still resolve through HPT.
-- The caller supplies virtual-to-virtual edges, never implementation edges.
installExactLexicalGraph
  :: ModuleGraph -> [(ExactIfaceArtifact, [(String, String)])]
  -> CheckedValueImportAuthority -> HscEnv
  -> IO (Either String HscEnv)
installExactLexicalGraph sourceGraph lexical checkedValues =
  installExactLexicalGraphWithScaffold sourceGraph lexical checkedValues noGeneratedScaffoldImports

installExactLexicalGraphWithScaffold
  :: ModuleGraph -> [(ExactIfaceArtifact, [(String,String)])]
  -> CheckedValueImportAuthority -> GeneratedScaffoldImportAuthority -> HscEnv
  -> IO (Either String HscEnv)
installExactLexicalGraphWithScaffold sourceGraph lexical (CheckedValueImportAuthority checkedValues)
    (GeneratedScaffoldImportAuthority scaffold) env
  | Set.size moduleNames /= length lexical = pure (Left "duplicate virtual lexical owner")
  | any (\(_, deps) -> any (`Set.notMember` owners) deps) lexical =
      pure (Left "virtual lexical edge leaves admitted graph")
  | any (\node -> case node of
      ModuleNode _ summary -> keyOf summary `Set.member` Set.union owners checkedValues
      _ -> False) (mgModSummaries' sourceGraph) =
      pure (Left "virtual lexical owner collides with source graph")
  | any (\(artifact, _) -> case lookupHpt (hsc_HPT env)
        (mkModuleName (exactModule artifact)) of
      Nothing -> True
      Just hmi -> mi_module (hm_iface hmi) /=
        mkModule (stringToUnit (exactUnit artifact))
          (mkModuleName (exactModule artifact))) lexical =
      pure (Left "virtual lexical interface missing from exact HPT")
  | not (null unadmittedHomeEdges) =
      pure (Left ("source graph imports unadmitted home implementation: "
        ++ show unadmittedHomeEdges))
  | otherwise = do
      forM_ lexical $ \(artifact, _) ->
        addHomeModuleToFinder (hsc_FC env) (hsc_home_unit env)
          (GWIB (mkModuleName (exactModule artifact)) NotBoot)
          (ms_location (virtualSummary env artifact))
      forM_ scaffold $ \(_,_,_,_,artifact) ->
        addHomeModuleToFinder (hsc_FC env) (hsc_home_unit env)
          (GWIB (mkModuleName (exactModule artifact)) NotBoot)
          (ms_location (virtualSummary env artifact))
      pure (Right env { hsc_mod_graph = mkModuleGraph (sourceNodes ++ virtualNodes) })
  where
    moduleNames = Set.fromList [exactModule artifact | (artifact, _) <- lexical]
    owners = Set.fromList
      [(exactUnit artifact, exactModule artifact) | (artifact, _) <- lexical]
    home = homeUnitId (hsc_home_unit env)
    known = Set.union (Set.union owners checkedValues) (Set.fromList
      [keyOf summary | ModuleNode _ summary <- mgModSummaries' sourceGraph])
    -- Downsweep intentionally excludes admitted exact owners. It may therefore
    -- omit the corresponding edge; textual imports still cannot expose an
    -- implementation-only HPT entry outside the selected lexical graph.
    unadmittedHomeEdges =
      [(keyOf summary, requested)
      | ModuleNode edges summary <- sourceNodes
      , requested <- Set.toAscList (Set.fromList
          ([owner | edge <- edges, Just owner <- [missing summary edge]]
           ++ [owner | imported <- ms_textual_imps summary ++ ms_srcimps summary
              , Just owner <- [hiddenImport summary imported]]))]
    permitted summary requested qualifier imported = qualifier == NoPkgQual && any
      (\(target,fingerprint,native,span',_) -> ms_mod summary == target && ms_hs_hash summary == fingerprint
        && requested == (executionUnit native,executionModule native) && getLoc imported == span') scaffold
    hiddenImport summary (qualifier, imported) =
      let name = unLoc imported
          local = case qualifier of
            NoPkgQual -> True
            ThisPkg unit -> unit == home
            OtherPkg _ -> False
      in if not local then Nothing else case lookupHpt (hsc_HPT env) name of
        Nothing -> Nothing
        Just hmi ->
          let owner = (unitString (moduleUnit (mi_module (hm_iface hmi))), moduleNameString name)
          in if owner `Set.member` known || permitted summary owner qualifier imported then Nothing else Just owner
    missing summary (NodeKey_Module (ModNodeKeyWithUid (GWIB name _) unit))
      | unit == home, (unitString unit, moduleNameString name) `Set.notMember` known =
          let requested = (unitString unit,moduleNameString name) in
          if any (\(qualifier, imported) -> unLoc imported == name && permitted summary requested qualifier imported)
              (ms_textual_imps summary) then Nothing else Just requested
    missing _ _ = Nothing
    keyOf summary =
      (unitString (moduleUnit (ms_mod summary)), moduleNameString (moduleName (ms_mod summary)))
    nodeKey (_, moduleName') = NodeKey_Module
      (ModNodeKeyWithUid (GWIB (mkModuleName moduleName') NotBoot) home)
    sourceNodes = [case node of
      ModuleNode edges summary ->
        let imports =
              [ (unitString home, moduleNameString (unLoc imported))
              | (qualifier, imported) <- ms_textual_imps summary
              , case qualifier of
                  NoPkgQual -> True
                  ThisPkg unit -> unit == home
                  OtherPkg _ -> False
              ]
            added = [nodeKey owner | owner <- imports, owner `Set.member` owners]
        in ModuleNode (Set.toList (Set.fromList (edges ++ added))) summary
      other -> other
      | node <- mgModSummaries' sourceGraph]
    virtualNodes =
      [ ModuleNode (map nodeKey deps) (virtualSummary env artifact)
      | (artifact, deps) <- lexical ]

virtualSummary :: HscEnv -> ExactIfaceArtifact -> ModSummary
virtualSummary env artifact = ModSummary
  { ms_mod = mkModule (stringToUnit (exactUnit artifact))
      (mkModuleName (exactModule artifact))
  , ms_hsc_src = HsSrcFile
  , ms_location = location
  , ms_hs_hash = fingerprintByteString BS.empty
  , ms_obj_date = Nothing
  , ms_dyn_obj_date = Nothing
  , ms_iface_date = Nothing
  , ms_hie_date = Nothing
  , ms_srcimps = []
  , ms_textual_imps = []
  , ms_ghc_prim_import = False
  , ms_parsed_mod = Nothing
  , ms_hspp_file = exactPath artifact
  , ms_hspp_opts = hsc_dflags env
  , ms_hspp_buf = Nothing
  }
  where location = ModLocation
          { ml_hs_file = Nothing
          , ml_hi_file = exactPath artifact
          , ml_dyn_hi_file = exactPath artifact
          , ml_obj_file = exactPath artifact
          , ml_dyn_obj_file = exactPath artifact
          , ml_hie_file = exactPath artifact
          }

hexBytes :: BS.ByteString -> String
hexBytes = concatMap (\byte -> let s = showHex byte "" in replicate (2 - length s) '0' ++ s)
  . BS.unpack
