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
-- Replacing a home interface after its dependencies have compiled cannot
-- undo inlining performed by load's own compiler pipeline. Withholding must
-- therefore apply before either pipeline simplifies the defining module.
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
-- uninitialised so the next module re-scopes it. The custom compiler path
-- explicitly scopes its saved environment before building an interface.
-- An unscoped environment must never fingerprint the entire retained set.
module Tidepool.RetainedUnfoldings
  ( RetainedContext, retainedContext, emptyRetainedContext
  , installRetainedUnfoldingsPlugin
  , scopeRetainedModuleGraph
  , scopeRetainedSummaryHscEnv, retainedDefinedBy
  , withholdRetainedUnfoldings
  ) where

import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Maybe (fromMaybe)
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Core
  ( Alt(..), Bind(..), CoreBind, CoreProgram, Expr(..), noUnfolding )
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..), bindsOnlyPass)
import GHC.Data.FastString (unpackFS)
import GHC.Driver.Session (DynFlags(..))
import GHC.Driver.Env (hscSetFlags)
import GHC.Driver.Env.Types (HscEnv(..))
import GHC.Driver.Plugins
  ( Plugin(..), PluginWithArgs(..), Plugins(..), StaticPlugin(..)
  , PluginRecompile(..), defaultPlugin )
import GHC.Utils.Fingerprint (fingerprintString)
import GHC.Fingerprint.Type (Fingerprint)
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

-- Each attempt installs one pass with immutable inputs. Lazy computations
-- captured by an earlier HscEnv keep their original policy. An empty retained
-- set leaves the compiled bytes unchanged.
data RetainedContext = RetainedContext
  { contextAll :: !(Set SymbolIdentity)
  , contextByModule :: !(Map (Text.Text, Text.Text) (Set SymbolIdentity, Fingerprint))
  , contextEmptyFingerprint :: !Fingerprint
  }

retainedContext :: Set SymbolIdentity -> RetainedContext
retainedContext identities = RetainedContext identities scopes (scopeFingerprint Set.empty)
  where
    groups = Map.fromListWith Set.union
      [ ((symbolUnit i, symbolModule i), Set.singleton i) | i <- Set.toList identities ]
    scopes = Map.map (\group -> (group, scopeFingerprint group)) groups

emptyRetainedContext :: RetainedContext
emptyRetainedContext = retainedContext Set.empty

scopeFingerprint :: Set SymbolIdentity -> Fingerprint
scopeFingerprint identities = fingerprintString
  ("tidepool-retained-unfoldings-v3:" ++ show (Set.toAscList identities))

installRetainedUnfoldingsPlugin :: RetainedContext -> HscEnv -> HscEnv
installRetainedUnfoldingsPlugin context hscEnv =
  hscEnv { hsc_plugins = plugins
    { staticPlugins = staticPlugin : filter (not . withholding) (staticPlugins plugins) } }
  where
    plugins = hsc_plugins hscEnv
    withholding plugin = case paArguments (spPlugin plugin) of
      marker : _ -> marker == pluginMarker
      [] -> False
    staticPlugin = StaticPlugin
      { spPlugin = PluginWithArgs
          { paPlugin = withholdingPlugin context
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

tag :: Module -> DynFlags -> DynFlags
tag m flags = flags
  { pluginModNameOpts =
      (pluginModule, show (moduleKey m))
        : [ opt | opt@(owner, _) <- pluginModNameOpts flags, owner /= pluginModule ]
  }

-- | The custom typecheck/desugar path does not pass through compileOne's
-- initializePlugins. Scope its saved environment before optimization and
-- interface construction, using the same module key as the load graph.
scopeRetainedHscEnv :: Module -> HscEnv -> HscEnv
scopeRetainedHscEnv m env = scopeToModule env { hsc_dflags = tag m (hsc_dflags env) }

-- | Interface production and validation use the summary's complete per-file
-- flags and the same module scope for the retained unfolding plugin.
scopeRetainedSummaryHscEnv :: ModSummary -> HscEnv -> HscEnv
scopeRetainedSummaryHscEnv summary =
  scopeRetainedHscEnv (ms_mod summary) . hscSetFlags (ms_hspp_opts summary)

-- | The retained identities a module defines: the only part of the retained
-- set that can change the module's own compilation (see VALIDITY above).
retainedDefinedBy :: Module -> RetainedContext -> Set SymbolIdentity
retainedDefinedBy m context = maybe Set.empty fst
  (Map.lookup (moduleKey m) (contextByModule context))

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
      | (marker : _) <- paArguments (spPlugin sp)
      , marker == pluginMarker = sp
          { spPlugin = (spPlugin sp) { paArguments = pluginMarker : take 1 recorded }
          , spInitialised = False
          }
      | otherwise = sp

withholdingPlugin :: RetainedContext -> Plugin
withholdingPlugin context = defaultPlugin
  { installCoreToDos = \_args todos ->
      pure (CoreDoPluginPass "WithholdRetainedUnfoldings" pass : todos)
  , driverPlugin = \_args env -> pure (scopeToModule env)
  , pluginRecompile = \args -> do
      pure $ case args of
        [_, recorded] | Just key <- readMaybe recorded ->
          MaybeRecompile (maybe (contextEmptyFingerprint context) snd
            (Map.lookup key (contextByModule context)))
        _ -> ForceRecompile
  }
  where
    pass = bindsOnlyPass $ \binds -> do
      pure (withholdRetainedUnfoldings (contextAll context) binds)

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
