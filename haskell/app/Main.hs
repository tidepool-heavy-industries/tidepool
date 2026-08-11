module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, (</>))
import System.Directory (createDirectoryIfMissing)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import qualified Data.Set as Set
import Numeric (showHex)
import Control.Exception (evaluate, try, SomeException, fromException)
import Data.Char (toUpper, isDigit, isAlphaNum, isSpace)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix, intercalate)
import Data.Maybe (fromMaybe, mapMaybe, isJust, listToMaybe)
import Control.Monad (foldM, when, forM, forM_, void)
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
  , TurnKind(..), parseTurnKind
  , TemplateSelector(..), templateSelectorForVerdict, templateSelectorWireName
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
import Tidepool.Translate (translateBinds, translateModuleClosed, ClosedModule(..), DCMeta(..), FlatNode, collectDataCons, collectUsedDataCons, collectTransitiveDCons, emittedConIds, collectReachableConDCs, collectReachableConDCsRaw, wiredInDataCons, mergeMetaPreserving, UnresolvedVar(..), dcToMeta, valueRepArity, mapBang, targetBindingHasIO, stableVarId)
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
  let parsedArgs = parseArgs rawArgs
  -- Read once at process entry (see Tidepool.Timing) and thread down;
  -- TIDEPOOL_TIMING is diagnostic-only and never touches stdout/the emitted
  -- files — see the module doc there and tidepool-harness/src/timing.rs.
  timing <- readTimingEnabled
  -- Harness compilation profile (generic-surface wave item 4, PART 2):
  -- rewrite the target to a pragma-prepended scratch copy BEFORE any mode
  -- dispatch below, so every mode (one-shot, session, turn) sees a plain
  -- file with no pragma-block requirement of its own. See
  -- 'spliceHarnessProfilePragma'.
  args <- if argHarnessProfile parsedArgs
            then spliceHarnessProfilePragma parsedArgs
            else pure parsedArgs
  case argFiles args of
    [] -> do
      hPutStrLn stderr "Usage: tidepool-extract-bin [--output-dir <dir>] [--target <name>] [--targets <a,b,...>] [--include <dir>] [--dump-core] [--harness-profile] [--classify --classify-out <out.json>] [--session-root <dir> --inject-val <mod> ...] [--session-bind --bind-name <occ> --bind-gen <g> --emit-bound-binders <out.json>] [--turn --turn-template <kind>=<file> --turn-out <out.cbor> [--json-output <out.json>] [--turn-verdict <kind>[:<names>]]] <file.hs> ..."
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

-- | Rewrites the FIRST target file (`argFiles`'s head) to a scratch copy
-- with 'harnessProfilePragmaLine' prepended — the harness compilation
-- profile's PART 2 (generic-surface wave item 4). A no-op when 'argFiles'
-- is empty (the usage-banner path handles that separately).
--
-- Splices SOURCE TEXT rather than toggling GHC extension FLAGS on the
-- original file's DynFlags. A prior version of this mechanism did exactly
-- that (per-module 'GHC.Driver.Session.DynFlags' patching inside
-- @Tidepool.GhcPipeline@) and was reverted: a compilation-request cache
-- (@tidepool_runtime::cache@, see @cache.rs@'s @cache_key_salted@) keys on
-- rendered SOURCE BYTES, the target binder, include-directory content
-- fingerprints, and the extract binary's own fingerprint — NOT on CLI
-- flags. A flags-based profile is therefore invisible to any future cached
-- caller: a with-profile and a without-profile compile of the byte-identical
-- source hash to the SAME key and could serve each other's stale CBOR.
-- Splicing the pragma into what actually gets compiled makes the profile a
-- property of the rendered source, cache-safe by construction — the same
-- reason the eval preamble's own LANGUAGE pragma block
-- (@EVAL_PRAGMAS@, tidepool-mcp/src/preamble.rs) is prepended to source
-- text rather than ever applied as a flag. It also collapses what used to
-- be a two-part mechanism (a per-module 'ms_hspp_opts' patch PLUS a
-- downsweep-level module-graph splice — 'depanal' bakes an implicit-Prelude
-- import edge into a module's dependency list using whatever flags were
-- ambient at DOWNSWEEP time, before any later per-module patch can affect
-- it) into this one function: the pragma line is part of the source
-- 'depanal' itself parses, so there is no "downswept under the wrong
-- flags" case to work around.
--
-- The scratch file lands under the resolved output dir, named identically
-- to the original (GHC derives the module name from the filename), so
-- @import@s of it from elsewhere still resolve by the expected name.
-- Diagnostics from a harness-profile compile report line numbers ONE
-- greater than the author's own file (the single prepended pragma line) —
-- a caller wiring this flag into a diagnostics-surfacing path (e.g.
-- tidepool-harness) is responsible for that rebasing, the same way
-- tidepool-mcp's own eval preamble rebases its (much larger) prepended
-- header today.
spliceHarnessProfilePragma :: Args -> IO Args
spliceHarnessProfilePragma args = case argFiles args of
  [] -> pure args
  (file : rest) -> do
    src <- readFile file
    let outDir = fromMaybe (takeDirectory file </> takeBaseName file ++ "_cbor") (argOutDir args)
        scratchPath = outDir </> takeFileName file
    createDirectoryIfMissing True outDir
    writeFile scratchPath (harnessProfilePragmaLine ++ "\n" ++ src)
    pure args { argFiles = scratchPath : rest }

-- | The harness compilation profile's standard extension set, rendered as
-- ONE @{-# LANGUAGE ... #-}@ line — what 'spliceHarnessProfilePragma'
-- prepends to an authored harness module's source, so the author's own file
-- on disk needs no pragma block of its own.
--
-- Mirrors @tidepool_mcp::preamble::EVAL_PRAGMAS@
-- (@tidepool-mcp\/src\/preamble.rs@) — the canonical extension list for "one
-- dialect everywhere" (repo CLAUDE.md). A Haskell string literal and a Rust
-- string constant cannot literally share a source across the language
-- boundary, so keep the two in sync BY HAND on any future change to either;
-- this is deliberately not a third independent copy — see EVAL_PRAGMAS's own
-- haddock for the two Rust-side copies already reconciled into one.
harnessProfilePragmaLine :: String
harnessProfilePragmaLine =
  "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot #-}"

-- | The shared epilogue every dispatch arm ends on: render the fixed-shape
-- JSON diagnostics report to stdout from a captured extraction result, with a
-- human-readable debug copy on stderr, exiting non-zero on failure. A STRICT
-- SUPERSET of every former per-call-site copy (once three were textually
-- identical, byte for byte, modulo comments) — it always keeps the
-- 'SourceError' distinction, including at call sites reached through
-- 'runReportingDiags' where no live GHC session exists to ever throw one:
-- 'fromException' can only take the 'Nothing' branch there, which is exactly
-- what the old parse-only epilogue always did, so folding it in changes
-- nothing observable for those callers.
reportDiags :: Either SomeException () -> IO ()
reportDiags (Left e) = do
  let diags = case fromException e of
        Just (se :: SourceError) -> diagsFromSourceError se
        Nothing                  -> [diagFromException e]
  putStrLn (renderDiagsJson diags)
  -- Debug copy for humans only; stdout (above) is the authoritative machine
  -- contract.
  case fromException e of
    Just (se :: SourceError) -> hPutStrLn stderr ("Compilation failed.\n" ++ show se)
    Nothing -> hPutStrLn stderr $ "Error: " ++ show e
  exitFailure
reportDiags (Right ()) = putStrLn (renderDiagsJson [])

-- | Run an @IO ()@ action that has no GHC 'SourceError' of its own (the parse-only
-- binder-extraction lanes), reporting the fixed-shape JSON diagnostics report on
-- stdout either way via 'reportDiags'. A caught exception always takes
-- 'reportDiags''s 'Nothing' branch (no live GHC session exists at these call
-- sites, so there is never a 'SourceError' to distinguish).
runReportingDiags :: IO () -> IO ()
runReportingDiags act = try act >>= reportDiags

-- | A session-aware turn: any of the @--session-*@ flags are present. Reference
-- turns set @--session-root@ (+ @--inject-val@); bind turns add @--session-bind@.
isSessionMode :: Args -> Bool
isSessionMode args = argSessionBind args || isJust (argSessionRoot args)

data Args = Args
  { argOutDir :: Maybe FilePath
  , argTarget :: Maybe String
  -- --targets mode (explicit multi-target emission,
  -- plans/post-restart/extract-wave/boot/03-targets-prereq.md): several
  -- explicitly-named targets, one merged meta.cbor. Deliberately a SEPARATE
  -- field from 'argTarget' (never sharing its Maybe-String slot) so the
  -- existing --target contract (single name, last flag wins) cannot be
  -- perturbed by this mode's parsing.
  , argTargets :: [String]
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
  -- Harness compilation profile (generic-surface wave item 4, PART 2): the
  -- standard extension set applied to the target module by prepending one
  -- LANGUAGE pragma line to a SCRATCH COPY of its source (see
  -- 'spliceHarnessProfilePragma') — never the author's own file on disk,
  -- and never a GHC FLAG (a compilation-request cache keys on rendered
  -- source bytes, not CLI flags; splicing the extensions into what actually
  -- gets compiled is cache-safe by construction, a flags-based toggle is
  -- not). See Tidepool.Harness.Prelude.
  , argHarnessProfile :: Bool
  }

parseArgs :: [String] -> Args
parseArgs = go (Args Nothing Nothing [] False False False [] []
                     False [] Nothing Nothing [] Nothing
                     False [] Nothing Nothing Nothing
                     False Nothing
                     False)
  where
    go a ("--output-dir" : dir : rest) = go a { argOutDir = Just dir } rest
    go a ("--target" : name : rest) = go a { argTarget = Just name } rest
    go a ("--targets" : ts : rest) = go a { argTargets = argTargets a ++ splitComma ts } rest
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
    go a ("--harness-profile" : rest) = go a { argHarnessProfile = True } rest
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

    if not (null (argTargets args))
      -- Explicit multi-target mode (--targets a,b): takes priority over
      -- --target/--all-closed, which stay untouched below for every other
      -- caller. One runPipeline invocation (already run, above), several
      -- named targets, one merged meta.cbor — see 'runMultiTargetClosed'.
      then runMultiTargetClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts (argTargets args)
      else case (mTarget, argAllClosed args) of
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
              let outFile = outDir </> cborFileName name
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
          let outFile = outDir </> cborFileName name
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

  reportDiags res

-- | Per-target translate step: run 'translateModuleClosed' for ONE target,
-- timed as the "translate" phase, and fail LOUDLY (never skip) if it
-- references an unresolved external. Shared by the single-target write path
-- ('writeWholeModuleClosed') and the multi-target '--targets' mode
-- ('runMultiTargetClosed').
--
-- __STRUCTURAL, not a policy flag.__ This function has NO catch/skip branch
-- at all — it always lets the exception through. @--all-closed@'s
-- skip-on-failure behaviour (@processFile@'s own @(_, True) -> do ...@ arm,
-- above, which wraps its OWN per-binding call to 'translateModuleClosed' in
-- a 'try' and discards a 'Left') is a COMPLETELY SEPARATE function, over a
-- separate loop, that never calls this one. There is no shared traversal
-- with an @if strict then error else skip@ switch for a later refactor to
-- quietly re-unify — strict mode's callers ('runMultiTargetClosed',
-- 'writeWholeModuleClosed') are structurally unable to reach a skip path
-- because none exists on this call graph edge. If you are tempted to fold
-- @--all-closed@'s try-and-skip into this function behind a 'Bool' argument:
-- don't — that reintroduces exactly the failure mode explicit multi-target
-- emission exists to prevent (a requested target silently missing, see
-- plans/post-restart/extract-wave/boot/03-targets-prereq.md), and it would
-- do so with every existing test still green, since the skip would only
-- fire when a caller passes the wrong 'Bool'.
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

-- | One target's write-ready pieces, gathered by 'writeClosedTargets' before
-- the cross-target metadata merge (the merge needs every target's pieces in
-- scope at once, so they can't be written as each target is translated).
-- | A binder name as a FILENAME component. Occ names can contain @/@ — the
-- derived 'Eq' method @/=@ yields a @$c/=_u...@ binder — which @(</>)@-built
-- paths read as a directory separator, so the write dies on a nonexistent
-- subdirectory (observed live: @$c/=_u....cbor: withBinaryFile: does not
-- exist@, surfaced by the first module whose types derive Eq under
-- whole-closure extraction). Percent-encode @/@ (and @%@ so the encoding is
-- injective). Readers that look files up BY NAME (the Rust side's
-- @<target>.cbor@) only ever use caller-chosen target names today; if a
-- target containing @/@ ever appears there, the Rust side must apply this
-- same encoding.
cborFileName :: String -> FilePath
cborFileName name = concatMap enc name ++ ".cbor"
  where
    enc '/' = "%2F"
    enc '%' = "%25"
    enc c   = [c]

data TargetWrite = TargetWrite
  { twOutFileBase :: String
  , twNodeCount   :: Int
  , twCbor        :: BS.ByteString
  , twUsedMeta    :: [DCMeta]
  , twReachBinds  :: [CoreBind]
  , twVarNames    :: [(Word64, Text)]
  , twHasIO       :: Bool
  , twAskSites    :: [(Word64, Text)]
  }

-- | Write step, shared by the single-target write path (a singleton input
-- list — see 'writeWholeModuleClosed') and the multi-target '--targets' mode
-- ('runMultiTargetClosed'): emit one @\<outFileBase\>.cbor@ per
-- @(targetName, outFileBase, ClosedModule)@ triple, plus ONE @meta.cbor@
-- merged across all of them. Returns each target's @outFileBase@ paired with
-- the runLLMTurn/runLLMTurnFork sites it wrote, so a caller that also needs
-- them (the turn mode's rich result) reads them off this one write rather
-- than re-deriving them.
--
-- __meta.cbor multi-target scalar rule__ (decided HERE, the one place every
-- target's metadata is in scope at once — see
-- plans/post-restart/extract-wave/boot/03-targets-prereq.md): @has_io@ is
-- the OR across targets (a turn compiled from ANY IO-carrying target counts
-- as IO-carrying); @var_names@ is the concatenation (bag union) of every
-- target's @cmVarNames@ — both are diagnostic-only (runtime "unresolved
-- variable" error naming, friction #12), so an honest over-approximation is
-- fine and a duplicate id is harmless (the Rust reader keys them in a
-- last-write-wins map). For the single-target case (the singleton input
-- list every existing caller passes) both reduce to exactly that one
-- target's own value, with NO reordering — @or [x] == x@ and
-- @concatMap f [x] == f x@ — so meta.cbor stays byte-for-byte unchanged for
-- every pre-existing caller. DataCon entries merge through
-- 'mergeMetaPreserving', which keeps a genuine (varId, qualified-name)
-- COLLISION as two distinct entries so the loader rejects it loudly rather
-- than one target's copy silently winning over another's — this function
-- must never soften that.
--
-- __asks.json sidecar shape__: exactly ONE target writes the existing flat
-- @\<outDir\>/asks.json@ array, UNCHANGED — the single-target contract every
-- pre-existing caller depends on. MORE THAN ONE target additionally writes
-- @\<outDir\>/\<outFileBase\>.asks.json@ per target (the same flat-array
-- shape, one file each): two targets' runLLMTurn/runLLMTurnFork sites are
-- DIFFERENT, and collapsing them into one file would misroute a hole, so
-- multi-target mode keeps them apart all the way to the Rust reader
-- (@tidepool_harness::compile::compile_turns@, which picks the right shape
-- from the same @targets.len() > 1@ test).
writeClosedTargets
  :: Bool -> FilePath -> [CoreBind] -> [TyCon] -> Maybe Text -> [Text]
  -> [(String, String, ClosedModule)]  -- ^ (targetName, outFileBase, closed)
  -> IO [(String, [(Word64, Text)])]   -- ^ outFileBase -> runLLMTurn sites
writeClosedTargets timing outDir binds tycons mCapturedTy warnTexts targets = do
  let multi = length targets > 1

  -- Encode step: every target's tree, timed together as one accumulated
  -- "cbor_encode" phase alongside the merged meta.cbor encode below (mirrors
  -- the single-target original, which encoded the target tree and meta.cbor
  -- together in one timeSection).
  (writes, encodeMsTotal) <- foldM (\(acc, msAcc) (targetName, outFileBase, closed) -> do
      let ClosedModule { cmNodes = nodes, cmUsedDCs = usedDCs, cmReachBinds = reachBinds
                        , cmVarNames = varNames, cmRunLLMTurnSites = runLLMTurnSites
                        } = closed
      (cbor, ms) <- timeSection (evaluate (encodeTree nodes))
      let w = TargetWrite
            { twOutFileBase = outFileBase
            , twNodeCount   = Seq.length nodes
            , twCbor        = cbor
            , twUsedMeta    = map dcToMeta (Map.elems usedDCs)
            , twReachBinds  = reachBinds
            , twVarNames    = varNames
            , twHasIO       = targetBindingHasIO binds targetName
            , twAskSites    = runLLMTurnSites
            }
      return (acc ++ [w], msAcc + ms)
    ) ([], 0) targets

  -- Merge metadata: TyCon-derived + translation-derived (all targets) +
  -- raw-binding-scan + transitive + wired-in. See the multi-target scalar
  -- rule in this function's doc comment above.
  let allReachBinds  = concatMap twReachBinds writes
      tyconMeta      = collectDataCons tycons
      transitiveMeta = collectTransitiveDCons allReachBinds
      wiredInMeta    = wiredInDataCons
      -- D1-B: 'scanMeta = collectUsedDataCons allReachBinds' is DELIBERATELY
      -- ABSENT here. It was a SECOND full run of the translator over every
      -- reachable RHS, purely to rediscover used DataCons that the
      -- authoritative translation already returns as 'cmUsedDCs'
      -- ('twUsedMeta' below).
      --
      -- Removing it is safe ONLY BECAUSE 'assertMetaCoversEmitted' CHECK A now
      -- HARD-FAILS below on any constructor that reaches the wire without
      -- metadata. Do not re-add it "for safety": the assert is the safety, and
      -- a second translation is a second chance to disagree, not a backstop.
      --
      -- It has now been re-introduced twice — once by a refactor that COPIED
      -- this body into 'writeClosedTargets' while D1-B's deletion applied to
      -- the old location, and once by the textual merge of that refactor.
      -- Neither showed up as a conflict. If you are moving this function,
      -- check this line survived the move.
      -- ('processFile's --all-closed branch keeps its own scanMeta — that path
      -- has no CHECK A and is out of D1-B's scope.)
      --
      -- Highest priority first; mergeMetaPreserving keeps colliding
      -- (same-varId, different-qualified-name) entries distinct so the
      -- loader rejects them loudly instead of one silently winning.
      allMeta = mergeMetaPreserving
                  [ wiredInMeta, tyconMeta, concatMap twUsedMeta writes, transitiveMeta ]
      hasIO       = or (map twHasIO writes)
      allVarNames = concatMap twVarNames writes

  -- D1 defense, re-wired at the extract-wave fold (2026-08-09): the merge of
  -- boot's '--targets' split with spawn-latency's D1-A left
  -- 'assertMetaCoversEmitted' DEFINED BUT UNCALLED, because the single-target
  -- body that used to call it was replaced by the 'writeWholeModuleClosed'
  -- thin wrapper. It belongs HERE — this is now the one write path, so every
  -- target on every mode passes through it.
  --
  -- Placement is load-bearing twice over. BEFORE the write step, because CHECK
  -- A's contract is "not a byte of output is written if the emitted metadata
  -- omits a constructor the program can reference". And BEFORE the encode
  -- timeSection, because forcing 'allMeta' here keeps that cost OUT of
  -- 'cbor_encode' — matching the pre-merge attribution, where the assert was
  -- untimed and was what first forced the metadata thunk.
  --
  -- Per target, against the MERGED metadata: multi-target shares one
  -- meta.cbor, so each target must be covered by the union, not by its own
  -- slice.
  forM_ targets $ \(tn, _, closed) ->
    assertMetaCoversEmitted tn (cmNodes closed) (cmReachBinds closed) allMeta

  (metaCbor, metaMs) <- timeSection (evaluate (encodeMetadata allMeta hasIO mCapturedTy allVarNames warnTexts))
  emitPhase timing "cbor_encode" (encodeMsTotal + metaMs)

  -- Write step: one <outFileBase>.cbor per target, ONE merged meta.cbor, and
  -- the asks sidecar(s) per the shape rule above — all timed together as one
  -- "write" phase (mirrors the single-target original).
  ((), writeMs) <- timeSection $ do
    forM_ writes $ \w -> do
      let outFile = outDir </> twOutFileBase w ++ ".cbor"
      BS.writeFile outFile (twCbor w)
      hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (twNodeCount w) ++ " nodes, " ++ show (BS.length (twCbor w)) ++ " bytes)"
      when multi $ do
        let asksFile = outDir </> twOutFileBase w ++ ".asks.json"
        writeFile asksFile (renderAsksJson (twAskSites w))
        hPutStrLn stderr $ "  Wrote: " ++ asksFile ++ " (" ++ show (length (twAskSites w)) ++ " sites)"

    let metaFile = outDir </> "meta.cbor"
    BS.writeFile metaFile metaCbor
    hPutStrLn stderr $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

    -- runLLMTurn (#R0) sidecar, single-target shape: ALWAYS written (empty
    -- list when the module has no runLLMTurn/runLLMTurnFork sites) — loud
    -- absence beats a silently-missing file for the Rust-side consumer
    -- (segment 30) to distinguish "no sites" from "extract too old".
    when (not multi) $ forM_ writes $ \w -> do
      let asksFile = outDir </> "asks.json"
      writeFile asksFile (renderAsksJson (twAskSites w))
      hPutStrLn stderr $ "  Wrote: " ++ asksFile ++ " (" ++ show (length (twAskSites w)) ++ " sites)"
  emitPhase timing "write" writeMs

  return [ (twOutFileBase w, twAskSites w) | w <- writes ]

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
-- | D1 defense (plans/post-restart/extract-wave/spawn-latency/00-spec.md,
-- codex-review-2026-08-08.md item 7): asserts BEFORE a single byte of this
-- binder's output is written that the emitted metadata covers every
-- constructor id the emitted program can reference.
--
-- CHECK A (primary, hard-fail): every id in @'emittedConIds' nodes@ — what
-- actually reaches the wire — must appear as some @allMeta@ entry's 'dcmId'.
-- Not a warning, not a merge: 'error's, caught by the caller's 'try' exactly
-- like any other extraction failure (nonzero exit, JSON diagnostics on
-- stdout, no result.cbor \/ meta.cbor written). A constructor emitted into
-- the IR but missing from the metadata is exactly the shape of the
-- still-owed garbage-con_tag intermittent: the runtime would receive a
-- constructor it cannot describe.
--
-- CHECK B (independence, DIAGNOSTIC): every DataCon 'collectReachableConDCs'
-- finds — an INDEPENDENT syntactic Core visitor that never calls the
-- translator — is compared against @allMeta@ and any gap is logged loudly to
-- stderr, but does NOT fail extraction. Downgraded from hard-fail (root
-- direction, 2026-08-09): the invariant "every DataCon in reachable Core is
-- in the metadata" is FALSE BY DESIGN — the translator's job legitimately
-- includes NOT translating whole classes of Core (interceptions, elisions,
-- desugarings; e.g. multi-return primop/FFI unboxed-tuple splitting, Case
-- clauses around line 2000, never reaches 'mapAltCon'/'recordDC'). CHECK A
-- never firing alongside a CHECK B gap is the load-bearing evidence that the
-- runtime was never at risk in that case. CHECK B stays wired in — a NEW,
-- previously-unseen divergence class should still surface here — but it no
-- longer blocks a build for an elision that is correct by design. Per the
-- D1 spec, a CHECK B diagnostic is still a REAL FINDING to read and, if it
-- names something outside the categorical unboxed-tuple exclusion below,
-- escalate — never silently ignore.
assertMetaCoversEmitted :: String -> Seq.Seq FlatNode -> [CoreBind] -> [DCMeta] -> IO ()
assertMetaCoversEmitted targetName nodes reachBinds allMeta = do
  let allMetaIds = Set.fromList (map dcmId allMeta)
      reachableMeta = map dcToMeta (collectReachableConDCs reachBinds)
      -- Deliberately NOT built from 'reachableMeta': CHECK A's name lookup
      -- must not inherit CHECK B's exclusions. See
      -- 'Tidepool.Translate.collectReachableConDCsRaw's haddock -- B's
      -- filters are about what B should assert on, A's map is about naming
      -- whatever actually failed, and a multi-element unboxed tuple CAN be
      -- emitted (Translate.hs ~2088-2090), so filtering it out here would
      -- print "<name unresolvable>" for exactly the constructor CHECK A
      -- most needs named.
      nameById = Map.fromList
        [ (dcmId m, dcmQualName m) | m <- map dcToMeta (collectReachableConDCsRaw reachBinds) ]
      nameOf vid = maybe "<name unresolvable>" T.unpack (Map.lookup vid nameById)
      missingEmitted = Set.toList (emittedConIds nodes `Set.difference` allMetaIds)
  when (not (null missingEmitted)) $ error $
       "D1 CHECK A (emitted-metadata subset) FAILED for binder " ++ targetName ++ ": "
    ++ show (length missingEmitted)
    ++ " constructor id(s) reach the wire (FlatNode NCon/FDataAlt) but are "
    ++ "missing from meta.cbor -- the runtime would receive a constructor it "
    ++ "cannot describe:\n"
    ++ unlines [ "  0x" ++ showHex vid "" ++ " " ++ nameOf vid | vid <- missingEmitted ]
  let missingReachable = filter (\m -> not (dcmId m `Set.member` allMetaIds)) reachableMeta
  when (not (null missingReachable)) $ hPutStrLn stderr $
       "D1 CHECK B (independent reachable-Core subset) DIAGNOSTIC for binder " ++ targetName ++ ": "
    ++ show (length missingReachable)
    ++ " DataCon(s) found by the independent syntactic Core visitor are "
    ++ "missing from meta.cbor -- the authoritative translation and the "
    ++ "independent collector disagree on reachability (informational only, "
    ++ "does not fail the build -- see assertMetaCoversEmitted's haddock):\n"
    ++ unlines [ "  0x" ++ showHex (dcmId m) "" ++ " " ++ T.unpack (dcmQualName m)
               | m <- missingReachable ]


--
-- A thin wrapper over 'translateTargetClosed' + 'writeClosedTargets' (a
-- singleton target list) since the multi-target '--targets' mode split this
-- function's original body into those two reusable steps; every existing
-- caller's signature and on-disk output are unchanged.
writeWholeModuleClosed :: Bool -> FilePath -> HscEnv -> [CoreBind] -> [TyCon] -> Maybe Text -> [Text] -> String -> String -> IO [(Word64, Text)]
writeWholeModuleClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetName outFileBase = do
  closed <- translateTargetClosed timing hscEnv binds targetName
  results <- writeClosedTargets timing outDir binds tycons mCapturedTy warnTexts [(targetName, outFileBase, closed)]
  case results of
    [(_, sites)] -> return sites
    _ -> error "writeWholeModuleClosed: writeClosedTargets returned an unexpected result shape"

-- | Explicit multi-target mode (@--targets a,b@,
-- plans/post-restart/extract-wave/boot/03-targets-prereq.md): one GHC
-- pipeline invocation (@binds@\/@tycons@\/@hscEnv@ come from the SAME
-- 'runPipeline' call the caller already made — see 'processFile'), several
-- explicitly-named targets translated independently via
-- 'translateTargetClosed' and written together via 'writeClosedTargets' —
-- one @\<name\>.cbor@ per target, outFileBase == targetName (mirroring
-- @--target@'s existing @foo@ -> @foo.cbor@ contract, just for N names
-- instead of one), plus ONE merged @meta.cbor@.
--
-- Unlike @--all-closed@ (a best-effort fixture sweep that catches a
-- per-binding translate failure and skips it), a target named here is a
-- CONTRACT: 'translateTargetClosed' is called directly inside this 'forM',
-- with no per-target 'try', so ANY bad target's exception propagates out of
-- this whole function uncaught. The caller's own top-level @try@ (in
-- 'processFile', the same one that already turns any extraction failure
-- into the stdout diagnostics report + non-zero exit) is what stops a
-- silently-missing target .cbor from ever reaching a caller.
runMultiTargetClosed :: Bool -> FilePath -> HscEnv -> [CoreBind] -> [TyCon] -> Maybe Text -> [Text] -> [String] -> IO ()
runMultiTargetClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetNames = do
  closedTargets <- forM targetNames $ \name -> do
    closed <- translateTargetClosed timing hscEnv binds name
    return (name, name, closed)
  _ <- writeClosedTargets timing outDir binds tycons mCapturedTy warnTexts closedTargets
  return ()

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
  reportDiags res

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
      KDecl -> do
        tmplFile <- case lookup (templateSelectorWireName SDecl) templates of
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
        -- template kind and skips the session-bind artifacts entirely —
        -- 'templateSelectorForVerdict' mirrors Rust's
        -- 'TemplateSelector::for_verdict' exactly.
        let selector = templateSelectorForVerdict kind (sbBinders sb)
        tmplFile <- case lookup (templateSelectorWireName selector) templates of
          Just f  -> return f
          Nothing -> error ("--turn: no --turn-template for kind " ++ templateSelectorWireName selector)
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
          SBind -> do
            g    <- requireArg "--bind-gen"     (argBindGen args)
            root <- requireArg "--session-root" (argSessionRoot args)
            bbs  <- mkBoundBinders (sbBinders sb) g root result
            return (TBind (map T.pack (sbBinders sb)) 0 bbs asksSites wrapped)
          SBindDiscard -> return (TBind [] 0 [] asksSites wrapped)
          SExpr -> return (TExpr 0 asksSites wrapped)
          SDecl -> error ("--turn: unexpected verdict kind: " ++ templateSelectorWireName selector)
    outFile <- requireArg "--turn-out" (argTurnOut args)
    let cbor = encodeTurnOut turnOut
    BS.writeFile outFile cbor
    hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (BS.length cbor) ++ " bytes)"
    case argJsonOutput args of
      Just jout -> do
        writeFile jout (renderTurnOutJson turnOut)
        hPutStrLn stderr $ "  Wrote: " ++ jout
      Nothing -> return ()
  reportDiags res

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
-- @kind@ goes through 'parseTurnKind', which fails loudly (caught by this
-- mode's surrounding @try@, same as any other extraction failure) on
-- anything but the three wire-name strings the Rust caller ever forwards.
parseTurnVerdictArg :: String -> IO StmtBinders
parseTurnVerdictArg s = case break (== ':') s of
  (kind, "")      -> return (StmtBinders (parseTurnKind kind) [])
  (kind, ':' : ns) -> return (StmtBinders (parseTurnKind kind) (splitComma ns))
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
