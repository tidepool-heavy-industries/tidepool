module SourceBootFixtureSupport
  ( writeExecutionScope, compactInventoryRows, hasIntResultLiteral, withTiming
  , manifest, writeManifestFor, originalCompilerInput, digest, withScratch, preparedNames ) where

import Codec.CBOR.Term (Term(..), encodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (bracket)
import Control.Monad (foldM)
import Crypto.Hash.SHA256 qualified as SHA
import Data.ByteString qualified as BS
import Data.Map.Strict qualified as Map
import GHC.Core qualified as Core
import GHC.Driver.Env (HscEnv(..))
import GHC.Driver.Session (importPaths)
import GHC.Tc.Types (tcg_mod)
import GHC.Types.Literal (Literal(..), LitNumType(..))
import GHC.Types.Name (getOccString)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import GenuineCandidateFixture (writeGenuineCandidateManifestFor, writeGenuineExecutionScope)
import Numeric (showHex)
import System.Directory (getTemporaryDirectory, removeFile, createDirectory, removeDirectoryRecursive)
import System.Environment (lookupEnv, setEnv, unsetEnv)
import System.FilePath ((</>))
import System.IO (openTempFile, hClose)
import Tidepool.DependencyEvidence (DependencyEvidence(..), DependencyModule(..))
import Tidepool.GhcPipeline (PreparedPipelineResult(..), PipelineResult(..))
import Tidepool.PreparedStg (PreparedModule(..))

writeExecutionScope :: FilePath -> FilePath -> PreparedPipelineResult -> [String] -> IO ()
writeExecutionScope path work original lexicalNames = do
  (source, roots) <- originalCompilerInput original
  let nativeOwners = map (moduleNameString . moduleName . pmModule) (pprModules original)
  writeGenuineExecutionScope nativeOwners lexicalNames work source roots path original


data FixtureInventory = FixtureInventory
  { fixtureSymbols :: Map.Map BS.ByteString Int
  , fixtureSymbolRows :: [Term]
  , fixtureGlobals :: Map.Map BS.ByteString Int
  , fixtureGlobalRows :: [Term]
  }

compactInventoryRows :: [Term] -> Either String (Term,Term,[Term])
compactInventoryRows rows = do
  (inventory,compact) <- mapFixtureInventory compactRow empty rows
  pure (TList (reverse (fixtureSymbolRows inventory)),TList (reverse (fixtureGlobalRows inventory)),compact)
  where
    empty = FixtureInventory Map.empty [] Map.empty []
    compactRow inventory (TList fields) | length fields == 16 = case drop 10 fields of
      TList groups:_ -> do
        (next,compact) <- mapFixtureInventory compactGroup inventory groups
        pure (next,TList (take 10 fields ++ [TList compact] ++ drop 11 fields))
      _ -> Left "fixture candidate lacks groups"
    compactRow _ _ = Left "fixture candidate must have sixteen fields"
    compactGroup inventory (TList [ordinal,TList binders,TList globals]) = do
      (withBinders,binderRefs) <- mapFixtureInventory internFixtureSymbol inventory binders
      (withGlobals,globalRefs) <- mapFixtureInventory internFixtureGlobal withBinders globals
      pure (withGlobals,TList [ordinal,TList binderRefs,TList globalRefs])
    compactGroup _ _ = Left "fixture original group must have three fields"

mapFixtureInventory :: (FixtureInventory -> a -> Either String (FixtureInventory,b))
  -> FixtureInventory -> [a] -> Either String (FixtureInventory,[b])
mapFixtureInventory step initial values = do
  (final,reversed) <- foldM (\(inventory,acc) value -> do
    (next,result) <- step inventory value
    pure (next,result:acc)) (initial,[]) values
  pure (final,reverse reversed)

internFixtureSymbol :: FixtureInventory -> Term -> Either String (FixtureInventory,Term)
internFixtureSymbol inventory value@(TList [_,_,_,_,_]) =
  let key = toStrictByteString (encodeTerm value)
  in case Map.lookup key (fixtureSymbols inventory) of
    Just index -> Right (inventory,TInt index)
    Nothing ->
      let index = Map.size (fixtureSymbols inventory)
      in Right (inventory
        { fixtureSymbols = Map.insert key index (fixtureSymbols inventory)
        , fixtureSymbolRows = value:fixtureSymbolRows inventory },TInt index)
internFixtureSymbol _ _ = Left "fixture symbol must have five fields"

internFixtureGlobal :: FixtureInventory -> Term -> Either String (FixtureInventory,Term)
internFixtureGlobal inventory value@(TList [identity,rep,signature,evaluated,generation]) =
  let key = toStrictByteString (encodeTerm value)
  in case Map.lookup key (fixtureGlobals inventory) of
    Just index -> Right (inventory,TInt index)
    Nothing -> do
      (withSymbol,symbolRef) <- internFixtureSymbol inventory identity
      let index = Map.size (fixtureGlobals withSymbol)
      pure (withSymbol
        { fixtureGlobals = Map.insert key index (fixtureGlobals withSymbol)
        , fixtureGlobalRows = TList [symbolRef,rep,signature,evaluated,generation]:fixtureGlobalRows withSymbol },TInt index)
internFixtureGlobal _ _ = Left "fixture global must have five fields"


hasIntResultLiteral :: Integer -> [Core.CoreBind] -> Bool
hasIntResultLiteral expected = any (\case
      Core.NonRec binder rhs -> getOccString binder == "__result" && contains rhs
      Core.Rec bindings -> any (\(binder, rhs) -> getOccString binder == "__result" && contains rhs) bindings)
  where
    contains = \case
      Core.Lit (LitNumber LitNumInt value) -> value == expected
      Core.App function argument -> contains function || contains argument
      Core.Lam _ body -> contains body
      Core.Let binding body -> any (contains . snd) (Core.flattenBinds [binding]) || contains body
      Core.Case scrutinee _ _ alternatives -> contains scrutinee
        || any (\(Core.Alt _ _ rhs) -> contains rhs) alternatives
      Core.Cast body _ -> contains body
      Core.Tick _ body -> contains body
      _ -> False


withTiming :: IO a -> IO a
withTiming action = bracket (lookupEnv "TIDEPOOL_TIMING") restore $ \_ ->
  setEnv "TIDEPOOL_TIMING" "1" >> action
  where restore = maybe (unsetEnv "TIDEPOOL_TIMING") (setEnv "TIDEPOOL_TIMING")


manifest :: FilePath -> FilePath
manifest work = work </> "module-candidates.cbor"


writeManifestFor :: [String] -> FilePath -> PreparedPipelineResult -> IO ()
writeManifestFor names work prepared = do
  (source, roots) <- originalCompilerInput prepared
  writeGenuineCandidateManifestFor names work source roots prepared

originalCompilerInput :: PreparedPipelineResult -> IO (FilePath, [FilePath])
originalCompilerInput prepared = do
  let result = pprPipelineResult prepared
      target = tcg_mod (prTargetTcGblEnv result)
      name = moduleNameString (moduleName target)
      unit = unitString (moduleUnit target)
  source <- case [dependencyModuleSource node | node <- dependencyModules (pprDependencies prepared)
      , dependencyModuleUnit node == unit, dependencyModuleName node == name
      , not (dependencyModuleBoot node)] of
    [path] -> pure path
    _ -> fail "fixture compiler input has no unique captured target source"
  pure (source, importPaths (hsc_dflags (prHscEnv result)))


digest :: BS.ByteString -> String
digest = concatMap (\byte -> let text = showHex byte ""
  in replicate (2 - length text) '0' ++ text) . BS.unpack . SHA.hash


withScratch :: (FilePath -> IO a) -> IO a
withScratch action = bracket
  (do root <- getTemporaryDirectory
      (path, handle) <- openTempFile root "tidepool-source-boot-test"
      hClose handle
      removeFile path
      createDirectory path
      pure path)
  removeDirectoryRecursive action

preparedNames :: PreparedPipelineResult -> [String]
preparedNames = map (moduleNameString . moduleName . pmModule) . pprModules
