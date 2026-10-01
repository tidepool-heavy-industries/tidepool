-- | Input continuity evidence; executable products have separate authority.
module Tidepool.CompileInput (writeCompileInputProof) where

import Codec.CBOR.Encoding
import Codec.CBOR.Write (toStrictByteString)
import Control.Monad (forM, unless)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.List (sortOn)
import Data.Map.Strict qualified as Map
import Data.Set qualified as Set
import Data.Text qualified as T
import GHC.Driver.Env (HscEnv)
import GHC.Unit.Module (ModuleName, mkModuleName)
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
  body <- if not (null boots)
    then pure (list [string "unsupported-boot", array owner boots])
    else do
      unless (Set.fromList (map (mkModuleName . dependencyModuleName) nodes) == Map.keysSet roots) $
        fail "compiler input proof lacks complete checked package-root owners"
      rows <- forM nodes $ \node -> do
        let matches = [source | source <- dependencySources evidence,
              dependencySourcePath source == dependencyModuleSource node]
        source <- case matches of
          [selected] -> pure selected
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
