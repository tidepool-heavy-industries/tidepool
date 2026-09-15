module Tidepool.PreparedSites
  ( buildYieldSite
  , SiteRejection(..)
  , elaboratePreparedSites
  , lookupPreparedVerb
  , resolvePreparedSiblings
  ) where

import Control.Monad.State.Strict
import Data.Bits ((.&.), xor)
import Data.List (find)
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Text qualified as T
import Data.Word (Word64)
import GHC.Core
import GHC.Core.Subst (cloneBndrs, mkEmptySubst, substExpr)
import GHC.Core.FVs (exprFreeVars)
import GHC.Types.Var.Env (mkInScopeSet)
import GHC.Core.Make (mkCoreConApps)
import GHC.Builtin.Types (intDataCon)
import GHC.Core.TyCo.Rep (Type, Scaled(..))
import GHC.Data.FastString (fsLit)
import GHC.Types.Unique.Supply (UniqSupply, initUs, mkSplitUniqSupply, takeUniqFromSupply)
import GHC.Core.Type (splitTyConApp_maybe)
import GHC.Types.TyThing.Ppr (pprTyThingInContext)
import GHC.Types.TyThing (TyThing (..))
import GHC.Iface.Type (ShowForAllFlag (..), ShowHowMuch (..), ShowSub (..))
import GHC.Types.Literal (LitNumType (..), Literal (..))
import GHC.Types.Name (isSystemName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Id (Id, idName, mkSysLocal)
import GHC.Utils.Fingerprint (Fingerprint (..), fingerprintString)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)
import GHC.Unit.Module (moduleName, moduleNameString)
import Tidepool.SiteClassifier
import Tidepool.EffectSchema
import Tidepool.Identity (binderQualName)
import Tidepool.TypePolicy
  ( modulesOfType
  , nominalHeadsOfType
  )

data ElaborationState = ElaborationState
  { esUniques :: UniqSupply
  , esCounters :: !(Map T.Text Word64)
  , esSites :: ![YieldSite]
  , esRejections :: ![SiteRejection]
  }

-- | A typed site that cannot carry concrete evidence, attached to the top
-- binder containing it. Whether it is an
-- error depends on the projected target: projection raises it only when that
-- binder's body is executed by the program, so an unrelated helper never
-- rejects a program that does not use it.
--
-- Ownership by top binder is sound because elaboration runs on tidied Core:
-- the top-level binders are final, and CorePrep/STG preparation keeps a site
-- application inside the closure of the top binder that contained it.
data SiteRejection = SiteRejection
  { srBinder :: Id
  , srMessage :: String
  }

-- | Rewrite typed surface sites onto their exact generated siblings while
-- Core still carries types. The caller supplies siblings collected from
-- tidied home-module guts in dependency order. A polymorphic site is left
-- unrewritten and recorded as a 'SiteRejection' for its top binder.
--
-- Every occurrence of a prepared verb is classified exactly once: an
-- application head with its arguments (after 'stripNospecSpine'), and a bare
-- reference (an eta-reduced alias, a higher-rank argument, a verb under a
-- cast or tick) with none, which leaves its result type open. No verb reference leaves
-- elaboration without either a rewrite or a recorded rejection; projection
-- then raises only the rejections its executable closure reaches.
elaboratePreparedSites :: Map String Id -> [CoreBind]
  -> IO ([CoreBind], [YieldSite], [SiteRejection])
elaboratePreparedSites siblings bindings = do
  uniques <- mkSplitUniqSupply 's'
  let (bindings', final) = runState (traverse rewriteBind bindings)
        (ElaborationState uniques mempty [] [])
  pure (bindings', reverse (esSites final), reverse (esRejections final))
  where
    rewriteBind (NonRec binder rhs) =
      NonRec binder <$> rewriteExpr (binder, binderQualName binder) rhs
    rewriteBind (Rec pairs) = Rec <$> traverse (\(binder, rhs) ->
      (binder,) <$> rewriteExpr (binder, binderQualName binder) rhs) pairs

    rewriteExpr origin expression = case expression of
      Var surface | Just _ <- lookupPreparedVerb surface -> rewriteApplication origin expression
      Var{} -> pure expression
      Lit{} -> pure expression
      Type{} -> pure expression
      Coercion{} -> pure expression
      Cast body coercion -> (`Cast` coercion) <$> rewriteExpr origin body
      Tick tick body -> Tick tick <$> rewriteExpr origin body
      Lam binder body -> Lam binder <$> rewriteExpr origin body
      Let binding body -> Let <$> rewriteNested binding <*> rewriteExpr origin body
      Case scrutinee binder resultType alternatives ->
        Case <$> rewriteExpr origin scrutinee <*> pure binder <*> pure resultType
          <*> traverse rewriteAlt alternatives
      App{} -> rewriteApplication origin expression
      where
        rewriteNested (NonRec binder rhs) = NonRec binder <$> rewriteExpr origin rhs
        rewriteNested (Rec pairs) = Rec <$> traverse (\(binder, rhs) ->
          (binder,) <$> rewriteExpr origin rhs) pairs
        rewriteAlt (Alt con binders rhs) =
          Alt con binders <$> rewriteExpr origin rhs

    rewriteApplication origin expression = do
      let (headExpr, arguments) = stripNospecSpine (collectArgs expression)
      rewrittenArguments <- traverse (rewriteExpr origin) arguments
      case headExpr of
        Var surface | Just spec <- lookupPreparedVerb surface -> do
          let (topBinder, originName) = origin
          case classifySiteOccurrence siblings spec surface rewrittenArguments of
            Left failure -> do
              modify' (\current -> current
                {esRejections = SiteRejection topBinder
                  (renderSiteFailure (T.unpack originName) spec failure) : esRejections current})
              pure (mkApps headExpr rewrittenArguments)
            Right plan -> do
              missing <- traverse freshEvidence (spMissingEvidence plan)
              ordinal <- nextOrdinal originName
              let site = buildYieldSite spec originName ordinal (spAnswer plan) (spInputs plan)
                  literal = mkCoreConApps intDataCon
                    [Lit (LitNumber LitNumInt (fromIntegral (ysSite site)))]
              modify' (\current -> current {esSites = site : esSites current})
              current <- get
              let body = mkLams missing (mkApps (Var (spSibling plan))
                    (map Type (spTypeArgs plan) ++ spEvidence plan ++ map Var missing ++ literal : spRest plan))
                  initialSubst = mkEmptySubst (mkInScopeSet (exprFreeVars body))
                  ((substitution, typeBinders), remainingUniques) = initUs (esUniques current)
                    (cloneBndrs initialSubst (spMissingTypes plan))
              put current {esUniques = remainingUniques}
              pure (mkLams typeBinders (substExpr substitution body))
        _ -> do
          rewrittenHead <- rewriteExpr origin headExpr
          pure (mkApps rewrittenHead rewrittenArguments)

    freshEvidence (Scaled mult ty) = do
      current <- get
      let (unique, rest) = takeUniqFromSupply (esUniques current)
      put current {esUniques = rest}
      pure (mkSysLocal (fsLit "siteEvidence") unique mult ty)

    nextOrdinal origin = do
      current <- get
      let ordinal = Map.findWithDefault 0 origin (esCounters current)
      put current {esCounters = Map.insert origin (ordinal + 1) (esCounters current)}
      pure ordinal

-- | Exact generated siblings present in one tidied home module. Merging these
-- maps in dependency order gives later modules the real imported Ids without
-- consulting interface unfoldings or reconstructing names.
resolvePreparedSiblings :: [CoreBind] -> Map String Id
resolvePreparedSiblings bindings = Map.fromList
  [ (vsName spec, binder)
  | spec <- sitedVerbs
  , binder : _ <- [filter (isSibling spec) (concatMap bindersOf bindings)]
  ]
  where
    isSibling spec binder =
      occNameString (nameOccName (idName binder)) == vsSitedName spec
        && not (isSystemName (idName binder))
        && case nameModule_maybe (idName binder) of
          Just modul -> moduleNameString (moduleName modul) == vsSitedModule spec
          Nothing -> False

lookupPreparedVerb :: Id -> Maybe VerbSpec
lookupPreparedVerb binder = find matches sitedVerbs
  where
    matches spec =
      occNameString (nameOccName (idName binder)) == vsName spec
        && case nameModule_maybe (idName binder) of
          Just modul -> moduleNameString (moduleName modul) == vsModule spec
          Nothing -> False

siteType :: Bool -> Type -> SiteType
siteType listAnswer ty = SiteType rendered
  (modulesOfType ty) (nominalHeadsOfType ty)
  where
    base = renderType ty
    rendered = T.pack (if listAnswer then "[" ++ base ++ "]" else base)

renderType :: Type -> String
renderType = renderWithContext defaultSDocContext . ppr

replyDeclaration :: Type -> Maybe T.Text
replyDeclaration ty = case splitTyConApp_maybe ty of
  Nothing -> Nothing
  Just (constructor, _) -> Just (T.pack (renderWithContext defaultSDocContext
    (pprTyThingInContext (ShowSub ShowIface ShowForAllWhen) (ATyCon constructor))))

buildYieldSite :: VerbSpec -> T.Text -> Word64 -> Type -> [Type] -> YieldSite
buildYieldSite spec origin ordinal answer inputs =
  let answerSite = siteType (vsListAnswer spec) answer
      inputSites = map (siteType False) inputs
      identity = siteIdentity spec origin ordinal answerSite inputSites
  in YieldSite identity origin ordinal answerSite inputSites
      (replyDeclaration answer)

siteIdentity :: VerbSpec -> T.Text -> Word64 -> SiteType -> [SiteType] -> Word64
siteIdentity spec origin ordinal answer inputs =
  let Fingerprint high low = fingerprintString
        (T.unpack origin ++ "#" ++ show ordinal ++ "#" ++ vsName spec
          ++ "#" ++ show answer ++ "#" ++ show inputs)
  in max 1 ((high `xor` low) .&. 0x7fffffffffffffff)
