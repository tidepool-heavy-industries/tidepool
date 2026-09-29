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
  [] -> bracket temporary removeDirectoryRecursive $ \root -> do
    let fixture = "test-cell-splitter/fixtures/declaration-join/exact-isolation"
    forM_ sourceFiles $ \file -> copyFile (fixture </> file) (root </> file)
    libdir <- ghcLibdir
    -- Each defining module is compiled once in its original lexical context.
    -- Old and Public deliberately have incompatible dictionaries; they are
    -- combined only through a selected interface, never an import wrapper.
    forM_ ["Old", "Public", "Conflict"] $ \target ->
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
      exports <- liftIO $ (++) <$> interfaceExports hydrated commonIface <*> interfaceExports hydrated oldIface
      outcome <- liftIO $ buildJoinedInterface hydrated joined (root </> "Joined.hi")
        ifaces exports selected fullFamilies
      liftIO $ case outcome of
        Left rejected -> fail ("sound isolation join rejected: " ++ show rejected)
        Right _ -> pure ()
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
      conflicting <- liftIO (artifact root "Conflict")
      conflictLoaded <- liftIO $ readExactIfaceArtifacts fresh (artifacts ++ [conflicting]) >>= either fail pure
      conflictEnv <- liftIO (hydrateExactScope fresh conflictLoaded)
      let conflictIfaces = map snd conflictLoaded
          conflictFamilies = [exportIdentity (ifFamInstAxiom i) | iface <- conflictIfaces, i <- mi_fam_insts iface]
      conflict <- liftIO $ buildJoinedInterface conflictEnv joined (root </> "ConflictJoin.hi")
        conflictIfaces exports selected conflictFamilies
      liftIO $ case conflict of
        Left (FamilyInstanceConflict, _) -> pure ()
        other -> fail ("hidden family conflict was not rejected: " ++ show other)
    -- No source restoration or original declaration replay in the fresh worker.
    forM_ ["Common", "Old", "Public", "Conflict"] $ \name ->
      renameFile (root </> name ++ ".hs") (root </> name ++ ".hidden")
    executable <- getExecutablePath
    callProcess executable ["--consumer", root]
    putStrLn "declaration join: persisted interface, fresh source-hidden consumer and retained family conflict passed"
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
  unless (output == "(11,19,22,33,True,'p',True,3,'z',11,44)\n")
    (fail ("original dictionary or selected lookup changed: " ++ show output))
  putStrLn "fresh consumer: four hidden-evidence failures and actual old/new dictionary execution passed"

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
  ["Common", "Old", "Public", "Conflict", "Consumer", "BadClass", "BadFamily", "BadAssociated", "BadFD"]

temporary :: IO FilePath
temporary = do
  parent <- getTemporaryDirectory
  (path, handle) <- openTempFile parent "tidepool-exact-declaration-join"
  hClose handle
  removeFile path
  createDirectory path
  pure path
