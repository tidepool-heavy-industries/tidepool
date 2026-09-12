module Tidepool.PreparedSites
  ( buildYieldSite
  , elaboratePreparedSites
  , lookupPreparedVerb
  , resolvePreparedSiblings
  ) where

import Control.Exception (throw)
import Control.Monad.State.Strict
import Data.Bits ((.&.), xor)
import Data.List (find)
import Data.List.NonEmpty qualified as NonEmpty
import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import Data.Text qualified as T
import Data.Word (Word64)
import GHC.Core
import GHC.Core.Utils (exprType)
import GHC.Core.Make (mkCoreConApps)
import GHC.Builtin.Types (intDataCon)
import GHC.Core.TyCo.FVs (tyCoVarsOfType)
import GHC.Core.TyCo.Rep (Type)
import GHC.Core.Type (splitTyConApp_maybe)
import GHC.Types.TyThing.Ppr (pprTyThingInContext)
import GHC.Types.TyThing (TyThing (..))
import GHC.Iface.Type (ShowForAllFlag (..), ShowHowMuch (..), ShowSub (..))
import GHC.Types.Literal (LitNumType (..), Literal (..))
import GHC.Types.Name (isSystemName, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Id (Id, idName)
import GHC.Types.Var.Set (isEmptyVarSet)
import GHC.Utils.Fingerprint (Fingerprint (..), fingerprintString)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)
import GHC.Unit.Module (moduleName, moduleNameString)
import Tidepool.DiagJson (SourceRejection (..))
import Tidepool.EffectSchema
import Tidepool.Identity (binderQualName)
import Tidepool.TypePolicy
  ( modulesOfType
  , nominalHeadsOfType
  , stabilizeEffectRows
  )

data ElaborationState = ElaborationState
  { esCounters :: !(Map T.Text Word64)
  , esSites :: ![YieldSite]
  }

-- | Rewrite typed surface sites onto their exact generated siblings while
-- Core still carries types. The caller supplies siblings collected from
-- tidied home-module guts in dependency order.
elaboratePreparedSites :: Map String Id -> [CoreBind] -> ([CoreBind], [YieldSite])
elaboratePreparedSites siblings bindings =
  let (bindings', final) = runState (traverse rewriteBind bindings)
        (ElaborationState mempty [])
  in (bindings', reverse (esSites final))
  where
    rewriteBind (NonRec binder rhs) =
      NonRec binder <$> rewriteExpr (binderQualName binder) rhs
    rewriteBind (Rec pairs) = Rec <$> traverse (\(binder, rhs) ->
      (binder,) <$> rewriteExpr (binderQualName binder) rhs) pairs

    rewriteExpr origin expression = case expression of
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
      let (headExpr, arguments) = collectArgs expression
      rewrittenArguments <- traverse (rewriteExpr origin) arguments
      case headExpr of
        Var surface
          | Just spec <- lookupPreparedVerb surface
          , Just sibling <- Map.lookup (vsName spec) siblings
          , length (filter isValueArgument rewrittenArguments) >= vsValueArity spec
          , let (prefix, values) = splitAt
                  (length rewrittenArguments - vsValueArity spec)
                  rewrittenArguments
          , all isValueArgument values
          , let siteTypes = [ty | Type ty <- prefix]
          , length siteTypes >= vsTypeArgs spec
          , Just visible <- NonEmpty.nonEmpty (take (vsTypeArgs spec) siteTypes) -> do
              let visibleTypes = NonEmpty.toList visible
                  firstType = NonEmpty.head visible
                  answer = case vsAnswerSource spec of
                    FirstTypeArgument -> firstType
                    TypeArgument index -> visibleTypes !! index
                    AppliedResultType -> exprType expression
                  inputs = map (visibleTypes !!) (vsInputTypeArgs spec)
              answer' <- validateType origin (vsName spec) "result" answer
              inputs' <- traverse (validateType origin (vsName spec) "input") inputs
              ordinal <- nextOrdinal origin
              let site = buildYieldSite spec origin ordinal answer' inputs'
                  identity = ysSite site
                  literal = mkCoreConApps intDataCon
                    [Lit (LitNumber LitNumInt (fromIntegral identity))]
              modify' (\current -> current {esSites = site : esSites current})
              pure (mkApps (Var sibling) (prefix ++ literal : values))
        Var surface
          | Just spec <- lookupPreparedVerb surface
          , vsMisShapeIsError spec -> throw (SourceRejection
              (vsName spec ++ " site in " ++ T.unpack origin
                ++ " is not fully applied or has unresolved type arguments"))
        _ -> pure (mkApps headExpr rewrittenArguments)

    -- Type and coercion applications do not satisfy a verb's runtime arity.
    -- In particular, a fully instantiated but value-partial @child@ site has
    -- several Core arguments even though it has no input value.  Counting the
    -- whole spine would rewrite it and inject the site id into the wrong
    -- position, leaving ill-typed Core for the later lint pass.
    isValueArgument Type{} = False
    isValueArgument Coercion{} = False
    isValueArgument _ = True

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

validateType :: T.Text -> String -> String -> Type -> State ElaborationState Type
validateType origin verb position ty = do
  let stable = stabilizeEffectRows ty
  if isEmptyVarSet (tyCoVarsOfType stable)
    then pure stable
    else throw (SourceRejection ("polymorphic " ++ verb ++ " " ++ position
      ++ " site in " ++ T.unpack origin ++ ": " ++ renderType stable))

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
