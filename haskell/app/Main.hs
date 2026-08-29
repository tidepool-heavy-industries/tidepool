module Main where

import System.Environment (getArgs)
import System.FilePath (takeBaseName, takeDirectory, takeFileName, (</>))
import System.Directory (createDirectoryIfMissing, setCurrentDirectory)
import qualified Data.ByteString as BS
import qualified Data.Map.Strict as Map
import qualified Data.Sequence as Seq
import Control.Exception (evaluate, try, SomeException, fromException, toException)
import Data.Char (isAlphaNum, isSpace)
import Data.List (isPrefixOf, isSuffixOf, stripPrefix, intercalate)
import Data.Maybe (fromMaybe, mapMaybe, isJust, listToMaybe)
import Control.Monad (foldM, when, forM_, void)
import System.Exit (ExitCode(..), exitWith)
import System.IO (hPutStrLn, stderr, stdin, stdout, hSetBinaryMode, hSetEncoding, utf8)

import GHC.Types.SourceError (SourceError)
import GHC (moduleName, moduleNameString, Type)
import GHC.Core (Bind(..))
import GHC.Core.Type (splitTyConApp_maybe)
import GHC.Core.TyCon (tyConName)
import GHC.Types.Name (nameOccName, nameModule_maybe, getOccString)
import GHC.Types.Id (idName)
import GHC.Types.Name.Occurrence (occNameString, mkVarOcc)
import Data.Word (Word64)
import qualified Data.Text as T

import Tidepool.Binders
  ( extractBindersNamed
  , extractStmtBinders, classifyBlock, exportItemName
  , TurnKind(..), parseTurnKind
  , TemplateSelector(..), templateSelectorForVerdict, templateSelectorWireName
  , StmtBinders(..), TurnOut(..), BoundBinder(..)
  , renderBoundBinderJson, renderVerdictsJson )
import Tidepool.Artifacts
  ( cborFileName, pruneAllClosedArtifacts, writeClosedTargets
  , writeWholeModuleClosed, runMultiTargetClosed, renderAsksJson )
import Tidepool.GhcPipeline
  ( runPipelineSession, PipelineResult(..), dumpCore
  , stripMonadHead, isClosureType, renderType, splitTupleType
  , withResidentPipeline )
import qualified Tidepool.WorkerServer as WorkerServer
import Tidepool.DiagJson (diagsFromSourceError, diagFromException, renderDiagsJson)
import Tidepool.ExtractUtil (capitalize)
import Tidepool.ExtractRequest (WorkerRequest(..), workerRequestFromArgv)
import Tidepool.Session
  ( SessionScope(..), SessionModule(..), SessionModuleKind(..), Generation(..)
  , sessionModuleString, parseSessionModule, sessionBinderName
  , mkThinSessionIface, writeSessionIface
  , scaffoldTargetName, scaffoldOutputBase )
import Tidepool.Translate
  ( ClosedModule(..), UnresolvedVar(..), collectDataCons
  , collectTransitiveDCons, collectUsedDataCons, mergeMetaPreserving
  , stableVarId, targetBindingHasIO, translateBinds, translateModuleClosed
  , typeMentionsEffectMonad, wiredInDataCons )
import Tidepool.CborEncode (encodeTree, encodeMetadata, encodeTurnOut)
import Tidepool.Timing (readTimingEnabled, timePhase)

type Compiler =
  Maybe SessionScope
  -> FilePath
  -> [FilePath]
  -> Maybe FilePath
  -> IO PipelineResult

-- | Serve one typed request. Stdout contains exactly one diagnostics document;
-- stderr is the human-readable channel.
main :: IO ()
main = do
  rawWorkerRequest <- getArgs
  if rawWorkerRequest == ["--worker-loop-v1"]
    then do
      hSetBinaryMode stdin True
      hSetBinaryMode stdout True
      withResidentPipeline [] $ \compiler ->
        WorkerServer.runWorkerLoop
          (\cwd argv -> setCurrentDirectory cwd >> runWorkerInvocation compiler argv)
    else do
      hSetEncoding stdout utf8
      runWorkerInvocation runPipelineSession rawWorkerRequest >>= exitWith

-- | Decode a Rust worker request and run one compilation. Direct and daemon transports use
-- the same versioned payload and therefore the same dispatch path.
runWorkerInvocation
  :: Compiler -> [String] -> IO ExitCode
runWorkerInvocation compiler rawWorkerRequest = do
  parsedWorkerRequest <- case workerRequestFromArgv rawWorkerRequest of
    Left err -> hPutStrLn stderr err >> pure Nothing
    Right (Just request) -> pure (Just request)
    Right Nothing -> hPutStrLn stderr "worker requires a versioned request" >> pure Nothing
  case parsedWorkerRequest of
    Nothing -> pure (ExitFailure 2)
    Just request -> runParsedInvocation compiler request

runParsedInvocation
  :: Compiler -> WorkerRequest -> IO ExitCode
runParsedInvocation compiler parsedWorkerRequest = do
  -- Read once per invocation (see Tidepool.Timing) and thread down;
  -- TIDEPOOL_TIMING is diagnostic-only and never touches stdout/the emitted
  -- files — see the module doc there and tidepool-harness/src/timing.rs.
  timing <- readTimingEnabled
  -- Apply the harness language profile by rewriting a scratch copy:
  -- rewrite the target to a pragma-prepended scratch copy BEFORE any mode
  -- dispatch below, so every mode (one-shot, session, turn) sees a plain
  -- file with no pragma-block requirement of its own. See
  -- 'spliceHarnessProfilePragma'.
  args <- if requestHarnessProfile parsedWorkerRequest
            then spliceHarnessProfilePragma parsedWorkerRequest
            else pure parsedWorkerRequest
  dispatch compiler timing args

-- | Dispatch one decoded worker request.
dispatch
  :: Compiler -> Bool -> WorkerRequest -> IO ExitCode
dispatch compiler timing args =
  case requestFiles args of
    [] -> reportDiags (Left (toException (userError "worker request contains no input")))
    (file : _)
        -- Classification consumes every input; all other modes use the first.
        | requestClassify args                    -> runClassifyMode timing args
        -- A turn may also carry session fields, so it precedes session dispatch.
        | requestTurn args                        -> runTurnMode compiler args file
        -- Multi-target compilation may carry a session scope and therefore
        -- precedes the single-target session operation.
        | not (null (requestTargets args))        -> timePhase timing "total" (processFile compiler timing args file)
        -- Session mode: bind/reference turn with iface injection +
        -- (for binds) thin-iface write + BoundBinder sidecar. Only the FIRST
        -- file is processed (matching the two guards above) — one invocation,
        -- one stdout report, per the module doc.
        | isSessionMode args                  -> processSessionFile compiler args file
        -- Normal one-shot extraction.
        | otherwise                           -> timePhase timing "total" (processFile compiler timing args file)


-- | Prepend the harness language profile to a scratch copy of the first input.
-- Putting the profile in source keeps it visible to GHC downsweep and to
-- source-based cache keys. The caller's file is never modified; diagnostics
-- are shifted by the inserted line.
spliceHarnessProfilePragma :: WorkerRequest -> IO WorkerRequest
spliceHarnessProfilePragma args = case requestFiles args of
  [] -> pure args
  (file : rest) -> do
    src <- readFile file
    let outDir = fromMaybe (takeDirectory file </> takeBaseName file ++ "_cbor") (requestOutDir args)
        scratchPath = outDir </> takeFileName file
    createDirectoryIfMissing True outDir
    writeFile scratchPath (harnessProfilePragmaLine ++ "\n" ++ src)
    pure args { requestFiles = scratchPath : rest }

-- | Harness language extensions. A cross-language consistency test pins this
-- to @tidepool_mcp::preamble::EVAL_PRAGMAS@.
harnessProfilePragmaLine :: String
harnessProfilePragmaLine =
  "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, UndecidableInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables, ExtendedDefaultRules, LambdaCase, TupleSections, MultiWayIf, RecordWildCards, NamedFieldPuns, ViewPatterns, BangPatterns, TypeApplications, BlockArguments, NumericUnderscores, MultilineStrings, DeriveFunctor, DeriveFoldable, DeriveTraversable, DeriveGeneric, DeriveAnyClass, QuasiQuotes, DuplicateRecordFields, OverloadedRecordDot, OverloadedLabels #-}"

-- | The shared epilogue every dispatch arm ends on: render the fixed-shape
-- JSON diagnostics report to stdout from a captured extraction result, with a
-- human-readable debug copy on stderr, exiting non-zero on failure. Also used
-- in parse-only modes (e.g. 'runClassifyMode') where no live GHC session
-- exists to ever throw a 'SourceError' — 'fromException' can only take the
-- 'Nothing' branch there.
reportDiags :: Either SomeException () -> IO ExitCode
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
  pure (ExitFailure 1)
reportDiags (Right ()) = putStrLn (renderDiagsJson []) >> pure ExitSuccess

-- | A session-aware turn: any of the @--session-*@ flags are present. Reference
-- turns set @--session-root@ (+ @--inject-val@); bind turns add @--session-bind@.
isSessionMode :: WorkerRequest -> Bool
isSessionMode args = requestSessionBind args || isJust (requestSessionRoot args)

-- | Project the session portion of a worker request. Callers decide whether
-- the resulting scope is active.
scopeFromWorkerRequest :: WorkerRequest -> SessionScope
scopeFromWorkerRequest args = SessionScope
  { ssRoot      = fromMaybe "" (requestSessionRoot args)
  , ssValIfaces = mapMaybe parseValModule (requestInjectVals args)
  }

processFile
  :: Compiler -> Bool -> WorkerRequest -> FilePath -> IO ExitCode
processFile compiler timing args path = do
  let mOutDir = requestOutDir args
      mTarget = requestTarget args
  hPutStrLn stderr $ "Processing: " ++ path
  res <- try $ do
    -- Multi-target extraction can inject stable session values without
    -- becoming a session bind/reference operation.
    let scope = if isSessionMode args then Just (scopeFromWorkerRequest args) else Nothing
    result <- compiler scope path (requestIncludes args) (requestBuildProductsDir args)
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

    if requestDumpCore args
      then hPutStrLn stderr (dumpCore binds)
      else return ()

    let outDir = case mOutDir of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir

    if not (null (requestTargets args))
      -- Explicit multi-target mode (--targets a,b): takes priority over
      -- --target/--all-closed, which stay untouched below for every other
      -- caller. One runPipeline invocation (already run, above), several
      -- named targets, one merged meta.cbor — see 'runMultiTargetClosed'.
      then runMultiTargetClosed timing outDir hscEnv binds mCapturedTy warnTexts (requestTargets args)
      else case (mTarget, requestAllClosed args) of
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
        let targetModName = capitalize (takeBaseName path)
            keepBinder b
              | not (requestTargetModuleOnly args) = True
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
          compileAttempt <- try $ do
            closed@ClosedModule { cmNodes = nodes, cmUnresolved = unresolved } <- translateModuleClosed hscEnv binds name
            if not (null unresolved) then do
              let names = map (\uv -> uvModule uv ++ "." ++ uvName uv) unresolved
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): unresolved external(s): " ++ unwords names
              return Nothing
            else do
              _ <- evaluate (BS.length (encodeTree nodes))
              return (Just closed)
          case compileAttempt of
            Left (e :: SomeException) -> do
              hPutStrLn stderr $ "  SKIPPED (" ++ name ++ "): " ++ show e
              return acc
            Right Nothing -> return acc
            Right (Just closed) -> return (acc ++ [(name, name, closed)])
          ) [] uniqueNames
        -- Validate and emit all surviving fixtures through the shared writer.
        void $ writeClosedTargets timing outDir binds mCapturedTy warnTexts closedTargets
        pruneAllClosedArtifacts outDir (map (\(_, outFileBase, _) -> outFileBase) closedTargets)

      (Just targetName, False) ->
        -- Whole-module mode: serialize all bindings as nested lets around the
        -- target (shared with the session path; see 'writeWholeModuleClosed').
        -- File base name matches the lookup name here (the general CLI
        -- contract: --target foo produces foo.cbor). This is the branch the
        -- self-iterating harness's full-compile path actually exercises
        -- (tidepool-harness/src/compile.rs passes --target, never
        -- --all-closed), so it's the one carrying translate/cbor_encode/write
        -- timing.
        void $ writeWholeModuleClosed timing outDir hscEnv binds mCapturedTy warnTexts targetName targetName

      (Nothing, False) -> do
        -- Per-binding mode (original behavior). NOT unified with
        -- 'writeClosedTargets': 'translateBinds' translates each binding
        -- standalone, over a bare 'TransState' with no unresolved-id set and
        -- none of the runLLMTurn interception's aux var ids wired (see its
        -- definition in Translate.hs) — it never runs the
        -- 'resolveExternals'/reachability closure 'translateModuleClosed'
        -- does, so it produces no 'ClosedModule' and structurally has
        -- neither 'cmReachBinds' (required for metadata validation) nor any
        -- unresolved/dangling tracking (what 'cmVarNames' is built from).
        -- Routing it through the shared writer would mean rebuilding that
        -- closure machinery here, i.e. changing Translate.hs's translation
        -- semantics for this call site, which is outside this write path,
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

-- | A session-eval turn (reference or bind). Compile through
-- 'runPipelineSession' with the live @Val.G<g>@ ifaces injected (so refs to
-- earlier bindings resolve), emit the JIT-able Core for @__result@, and — on a
-- bind turn — capture the bound value's type, write the thin session iface, and
-- emit the BoundBinder sidecar. Non-session extraction stays on 'processFile'.
processSessionFile
  :: Compiler -> WorkerRequest -> FilePath -> IO ExitCode
processSessionFile compiler args path = do
  -- The self-iterating harness's fused outer render/loop compile never
  -- reaches this session-mode path even though it now DOES carry
  -- --session-root/--inject-val for stable-value injection: 'dispatch' checks
  -- `not (null (requestTargets args))` (which its multi-target --targets
  -- result,__selfHarnessLoopEntry always is) BEFORE 'isSessionMode', so it
  -- always lands on 'processFile' instead. This read is here purely so
  -- 'writeWholeModuleClosed' (shared with 'processFile') behaves identically
  -- regardless of caller.
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (session): " ++ path
  let scope = scopeFromWorkerRequest args
      -- The repl wrapper's own compile-target binding is scaffold-reserved
      -- (@__result@, not @result@) so it can never collide with a user's own
      -- chosen bind name promoted into a later turn's session-lib import —
      -- see 'writeWholeModuleClosed''s doc for why the CBOR file it's
      -- written to stays named @result.cbor@ regardless.
      targetName = fromMaybe scaffoldTargetName (requestTarget args)
  res <- try $ do
    result <- compiler (Just scope) path (requestIncludes args) (requestBuildProductsDir args)
    let binds  = prBinds result
        hscEnv = prHscEnv result
        mCapturedTy = fmap T.pack (prCapturedType result)
        warnTexts = map T.pack (prWarnings result)
    hPutStrLn stderr $ "  Top-level bindings: " ++ show (length binds)
    if requestDumpCore args then hPutStrLn stderr (dumpCore binds) else return ()
    let outDir = case requestOutDir args of
          Just dir -> dir
          Nothing  -> takeDirectory path </> takeBaseName path ++ "_cbor"
    createDirectoryIfMissing True outDir
    -- The JIT-able Core for the target (same emission as whole-module mode).
    -- File base name is always "result" — every Rust session-turn caller
    -- expects result.cbor regardless of the (scaffold-reserved) lookup name.
    void $ writeWholeModuleClosed timing outDir hscEnv binds mCapturedTy warnTexts targetName scaffoldOutputBase
    -- BIND turn: capture the bound type, mint+write the thin iface, emit sidecar.
    when (requestSessionBind args) (emitBindArtifacts args result)
  reportDiags res

-- | Turn mode (@--turn@): classify the raw
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
runTurnMode
  :: Compiler -> WorkerRequest -> FilePath -> IO ExitCode
runTurnMode compiler args path = do
  timing <- readTimingEnabled
  hPutStrLn stderr $ "Processing (turn): " ++ path
  res <- timePhase timing "total" $ try $ do
    turnSrc   <- readFile path
    let templates = requestTurnTemplates args
    mVerdict  <- traverse parseTurnVerdictArg (requestTurnVerdict args)
    -- 'extractStmtBinders' emits no phases of its own. This mode times it as
    -- the single @classify@ phase, emitted
    -- only on the branch that actually classifies. With @--turn-verdict@
    -- supplied nothing is parsed, and an absent @classify@ row is the
    -- honest report rather than a phantom 0ms line.
    sb        <- maybe (timePhase timing "classify" (extractStmtBinders turnSrc)) return mVerdict
    let outDir     = fromMaybe (takeDirectory path </> takeBaseName path ++ "_cbor") (requestOutDir args)
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
        items <- extractBindersNamed modulePath (requestIncludes args) modName
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
        let scope = scopeFromWorkerRequest args
        result <- compiler (Just scope) modulePath (requestIncludes args) (requestBuildProductsDir args)
        let binds       = prBinds result
            hscEnv      = prHscEnv result
            mCapturedTy = fmap T.pack (prCapturedType result)
            warnTexts   = map T.pack (prWarnings result)
        -- The Core binding to look up. Scaffold-reserved by default, but a
        -- caller whose template names its own target says so with --target
        -- (the same knob 'processSessionFile' honours). The output file base
        -- stays "result" regardless — every Rust caller reads result.cbor.
        let targetName = fromMaybe scaffoldTargetName (requestTarget args)
        asksSites <- writeWholeModuleClosed timing outDir hscEnv binds mCapturedTy warnTexts targetName scaffoldOutputBase
        let wrapped = T.pack spliced
        case selector of
          SBind -> do
            g    <- requireArg "--bind-gen"     (requestBindGen args)
            root <- requireArg "--session-root" (requestSessionRoot args)
            bbs  <- mkBoundBinders False (sbBinders sb) g root result
            return (TBind (map T.pack (sbBinders sb)) 0 bbs asksSites wrapped)
          SBindDiscard -> return (TBind [] 0 [] asksSites wrapped)
          SExpr -> return (TExpr 0 asksSites wrapped)
          SDecl -> error ("--turn: unexpected verdict kind: " ++ templateSelectorWireName selector)
    outFile <- requireArg "--turn-out" (requestTurnOut args)
    let cbor = encodeTurnOut turnOut
    BS.writeFile outFile cbor
    hPutStrLn stderr $ "  Wrote: " ++ outFile ++ " (" ++ show (BS.length cbor) ++ " bytes)"
  reportDiags res

-- | Block classify mode (@--classify@):
-- classify EVERY positional file in 'requestFiles' with ONE GHC session boot
-- ('classifyBlock'), in argv order, and write the verdicts to
-- @--classify-out@. Serves @tidepool-repl@'s block runner, which segments a
-- block into decl runs before compiling any item and so needs every verdict
-- up front — one spawn for the whole block instead of one classify spawn per
-- item.
runClassifyMode :: Bool -> WorkerRequest -> IO ExitCode
runClassifyMode timing args =
  -- No live GHC session exists in this parse-only mode, so the caught
  -- exception below always takes 'reportDiags''s 'Nothing' branch (never a
  -- 'SourceError' to distinguish).
  timePhase timing "total" $
    try
      ( do
          out      <- requireArg "--classify-out" (requestClassifyOut args)
          srcs     <- mapM readFile (requestFiles args)
          verdicts <- classifyBlock timing srcs
          writeFile out (renderVerdictsJson verdicts)
          hPutStrLn stderr $ "  Wrote: " ++ out ++ " (" ++ show (length verdicts) ++ " verdicts)"
      )
      >>= reportDiags

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
-- and @--turn@'s bind path — one computation, three callers.
--
-- @probeOnly@ exempts the cross-row bind guard below: an ephemeral
-- type-probe bind (@:t@) reads the captured type and is discarded, never
-- registered as a session binding, so a row-mentioning type cannot "cross
-- fragments" here — there is no later fragment. A genuine session bind
-- passes 'False' and stays guarded.
mkBoundBinders :: Bool -> [String] -> Word64 -> FilePath -> PipelineResult -> IO [BoundBinder]
mkBoundBinders probeOnly bindNames g root result = do
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
  -- Cross-row bind guard (one-session plan Phase 3e, TASK 2; narrowed by
  -- stable-effects-core): a session bind whose captured type mentions the
  -- 'Eff' tycon itself cannot mean anything once it crosses into a LATER
  -- turn's compile, which applies it to its OWN row (the row is per-compile,
  -- same reasoning as 'Tidepool.Translate.checkRunLLMTurnType'). This no
  -- longer also rejects a bind merely for mentioning an effect GADT
  -- (@Console@, @KV@, a bridged record, …) — those now live in the STABLE
  -- @Tidepool.Effects.Core@ module (a pure function of the vocabulary alone,
  -- identical across turns/windows that share it), so a value naming one is
  -- exactly as safe to cross as a plain data value always was. Reject loudly
  -- here rather than let a genuine row-typed value silently reach a later
  -- turn as an unresolvable/wrongly-resolved reference. @probeOnly@ (an
  -- ephemeral @:t@ type-probe, never registered as a session binding — see
  -- 'mkBoundBinders' doc) has no later fragment to cross into, so it is
  -- exempt by construction.
  --
  -- NOT generalized the way an M-mentioning DECL signature now is (the
  -- decl-plane fix in @tidepool-runtime@'s @render_module@ that lets
  -- @probe :: M Value@ persist by stripping the signature and letting GHC
  -- infer): a bind's type is not an ANNOTATION to strip and re-infer, it is
  -- the type of a value THAT ALREADY EXISTS on the heap from running this
  -- turn's expression once. There is nothing to re-infer a bind's captured
  -- type FROM — the value is already computed, and if it genuinely mentions
  -- @Eff@ (e.g. the action itself, not its result, or a closure that would
  -- need to resume a suspended computation) that is a real fact about the
  -- value, not an artifact of how the model spelled a signature. This
  -- residual stays exactly as it was.
  when (not probeOnly) $
    forM_ (zip bindNames componentTypes) $ \(name, cty) ->
      when (typeMentionsEffectMonad cty) $
        error $ "session bind '" ++ name ++ "' captures the effect row in its type ("
              ++ renderType cty ++ "); row-typed values cannot cross turns/windows compiling a different row — "
              ++ "bind a pure value or inline the effectful part" ++ eitherBindHint cty
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

-- | Extra cross-row-bind-guard suggestion for an @Either e t@ reject: the
-- idiom that actually works is destructuring AT the bind (@Right x <- expr@
-- peels the success value off before the row-typed 'Either' ever needs to
-- cross a fragment), which the guard's generic "inline the effectful part"
-- text doesn't name. Empty string for any other shape, so it's safe to
-- append unconditionally.
eitherBindHint :: Type -> String
eitherBindHint cty = case splitTyConApp_maybe cty of
  Just (tc, [_e, _t]) | getOccString (tyConName tc) == "Either" ->
    " or destructure at the bind: `Right x <- <expr>` binds the success value"
  _ -> ""

-- | The @--session-bind@ artifacts: mint the 'BoundBinder' records via
-- 'mkBoundBinders' and, when requested, write the standalone JSON sidecar.
emitBindArtifacts :: WorkerRequest -> PipelineResult -> IO ()
emitBindArtifacts args result = do
  bindNames <- case requestBindNames args of
    []  -> error "session-bind requires at least one --bind-name"
    ns  -> return ns
  g       <- requireArg "--bind-gen"    (requestBindGen args)
  root    <- requireArg "--session-root" (requestSessionRoot args)
  binders <- mkBoundBinders (requestProbeOnly args) bindNames g root result
  case requestEmitBoundBinders args of
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

-- | Deduplicate binding names by appending _1, _2, etc. for collisions.
dedup :: Map.Map String Int -> [(String, a)] -> [(String, a)]
dedup _ [] = []
dedup seen ((name, val) : rest) =
  case Map.lookup name seen of
    Nothing -> (name, val) : dedup (Map.insert name 1 seen) rest
    Just n  -> (name ++ "_" ++ show n, val) : dedup (Map.insert name (n + 1) seen) rest
