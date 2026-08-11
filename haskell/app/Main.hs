module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, (</>))
import System.Directory (createDirectoryIfMissing)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import qualified Data.Set as Set
import Numeric (showHex, readHex)
import Control.Exception (evaluate, try, SomeException, fromException)
import Data.Char (toUpper, isAlphaNum, isSpace, isDigit)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix, intercalate, nub)
import Data.Maybe (fromMaybe, mapMaybe, isJust, listToMaybe)
import Control.Monad (foldM, when, forM, forM_, void)
import Data.IORef (newIORef, modifyIORef', readIORef)
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
  , renderBoundBinderJson, renderAskJson, renderVerdictsJson )
import Tidepool.GhcPipeline
  ( runPipeline, runPipelineSession, PipelineResult(..), dumpCore
  , stripMonadHead, isClosureType, renderType, splitTupleType
  , BatchItem(..), BatchItemResult(..), runBatchPipeline )
import Tidepool.DiagJson (Diag(..), diagsFromSourceError, diagFromException, renderDiagsJson, renderDiag)
import Tidepool.Json (jsonString)
import Tidepool.Session
  ( SessionScope(..), SessionModule(..), SessionModuleKind(..), Generation(..)
  , sessionModuleString, parseSessionModule, sessionBinderName
  , mkThinSessionIface, writeSessionIface
  , scaffoldTargetName, scaffoldOutputBase )
import Tidepool.Translate (translateBinds, translateModuleClosed, ClosedModule(..), DCMeta(..), FlatNode, collectDataCons, collectUsedDataCons, collectTransitiveDCons, siblingCloseDCons, emittedConIds, collectReachableConDCs, collectReachableConDCsRaw, wiredInDataCons, mergeMetaPreserving, UnresolvedVar(..), dcToMeta, valueRepArity, mapBang, targetBindingHasIO, stableVarId)
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
  case () of
    -- Turn-batch mode (plans/post-restart/batch-turns-feasibility.md §8): N
    -- item compiles in one GHC session. No positional file at all (the plan
    -- is a flag, not argFiles) — checked FIRST, ahead of the argFiles
    -- dispatch every other mode shares.
    _ | isJust (argTurnBatch args) -> runTurnBatchMode args
    _ -> case argFiles args of
      [] -> do
        hPutStrLn stderr "Usage: tidepool-extract-bin [--output-dir <dir>] [--target <name>] [--targets <a,b,...>] [--include <dir>] [--dump-core] [--harness-profile] [--classify --classify-out <out.json>] [--session-root <dir> --inject-val <mod> ...] [--session-bind --bind-name <occ> --bind-gen <g> --emit-bound-binders <out.json>] [--turn --turn-template <kind>=<file> --turn-out <out.cbor> [--turn-verdict <kind>[:<names>]]] [--turn-batch <plan.json> --batch-out <dir>] <file.hs> ..."
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
  , argTurnVerdict :: Maybe String
  -- --classify mode (block classify lane, plans/one-spawn-turn-protocol-phase-b.md):
  , argClassify :: Bool
  , argClassifyOut :: Maybe FilePath
  -- --turn-batch mode (plans/post-restart/batch-turns-feasibility.md §8):
  , argTurnBatch :: Maybe FilePath
  , argBatchOut :: Maybe FilePath
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
                     False [] Nothing Nothing
                     False Nothing
                     Nothing Nothing
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
    go a ("--turn-verdict" : v : rest) = go a { argTurnVerdict = Just v } rest
    go a ("--classify" : rest) = go a { argClassify = True } rest
    go a ("--classify-out" : out : rest) = go a { argClassifyOut = Just out } rest
    go a ("--turn-batch" : p : rest) = go a { argTurnBatch = Just p } rest
    go a ("--batch-out" : d : rest) = go a { argBatchOut = Just d } rest
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
        -- The try-and-skip loop stays UPSTREAM of the shared writer (per
        -- 'translateTargetClosed''s haddock — this is a completely separate
        -- function/loop, never folded into it behind a policy flag). It
        -- forces each candidate's CBOR encoding here (not just its
        -- translation) so a lazy-thunk failure (e.g. unsupported FFI calls,
        -- the reason 'evaluate' is used at all) still causes a skip rather
        -- than aborting the whole sweep — 'writeClosedTargets' below has no
        -- per-target skip of its own and re-encodes every survivor for the
        -- actual write.
        closedTargets <- foldM (\acc name -> do
          result <- try $ do
            closed@ClosedModule { cmNodes = nodes, cmUnresolved = unresolved } <- translateModuleClosed hscEnv binds name
            if not (null unresolved) then do
              let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): unresolved external(s): " ++ unwords names
              return Nothing
            else do
              _ <- evaluate (BS.length (encodeTree nodes))
              return (Just closed)
          case result of
            Left (e :: SomeException) -> do
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): " ++ show e
              return acc
            Right Nothing -> return acc
            Right (Just closed) -> return (acc ++ [(name, name, closed)])
          ) [] uniqueNames
        -- Surviving targets hand off to the single write path shared with
        -- --target/--targets: --all-closed now gets 'writeClosedTargets''s
        -- D1 assertMetaCoversEmitted defense, var_names, and the asks.json
        -- sidecar(s) it previously lacked. 'writeClosedTargets' deliberately
        -- omits the second-translation 'scanMeta' scan (D1-B) — do not
        -- re-add it here.
        void $ writeClosedTargets timing outDir binds tycons mCapturedTy warnTexts closedTargets

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
        -- Per-binding mode (original behavior). NOT unified with
        -- 'writeClosedTargets': 'translateBinds' translates each binding
        -- standalone, over a bare 'TransState' with no unresolved-id set and
        -- none of the runLLMTurn interception's aux var ids wired (see its
        -- definition in Translate.hs) — it never runs the
        -- 'resolveExternals'/reachability closure 'translateModuleClosed'
        -- does, so it produces no 'ClosedModule' and structurally has
        -- neither 'cmReachBinds' (what the D1 CHECK A/B walks need) nor any
        -- unresolved/dangling tracking (what 'cmVarNames' is built from).
        -- Routing it through the shared writer would mean rebuilding that
        -- closure machinery here, i.e. changing Translate.hs's translation
        -- semantics for this call site — out of a write-path lane's scope,
        -- and not a real unification if faked. It still gains the two
        -- things its own data honestly supports: a real 'hasIO' (was
        -- hardcoded False) and the asks.json sidecar's loud-absence
        -- contract — sites are structurally always empty on this path,
        -- since 'translateBind' never wires the runLLMTurn interception.
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
            hasIO = any (targetBindingHasIO binds . fst) dedupd
        let metaCbor = encodeMetadata allMeta hasIO mCapturedTy [] warnTexts []
        let metaFile = outDir </> "meta.cbor"
        BS.writeFile metaFile metaCbor
        hPutStrLn stderr $ "  Wrote: " ++ metaFile ++ " (" ++ show (length allMeta) ++ " entries, " ++ show (BS.length metaCbor) ++ " bytes)"

        let asksFile = outDir </> "asks.json"
        writeFile asksFile (renderAsksJson [])
        hPutStrLn stderr $ "  Wrote: " ++ asksFile ++ " (0 sites)"

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
  , twUsedDCs     :: [DataCon]
  , twReachBinds  :: [CoreBind]
  , twVarNames    :: [(Word64, Text)]
  , twHasIO       :: Bool
  , twAskSites    :: [(Word64, Text)]
  , twPoisoned    :: [(Word64, Text)]
  }

-- | Merge the per-target @poisoned@ tables (sentinel identity slot ->
-- qualified name) into the ONE merged meta.cbor. Slots are per-target
-- counters, so two targets can legitimately assign the SAME slot to
-- DIFFERENT externals; a slot whose name is not unanimous is DROPPED rather
-- than guessed, leaving the JIT to report an anonymous kind=4 for it — a
-- missing name is honest, a wrong one is not. Single-target (every
-- pre-existing caller) keeps its table verbatim, in ascending-slot order.
mergePoisonedTables :: [[(Word64, Text)]] -> [(Word64, Text)]
mergePoisonedTables tables =
  [ (slot, name) | (slot, [name]) <- Map.toList grouped ]
  where
    grouped = Map.map nub (Map.fromListWith (++) [ (s, [n]) | (s, n) <- concat tables ])

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
-- every pre-existing caller. The @poisoned@ table merges through
-- 'mergePoisonedTables' (slots are per-target, so an ambiguous slot is
-- dropped rather than guessed). DataCon entries merge through
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
writeClosedTargets timing outDir binds _tycons mCapturedTy warnTexts targets = do
  let multi = length targets > 1

  -- Encode step: every target's tree, timed together as one accumulated
  -- "cbor_encode" phase alongside the merged meta.cbor encode below (mirrors
  -- the single-target original, which encoded the target tree and meta.cbor
  -- together in one timeSection).
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

  -- Merge metadata: D2's RuntimeTypeClosure — runtime-observable roots only,
  -- no reachability-blind mg_tcs sweep. Three sources, all scoped to what
  -- this compile can actually observe at runtime:
  --   * wired-in: the small fixed floor (Bool/Int/tuples/...).
  --   * translation-derived (all targets) + its sibling closure: every
  --     DataCon actually built or matched in reachable Core, plus — for each
  --     such DataCon's parent TyCon — every OTHER constructor of that TyCon,
  --     so Rust can resolve a rendered type name to its full constructor set
  --     even for a variant this compile's Core never itself constructs (see
  --     'siblingCloseDCons').
  --   * transitive: the binder-type closure over the reachable binds
  --     ('collectTransitiveDCons') — target/result + boundary +
  --     session-bound types, and (per
  --     plans/post-restart/extract-wave/spawn-latency/04-turn-latency-plan.md's
  --     root ruling) the ONLY route by which the five freer-simple
  --     scaffolding constructors (Val/E/Union/Leaf/Node) are ever supplied,
  --     since they live in an external package and can never appear in any
  --     home module's mg_tcs. Do NOT replace or bypass this closure.
  --
  -- The home-TyCon sweep ('collectDataCons' over mg_tcs, formerly
  -- @tyconMeta@) is deliberately GONE from this merge: it swept every
  -- constructor of every home-module TyCon regardless of whether this
  -- compile ever touches it (measured 6.8:1 / 11.1:1 over-collection,
  -- plans/post-restart/extract-wave/spawn-latency/03-d2-handoff.md), and it
  -- structurally cannot supply anything the two sources above don't already
  -- cover for a runtime-observable root. Safe to remove ONLY because
  -- 'assertMetaCoversEmitted' CHECK A (below) hard-fails extraction the
  -- moment a narrowed table under-covers what the emitted program actually
  -- references — see D1. ('processFile's un-unified per-binding mode keeps
  -- its own unfiltered 'collectDataCons' call: it has no CHECK A, no
  -- 'cmReachBinds', and is not on any production path — narrowing it has no
  -- detector, so it is deliberately out of this change's scope.)
  --
  -- Highest priority first; mergeMetaPreserving keeps colliding
  -- (same-varId, different-qualified-name) entries distinct so the
  -- loader rejects them loudly instead of one silently winning.
  let allReachBinds  = concatMap twReachBinds writes
      allUsedDCs     = concatMap twUsedDCs writes
      siblingMeta    = siblingCloseDCons allUsedDCs
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
      allMeta = mergeMetaPreserving
                  [ wiredInMeta, concatMap twUsedMeta writes, siblingMeta, transitiveMeta ]
      hasIO       = or (map twHasIO writes)
      allVarNames = concatMap twVarNames writes
      allPoisoned = mergePoisonedTables (map twPoisoned writes)

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

  (metaCbor, metaMs) <- timeSection (evaluate (encodeMetadata allMeta hasIO mCapturedTy allVarNames warnTexts allPoisoned))
  emitPhase timing "cbor_encode" (encodeMsTotal + metaMs)

  -- Write step: one <outFileBase>.cbor per target, ONE merged meta.cbor, and
  -- the asks sidecar(s) per the shape rule above — all timed together as one
  -- "write" phase (mirrors the single-target original).
  ((), writeMs) <- timeSection $ do
    forM_ writes $ \w -> do
      let outFile = outDir </> cborFileName (twOutFileBase w)
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
      targetName = fromMaybe scaffoldTargetName (argTarget args)
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
    void $ writeWholeModuleClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetName scaffoldOutputBase
    -- BIND turn: capture the bound type, mint+write the thin iface, emit sidecar.
    when (argSessionBind args) (emitBindArtifacts args result)
  reportDiags res

-- | Turn mode (@--turn@, plans/one-spawn-turn-protocol.md): classify the RAW
-- turn text (or accept a caller-supplied @--turn-verdict@), splice the
-- matching template, compile through the EXISTING session-compile path
-- ('runPipelineSession' \/ 'writeWholeModuleClosed'), and write the rich
-- 'TurnOut' result as CBOR (@--turn-out@). A @decl@ verdict never compiles: its
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
        let targetName = fromMaybe scaffoldTargetName (argTarget args)
        asksSites <- writeWholeModuleClosed timing outDir hscEnv binds tycons mCapturedTy warnTexts targetName scaffoldOutputBase
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

--------------------------------------------------------------------------------
-- Turn-batch mode (--turn-batch <plan.json> --batch-out <dir>,
-- plans/post-restart/batch-turns-feasibility.md §8, ratified rulings in
-- §8.1): N item compiles in ONE tidepool-extract spawn, sharing GHC session
-- state (Tidepool.GhcPipeline's ModIfaceCache + per-module dep-guts memo)
-- across items. Every item writes the byte-identical single-turn output set
-- (result.cbor / meta.cbor / asks.json / turn.cbor) into its own
-- <batch-out>/i<k>/ directory — 'writeBatchItemOutput' below reuses the SAME
-- 'writeWholeModuleClosed' / 'mkBoundBinders' / 'encodeTurnOut' calls
-- 'runTurnMode' uses for a single turn, so a batched item cannot diverge in
-- what it writes from a per-item spawn.
--
-- §8.1 rulings this mode is built to (binding, not re-derived here):
--   1. Exits NON-ZERO whenever any item failed (mirrors a single --turn
--      spawn); the Rust caller does not read the exit code, only the stdout
--      document, but shell callers still need a sane code.
--   2. --turn-template / --include are BATCH-WIDE (repeated top-level
--      flags), never per item. A plan item's own "template" field is a
--      SELECTOR (a TemplateSelector wire name) into that shared table, never
--      a file path.
--   3. No per-item --target: every batchable shape compiles the
--      scaffold-reserved default ('scaffoldTargetName').
--   4. The per-item TurnOut sidecar is named literally "turn.cbor" inside
--      <batch-out>/i<k>/, matching run_turn's own convention.
--------------------------------------------------------------------------------

-- | One item's disposition, for the §8 stdout report's @items@ array.
data ItemStatus = ItemOk | ItemFailed [Diag]

-- | One parsed plan.json item (§8's wire shape). @piTemplate@ is a
-- TemplateSelector wire name ("decl"/"bind"/"binddiscard"/"expr") selecting
-- among the BATCH-WIDE @--turn-template@ table (§8.1 ruling 2) — never a
-- file path.
data PlanItem = PlanItem
  { piIndex       :: Int
  , piTurnText    :: String
  , piVerdictKind :: String
  , piBinders     :: [String]
  , piTemplate    :: String
  , piSessionRoot :: FilePath
  , piInjectVals  :: [String]
  , piBindGen     :: Maybe Word64
  }

-- | Parse @plan.json@'s @{"version":1,"items":[...]}@ shape into
-- 'PlanItem's, in order. Hand-rolled (no @aeson@ dependency — matches
-- "Tidepool.Json"'s own rationale: the shape here is small and fixed).
-- Malformed input 'error's, caught by 'runTurnBatchMode''s surrounding
-- 'try' like any other extraction failure.
parsePlanItems :: String -> IO [PlanItem]
parsePlanItems src = case parseJsonValue src of
  Left e -> error ("--turn-batch: malformed plan.json: " ++ e)
  Right v -> case jField "items" v of
    Just (JArr items) -> mapM parseOneItem items
    _ -> error "--turn-batch: plan.json missing an \"items\" array"
  where
    parseOneItem iv = do
      idx      <- reqInt "index" iv
      tt       <- reqStr "turn_text" iv
      verdictV <- reqField "verdict" iv
      kind     <- reqStr "kind" verdictV
      binders  <- case jField "binders" verdictV of
        Just (JArr bs) -> mapM reqJStr bs
        _              -> pure []
      tmpl  <- reqStr "template" iv
      root  <- case jField "session_root" iv of
        Just (JStr s) -> pure s
        _             -> pure ""
      injects <- case jField "inject_vals" iv of
        Just (JArr xs) -> mapM reqJStr xs
        _              -> pure []
      bg <- case jField "bind_gen" iv of
        Just (JNum n) -> pure (Just (fromInteger n))
        _             -> pure Nothing
      pure PlanItem
        { piIndex = fromInteger idx, piTurnText = tt
        , piVerdictKind = kind, piBinders = binders
        , piTemplate = tmpl, piSessionRoot = root
        , piInjectVals = injects, piBindGen = bg
        }
    reqField k iv = maybe (error ("--turn-batch: plan item missing \"" ++ k ++ "\"")) pure (jField k iv)
    reqStr k iv = reqField k iv >>= \v -> maybe (error ("--turn-batch: field \"" ++ k ++ "\" must be a string")) pure (jStr v)
    reqInt k iv = reqField k iv >>= \v -> maybe (error ("--turn-batch: field \"" ++ k ++ "\" must be a number")) pure (jInt v)
    reqJStr v = maybe (error "--turn-batch: expected a string in a JSON array") pure (jStr v)

--------------------------------------------------------------------------------
-- A minimal hand-rolled JSON parser (object/array/string/number/bool/null) —
-- everything plan.json's fixed shape needs, no more. Mirrors "Tidepool.Json"'s
-- own no-aeson-dependency rationale.
--------------------------------------------------------------------------------

data JValue
  = JObj [(String, JValue)]
  | JArr [JValue]
  | JStr String
  | JNum Integer
  | JBool Bool
  | JNull
  deriving (Eq, Show)

jField :: String -> JValue -> Maybe JValue
jField k (JObj kvs) = lookup k kvs
jField _ _           = Nothing

jStr :: JValue -> Maybe String
jStr (JStr s) = Just s
jStr _        = Nothing

jInt :: JValue -> Maybe Integer
jInt (JNum n) = Just n
jInt _        = Nothing

parseJsonValue :: String -> Either String JValue
parseJsonValue s = case pValue s of
  Right (v, rest) | all isSpace rest -> Right v
  Right (_, rest) -> Left ("trailing content: " ++ take 30 rest)
  Left e -> Left e

skipWs :: String -> String
skipWs = dropWhile (\c -> c == ' ' || c == '\t' || c == '\r' || c == '\n')

pValue :: String -> Either String (JValue, String)
pValue s0 = case skipWs s0 of
  ('{' : s) -> pObject s
  ('[' : s) -> pArray s
  ('"' : s) -> do (str, s') <- pStringLit s; pure (JStr str, s')
  s@(c : _) | c == '-' || isDigit c -> pNumber s
  s -> case stripPrefix "true" s of
    Just s' -> Right (JBool True, s')
    Nothing -> case stripPrefix "false" s of
      Just s' -> Right (JBool False, s')
      Nothing -> case stripPrefix "null" s of
        Just s' -> Right (JNull, s')
        Nothing -> Left ("unexpected input: " ++ take 30 s)

pObject :: String -> Either String (JValue, String)
pObject s0 = case skipWs s0 of
  ('}' : s) -> Right (JObj [], s)
  s         -> goPairs s []
  where
    goPairs s acc = do
      (k, s1) <- case skipWs s of
        ('"' : s') -> pStringLit s'
        s'         -> Left ("expected an object key, got: " ++ take 30 s')
      s2      <- expectChar ':' (skipWs s1)
      (v, s3) <- pValue s2
      let acc' = acc ++ [(k, v)]
      case skipWs s3 of
        (',' : s4) -> goPairs s4 acc'
        ('}' : s4) -> Right (JObj acc', s4)
        s4         -> Left ("expected ',' or '}' in object, got: " ++ take 30 s4)

pArray :: String -> Either String (JValue, String)
pArray s0 = case skipWs s0 of
  (']' : s) -> Right (JArr [], s)
  s         -> goItems s []
  where
    goItems s acc = do
      (v, s1) <- pValue s
      let acc' = acc ++ [v]
      case skipWs s1 of
        (',' : s2) -> goItems s2 acc'
        (']' : s2) -> Right (JArr acc', s2)
        s2         -> Left ("expected ',' or ']' in array, got: " ++ take 30 s2)

-- | Parses a string literal's BODY (the caller has already consumed the
-- opening quote), up to and including the closing quote.
pStringLit :: String -> Either String (String, String)
pStringLit = go id
  where
    go acc ('"' : rest)        = Right (acc [], rest)
    go acc ('\\' : c : rest)   = unescape c rest >>= \(ch, rest') -> go (acc . (ch :)) rest'
    go acc (c : rest)          = go (acc . (c :)) rest
    go _   []                  = Left "unterminated string literal"
    unescape 'n' rest  = Right ('\n', rest)
    unescape 't' rest  = Right ('\t', rest)
    unescape 'r' rest  = Right ('\r', rest)
    unescape '"' rest  = Right ('"', rest)
    unescape '\\' rest = Right ('\\', rest)
    unescape '/' rest  = Right ('/', rest)
    unescape 'b' rest  = Right ('\b', rest)
    unescape 'f' rest  = Right ('\f', rest)
    unescape 'u' rest = case splitAt 4 rest of
      (hex, rest') | length hex == 4, [(n, "")] <- readHex hex ->
        Right (toEnum n, rest')
      _ -> Left "bad \\u escape"
    unescape c _ = Left ("bad escape: \\" ++ [c])

pNumber :: String -> Either String (JValue, String)
pNumber s0 =
  let (numStr, rest) = span (\c -> isDigit c || c == '-') s0
  in if null numStr || numStr == "-"
       then Left ("expected a number, got: " ++ take 30 s0)
       else Right (JNum (read numStr), rest)

expectChar :: Char -> String -> Either String String
expectChar c (x : xs) | x == c = Right xs
expectChar c s = Left ("expected '" ++ [c] ++ "', got: " ++ take 30 s)

--------------------------------------------------------------------------------
-- Splicing + module-header renaming
--------------------------------------------------------------------------------

-- | One item's splice-ready pieces: which item shape it is, what module it
-- compiled to (a UNIQUE per-item name, never the template's own literal
-- header — see 'renameModuleHeader'), and everything 'writeBatchItemOutput'
-- needs to render this item's TurnOut sidecar the same way 'runTurnMode'
-- would have for one spawn.
data BatchPlanned = BatchPlanned
  { bpKind        :: TurnKind
  , bpBinders     :: [String]
  , bpModName     :: String
  , bpModulePath  :: FilePath
  , bpOutDir      :: FilePath
  , bpTurnOutPath :: FilePath
  , bpWrapped     :: String
  , bpBindGen     :: Maybe Word64
  , bpSessionRoot :: FilePath
  , bpInjectVals  :: [String]
  }

-- | Splice item @idx@'s template against its turn text, rewrite the module
-- header to a name UNIQUE to this item (@TurnItem<idx>@ — every item may
-- otherwise share the very same template, and hence the very same literal
-- @module X where@ header, which would collide in the shared HPT the moment
-- a second item tried to compile), and write the result under
-- @<batch-out>/i<idx>/@. Mirrors 'runTurnMode''s own @spliceInto@, plus the
-- renaming this mode alone needs (a single-turn spawn never shares a session
-- with a second module of the same name).
planBatchItem :: FilePath -> [(String, FilePath)] -> PlanItem -> IO BatchPlanned
planBatchItem batchOut templates item = do
  let kind       = parseTurnKind (piVerdictKind item)
      binders    = piBinders item
      itemOutDir = batchOut </> ("i" ++ show (piIndex item))
      bindersStr = intercalate ", " binders
      newModName = "TurnItem" ++ show (piIndex item)
  createDirectoryIfMissing True itemOutDir
  tmplFile <- case lookup (piTemplate item) templates of
    Just f  -> return f
    Nothing -> error ("--turn-batch: no --turn-template for kind " ++ piTemplate item
                        ++ " (item " ++ show (piIndex item) ++ ")")
  tmplSrc <- readFile tmplFile
  let spliced0    = spliceTemplate tmplSrc (piTurnText item) bindersStr
      spliced     = renameModuleHeader newModName spliced0
      modulePath  = itemOutDir </> newModName ++ ".hs"
  writeFile modulePath spliced
  pure BatchPlanned
    { bpKind = kind, bpBinders = binders, bpModName = newModName
    , bpModulePath = modulePath, bpOutDir = itemOutDir
    , bpTurnOutPath = itemOutDir </> "turn.cbor"
    , bpWrapped = spliced, bpBindGen = piBindGen item
    , bpSessionRoot = piSessionRoot item, bpInjectVals = piInjectVals item
    }

-- | Replace the module name in a source's FIRST @module <Name> ...@ header
-- line, leaving everything else (an export list, "where", indentation)
-- untouched. Everything after the first match is returned as-is — a
-- well-formed template has exactly one header.
renameModuleHeader :: String -> String -> String
renameModuleHeader newName src = unlines (go (lines src))
  where
    go [] = []
    go (l : ls) =
      let (indent, rest0) = span (== ' ') l
      in case stripPrefix "module " rest0 of
           Just rest1 ->
             let (nameSpace, rest2) = span (== ' ') rest1
                 oldName    = takeWhile (\c -> isAlphaNum c || c == '.' || c == '_') rest2
                 afterName  = drop (length oldName) rest2
             in if null oldName
                  then l : ls
                  else (indent ++ "module " ++ nameSpace ++ newName ++ afterName) : ls
           Nothing -> l : go ls

-- | 'PlanItem'/'BatchPlanned' -> the GhcPipeline-side compile request.
toGhcBatchItem :: BatchPlanned -> BatchItem
toGhcBatchItem bp = case bpKind bp of
  KDecl -> BatchDecl (bpModulePath bp) (bpModName bp)
  _     -> BatchCompile (bpModulePath bp)
             (SessionScope (bpSessionRoot bp) (mapMaybe parseValModule (bpInjectVals bp)))

-- | Write one item's byte-identical single-turn output set — reuses
-- 'writeWholeModuleClosed' / 'mkBoundBinders' / 'encodeTurnOut' exactly as
-- 'runTurnMode' does for a single spawn, so a batched item cannot diverge in
-- what it writes. A decl item writes only @turn.cbor@ (no result.cbor /
-- meta.cbor / asks.json — a decl turn never compiles, matching
-- 'runTurnMode''s KDecl branch).
writeBatchItemOutput :: Bool -> BatchPlanned -> BatchItemResult -> IO ()
writeBatchItemOutput _timing bp (BatchDeclResult items) = do
  let binders = if null (bpBinders bp)
                  then map (T.pack . exportItemName) items
                  else map T.pack (bpBinders bp)
      turnOut = TDecl binders items
  BS.writeFile (bpTurnOutPath bp) (encodeTurnOut turnOut)
writeBatchItemOutput timing bp (BatchCompileResult result) = do
  let binds       = prBinds result
      tycons      = prTyCons result
      hscEnv      = prHscEnv result
      mCapturedTy = fmap T.pack (prCapturedType result)
      warnTexts   = map T.pack (prWarnings result)
  asksSites <- writeWholeModuleClosed timing (bpOutDir bp) hscEnv binds tycons mCapturedTy warnTexts
                 scaffoldTargetName scaffoldOutputBase
  let wrapped = T.pack (bpWrapped bp)
  turnOut <- case (bpKind bp, bpBinders bp) of
    (KBind, ns@(_ : _)) -> do
      g   <- requireArg ("bind_gen (batch item, module " ++ bpModName bp ++ ")") (bpBindGen bp)
      bbs <- mkBoundBinders ns g (bpSessionRoot bp) result
      return (TBind (map T.pack ns) 0 bbs asksSites wrapped)
    (KBind, [])   -> return (TBind [] 0 [] asksSites wrapped)
    (KExpr, _)    -> return (TExpr 0 asksSites wrapped)
    (KDecl, _)    -> error "writeBatchItemOutput: unexpected KDecl on the compile path"
  BS.writeFile (bpTurnOutPath bp) (encodeTurnOut turnOut)

-- | The §8 stdout document: @{"version":1,"diagnostics":[...],"items":[...]}@.
-- @diags@ is the FLAT top-level array — the failing item's diagnostics
-- verbatim, or @[]@ on a clean batch — kept as a strict superset of today's
-- single-report shape so @parse_diag_report@'s exact-match version-1 reader
-- still parses this document and still sees the real error (§8's own
-- no-flag-day migration story).
renderBatchReportJson :: [Diag] -> [(Int, ItemStatus)] -> String
renderBatchReportJson diags items =
  "{\"version\":1,\"diagnostics\":[" ++ intercalate "," (map renderDiag diags)
    ++ "],\"items\":[" ++ intercalate "," (map renderItemStatus items) ++ "]}"
  where
    renderItemStatus (idx, ItemOk) =
      "{\"index\":" ++ show idx ++ ",\"status\":\"ok\",\"dir\":" ++ jsonString ("i" ++ show idx) ++ "}"
    renderItemStatus (idx, ItemFailed ds) =
      "{\"index\":" ++ show idx ++ ",\"status\":\"failed\",\"dir\":" ++ jsonString ("i" ++ show idx)
        ++ ",\"diagnostics\":[" ++ intercalate "," (map renderDiag ds) ++ "]}"

-- | The @--turn-batch <plan.json> --batch-out <dir>@ entry point. Plans every
-- item (splice + unique module rename), runs the shared session via
-- 'runBatchPipeline' (ModIfaceCache + guts memo threaded), writing each
-- item's output the moment its own compile succeeds — so an item BEFORE a
-- mid-batch failure keeps a complete output directory even though the batch
-- stops there (§3's run-until-first-error contract). Exits non-zero whenever
-- any item failed (§8.1 ruling 1); every failure mode (bad args, malformed
-- plan.json, a missing template, a compile failure, a write failure) reports
-- through the SAME §8 JSON document on stdout.
runTurnBatchMode :: Args -> IO ()
runTurnBatchMode args = do
  timing <- readTimingEnabled
  attempt <- timePhase timing "total" $ try $ do
    planPath  <- requireArg "--turn-batch" (argTurnBatch args)
    batchOut  <- requireArg "--batch-out" (argBatchOut args)
    hPutStrLn stderr $ "Processing (turn-batch): " ++ planPath
    createDirectoryIfMissing True batchOut
    planSrc   <- readFile planPath
    items     <- parsePlanItems planSrc
    templates <- mapM parseTurnTemplate (argTurnTemplates args)
    planned   <- mapM (planBatchItem batchOut templates) items
    let ghcItems = map toGhcBatchItem planned
        byIdx    = Map.fromList (zip [0 ..] planned)
    statusRef <- newIORef []
    (nDone, mExc) <- runBatchPipeline (argIncludes args) ghcItems $ \idx result -> do
      let bp = byIdx Map.! idx
      writeBatchItemOutput timing bp result
      modifyIORef' statusRef ((idx, ItemOk) :)
    statuses0 <- reverse <$> readIORef statusRef
    pure (statuses0, nDone, mExc)
  case attempt of
    Left (e :: SomeException) -> do
      putStrLn (renderBatchReportJson (diagOf e) [])
      hPutStrLn stderr ("Error: " ++ show e)
      exitFailure
    Right (statuses0, _nDone, Nothing) ->
      putStrLn (renderBatchReportJson [] statuses0)
    Right (statuses0, nDone, Just e) -> do
      let diags = diagOf e
      putStrLn (renderBatchReportJson diags (statuses0 ++ [(nDone, ItemFailed diags)]))
      hPutStrLn stderr ("Error: " ++ show e)
      exitFailure
  where
    diagOf e = case fromException e of
      Just (se :: SourceError) -> diagsFromSourceError se
      Nothing                  -> [diagFromException e]

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
-- 'SessionModule'. 'Nothing' for any other string — including a well-formed
-- @Lib@ module, since only @Val@ modules are ever passed to @--inject-val@
-- (silently dropped, matching the prior behavior: the runtime only ever
-- passes well-formed Val module names).
parseValModule :: String -> Maybe SessionModule
parseValModule s = case parseSessionModule s of
  Just sm@(SessionModule ValMod _) -> Just sm
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
