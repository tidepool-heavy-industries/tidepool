module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, (</>))
import System.Directory (createDirectoryIfMissing)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Control.Exception (evaluate, try, SomeException, fromException)
import Data.Char (toUpper, isDigit)
import Data.List (isPrefixOf, stripPrefix, intercalate)
import Data.Maybe (fromMaybe, mapMaybe, isJust)
import Control.Monad (foldM, when, forM_)
import System.Exit (exitFailure)
import System.IO (hPutStrLn, stderr)

import GHC.Types.SourceError (SourceError)
import GHC (moduleName, moduleNameString, TyCon)
import GHC.Driver.Env (HscEnv)
import GHC.Core (CoreBind, Bind(..))
import GHC.Core.DataCon (DataCon)
import GHC.Types.Name (nameOccName, isExternalName, nameModule_maybe)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString, mkVarOcc)
import GHC.Types.Unique (getKey)
import GHC.Types.Var (varUnique)
import Data.Word (Word64)
import Data.Text (Text)
import qualified Data.Text as T

import Tidepool.Binders (emitBinders, emitStmtBinders)
import Tidepool.GhcPipeline
  ( runPipeline, runPipelineSession, PipelineResult(..), dumpCore
  , stripMonadHead, isClosureType, renderType, splitTupleType )
import Tidepool.Session
  ( SessionScope(..), SessionModule(..), SessionModuleKind(..), Generation(..)
  , sessionModuleString, sessionBinderName
  , mkThinSessionIface, writeSessionIface )
import Tidepool.Translate (translateBinds, translateModuleClosed, collectDataCons, collectUsedDataCons, collectTransitiveDCons, wiredInDataCons, mergeMetaPreserving, UnresolvedVar(..), dcToMeta, valueRepArity, mapBang, targetBindingHasIO, stableVarId)
import Tidepool.CborEncode (encodeTree, encodeMetadata)

main :: IO ()
main = do
  rawArgs <- getArgs
  let args = parseArgs rawArgs
  case argFiles args of
    [] -> putStrLn "Usage: tidepool-harness [--output-dir <dir>] [--target <name>] [--include <dir>] [--dump-core] [--emit-binders <out.json>] [--emit-stmt-binders <out.json>] [--session-root <dir> --inject-val <mod> ...] [--session-bind --bind-name <occ> --bind-gen <g> --emit-bound-binders <out.json>] <file.hs> ..."
    (file : _)
      -- Statement binder extraction (parse-only): bind-vs-expr + bound names
      -- for one session-eval turn. Fast path, no Core pipeline.
      | Just out <- argEmitStmtBinders args -> emitStmtBinders file out
      -- Lane A: parse-only declaration binder extraction for the FIRST file.
      | Just out <- argEmitBinders args     -> emitBinders file (argIncludes args) out
      -- Session mode (Wave 3b): bind/reference turn with iface injection +
      -- (for binds) thin-iface write + BoundBinder sidecar.
      | isSessionMode args                  -> mapM_ (processSessionFile args) (argFiles args)
      -- Normal one-shot extraction (byte-identical to historical behaviour).
      | otherwise                           -> mapM_ (processFile args) (argFiles args)

-- | A session-aware turn: any of the @--session-*@ flags are present. Reference
-- turns set @--session-root@ (+ @--inject-val@); bind turns add @--session-bind@.
isSessionMode :: Args -> Bool
isSessionMode args = argSessionBind args || isJust (argSessionRoot args)

data Args = Args
  { argOutDir :: Maybe FilePath
  , argTarget :: Maybe String
  , argDumpCore :: Bool
  , argAllClosed :: Bool
  , argTargetModuleOnly :: Bool
  , argEmitBinders :: Maybe FilePath
  , argIncludes :: [FilePath]
  , argFiles :: [String]
  -- Wave 3b session-eval value binding:
  , argEmitStmtBinders :: Maybe FilePath
  , argSessionBind :: Bool
  , argBindNames :: [String]
  , argBindGen :: Maybe Word64
  , argSessionRoot :: Maybe FilePath
  , argInjectVals :: [String]
  , argEmitBoundBinders :: Maybe FilePath
  }

parseArgs :: [String] -> Args
parseArgs = go (Args Nothing Nothing False False False Nothing [] []
                     Nothing False [] Nothing Nothing [] Nothing)
  where
    go a ("--output-dir" : dir : rest) = go a { argOutDir = Just dir } rest
    go a ("--target" : name : rest) = go a { argTarget = Just name } rest
    go a ("--dump-core" : rest) = go a { argDumpCore = True } rest
    go a ("--all-closed" : rest) = go a { argAllClosed = True } rest
    go a ("--target-module-only" : rest) = go a { argTargetModuleOnly = True } rest
    go a ("--emit-binders" : out : rest) = go a { argEmitBinders = Just out } rest
    go a ("--emit-stmt-binders" : out : rest) = go a { argEmitStmtBinders = Just out } rest
    go a ("--session-bind" : rest) = go a { argSessionBind = True } rest
    go a ("--bind-name" : n : rest) = go a { argBindNames = argBindNames a ++ [n] } rest
    go a ("--bind-gen" : g : rest) = go a { argBindGen = Just (read g) } rest
    go a ("--session-root" : dir : rest) = go a { argSessionRoot = Just dir } rest
    go a ("--inject-val" : m : rest) = go a { argInjectVals = argInjectVals a ++ [m] } rest
    go a ("--emit-bound-binders" : out : rest) = go a { argEmitBoundBinders = Just out } rest
    go a ("--include" : dir : rest) = go a { argIncludes = argIncludes a ++ [dir] } rest
    go a (x : rest) = go a { argFiles = argFiles a ++ [x] } rest
    go a [] = a

processFile :: Args -> FilePath -> IO ()
processFile args path = do
  let mOutDir = argOutDir args
      mTarget = argTarget args
  putStrLn $ "Processing: " ++ path
  res <- try $ do
    result <- runPipeline path (argIncludes args)
    let binds = prBinds result
        tycons = prTyCons result
        hscEnv = prHscEnv result
        -- Inferred type of the eval's top expression (the @__user@ binding),
        -- threaded into meta.cbor for the Rust side. Nothing for non-eval
        -- extractions (no @__user@). See GhcPipeline.capturedUserType.
        mCapturedTy = fmap T.pack (prCapturedType result)
    putStrLn $ "  Top-level bindings: " ++ show (length binds)

    if argDumpCore args
      then putStrLn (dumpCore binds)
      else return ()

    let outDir = case mOutDir of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir

    case (mTarget, argAllClosed args) of
      (_, True) -> do
        -- All-closed mode: translate each binding independently via translateModuleClosed
        -- Use original names (not deduped) since translateModuleClosed looks up by name.
        -- Skip duplicates (GHC may produce multiple bindings with the same name).
        -- Include all top-level binders, not just External ones.
        -- GHC may mark user-defined bindings as Internal after optimization.
        -- Filter out GHC-generated names (starting with '$').
        -- Errors from translateModuleClosed are caught and those bindings are skipped.
        -- With --target-module-only, restrict fixture emission to binders
        -- DEFINED in the target module (by basename convention, mirroring
        -- GhcPipeline). Dep-module bindings (e.g. quasi-quoter internals
        -- from Tidepool.QQ) still participate in closed translation as
        -- dependencies — they just don't get their own fixtures, keeping
        -- the fixture sweep (and the JIT differential that walks it) to
        -- user-authored bindings.
        let targetModName = capitalizeMod (takeBaseName path)
            keepBinder b
              | not (argTargetModuleOnly args) = True
              | otherwise = case nameModule_maybe (idName b) of
                  Just m  -> moduleNameString (moduleName m) == targetModName
                  Nothing -> True
            allBinders = [ b | bind <- binds
                         , b <- case bind of
                                  NonRec b _ -> [b]
                                  Rec pairs  -> map fst pairs ]
            uniqueNames = Map.keys $ Map.fromList
              [(n, ()) | b <- allBinders
              , keepBinder b
              , let n = occNameString (nameOccName (idName b))
              , not ("$" `isPrefixOf` n)]
        (allMetaMap, allReachBinds) <- foldM (\(acc, reachAcc) name -> do
          result <- try $ do
            (nodes, usedDCs, unresolved, reachBinds) <- translateModuleClosed hscEnv binds name
            if not (null unresolved) then do
              let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
              putStrLn $ "  SKIPPED (" ++ name ++ "): unresolved external(s): " ++ unwords names
              return Nothing
            else do
              let cbor = encodeTree nodes
              -- Force CBOR encoding to surface errors from lazy thunks (e.g. unsupported FFI calls)
              _ <- evaluate (BS.length cbor)
              let outFile = outDir </> name ++ ".cbor"
              BS.writeFile outFile cbor
              putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
              let usedMeta = map dcToMeta (Map.elems usedDCs)
              return (Just (Map.fromList [(dcid, entry) | entry@(dcid, _, _, _, _, _) <- usedMeta], reachBinds))
          case result of
            Left (e :: SomeException) -> do
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): " ++ show e
              return (acc, reachAcc)
            Right Nothing -> return (acc, reachAcc)
            Right (Just (metaMap, reachBinds)) ->
              return (acc `Map.union` metaMap, reachAcc ++ reachBinds)
          ) (Map.empty, []) uniqueNames

        -- Write merged metadata. The scan/transitive walks run over the union
        -- of every target's REACHABLE binds (not the full closed graph), so
        -- they harvest only constructors the emitted fixtures reference. The
        -- meta therefore covers every fixture's needs and nothing else
        -- (quoter-internal Tidepool.QQ.* AST cons and TH machinery vanish).
        let tyconMeta = collectDataCons tycons
            scanMeta = collectUsedDataCons allReachBinds
            transitiveMeta = collectTransitiveDCons allReachBinds
            wiredInMeta = wiredInDataCons
            -- Highest priority first; mergeMetaPreserving keeps colliding
            -- (same-varId, different-qualified-name) entries distinct so the
            -- loader rejects them loudly instead of one silently winning.
            allMeta = mergeMetaPreserving
                        [ wiredInMeta, tyconMeta, Map.elems allMetaMap
                        , scanMeta, transitiveMeta ]
            hasIO = any (targetBindingHasIO binds) uniqueNames
        let metaCbor = encodeMetadata allMeta hasIO mCapturedTy
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

      (Just targetName, False) ->
        -- Whole-module mode: serialize all bindings as nested lets around the
        -- target (shared with the session path; see 'writeWholeModuleClosed').
        writeWholeModuleClosed outDir hscEnv binds tycons mCapturedTy targetName

      (Nothing, False) -> do
        -- Per-binding mode (original behavior)
        let translated = translateBinds binds
            dedupd = dedup Map.empty translated
        mapM_ (\(name, nodes) -> do
          let cbor = encodeTree nodes
          let outFile = outDir </> name ++ ".cbor"
          BS.writeFile outFile cbor
          putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
          ) dedupd

        -- Write DataCon metadata: merge TyCon-derived + usage-derived + transitive + wired-in
        let tyconMeta = collectDataCons tycons
            usedMeta = collectUsedDataCons binds
            transitiveMeta = collectTransitiveDCons binds
            wiredInMeta = wiredInDataCons
            -- Highest priority first; mergeMetaPreserving keeps colliding
            -- (same-varId, different-qualified-name) entries distinct so the
            -- loader rejects them loudly instead of one silently winning.
            allMeta = mergeMetaPreserving
                        [ wiredInMeta, tyconMeta, usedMeta, transitiveMeta ]
        let metaCbor = encodeMetadata allMeta False mCapturedTy
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

  case res of
    Left (e :: SomeException) -> do
      case fromException e of
        -- GHC's default logger ALREADY prints the diagnostics (formatted, with
        -- caret lines) to stderr during `load`, so re-printing `show se` here
        -- just doubled every compile error on the wire — expensive on a channel
        -- whose whole point is token economy. Emit only a terse marker; the
        -- formatted diagnostics above are what callers (tidepool-runtime
        -- ExtractFailed) surface.
        Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
        Nothing -> hPutStrLn stderr $ "Error: " ++ show e
      exitFailure
    Right () -> return ()

-- | Whole-module closed emission: translate all bindings as nested lets around
-- @targetName@, write its CBOR + the merged DataCon meta. Shared by the normal
-- whole-module mode ('processFile') and the Wave-3b session modes
-- ('processSessionFile') so the runtime gets identical JIT-able Core either way.
writeWholeModuleClosed :: FilePath -> HscEnv -> [CoreBind] -> [TyCon] -> Maybe Text -> String -> IO ()
writeWholeModuleClosed outDir hscEnv binds tycons mCapturedTy targetName = do
  (nodes, usedDCs, unresolved, reachBinds) <- translateModuleClosed hscEnv binds targetName
  if not (null unresolved) then do
    let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
    error $ "Unresolved external(s): " ++ unwords names
      ++ "\nThese functions don't expose their implementation to the GHC API."
      ++ "\nDefine them in your source or use equivalent inline definitions."
  else return ()
  let cbor = encodeTree nodes
  let outFile = outDir </> targetName ++ ".cbor"
  BS.writeFile outFile cbor
  putStrLn $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"

  -- Write metadata: merge TyCon-derived + translation-derived + raw-binding-scan + transitive + wired-in
  let tyconMeta = collectDataCons tycons
      usedMeta = map dcToMeta (Map.elems usedDCs)
      scanMeta = collectUsedDataCons reachBinds
      transitiveMeta = collectTransitiveDCons reachBinds
      wiredInMeta = wiredInDataCons
      -- Highest priority first; mergeMetaPreserving keeps colliding
      -- (same-varId, different-qualified-name) entries distinct so the
      -- loader rejects them loudly instead of one silently winning.
      allMeta = mergeMetaPreserving
                  [ wiredInMeta, tyconMeta, usedMeta, scanMeta, transitiveMeta ]
      hasIO = targetBindingHasIO binds targetName
  let metaCbor = encodeMetadata allMeta hasIO mCapturedTy
  let metaFile = outDir </> "meta.cbor"
  BS.writeFile metaFile metaCbor
  putStrLn $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

-- | A Wave-3b session-eval turn (reference or bind). Compile through
-- 'runPipelineSession' with the live @Val.G<g>@ ifaces injected (so refs to
-- earlier bindings resolve), emit the JIT-able Core for @result@, and — on a
-- bind turn — capture the bound value's type, write the thin session iface, and
-- emit the BoundBinder sidecar. Non-session extraction stays on 'processFile'.
processSessionFile :: Args -> FilePath -> IO ()
processSessionFile args path = do
  putStrLn $ "Processing (session): " ++ path
  let scope = SessionScope
        { ssRoot      = fromMaybe "" (argSessionRoot args)
        , ssValIfaces = mapMaybe parseValModule (argInjectVals args)
        }
      targetName = fromMaybe "result" (argTarget args)
  res <- try $ do
    result <- runPipelineSession (Just scope) path (argIncludes args)
    let binds  = prBinds result
        tycons = prTyCons result
        hscEnv = prHscEnv result
        mCapturedTy = fmap T.pack (prCapturedType result)
    putStrLn $ "  Top-level bindings: " ++ show (length binds)
    if argDumpCore args then putStrLn (dumpCore binds) else return ()
    let outDir = case argOutDir args of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir
    -- The JIT-able Core for the target (same emission as whole-module mode).
    writeWholeModuleClosed outDir hscEnv binds tycons mCapturedTy targetName
    -- BIND turn: capture the bound type, mint+write the thin iface, emit sidecar.
    when (argSessionBind args) (emitBindArtifacts args result)
  case res of
    Left (e :: SomeException) -> do
      case fromException e of
        Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
        Nothing -> hPutStrLn stderr $ "Error: " ++ show e
      exitFailure
    Right () -> return ()

-- | The BIND-turn artifacts: the bound value's type @T@ (stripped from
-- @result :: Eff stack T@), the thin @Tidepool.Session.Val.G<g>@ iface carrying
-- all N binders, and the BoundBinder JSON the runtime consumes (N records, one
-- per binder). For a single name the type @T@ is used directly; for N>1 names
-- @T@ must be an N-tuple and is split into per-component types via
-- 'splitTupleType'. The iface + ids are computed the SAME way a later reference
-- turn recomputes them, so the value plane and type plane agree on one key.
emitBindArtifacts :: Args -> PipelineResult -> IO ()
emitBindArtifacts args result = do
  bindNames <- case argBindNames args of
    []  -> error "session-bind requires at least one --bind-name"
    ns  -> return ns
  g    <- requireArg "--bind-gen"    (argBindGen args)
  root <- requireArg "--session-root" (argSessionRoot args)
  effTy <- case prResultType result of
    Just t  -> return t
    Nothing -> error "session-bind: could not capture the type of `result` \
                     \(no such top-level binder typechecked)"
  let hsc   = prHscEnv result
      sm    = SessionModule ValMod (Generation g)
      t     = stripMonadHead effTy          -- Eff stack T -> T
  componentTypes <- case bindNames of
    [_] -> return [t]
    _   -> case splitTupleType t of
      Nothing  -> error $ "multi-binder: bound type is not a tuple: " ++ renderType t
      Just tys ->
        if length tys /= length bindNames
          then error $ "multi-binder: " ++ show (length bindNames) ++ " binders but "
                     ++ "type is a " ++ show (length tys) ++ "-tuple: " ++ renderType t
          else return tys
  let mkEntry name cty =
        let occ    = mkVarOcc name
            varid  = stableVarId (sessionBinderName hsc sm occ)
            modStr = sessionModuleString sm
            tier   = if isClosureType cty then "Tier1Closure" else "Tier0Data"
            tdisp  = renderType cty
        in (name, varid, modStr, tier, tdisp, occ, cty)
      binders = zipWith mkEntry bindNames componentTypes
  iface <- mkThinSessionIface hsc sm [(occ, cty) | (_, _, _, _, _, occ, cty) <- binders]
  writeSessionIface hsc root sm iface
  forM_ binders $ \(name, varid, modStr, tier, tdisp, _, _) ->
    putStrLn $ "  Wrote session iface: " ++ modStr ++ " (" ++ name
             ++ " :: " ++ tdisp ++ ", " ++ tier ++ ", varId " ++ show varid ++ ")"
  case argEmitBoundBinders args of
    Just out -> do
      writeFile out (renderBoundBindersJson binders)
      putStrLn $ "  Wrote bound-binder sidecar: " ++ out
    Nothing -> return ()

-- | Parse a @--inject-val@ module name (@Tidepool.Session.Val.G<n>@) back into a
-- 'SessionModule'. 'Nothing' for any other string (silently dropped — the
-- runtime only ever passes well-formed Val module names).
parseValModule :: String -> Maybe SessionModule
parseValModule s = case stripPrefix "Tidepool.Session.Val.G" s of
  Just gs | not (null gs), all isDigit gs ->
    Just (SessionModule ValMod (Generation (read gs)))
  _ -> Nothing

requireArg :: String -> Maybe a -> IO a
requireArg flag = maybe (error ("session-bind requires " ++ flag)) return

-- | The BoundBinder JSON sidecar — one record per binder. @varId@ is a DECIMAL
-- STRING of the u64 (JSON f64 would lose 64-bit precision). Handles both single
-- and multi-binder turns (the runtime always reads a @binders@ array).
renderBoundBindersJson :: [(String, Word64, String, String, String, a, b)] -> String
renderBoundBindersJson binders =
  "{\"binders\":[" ++ intercalate "," (map renderOne binders) ++ "]}"
  where
    renderOne (name, varid, modul, tier, tdisp, _, _) =
      "{\"name\":" ++ js name
        ++ ",\"varId\":" ++ js (show varid)
        ++ ",\"module\":" ++ js modul
        ++ ",\"tier\":" ++ js tier
        ++ ",\"typeDisplay\":" ++ js tdisp ++ "}"
    js str = '"' : concatMap esc str ++ "\""
    esc '"'  = "\\\""
    esc '\\' = "\\\\"
    esc c    = [c]

-- | Module name from file basename, mirroring GhcPipeline's convention.
capitalizeMod :: String -> String
capitalizeMod [] = []
capitalizeMod (c:cs) = toUpper c : cs

-- | Deduplicate binding names by appending _1, _2, etc. for collisions.
dedup :: Map.Map String Int -> [(String, a)] -> [(String, a)]
dedup _ [] = []
dedup seen ((name, val) : rest) =
  case Map.lookup name seen of
    Nothing -> (name, val) : dedup (Map.insert name 1 seen) rest
    Just n  -> (name ++ "_" ++ show n, val) : dedup (Map.insert name (n + 1) seen) rest
