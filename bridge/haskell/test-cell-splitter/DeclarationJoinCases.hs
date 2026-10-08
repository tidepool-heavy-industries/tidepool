{-# LANGUAGE OverloadedStrings #-}
module DeclarationJoinCases where

import qualified Data.Set as Set
import Codec.CBOR.Term (Term(..), encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (IOException, bracket, try)
import Control.Monad (forM_, unless, void)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.IntMap.Strict qualified as IntMap
import Data.List (isInfixOf, sort)
import Data.String (fromString)
import Tidepool.OriginalProductRoots
  ( requiredOriginalPackageGlobals, requiredOriginalPackageGlobalsWithExact
  , requiredOriginalPackageGlobalsWithRetained
  , projectedOriginalGlobalDemand, candidateOriginalGlobalDemand )
import qualified Tidepool.ExecutionSchema as Execution
import Tidepool.ModuleCandidates
import Tidepool.ExactScope
  ( ExactOriginalGroup(..), originalGroupFromProjected, originalGroupFromCandidate )
import GHC
import GHC.Builtin.Types (doubleTy)
import GHC.Core.InstEnv (instEnvElts, is_dfun_name, is_tys)
import GHC.Core.FamInstEnv (fi_axiom)
import GHC.Core.Coercion.Axiom (coAxiomName)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Driver.Env (hsc_HPT, hscUpdateHPT, hscEPS, hptInstancesBelow)
import GHC.Driver.Env.Types (hsc_mod_graph)
import GHC.Driver.Pipeline (compileOne)
import GHC.Iface.Syntax (IfaceClsInst(..), IfaceFamInst(..))
import GHC.Types.SourceError (SourceError)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home.ModInfo
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Types (stringToUnit, GenWithIsBoot(..))
import GHC.Unit.Home (homeUnitId)
import GHC.Driver.Env (hsc_home_unit)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))
import Numeric (showHex)
import System.Directory
import System.Environment (lookupEnv)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (callProcess, readProcess)
import Tidepool.DeclarationJoin
import Tidepool.ExactHydration
import Tidepool.GhcPipeline (withExactInterfaceTransaction)

declarationJoinScenario :: IO ()
declarationJoinScenario = bracket temporary removeDirectoryRecursive $ \root -> do
    let fixture = "test-cell-splitter/fixtures/declaration-join/exact-isolation"
        inspectOwned artifacts = withExactInterfaceTransaction [root] $ \operations ->
          inspectDeclarationArtifacts operations artifacts
        validateOwned input = withExactInterfaceTransaction [root] $ \operations ->
          validateDeclarationJoin operations input
    forM_ sourceFiles $ \file -> copyFile (fixture </> file) (root </> file)
    libdir <- ghcLibdir
    -- Each defining module is compiled once in its original lexical context.
    -- Old and Public deliberately have incompatible dictionaries; they are
    -- combined only through a selected interface, never an import wrapper.
    forM_ ["Old", "Public", "Conflict", "AssociatedConflict", "DataConflict", "InjectiveConflict",
      "FamilyOnly", "RichInventory", "AmbiguousInventory"] $ \target ->
      runGhc (Just libdir) $ do
        configure root
        guessed <- guessTarget (root </> target ++ ".hs") Nothing Nothing
        setTargets [guessed]
        success <- load LoadAllTargets
        liftIO $ unless (succeeded success) (fail (target ++ " fixture compile failed"))
    runGhc (Just libdir) $ do
      configure root
      initial <- getSession
      fresh <- liftIO (freshExactState initial)
      artifacts <- liftIO (mapM (artifact root) ["Common", "Old", "Public"])
      loaded <- liftIO (readExactIfaceArtifacts fresh artifacts >>= either fail pure)
      hydrated <- liftIO (hydrateExactScope fresh loaded)
      originalInventories <- liftIO (mapM (\(_, iface) -> interfaceInventory hydrated iface >>= either fail pure) loaded)
      let ifaces = map snd loaded
          oldHmi = requireHmi hydrated "Old"
          publicIface = requireIface ifaces "Public"
          commonIface = requireIface ifaces "Common"
          oldIface = requireIface ifaces "Old"
          doubleInstances = [i | i <- instEnvElts (md_insts (hm_details oldHmi)),
            any (`eqType` doubleTy) (is_tys i)]
          selected = InstanceInventory
            [record | inventory <- originalInventories, record <- inventoryClasses inventory,
              instanceDfun record `elem` (map (exportIdentity . ifDFun) (mi_insts publicIface)
                ++ map (exportIdentity . is_dfun_name) doubleInstances)]
            (map (exportIdentity . ifFamInstAxiom) (mi_fam_insts publicIface))
          fullFamilies = [exportIdentity (ifFamInstAxiom i) | iface <- ifaces, i <- mi_fam_insts iface]
          joined = mkModule (stringToUnit "main") (mkModuleName "Joined")
      let declarationArtifacts = [DeclarationArtifact a Nothing | a <- artifacts]
          inventoryManifest = root </> "inventory.cbor"
      liftIO $ BS.writeFile inventoryManifest (encodeDeclarationInventory declarationArtifacts)
      operation <- liftIO (readDeclarationOperation inventoryManifest)
      liftIO $ unless (operation == InspectInventory declarationArtifacts)
        (fail "inventory manifest changed across transport")
      inspected <- liftIO (inspectOwned declarationArtifacts)
      liftIO $ case inspectionResult inspected of
        Right inventories | map inventoryArtifact inventories == declarationArtifacts
          , map inventoryInstances inventories == originalInventories -> pure ()
        other -> fail ("original interface inventory was not retained: " ++ show other)
      liftIO $ writeFile (root </> "inventory-receipt.json") (renderDeclarationInventoryOutcome inspected)
      liftIO $ do
        forM_ ["FamilyOnly", "RichInventory"] $ \name -> do
          extra <- artifact root name
          outcome <- inspectOwned
            (declarationArtifacts ++ [DeclarationArtifact extra Nothing])
          case inspectionResult outcome of
            Right inventories -> case [inventoryInstances entry | entry <- inventories,
              exactModule (artifactInterface (inventoryArtifact entry)) == name] of
              [inventory] | name == "FamilyOnly", null (inventoryClasses inventory),
                length (inventoryFamilies inventory) == 2 -> pure ()
              [inventory] | name == "RichInventory", length (inventoryClasses inventory) == 2,
                sort (map (length . instanceSelectedAxioms) (inventoryClasses inventory)) == [1,3],
                length (inventoryFamilies inventory) == 4 -> pure ()
              other -> fail ("rich original inventory incomplete: " ++ show other)
            other -> fail ("rich original inventory rejected: " ++ show other)
        ambiguous <- artifact root "AmbiguousInventory"
        outcome <- inspectOwned [DeclarationArtifact ambiguous Nothing]
        case inspectionResult outcome of
          Left (Unprovable, _) -> pure ()
          other -> fail ("ambiguous associated axiom owner was admitted: " ++ show other)
      exports <- liftIO $ (++) <$> interfaceExports hydrated commonIface <*> interfaceExports hydrated oldIface
      let input = DeclarationJoinInput "paired-public-snapshot" Nothing Nothing Nothing []
            (ReservedJoin "main" "Joined" (root </> "Joined.hi")) exports selected
            [DeclarationArtifact a Nothing | a <- artifacts] fullFamilies
          manifest = root </> "join.cbor"
      liftIO $ BS.writeFile manifest (encodeDeclarationJoin input)
      decoded <- liftIO (readDeclarationJoin manifest)
      liftIO $ unless (decoded == input) (fail "exact join manifest changed across transport")
      outcome <- liftIO (validateOwned decoded)
      liftIO $ case outcomeDecision outcome of
        JoinAccepted | outcomeInput outcome == input, Just _ <- outcomeArtifact outcome -> pure ()
        rejected -> fail ("sound isolation join rejected: " ++ show rejected)
      liftIO $ writeFile (root </> "join-receipt.json") (renderDeclarationJoinOutcome outcome)
      -- A previous Join can retain the same dfun with fewer selected axioms.
      -- The next Join combines its lexical anchor with original implementations
      -- and restores the requested family selection without duplicating dfuns.
      let partialSelected = InstanceInventory
            [record { instanceSelectedAxioms = [] } | record <- inventoryClasses selected] []
          partialOwner = mkModule (stringToUnit "main") (mkModuleName "PartialJoined")
      partial <- liftIO $ buildJoinedInterface hydrated partialOwner (root </> "PartialJoined.hi")
        ifaces exports partialSelected fullFamilies
      liftIO $ case partial of
        Right _ -> pure ()
        other -> fail ("partial family Join rejected: " ++ show other)
      partialArtifact <- liftIO (artifact root "PartialJoined")
      combinedLoaded <- liftIO $ readExactIfaceArtifacts fresh (artifacts ++ [partialArtifact]) >>= either fail pure
      combinedEnv <- liftIO (hydrateExactScope fresh combinedLoaded)
      secondJoin <- liftIO $ buildJoinedInterface combinedEnv
        (mkModule (stringToUnit "main") (mkModuleName "JoinedAgain")) (root </> "JoinedAgain.hi")
        (map snd combinedLoaded) exports selected fullFamilies
      liftIO $ case secondJoin of
        Right _ -> pure ()
        other -> fail ("repeated dfun with different selected-axiom subset rejected: " ++ show other)
      case inventoryClasses selected of
        record : rest -> do
          let wrongClass = (instanceClass record) { exportOccurrence = "WrongClass" }
              forged = selected { inventoryClasses = record : record { instanceClass = wrongClass } : rest }
          rejected <- liftIO $ buildJoinedInterface hydrated
            (mkModule (stringToUnit "main") (mkModuleName "WrongClassJoin")) (root </> "WrongClassJoin.hi")
            ifaces exports forged fullFamilies
          liftIO $ case rejected of
            Left (InstanceMismatch, _) -> pure ()
            other -> fail ("one dfun was merged across different classes: " ++ show other)
        [] -> liftIO (fail "selected class inventory unexpectedly empty")
      -- Rejection echoes the same paired version and exact request provenance.
      rejectedInput <- case implementationArtifacts input of
        entry : rest -> pure input { implementationArtifacts = entry
          { artifactInterface = (artifactInterface entry) { exactSha256 = replicate 64 '0' } } : rest }
        [] -> liftIO (fail "fixture implementation artifacts unexpectedly empty")
      rejected <- liftIO (validateOwned rejectedInput)
      liftIO $ case outcomeDecision rejected of
        JoinRejected ArtifactChanged _ | outcomeInput rejected == rejectedInput
          , outcomeArtifact rejected == Nothing -> pure ()
        other -> fail ("changed artifact did not produce bound rejection: " ++ show other)
      liftIO $ case implementationArtifacts input of
        entry : _ -> do
          let path = exactPath (artifactInterface entry)
          bracket (BS.readFile path) (BS.writeFile path) $ \_ -> do
            BS.writeFile path (BS.singleton 0)
            inspectedChanged <- inspectOwned (implementationArtifacts input)
            case inspectionResult inspectedChanged of
              Left (ArtifactChanged, _) -> pure ()
              other -> fail ("mutated interface passed worker inventory admission: " ++ show other)
            joinedChanged <- validateOwned input
            case outcomeDecision joinedChanged of
              JoinRejected ArtifactChanged _ | outcomeInput joinedChanged == input -> pure ()
              other -> fail ("mutated interface passed worker join admission: " ++ show other)
        [] -> fail "fixture implementation artifacts unexpectedly empty"
      -- Retrying an already published reservation cannot overwrite its bytes.
      before <- liftIO (BS.readFile (root </> "Joined.hi"))
      collision <- liftIO (try (buildJoinedInterface hydrated joined (root </> "Joined.hi")
        ifaces exports selected fullFamilies))
        :: Ghc (Either IOException (Either (JoinRejection, String) ModuleSnapshot))
      after <- liftIO (BS.readFile (root </> "Joined.hi"))
      liftIO $ case collision of
        Left _ | before == after -> pure ()
        _ -> fail "an existing immutable Join output was replaced"
      -- A hidden axiom still participates in consistency, though it will not
      -- participate in the downstream consumer's reduction environment.
      forM_ ["Conflict", "AssociatedConflict", "DataConflict", "InjectiveConflict"] $ \name -> do
        conflicting <- liftIO (artifact root name)
        conflictLoaded <- liftIO $ readExactIfaceArtifacts fresh (artifacts ++ [conflicting]) >>= either fail pure
        conflictEnv <- liftIO (hydrateExactScope fresh conflictLoaded)
        let conflictIfaces = map snd conflictLoaded
            conflictFamilies = [exportIdentity (ifFamInstAxiom i) | iface <- conflictIfaces, i <- mi_fam_insts iface]
        conflict <- liftIO $ buildJoinedInterface conflictEnv joined (root </> name ++ "Join.hi")
          conflictIfaces exports selected conflictFamilies
        liftIO $ case conflict of
          Left (FamilyInstanceConflict, _) -> pure ()
          other -> fail (name ++ " retained family conflict was not rejected: " ++ show other)
    -- No source restoration or original declaration replay in the fresh worker.
    forM_ ["Common", "Old", "Public", "Conflict", "AssociatedConflict", "DataConflict", "InjectiveConflict"] $ \name ->
      renameFile (root </> name ++ ".hs") (root </> name ++ ".hidden")
    executable <- declarationConsumer
    callProcess executable ["--consumer", root]
    putStrLn "declaration join: persisted interface, fresh source-hidden consumer and four retained family conflicts passed"

recoveredConsumer :: FilePath -> IO ()
recoveredConsumer root = do
  libdir <- ghcLibdir
  runGhc (Just libdir) $ do
    configure root
    initial <- getSession
    fresh <- liftIO (freshExactState initial)
    artifacts <- liftIO (mapM (artifact root) ["Common", "Old", "Public", "Joined", "PartialJoined", "JoinedAgain"])
    loaded <- liftIO (readExactIfaceArtifacts fresh artifacts >>= either fail pure)
    hydrated <- liftIO (hydrateExactScope fresh loaded)
    let joinedArtifacts = [a | a <- artifacts, exactModule a `elem` ["Joined", "PartialJoined", "JoinedAgain"]]
    setSession hydrated
    -- Depanal excludes the synthetic interface and every implementation module.
    -- The same explicit virtual node is installed for each ordinary consumer.
    let check file = do
          previous <- getSession
          setSession previous { hsc_mod_graph = emptyMG }
          target <- guessTarget (root </> file ++ ".hs") Nothing Nothing
          setTargets [target]
          graph <- depanal (map (mkModuleName . exactModule) artifacts) False
          env <- getSession
          installed <- liftIO (installExactLexicalGraph graph [(joinedArtifact, []) | joinedArtifact <- joinedArtifacts] noCheckedValueImports env)
          lexical <- either (liftIO . fail) pure installed
          setSession lexical
          summary <- getModSummary (if file == "Consumer" then mkModuleName "Main" else mkModuleName file)
          pure summary
    summary <- check "Consumer"
    env <- getSession
    let scoped = exactHomeInstancesFor summary env
        names environment = map is_dfun_name (instEnvElts (fst (hptInstancesBelow environment
          (homeUnitId (hsc_home_unit environment)) (GWIB (moduleName (ms_mod summary)) NotBoot))))
        originalNames = names env
        scopedNames = names scoped
        familyNames environment = map (coAxiomName . fi_axiom) (snd (hptInstancesBelow environment
          (homeUnitId (hsc_home_unit environment)) (GWIB (moduleName (ms_mod summary)) NotBoot)))
        originalFamilies = familyNames env
        scopedFamilies = familyNames scoped
    liftIO $ unless (length originalNames > Set.size (Set.fromList originalNames)
        && Set.fromList scopedNames == Set.fromList originalNames
        && length scopedNames == Set.size (Set.fromList scopedNames))
      (fail "reachable joined views did not preserve each exact original dfun once")
    liftIO $ unless (length originalFamilies > Set.size (Set.fromList originalFamilies)
        && Set.fromList scopedFamilies == Set.fromList originalFamilies
        && length scopedFamilies == Set.size (Set.fromList scopedFamilies))
      (fail "reachable joined views did not preserve each exact original family axiom once")
    consumer <- withExactHomeInstances summary $ do
      temporary <- getSession
      liftIO (compileOne temporary summary 1 1 Nothing emptyHomeModInfoLinkable)
    restored <- getSession
    liftIO $ unless (names restored == originalNames && familyNames restored == originalFamilies)
      (fail "target typecheck changed the retained instance environment")
    liftIO $ writeFile (root </> "consumer-object.txt") (ml_obj_file (ms_location summary))
    setSession (hscUpdateHPT (addHomeModInfoToHpt consumer) env)
    forM_ ["BadClass", "BadFamily", "BadAssociated", "BadFD"] $ \name -> do
      bad <- check name
      badEnv <- getSession
      result <- liftIO $ try (compileOne (exactHomeInstancesFor bad badEnv) bad 1 1 Nothing emptyHomeModInfoLinkable)
        :: Ghc (Either SourceError HomeModInfo)
      liftIO $ case result of
        Left _ -> pure ()
        Right _ -> fail (name ++ " unexpectedly acquired hidden typing evidence")
    nextSummary <- check "Next"
    nextEnv <- getSession
    next <- liftIO (compileOne (exactHomeInstancesFor nextSummary nextEnv) nextSummary 1 1 Nothing emptyHomeModInfoLinkable)
    setSession (hscUpdateHPT (addHomeModInfoToHpt next) nextEnv)
    liftIO $ writeFile (root </> "next-object.txt") (ml_obj_file (ms_location nextSummary))
    final <- getSession
    eps <- liftIO (hscEPS final)
    let homeInstances = [i | i <- instEnvElts (eps_inst_env eps),
          moduleUnit (nameModule (is_dfun_name i)) == stringToUnit "main"]
    liftIO $ unless (null homeInstances) (fail "home instance leaked into EPS")
  -- Link already compiled originals with the new consumer. This neither reads
  -- the hidden sources nor performs typechecking in a different instance scope.
  consumerObject <- readFile (root </> "consumer-object.txt")
  let executable = root </> "joined-consumer"
      objects = map (\name -> root </> name ++ ".o") ["Common", "Old", "Public"] ++ [consumerObject]
  callProcess "ghc" (objects ++ ["-o", executable])
  output <- readProcess executable [] ""
  unless (output == "(11,19,22,33,True,'p',True,3,'z',11,44,True,'j')\n")
    (fail ("original dictionary or selected lookup changed: " ++ show output))
  putStrLn "fresh consumer: four hidden-evidence failures and actual old/new dictionary execution passed"
  renameFile (root </> "Next.hs") (root </> "Next.hidden")
  self <- declarationConsumer
  callProcess self ["--next-consumer", root]

recoveredNextConsumer :: FilePath -> IO ()
recoveredNextConsumer root = do
  libdir <- ghcLibdir
  runGhc (Just libdir) $ do
    configure root
    initial <- getSession
    fresh <- liftIO (freshExactState initial)
    artifacts <- liftIO (mapM (artifact root) ["Common", "Old", "Public", "Joined", "Next"])
    loaded <- liftIO (readExactIfaceArtifacts fresh artifacts >>= either fail pure)
    hydrated <- liftIO (hydrateExactScope fresh loaded)
    let lexical = [(a, if exactModule a == "Next" then [("main", "Joined")] else [])
          | a <- artifacts, exactModule a `elem` ["Joined", "Next"]]
        check name = do
          previous <- getSession
          setSession previous { hsc_mod_graph = emptyMG }
          target <- guessTarget (root </> name ++ ".hs") Nothing Nothing
          setTargets [target]
          graph <- depanal (map (mkModuleName . exactModule) artifacts) False
          env <- getSession
          installed <- liftIO (installExactLexicalGraph graph lexical noCheckedValueImports env >>= either fail pure)
          setSession installed
          getModSummary (if name == "NextConsumer" then mkModuleName "Main" else mkModuleName name)
    setSession hydrated
    summary <- check "NextConsumer"
    env <- getSession
    _ <- liftIO (compileOne env summary 1 1 Nothing emptyHomeModInfoLinkable)
    liftIO $ writeFile (root </> "next-consumer-object.txt") (ml_obj_file (ms_location summary))
    forM_ ["BadRetraction", "BadNextClass", "BadNextFamily"] $ \name -> do
      bad <- check name
      badEnv <- getSession
      result <- liftIO $ try (compileOne badEnv bad 1 1 Nothing emptyHomeModInfoLinkable)
        :: Ghc (Either SourceError HomeModInfo)
      liftIO $ case result of
        Left _ -> pure ()
        Right _ -> fail (name ++ " acquired retracted exports or hidden instances after next-turn recovery")
  nextObject <- readFile (root </> "next-object.txt")
  consumerObject <- readFile (root </> "next-consumer-object.txt")
  let executable = root </> "next-consumer"
      objects = map (\name -> root </> name ++ ".o") ["Common", "Old", "Public"] ++ [nextObject, consumerObject]
  callProcess "ghc" (objects ++ ["-o", executable])
  output <- readProcess executable [] ""
  unless (output == "(99,11,11,True,True)\n") (fail ("next-turn export identity changed: " ++ show output))
  putStrLn "next-turn recovery: original constructors, replacement and retraction passed"

configure :: FilePath -> Ghc ()
configure root = do
  flags <- getSessionDynFlags
  void $ setSessionDynFlags flags
    { importPaths = [root], ghcLink = NoLink, backend = ncgBackend
    , hiDir = Just root, objectDir = Just root
    }

ghcLibdir :: IO FilePath
ghcLibdir = reverse . dropWhile (`elem` ['\r', '\n']) . reverse <$> readProcess "ghc" ["--print-libdir"] ""

artifact :: FilePath -> String -> IO ExactIfaceArtifact
artifact root name = do
  let path = root </> name ++ ".hi"
  bytes <- BS.readFile path
  let digest = concatMap (\byte -> let digits = showHex byte "" in replicate (2 - length digits) '0' ++ digits)
        (BS.unpack (SHA.hash bytes))
  pure (ExactIfaceArtifact "main" name path digest [])

requireHmi :: HscEnv -> String -> HomeModInfo
requireHmi env name = maybe (error ("missing fixture HMI " ++ name)) id
  (lookupHpt (hsc_HPT env) (mkModuleName name))

requireIface :: [ModIface] -> String -> ModIface
requireIface ifaces name = case [iface | iface <- ifaces, moduleName (mi_module iface) == mkModuleName name] of
  iface : _ -> iface
  [] -> error ("missing fixture iface " ++ name)

sourceFiles :: [String]
sourceFiles = map (++ ".hs")
  ["Common", "Old", "Public", "Conflict", "AssociatedConflict", "DataConflict", "InjectiveConflict", "FamilyOnly", "RichInventory", "AmbiguousInventory", "Consumer", "BadClass", "BadFamily", "BadAssociated", "BadFD", "Next", "NextConsumer", "BadRetraction", "BadNextClass", "BadNextFamily"]

temporary :: IO FilePath
temporary = do
  parent <- getTemporaryDirectory
  (path, handle) <- openTempFile parent "tidepool-exact-declaration-join"
  hClose handle
  removeFile path
  createDirectory path
  pure path

emptyWireInput :: DeclarationJoinInput
emptyWireInput = DeclarationJoinInput "paired-snapshot" Nothing Nothing Nothing []
  (ReservedJoin "main" "Join" "/scratch/Join.hi") [] (InstanceInventory [] []) [] []

typedWireInput :: DeclarationJoinInput
typedWireInput = emptyWireInput
  { expectedInstances = InstanceInventory
      [ClassInstanceEvidence (identity ValueNamespace "$fClassInt") (identity TypeNamespace "Class") [selected]]
      [selected, standalone]
  , retainedFamilyClosure = [selected, standalone, hidden]
  }
  where
    identity namespace occurrence = ExportIdentity "main" "Original" namespace occurrence Nothing
    selected = identity TypeNamespace "AssociatedAxiom"
    standalone = identity TypeNamespace "StandaloneAxiom"
    hidden = identity TypeNamespace "HiddenAxiom"

-- Exercise exact original-group traversal without compiling an unrelated
-- fixture. The real parser package dictionary is admitted by the worker gate.
originalProductRootsProof :: IO ()
originalProductRootsProof = originalProductRootsProofWith projectedOriginalGlobalDemand

originalProductRootsProofWith :: (Execution.GlobalDecl -> (Execution.SymbolIdentity, Bool)) -> IO ()
originalProductRootsProofWith projectedDemand = do
  -- The package-root walker consumes groups only. Decode an unvalidated wire
  -- descriptor rather than constructing or authenticating a module proof.
  parent <- getTemporaryDirectory
  base <- bracket (openTempFile parent "original-group-descriptor.cbor")
    (\(path,_) -> removeFile path) $ \(path,handle) -> do
      hClose handle
      let seal = TString (fromString (replicate 64 'a'))
          row = TList [TString "main",TString "Fixture",TString "/fixture/Source.hs",seal
            ,TString "/fixture/Source.hi",seal,seal,seal,seal,TList [],TList []
            ,TString "/fixture/Source.packages",seal,TString "/fixture/Source.tpmod",TList []
            ,TList [TString "module",TString "/fixture/module.cbor",seal,TString "/fixture/Core",seal]]
          packet = TList [TString "TPMCAN",TString "10",TList [],TList [],TList [row]
            ,TList [TList [],TList []],seal]
      BS.writeFile path (toStrictByteString (encodeTerm packet))
      readModuleCandidates path >>= \case
        Right [candidate] -> pure candidate
        _ -> fail "structural original-group candidate did not decode"
  let identity unit name occurrence = Execution.SymbolIdentity
        (fromString unit) (fromString name) (fromString "value") (fromString occurrence) Nothing
      source = identity "main" "Tidepool.Aeson.FromJSON" "$fFromJSONInt_$cparseJSON"
      sibling = identity "main" "Tidepool.Aeson.FromJSON" "parseSibling"
      caller = identity "main" "Caller" "parseInput"
      package = identity "ghc-internal" "GHC.Internal.Real" "$fIntegralInt"
      unused = identity "ghc-internal" "GHC.Internal.Real" "$fIntegralWord"
      global value = Execution.GlobalDecl value Execution.LiftedRefRep Nothing False Nothing
      candidate name groups = base {candidateModule=name,candidateGroups=groups}
      imported value generation = CandidateGlobal value Execution.LiftedRefRep Nothing False generation
      parserCandidate = candidate "Tidepool.Aeson.FromJSON"
        [CandidateGroup 137 [source, sibling] [imported sibling Nothing, imported package Nothing]]
      input = candidate "Caller" [CandidateGroup 2 [caller] [imported source Nothing]]
      other = candidate "Unused" [CandidateGroup 7 [identity "main" "Unused" "entry"]
        [imported unused Nothing]]
      roots = requiredOriginalPackageGlobals [] [parserCandidate, input, other]
  let retainedRoots retained targets = requiredOriginalPackageGlobalsWithRetained [] [parserCandidate, input, other] [] retained targets
  unless (retainedRoots (Set.singleton package) [global caller] == Right [])
    (fail "retained package import reopened original executable recovery")
  unless (retainedRoots (Set.singleton unused) [global caller] == Right [package])
    (fail "different retained package identity hid a required executable root")
  unless (retainedRoots (Set.singleton source) [global caller] == Right [])
    (fail "retained original home import reopened its implementation closure")
  unless (roots [global caller] == Right [package])
    (fail "demanded parser original-group package closure changed")
  unless (roots [global (identity "other-unit" "Caller" "parseInput")] == Right [])
    (fail "package root selection inferred a different unit from occurrence")
  unless (roots [(global caller) { Execution.globalRequiredGeneration = Just 7 }] == Right [])
    (fail "retained generation reopened original implementation closure")
  let duplicateRoots = requiredOriginalPackageGlobals [] [parserCandidate, parserCandidate]
  forM_ [[global caller], []] $ \targets -> case duplicateRoots targets of
    Left _ -> pure ()
    Right _ -> fail "duplicate original binder was accepted in package root inventory"
  let failedOwner = ("main", "Broken", Left "fixture projection failure")
      broken = identity "main" "Broken" "entry"
      failedRoots = requiredOriginalPackageGlobals [failedOwner] [parserCandidate, input]
  unless (failedRoots [global caller] == Right [package])
    (fail "unrelated fresh product miss became fatal")
  case failedRoots [global broken] of
    Left reason | "fixture projection failure" `isInfixOf` reason -> pure ()
    _ -> fail "reusing original inventory lost a newly demanded failed owner"
  case requiredOriginalPackageGlobalsWithRetained [failedOwner] [] [] (Set.singleton package) [global broken] of
    Left reason | "fixture projection failure" `isInfixOf` reason -> pure ()
    _ -> fail "reached failed fresh home owner lost its projection diagnostic"
  let newlyReached = identity "main" "Later" "entry"
      later = candidate "Later" [CandidateGroup 3 [newlyReached] [imported unused Nothing]]
      growingRoots = requiredOriginalPackageGlobals [] [parserCandidate, input, later]
      first = growingRoots [global caller]
      second = growingRoots [global caller, global newlyReached]
      reversed = growingRoots [global newlyReached, global caller, global caller]
      laterOnly = growingRoots [global newlyReached]
  unless (first == Right [package] && second == Right (sort [package, unused])
      && reversed == second && laterOnly == Right [unused])
    (fail "recovery-induced source global did not extend exact package roots")
  let dictionary = identity "main" "Tidepool.Aeson.FromJSON" "dictionary"
      envelope = Execution.ProgramEnvelope Execution.schemaVersion
        (fromString "ghc-9.12-prepared-stg") (fromString "ghc-9.12.2")
        Execution.executionAbiVersion
        (Execution.TargetDescriptor Execution.X86_64 Execution.LittleEndian 64 64 (fromString "sysv64") [])
      projected ordinal binders globals = Execution.ProjectedGroup ordinal binders
        (Execution.ProjectedGroupBody
          { Execution.projectedEnvelope = envelope
          , Execution.projectedSignatures = []
          , Execution.projectedGlobals = globals
          , Execution.projectedConstructors = []
          , Execution.projectedOperations = []
          , Execution.projectedBindings = []
          , Execution.projectedTypes = Execution.TypeGraph IntMap.empty IntMap.empty
          , Execution.projectedSites = []
          , Execution.projectedConstructorReplies = []
          , Execution.projectedJsonLayout = Nothing
          })
      dictionaryGlobals = [global package, (global unused)
        { Execution.globalRequiredEvaluated = True, Execution.globalRequiredGeneration = Just 4 }]
      freshOutlines =
        [originalGroupFromProjected (projected 137 [source] [global dictionary])
        ,originalGroupFromProjected (projected 138 [dictionary] dictionaryGlobals)]
      cachedOutlines =
        [originalGroupFromCandidate (CandidateGroup 137 [source] [imported dictionary Nothing])
        ,originalGroupFromCandidate (CandidateGroup 138 [dictionary]
          [imported package Nothing, (imported unused (Just 4)) {candidateGlobalEvaluated = True}])]
      exactRoots outlines = requiredOriginalPackageGlobalsWithRetained [] []
        [("main", "Tidepool.Aeson.FromJSON",
          [(originalOrdinal group, originalBinders group, originalGlobals group) | group <- outlines])]
        Set.empty [global source]
  unless (exactRoots freshOutlines == Right [package])
    (fail "fresh exact outline dropped unevaluated dictionary/package imports or reopened retained code")
  unless (exactRoots cachedOutlines == Right [package])
    (fail "cached exact outline dropped unevaluated dictionary/package imports or reopened retained code")
  let agent occurrence = identity "main" "Tidepool.Actors.Internal.Agent" occurrence
      effect occurrence = identity "main" "Tidepool.Effects.Core" occurrence
      textShow = identity "text-2.1.2-2594" "Data.Text.Show" "$w$cshowsPrec"
      request = agent "request"
      requestSited = agent "requestSited"
      requestConfigured = agent "requestConfiguredSited"
      effectDictionary = effect "$fShowWorktreeError"
      showListMethod = effect "$fShowWorktreeError_$cshowList"
      showsPrecWorker = effect "$w$cshowsPrec104"
      dictionaryGlobal = (global effectDictionary) { Execution.globalRequiredEvaluated = False }
      dictionaryCandidate = (imported effectDictionary Nothing) { candidateGlobalEvaluated = False }
      originalRows demand =
        [ ("main", "Tidepool.Actors.Internal.Agent",
            [(295,[request],[projectedOriginalGlobalDemand (global requestSited)])
            ,(292,[requestSited],[projectedOriginalGlobalDemand (global requestConfigured)])
            ,(288,[requestConfigured],[demand])])
        , ("main", "Tidepool.Effects.Core",
            [(1380,[effectDictionary],[projectedOriginalGlobalDemand (global showListMethod)])
            ,(1378,[showListMethod],[projectedOriginalGlobalDemand (global showsPrecWorker)])
            ,(1376,[showsPrecWorker],[projectedOriginalGlobalDemand (global textShow)])]) ]
      originalRoots demand = requiredOriginalPackageGlobalsWithExact [] []
        (originalRows demand) [global request]
  unless (originalRoots (projectedDemand dictionaryGlobal) == Right [textShow]
      && originalRoots (candidateOriginalGlobalDemand dictionaryCandidate) == Right [textShow])
    (fail "unevaluated original dictionary lost its exact package worker definition")
  let retainedDictionary = dictionaryGlobal { Execution.globalRequiredGeneration = Just 7 }
      retainedCandidate = dictionaryCandidate { candidateGlobalGeneration = Just 7 }
  unless (originalRoots (projectedOriginalGlobalDemand retainedDictionary) == Right []
      && originalRoots (candidateOriginalGlobalDemand retainedCandidate) == Right []
      && requiredOriginalPackageGlobalsWithRetained [] []
          (originalRows (projectedOriginalGlobalDemand dictionaryGlobal))
          (Set.singleton textShow) [global request] == Right [])
    (fail "original package recovery crossed an exact retained generation boundary")
  putStrLn "original-product-roots: PASS (demanded parser, whole group, unrelated omission, exact unit, retained boundary, duplicate refusal, failed home owner, recovery-induced source root, fresh/cached exact outlines, unevaluated dictionary package worker)"

declarationConsumer :: IO FilePath
declarationConsumer = lookupEnv "TIDEPOOL_TEST_DECLARATION_JOIN_CHILD" >>= maybe
  (fail "missing declared declaration-join consumer executable") pure

wireRoundTripChecks :: IO ()
wireRoundTripChecks = bracket temporary removeDirectoryRecursive $ \root -> do
  forM_ [emptyWireInput, typedWireInput] $ \input -> do
    let path = root </> "join.cbor"
    BS.writeFile path (encodeDeclarationJoin input)
    operation <- readDeclarationOperation path
    case operation of
      ValidateJoin actual | actual == input -> pure ()
      _ -> fail "current typed declaration join did not roundtrip"

  BS.writeFile (root </> "inventory.cbor") (encodeDeclarationInventory [])
  inventory <- readDeclarationOperation (root </> "inventory.cbor")
  unless (inventory == InspectInventory []) (fail "typed empty inventory did not roundtrip")
