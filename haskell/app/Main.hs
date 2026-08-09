module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, (</>))
import System.Directory (createDirectoryIfMissing)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Control.Exception (evaluate, try, SomeException, fromException)
import Data.Char (toUpper, isDigit, isAlphaNum, isSpace)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix, intercalate)
import Data.Maybe (fromMaybe, mapMaybe, isJust, listToMaybe)
import Control.Monad (foldM, when, forM_, void)
import System.Exit (exitFailure)
import System.IO (hPutStrLn, stderr, stdout, hSetEncoding, utf8)

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

import Tidepool.Binders
  ( extractBindersNamed
  , extractStmtBinders, classifyBlock, exportItemName
  , StmtBinders(..), TurnOut(..), BoundBinder(..)
  , renderTurnOutJson, renderBoundBinderJson, renderAskJson, renderVerdictsJson )
import Tidepool.GhcPipeline
  ( runPipeline, runPipelineSession, PipelineResult(..), dumpCore
  , stripMonadHead, isClosureType, renderType, splitTupleType )
import Tidepool.DiagJson (diagsFromSourceError, diagFromException, renderDiagsJson)
import Tidepool.Session
  ( SessionScope(..), SessionModule(..), SessionModuleKind(..), Generation(..)
  , sessionModuleString, sessionBinderName
  , mkThinSessionIface, writeSessionIface )
import Tidepool.Translate (translateBinds, translateModuleClosed, ClosedModule(..), DCMeta(..), collectDataCons, collectUsedDataCons, collectTransitiveDCons, wiredInDataCons, mergeMetaPreserving, UnresolvedVar(..), dcToMeta, valueRepArity, mapBang, targetBindingHasIO, stableVarId)
import Tidepool.CborEncode (encodeTree, encodeMetadata, encodeTurnOut)
import Tidepool.Timing (readTimingEnabled, timePhase, timeSection, emitPhase)

-- | Every dispatch arm below prints exactly ONE JSON diagnostics report to
-- stdout before exiting (see 'Tidepool.DiagJson') — empty @diagnostics@ on
-- success, one entry per compile diagnostic on failure. stdout is the
-- authoritative machine contract; stderr carries a human-readable debug copy
-- only.
main :: IO ()
main = do
  hSetEncoding stdout utf8
  rawArgs <- getArgs
  let args = parseArgs rawArgs
  -- Read once at process entry (see Tidepool.Timing) and thread down;
  -- TIDEPOOL_TIMING is diagnostic-only and never touches stdout/the emitted
  -- files — see the module doc there and tidepool-harness/src/timing.rs.
  timing <- readTimingEnabled
  case argFiles args of
    [] -> do
      hPutStrLn stderr "Usage: tidepool-extract-bin [--output-dir <dir>] [--target <name>] [--include <dir>] [--dump-core] [--classify --classify-out <out.json>] [--session-root <dir> --inject-val <mod> ...] [--session-bind --bind-name <occ> --bind-gen <g> --emit-bound-binders <out.json>] [--turn --turn-template <kind>=<file> --turn-out <out.cbor> [--json-output <out.json>] [--turn-verdict <kind>[:<names>]]] <file.hs> ..."
      putStrLn (renderDiagsJson [])
    (file : _)
      -- Block classify lane: every positional file is one item, classified
      -- in ONE GHC session boot. Checked before '--turn' since it reads the
      -- FULL 'argFiles' list rather than just the head.
      | argClassify args                    -> runClassifyMode timing args
      -- Turn mode (one-spawn-per-turn protocol): classify + splice + compile
      -- + rich-result emission, in one process. Checked before 'isSessionMode'
      -- since a bind/expr turn also carries --session-root/--inject-val.
      | argTurn args                        -> runTurnMode args file
      -- Session mode (Wave 3b): bind/reference turn with iface injection +
      -- (for binds) thin-iface write + BoundBinder sidecar. Only the FIRST
      -- file is processed (matching the two guards above) — one invocation,
      -- one stdout report, per the module doc.
      | isSessionMode args                  -> processSessionFile args file
      -- Normal one-shot extraction (byte-identical to historical behaviour).
      | otherwise                           -> timePhase timing "total" (processFile timing args file)

-- | Run an @IO ()@ action that has no GHC 'SourceError' of its own (the parse-only
-- binder-extraction lanes), reporting the fixed-shape JSON diagnostics report on
-- stdout either way. A caught exception always yields 'diagFromException' (no
-- live GHC session exists at these call sites, so there is never a
-- 'SourceError' to distinguish).
runReportingDiags :: IO () -> IO ()
runReportingDiags act = do
  res <- try act
  case res of
    Left (e :: SomeException) -> do
      putStrLn (renderDiagsJson [diagFromException e])
      hPutStrLn stderr ("Error: " ++ show e)
      exitFailure
    Right () -> putStrLn (renderDiagsJson [])

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
  , argIncludes :: [FilePath]
  , argFiles :: [String]
  -- Wave 3b session-eval value binding:
  , argSessionBind :: Bool
  , argBindNames :: [String]
  , argBindGen :: Maybe Word64
  , argSessionRoot :: Maybe FilePath
  , argInjectVals :: [String]
  , argEmitBoundBinders :: Maybe FilePath
  -- --turn mode (one-spawn-per-turn protocol, plans/one-spawn-turn-protocol.md):
  , argTurn :: Bool
  , argTurnTemplates :: [String]
  , argTurnOut :: Maybe FilePath
  , argJsonOutput :: Maybe FilePath
  , argTurnVerdict :: Maybe String
  -- --classify mode (block classify lane, plans/one-spawn-turn-protocol-phase-b.md):
  , argClassify :: Bool
  , argClassifyOut :: Maybe FilePath
  }

parseArgs :: [String] -> Args
parseArgs = go (Args Nothing Nothing False False False [] []
                     False [] Nothing Nothing [] Nothing
                     False [] Nothing Nothing Nothing
                     False Nothing)
  where
    go a ("--output-dir" : dir : rest) = go a { argOutDir = Just dir } rest
    go a ("--target" : name : rest) = go a { argTarget = Just name } rest
    go a ("--dump-core" : rest) = go a { argDumpCore = True } rest
    go a ("--all-closed" : rest) = go a { argAllClosed = True } rest
    go a ("--target-module-only" : rest) = go a { argTargetModuleOnly = True } rest
    go a ("--session-bind" : rest) = go a { argSessionBind = True } rest
    go a ("--bind-name" : n : rest) = go a { argBindNames = argBindNames a ++ [n] } rest
    go a ("--bind-gen" : g : rest) = go a { argBindGen = Just (read g) } rest
    go a ("--session-root" : dir : rest) = go a { argSessionRoot = Just dir } rest
    go a ("--inject-val" : m : rest) = go a { argInjectVals = argInjectVals a ++ [m] } rest
    go a ("--emit-bound-binders" : out : rest) = go a { argEmitBoundBinders = Just out } rest
    go a ("--turn" : rest) = go a { argTurn = True } rest
    go a ("--turn-template" : kv : rest) = go a { argTurnTemplates = argTurnTemplates a ++ [kv] } rest
    go a ("--turn-out" : out : rest) = go a { argTurnOut = Just out } rest
    go a ("--json-output" : out : rest) = go a { argJsonOutput = Just out } rest
    go a ("--turn-verdict" : v : rest) = go a { argTurnVerdict = Just v } rest
    go a ("--classify" : rest) = go a { argClassify = True } rest
    go a ("--classify-out" : out : rest) = go a { argClassifyOut = Just out } rest
    go a ("--include" : dir : rest) = go a { argIncludes = argIncludes a ++ [dir] } rest
    go a (x : rest) = go a { argFiles = argFiles a ++ [x] } rest
    go a [] = a

processFile :: Bool -> Args -> FilePath -> IO ()
processFile timing args path = do
  let mOutDir = argOutDir args
      mTarget = argTarget args
  hPutStrLn stderr $ "Processing: " ++ path
  res <- try $ do
    result <- runPipeline path (argIncludes args)
    let binds = prBinds result
        tycons = prTyCons result
        hscEnv = prHscEnv result
        -- Inferred type of the eval's top expression (the @__user@ binding),
        -- threaded into meta.cbor for the Rust side. Nothing for non-eval
        -- extractions (no @__user@). See GhcPipeline.capturedUserType.
        mCapturedTy = fmap T.pack (prCapturedType result)
        -- Success-path GHC warnings for the target module (empty on a clean
        -- compile). See GhcPipeline.prWarnings.
        warnTexts = map T.pack (prWarnings result)
    hPutStrLn stderr $ "  Top-level bindings: " ++ show (length binds)

    if argDumpCore args
      then hPutStrLn stderr (dumpCore binds)
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
            ClosedModule { cmNodes = nodes, cmUsedDCs = usedDCs
                         , cmUnresolved = unresolved, cmReachBinds = reachBinds
                         } <- translateModuleClosed hscEnv binds name
            if not (null unresolved) then do
              let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): unresolved external(s): " ++ unwords names
              return Nothing
            else do
              let cbor = encodeTree nodes
              -- Force CBOR encoding to surface errors from lazy thunks (e.g. unsupported FFI calls)
              _ <- evaluate (BS.length cbor)
              let outFile = outDir </> name ++ ".cbor"
              BS.writeFile outFile cbor
              hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
              let usedMeta = map dcToMeta (Map.elems usedDCs)
              -- Keyed by (dcid, qname), matching 'tsUsedDCs' and
              -- 'mergeMetaPreserving': a dcid-alone key would let this
              -- cross-target Map.union silently drop one of a colliding pair
              -- (same varId, different qualified name) before it ever reaches
              -- the loud collision-preserving merge below.
              return (Just (Map.fromList [((dcmId entry, dcmQualName entry), entry) | entry <- usedMeta], reachBinds))
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
        let metaCbor = encodeMetadata allMeta hasIO mCapturedTy [] warnTexts
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        hPutStrLn stderr $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

      (Just targetName, False) ->
        -- Whole-module mode: serialize all bindings as nested lets around the
        -- target (shared with the session path; see 'writeWholeModuleClosed').
        -- File base name matches the lookup name here (the general CLI
        -- contract: --target foo produces foo.cbor). This is the branch the
        -- self-iterating harness's full-compile lane actually exercises
        -- (tidepool-harness/src/compile.rs passes --target, never
        -- --all-closed), so it's the one carrying translate/cbor_encode/write
        -- timing.
        void $ writeWholeModuleClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetName targetName

      (Nothing, False) -> do
        -- Per-binding mode (original behavior)
        let translated = translateBinds binds
            dedupd = dedup Map.empty translated
        mapM_ (\(name, nodes) -> do
          let cbor = encodeTree nodes
          let outFile = outDir </> name ++ ".cbor"
          BS.writeFile outFile cbor
          hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"
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
        let metaCbor = encodeMetadata allMeta False mCapturedTy [] warnTexts
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        hPutStrLn stderr $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

  case res of
    Left (e :: SomeException) -> do
      let diags = case fromException e of
            Just (se :: SourceError) -> diagsFromSourceError se
            Nothing                  -> [diagFromException e]
      putStrLn (renderDiagsJson diags)
      -- Debug copy for humans only; stdout (above) is the authoritative
      -- machine contract.
      case fromException e of
        Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
        Nothing -> hPutStrLn stderr $ "Error: " ++ show e
      exitFailure
    Right () -> putStrLn (renderDiagsJson [])

-- | Whole-module closed emission: translate all bindings as nested lets around
-- @targetName@ (the Core-level binding to look up), write its CBOR under
-- @outFileBase@.cbor + the merged DataCon meta. @targetName@ and
-- @outFileBase@ are DELIBERATELY separate parameters: the session path
-- (session-eval wrapper) may compile a binding under a scaffold-reserved
-- name that differs from the file every Rust caller expects (see
-- 'processSessionFile'), while the general whole-module CLI path
-- ('processFile') passes the same string for both, preserving its existing
-- @--target foo@ → @foo.cbor@ contract. Shared by both so the runtime gets
-- identical JIT-able Core either way. Returns the runLLMTurn/runLLMTurnFork
-- @{site, type}@ pairs it wrote to @asks.json@, so a caller that also needs
-- them (the turn mode's rich result) reads them off this one translation
-- rather than re-running 'translateModuleClosed'.
writeWholeModuleClosed :: Bool -> FilePath -> HscEnv -> [CoreBind] -> [TyCon] -> Maybe Text -> [Text] -> String -> String -> IO [(Word64, Text)]
writeWholeModuleClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetName outFileBase = do
  (closed, translateMs) <- timeSection (translateModuleClosed hscEnv binds targetName)
  emitPhase timing "translate" translateMs
  let ClosedModule { cmNodes = nodes, cmUsedDCs = usedDCs, cmUnresolved = unresolved
                    , cmReachBinds = reachBinds, cmVarNames = varNames
                    , cmRunLLMTurnSites = runLLMTurnSites
                    } = closed
  if not (null unresolved) then do
    let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
    error $ "Unresolved external(s): " ++ unwords names
      ++ "\nThese functions don't expose their implementation to the GHC API."
      ++ "\nDefine them in your source or use equivalent inline definitions."
  else return ()

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

  -- 'cbor_encode' forces both ByteStrings here (rather than leaving them as
  -- thunks BS.writeFile forces below) purely so the wire-format-inert timing
  -- can attribute encode vs. write honestly — mirrors the existing
  -- 'evaluate (BS.length cbor)' force in processFile's --all-closed branch,
  -- which forces for the same reason (surfacing lazy-thunk errors early).
  ((cbor, metaCbor), cborMs) <- timeSection $ do
    c <- evaluate (encodeTree nodes)
    m <- evaluate (encodeMetadata allMeta hasIO mCapturedTy varNames warnTexts)
    pure (c, m)
  emitPhase timing "cbor_encode" cborMs

  ((), writeMs) <- timeSection $ do
    let outFile = outDir </> outFileBase ++ ".cbor"
    BS.writeFile outFile cbor
    hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (Seq.length nodes) ++ " nodes, " ++ show (BS.length cbor) ++ " bytes)"

    let metaFile = outDir </> "meta.cbor"
    BS.writeFile metaFile metaCbor
    hPutStrLn stderr $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

    -- runLLMTurn (#R0) sidecar: {site, type} pairs next to meta.cbor, ALWAYS
    -- written (empty list when the module has no runLLMTurn/runLLMTurnFork
    -- sites) — loud absence beats a silently-missing file for the Rust-side
    -- consumer (segment 30) to distinguish "no sites" from "extract too old".
    let asksFile = outDir </> "asks.json"
    writeFile asksFile (renderAsksJson runLLMTurnSites)
    hPutStrLn stderr $ "  Wrote: " ++ asksFile ++ " (" ++ show (length runLLMTurnSites) ++ " sites)"
  emitPhase timing "write" writeMs
  return runLLMTurnSites

-- | A Wave-3b session-eval turn (reference or bind). Compile through
-- 'runPipelineSession' with the live @Val.G<g>@ ifaces injected (so refs to
-- earlier bindings resolve), emit the JIT-able Core for @__result@, and — on a
-- bind turn — capture the bound value's type, write the thin session iface, and
-- emit the BoundBinder sidecar. Non-session extraction stays on 'processFile'.
processSessionFile :: Args -> FilePath -> IO ()
processSessionFile args path = do
  -- The self-iterating harness's full-compile lane never reaches this
  -- session-mode path (compile.rs passes only --target, never
  -- --session-root) — this read is here purely so 'writeWholeModuleClosed'
  -- (shared with 'processFile') behaves identically regardless of caller.
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (session): " ++ path
  let scope = SessionScope
        { ssRoot      = fromMaybe "" (argSessionRoot args)
        , ssValIfaces = mapMaybe parseValModule (argInjectVals args)
        }
      -- The repl wrapper's own compile-target binding is scaffold-reserved
      -- (@__result@, not @result@) so it can never collide with a user's own
      -- chosen bind name promoted into a later turn's session-lib import —
      -- see 'writeWholeModuleClosed''s doc for why the CBOR file it's
      -- written to stays named @result.cbor@ regardless.
      targetName = fromMaybe "__result" (argTarget args)
  res <- try $ do
    result <- runPipelineSession (Just scope) path (argIncludes args)
    let binds  = prBinds result
        tycons = prTyCons result
        hscEnv = prHscEnv result
        mCapturedTy = fmap T.pack (prCapturedType result)
        warnTexts = map T.pack (prWarnings result)
    hPutStrLn stderr $ "  Top-level bindings: " ++ show (length binds)
    if argDumpCore args then hPutStrLn stderr (dumpCore binds) else return ()
    let outDir = case argOutDir args of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir
    -- The JIT-able Core for the target (same emission as whole-module mode).
    -- File base name is always "result" — every Rust session-turn caller
    -- expects result.cbor regardless of the (scaffold-reserved) lookup name.
    void $ writeWholeModuleClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetName "result"
    -- BIND turn: capture the bound type, mint+write the thin iface, emit sidecar.
    when (argSessionBind args) (emitBindArtifacts args result)
  case res of
    Left (e :: SomeException) -> do
      let diags = case fromException e of
            Just (se :: SourceError) -> diagsFromSourceError se
            Nothing                  -> [diagFromException e]
      putStrLn (renderDiagsJson diags)
      -- Debug copy for humans only — see the identical branch in 'processFile'
      -- for why @show se@ is printed even though GHC's logger usually already
      -- did.
      case fromException e of
        Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
        Nothing -> hPutStrLn stderr $ "Error: " ++ show e
      exitFailure
    Right () -> putStrLn (renderDiagsJson [])

-- | Turn mode (@--turn@, plans/one-spawn-turn-protocol.md): classify the RAW
-- turn text (or accept a caller-supplied @--turn-verdict@), splice the
-- matching template, compile through the EXISTING session-compile path
-- ('runPipelineSession' \/ 'writeWholeModuleClosed'), and write the rich
-- 'TurnOut' result — CBOR always (@--turn-out@), JSON rendering too when
-- @--json-output@ is given. A @decl@ verdict never compiles: its
-- 'toDeclItems' come from a whole-module parse over the turn's OWN spliced
-- scratch module ('extractBindersNamed', exact-name match — see
-- @--turn-template decl=<file>@ below), never from the single-statement
-- parse that serves the verdict — a decl-batch caller
-- (@--turn-verdict decl@ over N declarations joined into one module) has no
-- single statement to parse, only the module.
--
-- The template kind selected for a @bind@ verdict is either @bind@ (at least
-- one bound name) or @binddiscard@ (a bind that binds no name, e.g.
-- @_ <- e@) — mirroring the Rust @TemplateSelector@'s four-shape split. A
-- @binddiscard@ turn compiles but is routed entirely around the
-- session-bind artifacts (no @--bind-gen@\/@--session-root@ requirement, no
-- 'mkBoundBinders', no thin-iface write): it runs for effect and discards,
-- so it reaches 'TBind' with empty binders and an empty bound-binder list,
-- same shape a caller already handles for any other zero-binder bind.
runTurnMode :: Args -> FilePath -> IO ()
runTurnMode args path = do
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (turn): " ++ path
  res <- timePhase timing "total" $ try $ do
    turnSrc   <- readFile path
    templates <- mapM parseTurnTemplate (argTurnTemplates args)
    mVerdict  <- traverse parseTurnVerdictArg (argTurnVerdict args)
    -- 'extractStmtBinders' emits no phases of its own (a substep, not a
    -- lane) — this mode times it as the single @classify@ phase, emitted
    -- only on the branch that actually classifies. With @--turn-verdict@
    -- supplied nothing is parsed, and an absent @classify@ row is the
    -- honest report rather than a phantom 0ms line.
    sb        <- maybe (timePhase timing "classify" (extractStmtBinders turnSrc)) return mVerdict
    let outDir     = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (argOutDir args)
        bindersStr = intercalate ", " (sbBinders sb)
        -- Splice @tmplFile@ against the turn text, write the spliced module
        -- to a scratch file under 'outDir', and return it alongside the
        -- module name derived from its own @module X where@ header. The
        -- scratch file's basename must match that header — 'runPipelineSession'
        -- looks up the compiled module by @capitalize (takeBaseName path)@
        -- (GhcPipeline.hs) exactly as 'tidepool_runtime::extract_module_name'
        -- does today for the existing two-spawn wrap_* templates
        -- (session.rs), which this mode's templates carry over unchanged.
        spliceInto :: FilePath -> IO (String, String, FilePath)
        spliceInto tmplFile = do
          tmplSrc <- readFile tmplFile
          let spliced = spliceTemplate tmplSrc turnSrc bindersStr
              modName = fromMaybe "Input" (extractModuleName spliced)
          createDirectoryIfMissing True outDir
          let modulePath = outDir </> modName ++ ".hs"
          writeFile modulePath spliced
          return (spliced, modName, modulePath)
    turnOut <- case sbKind sb of
      "decl" -> do
        tmplFile <- case lookup "decl" templates of
          Just f  -> return f
          Nothing -> error "--turn: no --turn-template for kind decl"
        (_spliced, modName, modulePath) <- spliceInto tmplFile
        items <- extractBindersNamed modulePath (argIncludes args) modName
        let binders = if null (sbBinders sb)
                        then map (T.pack . exportItemName) items
                        else map T.pack (sbBinders sb)
        return (TDecl binders items)
      kind -> do
        -- Four-shape selection (protocol note, "the verdict space has four
        -- shapes, not three"): a bind that binds no name selects its own
        -- template kind and skips the session-bind artifacts entirely,
        -- mirroring Rust's 'TemplateSelector::for_verdict'.
        let selector = if kind == "bind" && null (sbBinders sb) then "binddiscard" else kind
        tmplFile <- case lookup selector templates of
          Just f  -> return f
          Nothing -> error ("--turn: no --turn-template for kind " ++ selector)
        (spliced, _modName, modulePath) <- spliceInto tmplFile
        let scope = SessionScope
              { ssRoot      = fromMaybe "" (argSessionRoot args)
              , ssValIfaces = mapMaybe parseValModule (argInjectVals args)
              }
        result <- runPipelineSession (Just scope) modulePath (argIncludes args)
        let binds       = prBinds result
            tycons      = prTyCons result
            hscEnv      = prHscEnv result
            mCapturedTy = fmap T.pack (prCapturedType result)
            warnTexts   = map T.pack (prWarnings result)
        -- The Core binding to look up. Scaffold-reserved by default, but a
        -- caller whose template names its own target says so with --target
        -- (the same knob 'processSessionFile' honours). The output file base
        -- stays "result" regardless — every Rust caller reads result.cbor.
        let targetName = fromMaybe "__result" (argTarget args)
        asksSites <- writeWholeModuleClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetName "result"
        let wrapped = T.pack spliced
        case selector of
          "bind" -> do
            g    <- requireArg "--bind-gen"     (argBindGen args)
            root <- requireArg "--session-root" (argSessionRoot args)
            bbs  <- mkBoundBinders (sbBinders sb) g root result
            return (TBind (map T.pack (sbBinders sb)) 0 bbs asksSites wrapped)
          "binddiscard" -> return (TBind [] 0 [] asksSites wrapped)
          "expr" -> return (TExpr 0 asksSites wrapped)
          other  -> error ("--turn: unexpected verdict kind: " ++ other)
    outFile <- requireArg "--turn-out" (argTurnOut args)
    let cbor = encodeTurnOut turnOut
    BS.writeFile outFile cbor
    hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (BS.length cbor) ++ " bytes)"
    case argJsonOutput args of
      Just jout -> do
        writeFile jout (renderTurnOutJson turnOut)
        hPutStrLn stderr $ "  Wrote: " ++ jout
      Nothing -> return ()
  case res of
    Left (e :: SomeException) -> do
      let diags = case fromException e of
            Just (se :: SourceError) -> diagsFromSourceError se
            Nothing                  -> [diagFromException e]
      putStrLn (renderDiagsJson diags)
      case fromException e of
        Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
        Nothing -> hPutStrLn stderr $ "Error: " ++ show e
      exitFailure
    Right () -> putStrLn (renderDiagsJson [])

-- | Block classify lane (@--classify@, plans/one-spawn-turn-protocol-phase-b.md):
-- classify EVERY positional file in 'argFiles' with ONE GHC session boot
-- ('classifyBlock'), in argv order, and write the verdicts to
-- @--classify-out@. Serves @tidepool-repl@'s block runner, which segments a
-- block into decl runs before compiling any item and so needs every verdict
-- up front — one spawn for the whole block instead of one classify spawn per
-- item.
runClassifyMode :: Bool -> Args -> IO ()
runClassifyMode timing args =
  timePhase timing "total" $ runReportingDiags $ do
    out      <- requireArg "--classify-out" (argClassifyOut args)
    srcs     <- mapM readFile (argFiles args)
    verdicts <- classifyBlock timing srcs
    writeFile out (renderVerdictsJson verdicts)
    hPutStrLn stderr $ "  Wrote: " ++ out ++ " (" ++ show (length verdicts) ++ " verdicts)"

-- | Parse one raw @--turn-template kind=file@ argument. Validated here (not in
-- 'parseArgs', which stays total) so a malformed flag surfaces through
-- 'runTurnMode''s @try@ as the same JSON diagnostics report every other
-- failure does, not a bare crash.
parseTurnTemplate :: String -> IO (String, FilePath)
parseTurnTemplate kv = case break (== '=') kv of
  (kind, '=' : file) | not (null kind), not (null file) -> return (kind, file)
  _ -> error ("--turn: malformed --turn-template (expected kind=file): " ++ kv)

-- | Parse one raw @--turn-verdict kind[:name,name…]@ argument into the same
-- 'StmtBinders' shape 'extractStmtBinders' would have produced, so the rest of
-- 'runTurnMode' never has to distinguish a supplied verdict from a parsed one.
parseTurnVerdictArg :: String -> IO StmtBinders
parseTurnVerdictArg s = case break (== ':') s of
  (kind, "")      -> return (StmtBinders kind [])
  (kind, ':' : ns) -> return (StmtBinders kind (splitComma ns))
  _               -> error ("--turn: malformed --turn-verdict: " ++ s)

splitComma :: String -> [String]
splitComma s = case break (== ',') s of
  (a, [])       -> [a]
  (a, _ : rest) -> a : splitComma rest

-- | Splice a turn template: literal replacement of @{{TURN}}@ (the raw turn
-- text, verbatim), @{{TURN_STMT}}@ (the turn text placed as a @do@-block
-- statement — see 'placeTurnStmt'), and @{{BINDERS}}@ (the harvested binder
-- names, comma-joined) against the ORIGINAL template text only — a single
-- left-to-right scan, never re-scanning already-spliced text, so a
-- @{{TURN}}@\/@{{TURN_STMT}}@\/@{{BINDERS}}@ marker occurring verbatim inside
-- the turn text itself is never mistaken for a second substitution point.
-- @{{TURN}}@ is not a string prefix of @{{TURN_STMT}}@ (they diverge at the
-- 7th character, @}@ vs @_@), so checking both at every position is
-- unambiguous regardless of order.
spliceTemplate :: String -> String -> String -> String
spliceTemplate tmpl turnText bindersStr = go tmpl
  where
    go s
      | "{{TURN_STMT}}" `isPrefixOf` s = placeTurnStmt turnText ++ go (drop 13 s)
      | "{{TURN}}" `isPrefixOf` s      = turnText ++ go (drop 8 s)
      | "{{BINDERS}}" `isPrefixOf` s   = bindersStr ++ go (drop 11 s)
    go (c : cs) = c : go cs
    go []       = []

-- | Place @turnText@ as a @do@-block statement — the @{{TURN_STMT}}@
-- placement mode. Mirrors Rust's @place_turn_stmt@
-- (@tidepool-runtime/src/session/turn.rs@) and the repl's
-- @push_braced_stmt@ (@tidepool-repl/src/session.rs@) byte for byte: a
-- @let@ turn (at column 1, since a raw turn has no leading indentation)
-- needs explicit decl braces there (a layout @let@ swallows the following
-- @;@), so it is rewritten to @let { <rest> }@; anything else is placed
-- verbatim. Both branches guarantee a trailing newline so a template's own
-- following text always starts on a fresh line.
placeTurnStmt :: String -> String
placeTurnStmt turnText = case letRest of
  Just rest | not ("{" `isPrefixOf` dropWhile isSpace rest) ->
    "let {" ++ rest ++ (if "\n" `isSuffixOf` rest then "" else "\n") ++ " }\n"
  _ ->
    turnText ++ (if "\n" `isSuffixOf` turnText then "" else "\n")
  where
    trimmed = dropWhile isSpace turnText
    letRest = case stripPrefix "let" trimmed of
      Just rest@(c : _) | isSpace c -> Just rest
      _                             -> Nothing

-- | Extract the name from a source's @module X where@ (or @module X (@
-- export-list) header — mirrors @tidepool_runtime::extract_module_name@
-- exactly, so the scratch file this mode writes lands under the SAME name the
-- existing session compile path already derives from a wrap_* template's
-- header.
extractModuleName :: String -> Maybe String
extractModuleName src = listToMaybe
  [ name
  | line <- lines src
  , Just rest <- [stripPrefix "module " (dropWhile (== ' ') line)]
  , let name = takeWhile (\c -> isAlphaNum c || c == '.' || c == '_') (dropWhile (== ' ') rest)
  , not (null name)
  ]

-- | The BIND-turn binder records: the bound value's type @T@ (stripped from
-- @result :: Eff stack T@), the thin @Tidepool.Session.Val.G<g>@ iface carrying
-- all N binders, and one 'BoundBinder' per bound name. For a single name the
-- type @T@ is used directly; for N>1 names @T@ must be an N-tuple and is split
-- into per-component types via 'splitTupleType'. The iface + ids are computed
-- the SAME way a later reference turn recomputes them, so the value plane and
-- type plane agree on one key. Shared by @--session-bind@ ('emitBindArtifacts')
-- and @--turn@'s bind path — one computation, two callers.
mkBoundBinders :: [String] -> Word64 -> FilePath -> PipelineResult -> IO [BoundBinder]
mkBoundBinders bindNames g root result = do
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
        in (BoundBinder name varid modStr tier tdisp, occ, cty)
      built   = zipWith mkEntry bindNames componentTypes
      binders = [ b | (b, _, _) <- built ]
  iface <- mkThinSessionIface hsc sm [(occ, cty) | (_, occ, cty) <- built]
  writeSessionIface hsc root sm iface
  forM_ binders $ \(BoundBinder name varid modStr tier tdisp) ->
    hPutStrLn stderr $ "  Wrote session iface: " ++ modStr ++ " (" ++ name
             ++ " :: " ++ tdisp ++ ", " ++ tier ++ ", varId " ++ show varid ++ ")"
  return binders

-- | The @--session-bind@ artifacts: mint the 'BoundBinder' records via
-- 'mkBoundBinders' and, when requested, write the standalone JSON sidecar.
emitBindArtifacts :: Args -> PipelineResult -> IO ()
emitBindArtifacts args result = do
  bindNames <- case argBindNames args of
    []  -> error "session-bind requires at least one --bind-name"
    ns  -> return ns
  g       <- requireArg "--bind-gen"    (argBindGen args)
  root    <- requireArg "--session-root" (argSessionRoot args)
  binders <- mkBoundBinders bindNames g root result
  case argEmitBoundBinders args of
    Just out -> do
      writeFile out (renderBoundBindersJson binders)
      hPutStrLn stderr $ "  Wrote bound-binder sidecar: " ++ out
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
requireArg flag = maybe (error ("required argument missing: " ++ flag)) return

-- | The BoundBinder JSON sidecar — one record per binder ('renderBoundBinderJson',
-- shared with the 'TBind' rich-result rendering). Handles both single and
-- multi-binder turns (the runtime always reads a @binders@ array).
renderBoundBindersJson :: [BoundBinder] -> String
renderBoundBindersJson binders =
  "{\"binders\":[" ++ intercalate "," (map renderBoundBinderJson binders) ++ "]}"

-- | runLLMTurn/runLLMTurnFork {site, type} pairs (#R0) as the asks.json
-- sidecar: @[{"site": <u32>, "type": "<rendered>"}]@. @site@ is a bare JSON
-- number (extract's own monotonic per-module counter, well inside u32 range).
-- Per-entry rendering ('renderAskJson') is shared with the 'TBind'/'TExpr'
-- rich-result rendering.
renderAsksJson :: [(Word64, Text)] -> String
renderAsksJson sites =
  "[" ++ intercalate "," (map renderAskJson sites) ++ "]"

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
