-- | Input continuity evidence; executable products have separate authority.
module Tidepool.CompileInput (writeCompileInputProof) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (forM, unless)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.List (sortOn)
import Data.Map.Strict qualified as Map
import Data.Maybe (catMaybes)
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC (ms_mod, ms_textual_imps, ms_srcimps)
import GHC.Builtin.Names (gHC_PRIM)
import GHC.Driver.Env (HscEnv(..))
import GHC.Types.SrcLoc (unLoc)
import GHC.Unit.Finder (FindResult(Found), findImportedModule)
import GHC.Unit.Module (Module, ModuleName, mkModuleName, moduleUnit, moduleName, moduleNameString)
import GHC.Unit.Module.Graph (ModuleGraphNode(..), mgModSummaries')
import GHC.Unit.Types (unitString)
import Numeric (showHex)
import System.FilePath ((</>))
import Tidepool.DependencyEvidence
import Tidepool.PackageWitness

writeCompileInputProof :: FilePath -> HscEnv -> DependencyEvidence
  -> Map.Map ModuleName [PackageImportRoot] -> IO ()
writeCompileInputProof directory env evidence roots = do
  evidenceBytes <- BS.readFile (directory </> "dependencies.json")
  let nodes = sortOn (\node -> (dependencyModuleUnit node, dependencyModuleName node))
        (dependencyModules evidence)
      boots = filter dependencyModuleBoot nodes
      owner node = list [string (dependencyModuleUnit node), string (dependencyModuleName node)]
  wired <- if null boots then directWiredInputs env nodes else pure []
  body <- if not (null boots)
    then pure (list [string "unsupported-boot", array owner boots])
    else case wired of
      (node, primitive):_ -> pure (list [string "unsupported-wired", owner node,
        list [string (unitString (moduleUnit primitive)),
          string (moduleNameString (moduleName primitive))]])
      [] -> do
        let checkedNames = Set.fromList (map (mkModuleName . dependencyModuleName) nodes)
            sources = Map.fromListWith (++)
              [(dependencySourcePath source, [source]) | source <- dependencySources evidence]
        unless (Set.size checkedNames == length nodes && checkedNames == Map.keysSet roots) $
          fail "compiler input proof lacks complete checked package-root owners"
        rows <- forM nodes $ \node -> do
          source <- case Map.lookup (dependencyModuleSource node) sources of
            Just [selected] -> pure selected
            _ -> fail "compiler input proof lacks one checked source digest"
          selected <- maybe (fail "compiler input proof lacks checked roots") pure
            (Map.lookup (mkModuleName (dependencyModuleName node)) roots)
          pure (list [owner node, string (dependencySourceSha256 source), array root selected])
        closure <- packageInputClosure env (concat (Map.elems roots)) >>= either fail pure
        pure (list [string "checked", list rows, array root closure])
  BS.writeFile (directory </> "compiler-inputs.cbor") $ toStrictByteString $
    list [string "TPCINPUT", encodeWord 1, string (digest evidenceBytes), body]
  where
    string = encodeString . T.pack
    list values = encodeListLen (fromIntegral (length values)) <> mconcat values
    array encode values = list (map encode values)
    root value = list (map string
      [packageUnit value, packageModule value, packagePath value, packageSha256 value])
    digest = concatMap (\byte -> let rendered = showHex byte ""
      in replicate (2 - length rendered) '0' ++ rendered) . BS.unpack . SHA.hash

-- The positive GHC resolution decides this unsupported category. Text naming
-- the primitive module merely avoids finder work for unrelated imports.
directWiredInputs :: HscEnv -> [DependencyModule] -> IO [(DependencyModule, Module)]
directWiredInputs env nodes = do
  let summaries = Map.fromListWith (++)
        [((unitString (moduleUnit (ms_mod summary)), moduleNameString (moduleName (ms_mod summary))),
          [summary]) | ModuleNode _ summary <- mgModSummaries' (hsc_mod_graph env)]
  fmap concat $ forM nodes $ \node -> do
    summary <- case Map.lookup (dependencyModuleUnit node, dependencyModuleName node) summaries of
      Just [selected] -> pure selected
      _ -> fail "compiler input proof lacks one checked module summary"
    catMaybes <$> forM (ms_textual_imps summary ++ ms_srcimps summary) (\(qualifier, imported) ->
      if unLoc imported /= moduleName gHC_PRIM then pure Nothing else do
        resolved <- findImportedModule env (unLoc imported) qualifier
        pure $ case resolved of
          Found _ actual | actual == gHC_PRIM -> Just (node, actual)
          _ -> Nothing)
