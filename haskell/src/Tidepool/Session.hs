{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE PatternSynonyms #-}

-- | Option-C session type-carrier — PRODUCTIONIZED from the proven spikes.
--
-- This is the haskell-extract half of the @tidepool-repl@ TYPE plane. A
-- value binding produced by one turn (e.g. @x <- compute@) has a GHC-inferred
-- type we must carry to later turns so references typecheck — WITHOUT GHC ever
-- holding the value (the value lives in the persistent JIT machine's heap,
-- resolved at codegen via the @ExternalEnv@ override keyed on the binder's
-- @stableVarId@; see Resolve.isSessionValVar + emit/expr.rs).
--
-- The mechanism (proven in @spike-optionc/Spike.hs@ + @spike-extract/Spike.hs@,
-- and re-proven from a bare 'TyThing' before this module was written):
--
--   * 'mkThinSessionIface' — synthesize a THIN 'ModIface' for a session module
--     @Tidepool.Session.Val.G<g>@ carrying ONLY the binder's type 'IfaceDecl'.
--     NO @mi_extra_decls@ (we never set @Opt_WriteIfSimplifiedCore@ here) and NO
--     @ifIdUnfolding@ — so the front-end typechecker sees the type but GHC has
--     no body to inline (kimi B1). Built directly from @(OccName, Type)@ pairs,
--     NOT by rendering the type back to source (avoids the ppr round-trip that
--     Option C exists to eliminate).
--
--   * 'writeSessionIface' — 'writeBinIface' it to the session @.hi@.
--
--   * 'injectSessionIface' — per turn, in the fresh batch @runGhc@ extract:
--     'readIface' by RAW path (NOT 'findAndReadIface' — the finder is
--     source-anchored and rejects a source-less module), 'typecheckIface' in
--     'initIfaceCheck' to reconstruct the 'TyThing's, then inject as a NORMAL
--     HPT home module ('addHomeModInfoToHpt' + 'addHomeModuleToFinder' with
--     @ml_hs_file = Nothing@). This sidesteps the @interactive:GhciN@ package
--     and the finder-exclusion blocker.
--
-- GATING: nothing here runs on the normal one-shot eval path. 'SessionScope'
-- with an empty 'ssValIfaces' is inert; 'GhcPipeline.runPipeline' calls the
-- session machinery only when a scope is supplied. See
-- @plans/ghci-implementation-plan.md@ §2 + §5.3.
--
-- INSTANCE REPLAY (kimi #6): 'typecheckIface' faithfully reconstructs an
-- iface's @mi_insts@ into @md_insts@ (verified), so a session module that
-- carries instances makes them available to importing reference modules via the
-- HPT. For Wave 3a's actual cases — binders whose types mention only
-- library classes/types — NO replay is needed: the reference module imports
-- @base@ and resolves @Ord Int@/@Num Int@/… at the use site (spike-extract R4).
-- Replaying a USER/orphan instance whose dfun is DEFINED in the injected module
-- requires the self-reference knot ('if_rec_types') that GHC's real
-- 'GHC.Iface.Load.loadInterface' ties; manual injection of such a module fails
-- with "module … which is not loaded" when the dfun thunk is forced. That only
-- arises with Lane-A session types, so it is a documented Wave-3b follow-on
-- (NOT swept under the rug — see plans/ghci-implementation-plan.md §7.2).
module Tidepool.Session
  ( -- * Identifiers (mirror the Rust domain model §1–2)
    Generation(..)
  , SessionModuleKind(..)
  , SessionModule(..)
  , renderSessionModule
  , sessionModuleString
  , sessionHiPath
    -- * Session scope (what a turn injects; empty = inert)
  , SessionScope(..)
  , emptySessionScope
  , isSessionScopeActive
    -- * The Option-C write/inject mechanism
  , mkThinSessionIface
  , writeSessionIface
  , injectSessionIface
  , injectSessionScope
  ) where

import GHC.Driver.Env
  ( HscEnv, hsc_dflags, hsc_NC, hsc_home_unit, hsc_FC, hscUpdateHPT )
import GHC.Driver.Session (targetProfile)

import GHC.Types.Avail (AvailInfo(..))
import GHC.Types.Name (mkExternalName)
import GHC.Types.Name.Occurrence (OccName)
import GHC.Types.Id (mkVanillaGlobal)
import GHC.Types.TyThing (TyThing(..))
import GHC.Types.Unique (mkUniqueGrimily)
import GHC.Core.Type (Type)

import GHC.Iface.Decl (tyThingToIfaceDecl)
import GHC.Iface.Make (mkIfaceExports)
import GHC.Iface.Binary
  ( writeBinIface, TraceBinIFace(..), CompressionIFace(..) )
import GHC.Iface.Load (readIface)
import GHC.IfaceToCore (typecheckIface)
import GHC.Tc.Utils.Monad (initIfaceCheck)
import GHC.Unit.Module.ModIface
  ( ModIface, emptyFullModIface, set_mi_decls, set_mi_exports )
import GHC.Unit.Module.ModDetails (ModDetails)

import GHC.Unit.Home (homeUnitAsUnit)
import GHC.Unit.Finder (addHomeModuleToFinder)
import GHC.Unit.Module.Location
  ( ModLocation, pattern ModLocation
  , ml_hs_file, ml_hi_file, ml_dyn_hi_file
  , ml_obj_file, ml_dyn_obj_file, ml_hie_file )
import GHC.Unit.Home.ModInfo
  ( HomeModInfo(..), addHomeModInfoToHpt, emptyHomeModInfoLinkable )
import GHC.Unit.Types (mkModule, GenWithIsBoot(..), ModuleNameWithIsBoot)
import GHC.Unit.Module (ModuleName, mkModuleName)
import Language.Haskell.Syntax.ImpExp (IsBootInterface(..))

import GHC.Utils.Fingerprint (fingerprint0)
import GHC.Utils.Outputable (text)
import GHC.Types.SrcLoc (noSrcSpan)
import qualified GHC.Data.Maybe as MErr

import Control.Monad (foldM)
import Control.Monad.IO.Class (MonadIO, liftIO)
import Data.Word (Word64)
import System.Directory (createDirectoryIfMissing)
import System.FilePath (takeDirectory, (</>), (<.>))

--------------------------------------------------------------------------------
-- Identifiers (mirror plans/ghci-domain-model.md §1–2)
--------------------------------------------------------------------------------

-- | Monotonic per-session generation (= GHCi's @ic_mod_index@). Only ever bumped.
newtype Generation = Generation Word64 deriving (Eq, Ord, Show)

-- | @Val@ = value-binding ifaces (this Wave). @Lib@ = user decls (Lane A / Wave 3b).
data SessionModuleKind = ValMod | LibMod deriving (Eq, Show)

-- | The ONE place a gen-versioned session module is represented. The
-- module-name string is produced ONLY by 'sessionModuleString' — no bare
-- @"Tidepool.Session.…"@ strings anywhere else (domain model §2, kills a class
-- of "wrong module string" bugs).
data SessionModule = SessionModule
  { smKind :: !SessionModuleKind
  , smGen  :: !Generation
  } deriving (Eq, Show)

sessionKindString :: SessionModuleKind -> String
sessionKindString ValMod = "Val"
sessionKindString LibMod = "Lib"

-- | @"Tidepool.Session.Val.G3"@. The single source of the module-name string.
sessionModuleString :: SessionModule -> String
sessionModuleString (SessionModule k (Generation g)) =
  "Tidepool.Session." ++ sessionKindString k ++ ".G" ++ show g

renderSessionModule :: SessionModule -> ModuleName
renderSessionModule = mkModuleName . sessionModuleString

-- | Path of the session @.hi@ this module's iface is written to / read from,
-- under @root@. E.g. @root/Tidepool/Session/Val/G3.hi@. (The dotted module name
-- becomes a directory path, matching GHC's own @hiDir@ layout, so the same path
-- is valid for both 'writeSessionIface' and the raw 'readIface' on inject.)
sessionHiPath :: FilePath -> SessionModule -> FilePath
sessionHiPath root sm = root </> dotsToSlashes (sessionModuleString sm) <.> "hi"
  where dotsToSlashes = map (\c -> if c == '.' then '/' else c)

--------------------------------------------------------------------------------
-- Session scope — what a turn brings into the fresh batch session
--------------------------------------------------------------------------------

-- | The session state a turn injects before compiling. @ssValIfaces@ are the
-- live @Val.G<g>@ modules to @readIface@+HPT-inject (domain model §7
-- @SessionScope@). EMPTY = inert: the normal one-shot eval path passes
-- 'emptySessionScope' (or @Nothing@) and nothing here fires.
data SessionScope = SessionScope
  { ssRoot      :: !FilePath          -- ^ dir the session @.hi@ files live under
  , ssValIfaces :: ![SessionModule]   -- ^ inject these (readIface raw -> HPT)
  } deriving (Show)

emptySessionScope :: SessionScope
emptySessionScope = SessionScope "" []

isSessionScopeActive :: SessionScope -> Bool
isSessionScopeActive = not . null . ssValIfaces

--------------------------------------------------------------------------------
-- mkThinSessionIface — synthesize a type-only iface from TyThings
--------------------------------------------------------------------------------

-- | Build a THIN 'ModIface' for a session module carrying ONLY the given
-- binders' type 'IfaceDecl's. No @mi_extra_decls@, no unfoldings: there is
-- nothing for GHC to inline, so a reference to a session binder survives to
-- Core as a bare external @Var@ (the contract; kimi B1).
--
-- The binders are minted as external 'Name's IN the session module
-- (@mkExternalName … modl occ@) with the supplied 'Type'. The 'Unique' is a
-- deterministic per-(gen,index) seed — it is reallocated on read (interface
-- 'Name's are content-addressed by @(Module, OccName)@), so its only job is to
-- keep the binders distinct within this one iface.
mkThinSessionIface :: HscEnv -> SessionModule -> [(OccName, Type)] -> IO ModIface
mkThinSessionIface hsc sm binders = pure iface
  where
    modl = mkModule (homeUnitAsUnit (hsc_home_unit hsc)) (renderSessionModule sm)
    mkBinder (i, (occ, ty)) =
      let nm   = mkExternalName (mkUniqueGrimily (sessionUniqueSeed sm i)) modl occ noSrcSpan
          bndr = mkVanillaGlobal nm ty
      in (nm, AnId bndr)
    tts     = map mkBinder (zip [(0 :: Int) ..] binders)
    decls   = [ (fingerprint0, tyThingToIfaceDecl False tt) | (_, tt) <- tts ]
    exports = mkIfaceExports [ Avail nm | (nm, _) <- tts ]
    iface   = set_mi_exports exports
            $ set_mi_decls   decls
            $ emptyFullModIface modl

-- | A deterministic, collision-free-within-an-iface 'Unique' seed for the
-- @i@-th binder of session module @sm@. Large base to stay clear of builtin
-- uniques during the (transient) write session.
sessionUniqueSeed :: SessionModule -> Int -> Word64
sessionUniqueSeed (SessionModule k (Generation g)) i =
  0x5E550000 + kindBit * 0x100000 + g * 0x1000 + fromIntegral i
  where kindBit = case k of ValMod -> 0; LibMod -> 1

--------------------------------------------------------------------------------
-- writeSessionIface — serialize the thin iface to the session .hi
--------------------------------------------------------------------------------

-- | 'writeBinIface' the (thin) iface to @sessionHiPath root sm@, creating the
-- @Tidepool/Session/<Kind>@ directory if needed. THIN by construction: the
-- write profile carries no simplified-core because 'mkThinSessionIface' never
-- populated @mi_extra_decls@.
writeSessionIface :: HscEnv -> FilePath -> SessionModule -> ModIface -> IO ()
writeSessionIface hsc root sm iface = do
  let prof = targetProfile (hsc_dflags hsc)
      path = sessionHiPath root sm
  createDirectoryIfMissing True (takeDirectory path)
  writeBinIface prof QuietBinIFace NormalCompression path iface

--------------------------------------------------------------------------------
-- injectSessionIface — read the thin .hi back into a fresh session's HPT
--------------------------------------------------------------------------------

-- | Inject a session module's @.hi@ into the current session as a normal HPT
-- home module. Read by RAW path ('readIface'), reconstruct via 'typecheckIface',
-- push 'HomeModInfo' into the HPT, and register a source-less 'ModLocation'
-- (@ml_hs_file = Nothing@) in the finder cache so an @import@ of the module
-- resolves purely from the serialized interface. Returns the updated 'HscEnv'.
--
-- The reconstructed @md_insts@ (if the iface carried any) become available to
-- importing modules through the HPT — see the module header on instance replay.
injectSessionIface :: MonadIO m => FilePath -> SessionModule -> HscEnv -> m HscEnv
injectSessionIface root sm hsc0 = liftIO $ do
  let fc     = hsc_FC hsc0
      homeU  = hsc_home_unit hsc0
      modNm  = renderSessionModule sm
      theMod = mkModule (homeUnitAsUnit homeU) modNm
      path   = sessionHiPath root sm
  readRes <- readIface (hsc_dflags hsc0) (hsc_NC hsc0) theMod path
  case readRes of
    MErr.Failed _ ->
      ioError (userError ("injectSessionIface: readIface failed for "
                          ++ sessionModuleString sm ++ " at " ++ path))
    MErr.Succeeded iface -> do
      details <- injectDetails hsc0 modNm iface
      let hmi    = HomeModInfo iface details emptyHomeModInfoLinkable
          hsc1   = hscUpdateHPT (addHomeModInfoToHpt hmi) hsc0
          modLoc = sourcelessModLocation path
          mnwib  = GWIB modNm NotBoot :: ModuleNameWithIsBoot
      _ <- addHomeModuleToFinder fc homeU mnwib modLoc
      pure hsc1
  where
    injectDetails :: HscEnv -> ModuleName -> ModIface -> IO ModDetails
    injectDetails hsc modNm iface =
      initIfaceCheck (text "tidepool session inject") hsc (typecheckIface iface)

-- | A source-less 'ModLocation' anchored at the session @.hi@ — the load-bearing
-- @ml_hs_file = Nothing@ is what lets the finder accept a module with no source.
sourcelessModLocation :: FilePath -> ModLocation
sourcelessModLocation hi = ModLocation
  { ml_hs_file      = Nothing
  , ml_hi_file      = hi
  , ml_dyn_hi_file  = hi
  , ml_obj_file     = hi   -- never linked (NoLink); a placeholder path
  , ml_dyn_obj_file = hi
  , ml_hie_file     = hi
  }

-- | Inject every live @Val.G<g>@ iface named by a 'SessionScope'. No-op (returns
-- the env unchanged) for an inert scope, so the normal eval path is unaffected.
injectSessionScope :: MonadIO m => SessionScope -> HscEnv -> m HscEnv
injectSessionScope scope hsc =
  foldM (\h sm -> injectSessionIface (ssRoot scope) sm h) hsc (ssValIfaces scope)
