-- | Withholds GHC unfoldings for retained-generation symbols before the
-- simplifier ever sees them, so a symbol carried over from an earlier
-- session generation cannot be inlined into a module compiled alongside it.
--
-- WHY THIS EXISTS
--
-- 'Tidepool.ExecutionProjection' already refuses to recover a
-- retained-generation symbol's own body and instead emits a 'GlobalDecl'
-- reference for it ('ExecutionProjection.dropRetainedTops' /
-- 'ExecutionProjection.internGlobal'). That check runs on the wire
-- projection, AFTER every home module has finished compiling. By then it can
-- already be too late: 'Tidepool.GhcPipeline.canonicalizeDFlags' turns on
-- @Opt_ExposeAllUnfoldings@ (needed so ordinary cross-module Core --
-- library calls, dictionaries, and the like -- resolves without a runtime
-- import), and GHC's own batch driver (@load'@, called once for the whole
-- module graph before 'Tidepool.GhcPipeline''s own per-module
-- typecheck/desugar/@core2core@ redo loop even starts) already runs the
-- real simplifier and may inline a retained id's unfolding into every home
-- module that references it. The projection stage then sees only the
-- aftermath: a consumer whose Core already carries a recovered copy
-- (floated sub-bindings such as @producerValue1@..@producerValue5@, a
-- specialised worker such as @$wproducerFn@), with no trace of the original
-- occurrence left to match against the retained map.
--
-- Patching this after the fact -- rewriting a compiled retained module's
-- own 'GHC.Unit.Module.ModDetails.ModDetails'/'GHC.Unit.Module.ModIface.ModIface'
-- once 'Tidepool.GhcPipeline' has it in hand -- does not reach far enough
-- either: for the plain (non-session) pipeline, that hook
-- ('CompilePlan.cpAfterModule') is a no-op, and the module that actually
-- puts a retained id's optimized unfolding into 'HomePackageTable' scope is
-- GHC's own @load'@, which this pipeline never gets a per-module callback
-- into.
--
-- The one seam that reaches BOTH @load'@'s internal simplification and this
-- pipeline's own later @core2core@ redo is the seam GHC itself exposes for
-- exactly this purpose: a Core plugin registered on the session's
-- 'HscEnv' (@hsc_plugins@). 'Plugin's @installCoreToDos@ lets a plugin
-- prepend a pass to the @CoreToDo@ list that @core2core@ -- and @load'@'s
-- internal use of the very same machinery -- runs for every home module, in
-- every compile that consults this 'HscEnv'. That covers both compile paths
-- uniformly, without this pipeline needing to know which one produced a
-- given module's guts. This module's pass runs FIRST, before
-- @CoreDoSimplify@, on the freshly desugared Core: it rewrites every
-- retained-generation 'Id' (matched by 'SymbolIdentity', external-name-only,
-- exactly as 'ExecutionProjection.idSymbol' matches it) to the same 'Id'
-- with 'noUnfolding' and 'neverInlinePragma'. That is exactly what a source
-- @{-# NOINLINE #-}@ pragma already produces at desugaring time (see the
-- former stand-in comment this pass replaces in
-- @test-prepared-stg/ImportProducer.hs@) -- this pass only derives the same
-- effect from the retained-generation map instead of requiring the pragma
-- text, so a notebook user never has to write it.
--
-- VALIDITY
--
-- The pass rewrites only the module's OWN top-level binders (and their
-- occurrences). A module's output is therefore a function of its source,
-- the interfaces it imports, and @retained ∩ definedBy(module)@; a retained
-- identity defined elsewhere reaches it only through that defining module's
-- interface, which GHC's usage fingerprints and the memo's dependency check
-- already track. Both validity checks use exactly that intersection
-- ('retainedDefinedBy'): the pipeline's memo, and the plugin's
-- recompilation fingerprint.
--
-- GHC 9.12 calls @pluginRecompile@ with the plugin's arguments only, with no
-- module in scope, both when checking an old interface ('checkPlugins' in
-- @hscRecompStatus@) and when stamping a new one ('fingerprintPlugins' in
-- 'mkFullIface'). The make driver's 'compileOne'' however runs
-- 'initializePlugins' on the module-local 'HscEnv' (the summary's
-- @ms_hspp_opts@) immediately before both, and that runs each uninitialised
-- static plugin's @driverPlugin@. 'scopeRetainedModuleGraph' records each
-- summary's module identity as a plugin option in those flags; the driver
-- action copies it into the plugin's own arguments and leaves the plugin
-- uninitialised so the next module re-scopes it. Plugin options are not part
-- of GHC's flag fingerprint. A check without a recorded module (a session
-- 'HscEnv', or a graph that was not scoped) fingerprints the full retained
-- set, which is conservative.
module Tidepool.RetainedUnfoldings
  ( installRetainedUnfoldingsPlugin
  , scopeRetainedModuleGraph
  , retainedDefinedBy
  , withholdRetainedUnfoldings
  ) where

import Control.Monad.IO.Class (liftIO)
import Data.IORef (IORef, readIORef)
import Data.Maybe (fromMaybe)
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Core
  ( Alt(..), Bind(..), CoreBind, CoreProgram, Expr(..), noUnfolding )
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..), bindsOnlyPass)
import GHC.Data.FastString (unpackFS)
import GHC.Driver.Session (DynFlags(..))
import GHC.Driver.Env.Types (HscEnv(..))
import GHC.Driver.Plugins
  ( Plugin(..), PluginWithArgs(..), Plugins(..), StaticPlugin(..)
  , PluginRecompile(..), defaultPlugin )
import GHC.Utils.Fingerprint (fingerprintString)
import GHC.Types.Basic (neverInlinePragma)
import GHC.Types.Id (Id, idName, setIdUnfolding, setInlinePragma)
import GHC.Types.Name (isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (fieldOcc_maybe, occNameString)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Unit.Module (Module, ModuleName, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Graph (ModuleGraph, mapMG)
import GHC.Unit.Module.ModSummary (ModSummary(..))
import Text.Read (readMaybe)
import GHC.Unit.Types (unitString)
import Tidepool.ExecutionSchema (SymbolIdentity(..))

-- | Install the withholding pass on a session's 'HscEnv', ahead of every
-- other registered plugin. UNCONDITIONALLY installs (unlike the pure
-- 'withholdRetainedUnfoldings' this pass wraps): the pass reads the given
-- 'IORef' at RUN time, once per module, so the same installed pass serves
-- every later retained set a caller writes into the cell -- including a
-- 'Set.null' one, which stays a true no-op on the compiled bytes. This lets
-- a long-lived 'HscEnv' (the resident daemon's session) install the pass
-- exactly ONCE and vary the retained set per request by writing the cell,
-- instead of installing one pass per request and accumulating them. A
-- one-shot caller that only ever compiles once may still pass a fresh
-- 'IORef' seeded with its one request's set.
installRetainedUnfoldingsPlugin :: IORef (Set SymbolIdentity) -> HscEnv -> HscEnv
installRetainedUnfoldingsPlugin retainedRef hscEnv =
  hscEnv { hsc_plugins = plugins { staticPlugins = staticPlugin : staticPlugins plugins } }
  where
    plugins = hsc_plugins hscEnv
    staticPlugin = StaticPlugin
      { spPlugin = PluginWithArgs
          { paPlugin = withholdingPlugin retainedRef
          , paArguments = [pluginMarker]
          }
      -- Uninitialised so that every 'initializePlugins' call, including the
      -- per-module one, runs 'scopeToModule' (see VALIDITY above).
      , spInitialised = False
      }

-- | Record each summary's module identity for the withholding plugin's
-- per-module recompilation fingerprint. Apply to the graph handed to @load'@.
scopeRetainedModuleGraph :: ModuleGraph -> ModuleGraph
scopeRetainedModuleGraph = mapMG scope
  where
    scope ms = ms { ms_hspp_opts = tag (ms_mod ms) (ms_hspp_opts ms) }
    tag m flags = flags
      { pluginModNameOpts =
          (pluginModule, show (moduleKey m))
            : [ opt | opt@(owner, _) <- pluginModNameOpts flags, owner /= pluginModule ]
      }

-- | The retained identities a module defines: the only part of the retained
-- set that can change the module's own compilation (see VALIDITY above).
retainedDefinedBy :: Module -> Set SymbolIdentity -> Set SymbolIdentity
retainedDefinedBy m = definedIn (moduleKey m)

definedIn :: (Text.Text, Text.Text) -> Set SymbolIdentity -> Set SymbolIdentity
definedIn (unit, modName) =
  Set.filter (\i -> symbolUnit i == unit && symbolModule i == modName)

moduleKey :: Module -> (Text.Text, Text.Text)
moduleKey m =
  (Text.pack (unitString (moduleUnit m)), Text.pack (moduleNameString (moduleName m)))

pluginModule :: ModuleName
pluginModule = mkModuleName "Tidepool.RetainedUnfoldings"

-- Identifies this plugin's entry among the session's static plugins.
pluginMarker :: String
pluginMarker = "tidepool-retained-unfoldings"

-- | Driver action: set this plugin's arguments to the module recorded in the
-- current flags (none for a session-level 'HscEnv'), and stay uninitialised.
scopeToModule :: HscEnv -> HscEnv
scopeToModule env = env { hsc_plugins = plugins { staticPlugins = map rescope (staticPlugins plugins) } }
  where
    plugins = hsc_plugins env
    recorded = [ opt | (owner, opt) <- pluginModNameOpts (hsc_dflags env), owner == pluginModule ]
    rescope sp
      | (pluginMarker : _) <- paArguments (spPlugin sp) = sp
          { spPlugin = (spPlugin sp) { paArguments = pluginMarker : take 1 recorded }
          , spInitialised = False
          }
      | otherwise = sp

withholdingPlugin :: IORef (Set SymbolIdentity) -> Plugin
withholdingPlugin retainedRef = defaultPlugin
  { installCoreToDos = \_args todos ->
      pure (CoreDoPluginPass "WithholdRetainedUnfoldings" pass : todos)
  , driverPlugin = \_args env -> pure (scopeToModule env)
  , pluginRecompile = \args -> do
      retained <- readIORef retainedRef
      -- Show's escaped, delimited representation preserves every identity
      -- field; Set ordering makes the encoding independent of insertion order.
      -- Bump the version when the withholding transformation changes.
      let (scopeLabel, relevant) = case args of
            [_, recorded] | Just key <- readMaybe recorded ->
              ("module " ++ show key, definedIn key retained)
            _ -> ("unscoped", retained)
      pure (MaybeRecompile (fingerprintString
        ("tidepool-retained-unfoldings-v2:" ++ scopeLabel ++ ":"
          ++ show (Set.toAscList relevant))))
  }
  where
    pass = bindsOnlyPass $ \binds -> do
      retained <- liftIO (readIORef retainedRef)
      pure (withholdRetainedUnfoldings retained binds)

-- | The pure Core-to-Core rewrite: every top-level binder whose
-- 'SymbolIdentity' is a member of the retained set, and every occurrence of
-- it anywhere in this module's own bindings, is replaced by the same 'Id'
-- carrying 'noUnfolding' and 'neverInlinePragma'. Mirrors
-- 'Tidepool.GhcPipeline.externalizeInternalTops''s binder-substitution
-- shape: only top-level binder identity is ever rewritten, so a retained
-- symbol's own nested lets and lambdas are untouched.
withholdRetainedUnfoldings :: Set SymbolIdentity -> CoreProgram -> CoreProgram
withholdRetainedUnfoldings retained binds
  | Set.null retained = binds
  | otherwise = map substTop binds
  where
    fixes :: VarEnv Id
    fixes = foldr addFix emptyVarEnv (concatMap topBinders binds)
    addFix v env = case idIdentity v of
      Just identity | identity `Set.member` retained -> extendVarEnv env v (withhold v)
      _ -> env
    topBinders (NonRec b _) = [b]
    topBinders (Rec ps) = map fst ps
    withhold v = setIdUnfolding (setInlinePragma v neverInlinePragma) noUnfolding
    sub v = fromMaybe v (lookupVarEnv fixes v)
    substTop :: CoreBind -> CoreBind
    substTop (NonRec b rhs) = NonRec (sub b) (substExpr rhs)
    substTop (Rec ps) = Rec [ (sub b, substExpr rhs) | (b, rhs) <- ps ]
    substExpr e = case e of
      Var v -> Var (sub v)
      Lit _ -> e
      App f a -> App (substExpr f) (substExpr a)
      Lam b body -> Lam b (substExpr body)
      Let b body -> Let (substTop b) (substExpr body)
      Case s b t alts -> Case (substExpr s) b t
        [ Alt c bs (substExpr rhs) | Alt c bs rhs <- alts ]
      Cast e' co -> Cast (substExpr e') co
      Tick t e' -> Tick t (substExpr e')
      Type _ -> e
      Coercion _ -> e

-- | Retained-generation matching is external-name-only (a local/internal
-- float can never be the SAME identity a caller retained from an earlier
-- generation): mirrors 'Tidepool.ExecutionProjection.idSymbol' \"value\".
idIdentity :: Id -> Maybe SymbolIdentity
idIdentity v
  | isExternalName name = case nameModule_maybe name of
      Just m -> Just SymbolIdentity
        { symbolUnit = Text.pack (unitString (moduleUnit m))
        , symbolModule = Text.pack (moduleNameString (moduleName m))
        , symbolNamespace = "value"
        , symbolOccurrence = Text.pack (occNameString (nameOccName name))
        , symbolRecordParent = Text.pack . unpackFS <$> fieldOcc_maybe (nameOccName name)
        }
      Nothing -> Nothing
  | otherwise = Nothing
  where name = idName v
