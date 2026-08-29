-- | Translation output, metadata validation, and artifact-set emission.
--
-- This module owns the compiler-specific artifact contract. Request dispatch
-- and process transport stay outside it; callers provide an already-compiled
-- Core program and choose which targets to emit.
module Tidepool.Artifacts
  ( cborFileName
  , pruneAllClosedArtifacts
  , writeClosedTargets
  , writeWholeModuleClosed
  , runMultiTargetClosed
  , renderAsksJson
  ) where

import Control.Exception (evaluate)
import Control.Monad (foldM, forM, forM_, when)
import qualified Data.ByteString as BS
import Data.List (intercalate, isSuffixOf, nub)
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import qualified Data.Set as Set
import Data.Text (Text)
import qualified Data.Text as T
import Data.Word (Word64)
import Numeric (showHex)
import System.Directory (doesFileExist, listDirectory, removeFile)
import System.FilePath ((</>))
import System.IO (hPutStrLn, stderr)

import GHC.Core (CoreBind)
import GHC.Core.DataCon (DataCon)
import GHC.Driver.Env (HscEnv)

import Tidepool.Binders (renderAskJson)
import Tidepool.CborEncode (encodeMetadata, encodeTree)
import Tidepool.IR (FlatNode)
import Tidepool.Metadata (DCMeta(..))
import Tidepool.Timing (emitPhase, timeSection)
import Tidepool.Translate
  ( ClosedModule(..)
  , UnresolvedVar(..)
  , collectReachableConDCs
  , collectReachableConDCsRaw
  , collectTransitiveDCons
  , dcToMeta
  , emittedConIds
  , mergeMetaPreserving
  , siblingCloseDCons
  , targetBindingHasIO
  , translateModuleClosed
  , wiredInDataCons
  )

-- | Translate one required target. Unresolved externals are fatal; callers
-- implementing a best-effort fixture sweep must catch failures before
-- passing successful translations to 'writeClosedTargets'.
translateTargetClosed :: Bool -> HscEnv -> [CoreBind] -> String -> IO ClosedModule
translateTargetClosed timing hscEnv binds targetName = do
  (closed, translateMs) <- timeSection (translateModuleClosed hscEnv binds targetName)
  emitPhase timing "translate" translateMs
  let ClosedModule { cmUnresolved = unresolved } = closed
  if not (null unresolved) then do
    let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
    error $ "Unresolved external(s): " ++ unwords names
      ++ "\nThese functions don't expose their implementation to the GHC API."
      ++ "\nDefine them in your source or use equivalent inline definitions."
  else return ()
  return closed

-- | Encode a binder name as one filename component. Encoding @%@ as well as
-- @/@ keeps the mapping injective.
artifactFileBase :: String -> FilePath
artifactFileBase = concatMap enc
  where
    enc '/' = "%2F"
    enc '%' = "%25"
    enc c   = [c]

cborFileName :: String -> FilePath
cborFileName name = artifactFileBase name ++ ".cbor"

asksFileName :: String -> FilePath
asksFileName name = artifactFileBase name ++ ".asks.json"

-- | Make a successful @--all-closed@ write an exact snapshot rather than an
-- append-only directory. GHC-generated lifted-local names are unstable under
-- harmless source edits, so leaving files from an earlier run silently grows
-- the differential corpus with bindings the current module no longer emits.
--
-- This is deliberately called only by the @--all-closed@ branch. Explicit
-- @--target@/@--targets@, session, turn, and per-binding modes may share an
-- output directory with caller-owned artifacts and must never prune it.
-- Within an all-closed directory we own only extractor artifacts: @*.cbor@
-- and @*.asks.json@. Other files are preserved. Pruning happens after the
-- complete write succeeds, so a failed translation cannot erase the previous
-- usable corpus.
pruneAllClosedArtifacts :: FilePath -> [String] -> IO ()
pruneAllClosedArtifacts outDir outFileBases = do
  entries <- listDirectory outDir
  let multi = length outFileBases > 1
      expected = Set.fromList $
        ["meta.cbor"]
        ++ map cborFileName outFileBases
        ++ if multi
             then map asksFileName outFileBases
             else if null outFileBases then [] else ["asks.json"]
      isOwnedArtifact name = ".cbor" `isSuffixOf` name || ".asks.json" `isSuffixOf` name
      stale = [name | name <- entries, isOwnedArtifact name, name `Set.notMember` expected]
  forM_ stale $ \name -> do
    let path = outDir </> name
    isFile <- doesFileExist path
    when isFile $ do
      removeFile path
      hPutStrLn stderr $ "  Pruned stale all-closed artifact: " ++ path

data TargetWrite = TargetWrite
  { twOutFileBase :: String
  , twNodeCount   :: Int
  , twCbor        :: BS.ByteString
  , twUsedMeta    :: [DCMeta]
  , twUsedDCs     :: [DataCon]
  , twReachBinds  :: [CoreBind]
  , twVarNames    :: [(Word64, Text)]
  , twHasIO       :: Bool
  , twAskSites    :: [(Word64, Text, [Text])]
  , twPoisoned    :: [(Word64, Text)]
  }

-- | Merge per-target poisoned-symbol labels. A slot is retained only when
-- every target that uses it agrees on the name; slots are target-local, so a
-- conflicting global label would be false information.
mergePoisonedTables :: [[(Word64, Text)]] -> [(Word64, Text)]
mergePoisonedTables tables =
  [ (slot, name) | (slot, [name]) <- Map.toList grouped ]
  where
    grouped = Map.map nub (Map.fromListWith (++) [ (s, [n]) | (s, n) <- concat tables ])

-- | Emit translated targets and their shared metadata atomically with respect
-- to validation: all constructor-coverage checks run before the first write.
-- A single target uses @asks.json@; multiple targets use one
-- @<target>.asks.json@ each so ask sites cannot be associated with the wrong
-- program. Metadata unions constructor and diagnostic information across all
-- targets; @has_io@ is true when any target carries IO.
writeClosedTargets
  :: Bool -> FilePath -> [CoreBind] -> Maybe Text -> [Text]
  -> [(String, String, ClosedModule)]  -- ^ (targetName, outFileBase, closed)
  -> IO [(String, [(Word64, Text, [Text])])]   -- ^ outFileBase -> runLLMTurn sites
writeClosedTargets timing outDir binds mCapturedTy warnTexts targets = do
  let multi = length targets > 1

  -- Tree and metadata encoding share one timing phase.
  (writes, encodeMsTotal) <- foldM (\(acc, msAcc) (targetName, outFileBase, closed) -> do
      let ClosedModule { cmNodes = nodes, cmUsedDCs = usedDCs, cmReachBinds = reachBinds
                        , cmVarNames = varNames, cmRunLLMTurnSites = runLLMTurnSites
                        , cmPoisoned = poisoned
                        } = closed
      (cbor, ms) <- timeSection (evaluate (encodeTree nodes))
      let w = TargetWrite
            { twOutFileBase = outFileBase
            , twNodeCount   = Seq.length nodes
            , twCbor        = cbor
            , twUsedMeta    = map dcToMeta (Map.elems usedDCs)
            , twUsedDCs     = Map.elems usedDCs
            , twReachBinds  = reachBinds
            , twVarNames    = varNames
            , twHasIO       = targetBindingHasIO binds targetName
            , twAskSites    = runLLMTurnSites
            , twPoisoned    = poisoned
            }
      return (acc ++ [w], msAcc + ms)
    ) ([], 0) targets

  -- Metadata is limited to runtime-observable roots: the wired-in floor,
  -- constructors recorded by translation (closed over sibling constructors),
  -- and constructors reachable through boundary types. The validation below
  -- makes this narrower set safe without rerunning translation.
  let allReachBinds  = concatMap twReachBinds writes
      allUsedDCs     = concatMap twUsedDCs writes
      siblingMeta    = siblingCloseDCons allUsedDCs
      transitiveMeta = collectTransitiveDCons allReachBinds
      wiredInMeta    = wiredInDataCons
      allMeta = mergeMetaPreserving
                  [ wiredInMeta, concatMap twUsedMeta writes, siblingMeta, transitiveMeta ]
      hasIO       = or (map twHasIO writes)
      allVarNames = concatMap twVarNames writes
      allPoisoned = mergePoisonedTables (map twPoisoned writes)

  -- Validate every target against the shared metadata before writing.
  forM_ targets $ \(tn, _, closed) ->
    assertMetaCoversEmitted tn (cmNodes closed) (cmReachBinds closed) allMeta

  (metaCbor, metaMs) <- timeSection (evaluate (encodeMetadata allMeta hasIO mCapturedTy allVarNames warnTexts allPoisoned))
  emitPhase timing "cbor_encode" (encodeMsTotal + metaMs)

  -- Write one program per target and one shared metadata file.
  ((), writeMs) <- timeSection $ do
    forM_ writes $ \w -> do
      let outFile = outDir </> cborFileName (twOutFileBase w)
      BS.writeFile outFile (twCbor w)
      hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (twNodeCount w) ++ " nodes, " ++ show (BS.length (twCbor w)) ++ " bytes)"
      when multi $ do
        let asksFile = outDir </> asksFileName (twOutFileBase w)
        writeFile asksFile (renderAsksJson (twAskSites w))
        hPutStrLn stderr $ "  Wrote: " ++ asksFile ++ " (" ++ show (length (twAskSites w)) ++ " sites)"

    let metaFile = outDir </> "meta.cbor"
    BS.writeFile metaFile metaCbor
    hPutStrLn stderr $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

    -- Always materialize the single-target sidecar, including an empty list.
    when (not multi) $ forM_ writes $ \w -> do
      let asksFile = outDir </> "asks.json"
      writeFile asksFile (renderAsksJson (twAskSites w))
      hPutStrLn stderr $ "  Wrote: " ++ asksFile ++ " (" ++ show (length (twAskSites w)) ++ " sites)"
  emitPhase timing "write" writeMs

  return [ (twOutFileBase w, twAskSites w) | w <- writes ]

-- | Enforce the artifact metadata contract before writing.
--
-- Every constructor id present in the emitted IR must have metadata; a miss
-- is fatal. An independent Core visitor also reports broader reachability
-- differences, but those are diagnostic because translation intentionally
-- elides some Core constructs.
assertMetaCoversEmitted :: String -> Seq.Seq FlatNode -> [CoreBind] -> [DCMeta] -> IO ()
assertMetaCoversEmitted targetName nodes reachBinds allMeta = do
  let allMetaIds = Set.fromList (map dcmId allMeta)
      reachableMeta = map dcToMeta (collectReachableConDCs reachBinds)
      -- Use the raw collector for names so diagnostic-only exclusions cannot
      -- hide the identity of an emitted constructor.
      nameById = Map.fromList
        [ (dcmId m, dcmQualName m) | m <- map dcToMeta (collectReachableConDCsRaw reachBinds) ]
      nameOf vid = maybe "<name unresolvable>" T.unpack (Map.lookup vid nameById)
      missingEmitted = Set.toList (emittedConIds nodes `Set.difference` allMetaIds)
  when (not (null missingEmitted)) $ error $
       "artifact metadata contract failed for binder " ++ targetName ++ ": "
    ++ show (length missingEmitted)
    ++ " constructor id(s) reach the wire (FlatNode NCon/FDataAlt) but are "
    ++ "missing from meta.cbor -- the runtime would receive a constructor it "
    ++ "cannot describe:\n"
    ++ unlines [ "  0x" ++ showHex vid "" ++ " " ++ nameOf vid | vid <- missingEmitted ]
  let missingReachable = filter (\m -> not (dcmId m `Set.member` allMetaIds)) reachableMeta
  when (not (null missingReachable)) $ hPutStrLn stderr $
       "artifact metadata reachability diagnostic for binder " ++ targetName ++ ": "
    ++ show (length missingReachable)
    ++ " DataCon(s) found by the independent syntactic Core visitor are "
    ++ "missing from meta.cbor -- the authoritative translation and the "
    ++ "independent collector disagree on reachability (informational only, "
    ++ "does not fail the build -- see assertMetaCoversEmitted's haddock):\n"
    ++ unlines [ "  0x" ++ showHex (dcmId m) "" ++ " " ++ T.unpack (dcmQualName m)
               | m <- missingReachable ]
-- | Translate and emit one target. The compiler binding name and output file
-- base are separate because session scaffolds use a reserved binding while
-- callers still consume @result.cbor@.
writeWholeModuleClosed :: Bool -> FilePath -> HscEnv -> [CoreBind] -> Maybe Text -> [Text] -> String -> String -> IO [(Word64, Text, [Text])]
writeWholeModuleClosed timing outDir hscEnv binds mCapturedTy warnTexts targetName outFileBase = do
  closed <- translateTargetClosed timing hscEnv binds targetName
  results <- writeClosedTargets timing outDir binds mCapturedTy warnTexts [(targetName, outFileBase, closed)]
  case results of
    [(_, sites)] -> return sites
    _ -> error "writeWholeModuleClosed: writeClosedTargets returned an unexpected result shape"

-- | Translate and emit several required targets against one compiler result.
-- Any target failure aborts the operation; best-effort fixture sweeps use the
-- lower-level 'writeClosedTargets' after selecting their survivors.
runMultiTargetClosed :: Bool -> FilePath -> HscEnv -> [CoreBind] -> Maybe Text -> [Text] -> [String] -> IO ()
runMultiTargetClosed timing outDir hscEnv binds mCapturedTy warnTexts targetNames = do
  closedTargets <- forM targetNames $ \name -> do
    closed <- translateTargetClosed timing hscEnv binds name
    return (name, name, closed)
  _ <- writeClosedTargets timing outDir binds mCapturedTy warnTexts closedTargets
  return ()

-- | Encode the ask sites associated with one emitted target.
renderAsksJson :: [(Word64, Text, [Text])] -> String
renderAsksJson sites =
  "[" ++ intercalate "," (map renderAskJson sites) ++ "]"
