-- | Module-local compiler boundaries, installed before either compilation
-- route simplifies Core. Retained values keep their existing withholding
-- policy. An issued native declaration additionally keeps ordinary calls
-- opaque, while constructor and dictionary representation remains available.
-- The same complete module policy owns compiler and memo validity.
module Tidepool.RetainedUnfoldings
  ( RetainedContext, retainedContext, emptyRetainedContext, extendRetainedContext
  , ModuleUnfoldingPolicy, moduleUnfoldingPolicyFor, withNativeDeclarationBoundary
  , installRetainedUnfoldingsPlugin
  , scopeRetainedModuleGraph
  , scopeRetainedSummaryHscEnv
  ) where

import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Maybe (fromMaybe)
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Core
  ( Alt(..), Bind(..), CoreBind, CoreProgram, Expr(..), noUnfolding, isBuiltinRule )
import GHC.Core.FVs (exprFreeVars, mkRuleInfo)
import GHC.Core.Subst qualified as Subst
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..))
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
import GHC.Parser.PostProcess (mkOpaquePragma)
import GHC.Types.SourceText (SourceText(NoSourceText))
import GHC.Types.Id
  ( Id, idName, setIdUnfolding, setInlinePragma, isDataConId, isDFunId
  , isClassOpId, idSpecialisation, realIdUnfolding, setIdSpecialisation )
import GHC.Types.Id.Info (emptyRuleInfo, ruleInfoRules)
import GHC.Types.Name (isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (fieldOcc_maybe, occNameString)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv, isEmptyVarEnv, mkInScopeSet, mkVarEnv)
import GHC.Types.Var.Set (mkVarSet, unionVarSets)
import GHC.Unit.Module.ModGuts (ModGuts(..))
import GHC.Unit.Module (Module, ModuleName, mkModuleName, moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Module.Graph (ModuleGraph, mapMG)
import GHC.Unit.Module.ModSummary (ModSummary(..))
import Text.Read (readMaybe)
import GHC.Unit.Types (unitString)
import Tidepool.ExecutionSchema (SymbolIdentity(..))

data DefinitionBoundary = OrdinaryDefinitions | NativeDeclarationDefinitions
  deriving (Eq, Ord, Show)

data ModuleUnfoldingPolicy = ModuleUnfoldingPolicy
  !(Set SymbolIdentity) !DefinitionBoundary
  deriving (Eq, Ord, Show)

-- Each attempt captures immutable policies. An old environment retains its
-- original policy even when a later attempt extends its retained inputs.
data RetainedContext = RetainedContext
  { contextByModule :: !(Map (Text.Text, Text.Text) (ModuleUnfoldingPolicy, Fingerprint))
  , contextEmptyFingerprint :: !Fingerprint
  }

retainedContext :: Set SymbolIdentity -> RetainedContext
retainedContext identities = extendRetainedContext identities emptyRetainedContext

emptyRetainedContext :: RetainedContext
emptyRetainedContext = RetainedContext Map.empty (scopeFingerprint emptyModulePolicy)

emptyModulePolicy :: ModuleUnfoldingPolicy
emptyModulePolicy = ModuleUnfoldingPolicy Set.empty OrdinaryDefinitions

moduleUnfoldingPolicyFor :: Module -> RetainedContext -> ModuleUnfoldingPolicy
moduleUnfoldingPolicyFor owner context = policyForKey (moduleKey owner) context

policyForKey :: (Text.Text, Text.Text) -> RetainedContext -> ModuleUnfoldingPolicy
policyForKey key context = maybe emptyModulePolicy fst (Map.lookup key (contextByModule context))

updatePolicy :: (Text.Text, Text.Text) -> (ModuleUnfoldingPolicy -> ModuleUnfoldingPolicy)
  -> RetainedContext -> RetainedContext
updatePolicy key update context = context {contextByModule = Map.insert key
  (policy, scopeFingerprint policy) (contextByModule context)}
  where policy = update (policyForKey key context)

withNativeDeclarationBoundary :: Module -> RetainedContext -> RetainedContext
withNativeDeclarationBoundary owner = updatePolicy (moduleKey owner)
  (\(ModuleUnfoldingPolicy retained _) -> ModuleUnfoldingPolicy retained NativeDeclarationDefinitions)

extendRetainedContext :: Set SymbolIdentity -> RetainedContext -> RetainedContext
extendRetainedContext identities initial = Map.foldlWithKey' extend initial groups
  where
    groups = Map.fromListWith Set.union
      [((symbolUnit identity, symbolModule identity), Set.singleton identity)
      | identity <- Set.toList identities]
    extend context key more = updatePolicy key
      (\(ModuleUnfoldingPolicy retained boundary) ->
        ModuleUnfoldingPolicy (Set.union retained more) boundary) context

scopeFingerprint :: ModuleUnfoldingPolicy -> Fingerprint
scopeFingerprint policy = fingerprintString
  ("tidepool-retained-unfoldings-v4:" ++ show policy)

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
    pass guts = pure $ case moduleUnfoldingPolicyFor (mg_module guts) context of
      ModuleUnfoldingPolicy retained OrdinaryDefinitions ->
        guts {mg_binds = withholdRetainedUnfoldings retained (mg_binds guts)}
      ModuleUnfoldingPolicy _ NativeDeclarationDefinitions ->
        -- At this phase mg_rules contains this module's authored rules for
        -- imported heads. Imported HPT/EPS rules remain with their own owners.
        guts {mg_binds = opaqueNativeBindings (mg_binds guts), mg_rules = []}

-- Ordinary executable tops are LocalIds before tidy, including exported
-- external Names. Representation Ids may be GlobalIds; they are not replaced
-- by the substitution. Rebind Core and preserved metadata together so a DFun's
-- unfolding cannot retain an ordinary method's older, unprotected IdInfo.
opaqueNativeBindings :: CoreProgram -> CoreProgram
opaqueNativeBindings binds = map rewrite binds
  where
    identifiers = concatMap topBinders binds
    scope = Subst.mkEmptySubst (mkInScopeSet (unionVarSets
      (mkVarSet identifiers : [exprFreeVars rhs | binding <- binds, rhs <- rightSides binding])))
    substitution = Subst.extendIdSubstList scope [(old, Var (rebind old)) | old <- identifiers]
    rewritten = mkVarEnv [(identifier, rewriteId identifier) | identifier <- identifiers]
    rebind identifier = fromMaybe identifier (lookupVarEnv rewritten identifier)
    ordinary identifier = not (isDataConId identifier || isDFunId identifier || isClassOpId identifier)
    rewriteId identifier
      | ordinary identifier = setIdSpecialisation
          (setIdUnfolding (setInlinePragma identifier (mkOpaquePragma NoSourceText)) noUnfolding)
          emptyRuleInfo
      | otherwise = setIdSpecialisation
          (setIdUnfolding identifier (Subst.substUnfolding substitution (realIdUnfolding identifier)))
          (Subst.substRuleInfo substitution identifier (mkRuleInfo
            (filter isBuiltinRule (ruleInfoRules (idSpecialisation identifier)))))
    rewrite (NonRec identifier rhs) = NonRec (rebind identifier) (Subst.substExpr substitution rhs)
    rewrite (Rec pairs) = Rec [(rebind identifier, Subst.substExpr substitution rhs) | (identifier,rhs) <- pairs]
    rightSides (NonRec _ rhs) = [rhs]
    rightSides (Rec pairs) = map snd pairs

topBinders :: CoreBind -> [Id]
topBinders (NonRec binder _) = [binder]
topBinders (Rec pairs) = map fst pairs

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
  | isEmptyVarEnv fixes = binds
  | otherwise = map substTop binds
  where
    fixes :: VarEnv Id
    fixes = foldr addFix emptyVarEnv (concatMap topBinders binds)
    addFix v env = case idIdentity v of
      Just identity | identity `Set.member` retained -> extendVarEnv env v (withhold v)
      _ -> env
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
