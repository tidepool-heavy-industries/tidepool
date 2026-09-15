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
module Tidepool.RetainedUnfoldings
  ( installRetainedUnfoldingsPlugin
  , withholdRetainedUnfoldings
  ) where

import Data.Maybe (fromMaybe)
import Data.Set (Set)
import Data.Set qualified as Set
import Data.Text qualified as Text
import GHC.Core
  ( Alt(..), Bind(..), CoreBind, CoreProgram, Expr(..), noUnfolding )
import GHC.Core.Opt.Pipeline.Types (CoreToDo(..), bindsOnlyPass)
import GHC.Data.FastString (unpackFS)
import GHC.Driver.Env.Types (HscEnv(..))
import GHC.Driver.Plugins
  ( Plugin(..), PluginWithArgs(..), Plugins(..), StaticPlugin(..)
  , defaultPlugin )
import GHC.Types.Basic (neverInlinePragma)
import GHC.Types.Id (Id, idName, setIdUnfolding, setInlinePragma)
import GHC.Types.Name (isExternalName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (fieldOcc_maybe, occNameString)
import GHC.Types.Var.Env (VarEnv, emptyVarEnv, extendVarEnv, lookupVarEnv)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import Tidepool.ExecutionSchema (SymbolIdentity(..))

-- | Install the withholding pass on a session's 'HscEnv', ahead of every
-- other registered plugin. A 'Set.null' retained set is a true no-op: the
-- 'HscEnv' is returned unchanged, so an empty retained map (today's default
-- everywhere except this feature's own tests) costs nothing and changes no
-- compiled byte.
installRetainedUnfoldingsPlugin :: Set SymbolIdentity -> HscEnv -> HscEnv
installRetainedUnfoldingsPlugin retained hscEnv
  | Set.null retained = hscEnv
  | otherwise = hscEnv { hsc_plugins = plugins { staticPlugins = staticPlugin : staticPlugins plugins } }
  where
    plugins = hsc_plugins hscEnv
    staticPlugin = StaticPlugin
      { spPlugin = PluginWithArgs
          { paPlugin = withholdingPlugin retained
          , paArguments = []
          }
      -- No 'driverPlugin' action to run; nothing further to initialise.
      , spInitialised = True
      }

withholdingPlugin :: Set SymbolIdentity -> Plugin
withholdingPlugin retained = defaultPlugin
  { installCoreToDos = \_args todos ->
      pure (CoreDoPluginPass "WithholdRetainedUnfoldings" pass : todos)
  }
  where
    pass = bindsOnlyPass (pure . withholdRetainedUnfoldings retained)

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
