module Main where

import Control.Exception (IOException, bracket, try)
import Control.Monad (forM_, unless, void)
import Control.Monad.IO.Class (liftIO)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import GHC
import GHC.Builtin.Types (doubleTy)
import GHC.Core.InstEnv (instEnvElts, is_dfun_name, is_tys)
import GHC.Core.TyCo.Compare (eqType)
import GHC.Driver.Env (hsc_HPT, hscUpdateHPT, hscEPS)
import GHC.Driver.Env.Types (hsc_mod_graph)
import GHC.Driver.Pipeline (compileOne)
import GHC.Iface.Syntax (IfaceClsInst(..), IfaceFamInst(..))
import GHC.Types.SourceError (SourceError)
import GHC.Unit.External (ExternalPackageState(..))
import GHC.Unit.Home.ModInfo
import GHC.Unit.Module.ModDetails (ModDetails(..))
import GHC.Unit.Types (stringToUnit)
import Numeric (showHex)
import System.Directory
import System.Environment (getArgs, getExecutablePath)
import System.FilePath ((</>))
import System.IO (hClose, openTempFile)
import System.Process (callProcess, readProcess)
import Tidepool.DeclarationJoin
import Tidepool.ExactHydration

main :: IO ()
main = getArgs >>= \case
  ["--consumer", root] -> recoveredConsumer root
  ["--next-consumer", root] -> recoveredNextConsumer root
  ["--wire-fixture", root] -> do
    let input = emptyWireInput
    BS.writeFile (root </> "join-v2.cbor") (encodeDeclarationJoin input)
    writeFile (root </> "join-v2.json") (renderDeclarationJoinOutcome
      (DeclarationJoinOutcome input (Just (ModuleSnapshot "Join" "/scratch/Join.hi" (replicate 64 '0'))) JoinAccepted))
    BS.writeFile (root </> "inventory-v2.cbor") (encodeDeclarationInventory [])
    writeFile (root </> "inventory-v2.json") (renderDeclarationInventoryOutcome
      (DeclarationInventoryOutcome [] (Right [])))
  [] -> bracket temporary removeDirectoryRecursive $ \root -> do
    golden <- BS.readFile "test-cell-splitter/fixtures/declaration-join/join-v2.cbor"
    unless (golden == encodeDeclarationJoin emptyWireInput) (fail "join CBOR differs from cross-language fixture")
    expectedReceipt <- readFile "test-cell-splitter/fixtures/declaration-join/join-v2.json"
    unless (expectedReceipt == renderDeclarationJoinOutcome (DeclarationJoinOutcome emptyWireInput
      (Just (ModuleSnapshot "Join" "/scratch/Join.hi" (replicate 64 '0'))) JoinAccepted))
      (fail "join receipt differs from cross-language fixture")
    let fixture = "test-cell-splitter/fixtures/declaration-join/exact-isolation"
    forM_ sourceFiles $ \file -> copyFile (fixture </> file) (root </> file)
    libdir <- ghcLibdir
    -- Each defining module is compiled once in its original lexical context.
    -- Old and Public deliberately have incompatible dictionaries; they are
    -- combined only through a selected interface, never an import wrapper.
    forM_ ["Old", "Public", "Conflict", "AssociatedConflict", "DataConflict", "InjectiveConflict"] $ \target ->
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
      let ifaces = map snd loaded
          oldHmi = requireHmi hydrated "Old"
          publicIface = requireIface ifaces "Public"
          commonIface = requireIface ifaces "Common"
          oldIface = requireIface ifaces "Old"
          doubleInstances = [i | i <- instEnvElts (md_insts (hm_details oldHmi)),
            any (`eqType` doubleTy) (is_tys i)]
          selected = InstanceInventory
            (map (exportIdentity . ifDFun) (mi_insts publicIface)
              ++ map (exportIdentity . is_dfun_name) doubleInstances)
            (map (exportIdentity . ifFamInstAxiom) (mi_fam_insts publicIface))
          fullFamilies = [exportIdentity (ifFamInstAxiom i) | iface <- ifaces, i <- mi_fam_insts iface]
          joined = mkModule (stringToUnit "main") (mkModuleName "Joined")
      let declarationArtifacts = [DeclarationArtifact a Nothing | a <- artifacts]
          inventoryManifest = root </> "inventory.cbor"
      liftIO $ BS.writeFile inventoryManifest (encodeDeclarationInventory declarationArtifacts)
      operation <- liftIO (readDeclarationOperation inventoryManifest)
      liftIO $ unless (operation == InspectInventory declarationArtifacts)
        (fail "inventory manifest changed across transport")
      inspected <- liftIO (inspectDeclarationArtifacts initial declarationArtifacts)
      liftIO $ case inspectionResult inspected of
        Right inventories | map inventoryArtifact inventories == declarationArtifacts
          , map inventoryInstances inventories == map (interfaceInventory . snd) loaded -> pure ()
        other -> fail ("original interface inventory was not retained: " ++ show other)
      liftIO $ writeFile (root </> "inventory-receipt.json") (renderDeclarationInventoryOutcome inspected)
      exports <- liftIO $ (++) <$> interfaceExports hydrated commonIface <*> interfaceExports hydrated oldIface
      let input = DeclarationJoinInput "paired-public-snapshot" Nothing Nothing Nothing []
            (ReservedJoin "main" "Joined" (root </> "Joined.hi")) exports selected
            [DeclarationArtifact a Nothing | a <- artifacts] fullFamilies
          manifest = root </> "join.cbor"
      liftIO $ BS.writeFile manifest (encodeDeclarationJoin input)
      decoded <- liftIO (readDeclarationJoin manifest)
      liftIO $ unless (decoded == input) (fail "exact join manifest changed across transport")
      outcome <- liftIO (validateDeclarationJoin initial decoded)
      liftIO $ case outcomeDecision outcome of
        JoinAccepted | outcomeInput outcome == input, Just _ <- outcomeArtifact outcome -> pure ()
        rejected -> fail ("sound isolation join rejected: " ++ show rejected)
      liftIO $ writeFile (root </> "join-receipt.json") (renderDeclarationJoinOutcome outcome)
      -- Rejection echoes the same paired version and exact request provenance.
      rejectedInput <- case implementationArtifacts input of
        entry : rest -> pure input { implementationArtifacts = entry
          { artifactInterface = (artifactInterface entry) { exactSha256 = replicate 64 '0' } } : rest }
        [] -> liftIO (fail "fixture implementation artifacts unexpectedly empty")
      rejected <- liftIO (validateDeclarationJoin initial rejectedInput)
      liftIO $ case outcomeDecision rejected of
        JoinRejected ArtifactChanged _ | outcomeInput rejected == rejectedInput
          , outcomeArtifact rejected == Nothing -> pure ()
        other -> fail ("changed artifact did not produce bound rejection: " ++ show other)
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
    executable <- getExecutablePath
    callProcess executable ["--consumer", root]
    putStrLn "declaration join: persisted interface, fresh source-hidden consumer and four retained family conflicts passed"
  _ -> fail "unexpected declaration join test arguments"

recoveredConsumer :: FilePath -> IO ()
recoveredConsumer root = do
  libdir <- ghcLibdir
  runGhc (Just libdir) $ do
    configure root
    initial <- getSession
    fresh <- liftIO (freshExactState initial)
    artifacts <- liftIO (mapM (artifact root) ["Common", "Old", "Public", "Joined"])
    loaded <- liftIO (readExactIfaceArtifacts fresh artifacts >>= either fail pure)
    hydrated <- liftIO (hydrateExactScope fresh loaded)
    joinedArtifact <- case [a | a <- artifacts, exactModule a == "Joined"] of
      a : _ -> pure a
      [] -> liftIO (fail "missing persisted Join artifact")
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
          installed <- liftIO (installExactLexicalGraph graph [(joinedArtifact, [])] env)
          lexical <- either (liftIO . fail) pure installed
          setSession lexical
          summary <- getModSummary (if file == "Consumer" then mkModuleName "Main" else mkModuleName file)
          pure summary
    summary <- check "Consumer"
    env <- getSession
    consumer <- liftIO (compileOne env summary 1 1 Nothing emptyHomeModInfoLinkable)
    liftIO $ writeFile (root </> "consumer-object.txt") (ml_obj_file (ms_location summary))
    setSession (hscUpdateHPT (addHomeModInfoToHpt consumer) env)
    forM_ ["BadClass", "BadFamily", "BadAssociated", "BadFD"] $ \name -> do
      bad <- check name
      badEnv <- getSession
      result <- liftIO $ try (compileOne badEnv bad 1 1 Nothing emptyHomeModInfoLinkable)
        :: Ghc (Either SourceError HomeModInfo)
      liftIO $ case result of
        Left _ -> pure ()
        Right _ -> fail (name ++ " unexpectedly acquired hidden typing evidence")
    nextSummary <- check "Next"
    nextEnv <- getSession
    next <- liftIO (compileOne nextEnv nextSummary 1 1 Nothing emptyHomeModInfoLinkable)
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
  self <- getExecutablePath
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
          installed <- liftIO (installExactLexicalGraph graph lexical env >>= either fail pure)
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
ghcLibdir = reverse . dropWhile (`elem` "\r\n") . reverse <$> readProcess "ghc" ["--print-libdir"] ""

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
  ["Common", "Old", "Public", "Conflict", "AssociatedConflict", "DataConflict", "InjectiveConflict", "Consumer", "BadClass", "BadFamily", "BadAssociated", "BadFD", "Next", "NextConsumer", "BadRetraction", "BadNextClass", "BadNextFamily"]

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
