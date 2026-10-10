-- | Typed application-spine policy shared by both executable backends.
module Tidepool.SiteClassifier
  ( SitePlan(..), SiteFailure(..), classifySiteOccurrence
  , renderSiteFailure, stripNospecSpine, isNospecVar
  ) where

import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import GHC.Core
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.FVs (tyCoVarsOfType)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.Type
  ( Type, mkTyVarTy, splitForAllTyCoVars, piResultTys, splitFunTys
  , mkPiTys, mkVisFunTy, splitInvisPiTys, splitTyConApp_maybe )
import GHC.Types.Id (Id, idName, idType)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (PiTyBinder(..), TyCoVar)
import GHC.Types.Var.Set (isEmptyVarSet)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Utils.Outputable (defaultSDocContext, ppr, renderWithContext)
import Tidepool.EffectSchema
import Tidepool.Identity (normalizeMod)
import Tidepool.TypePolicy (stabilizeEffectRows)

-- | Residual forall and evidence binders represent eta-reduced applications.
-- Consumers introduce fresh binders; site input/result types remain closed.
data SitePlan = SitePlan
  { spSibling :: Id
  , spTypeArgs :: [Type]
  , spMissingTypes :: [TyCoVar]
  , spEvidence :: [CoreExpr]
  , spMissingEvidence :: [Scaled Type]
  , spRest :: [CoreExpr]
  , spAnswer :: Type
  , spInputs :: [Type]
  , spCarrierType :: Type
  }

data SiteFailure
  = MissingTypeArgument Int
  | MissingEvidence Int
  | OpenSiteType SiteTypePosition Type
  | MissingSibling
  | MissingSiteOwner
  | MismatchedSiblingUnit
  | IncompatibleSibling
  | MissingEffectResult

classifySiteOccurrence :: Map String Id -> VerbSpec -> Id -> [CoreExpr]
  -> Either SiteFailure SitePlan
classifySiteOccurrence siblings spec surface args = do
  -- Siblings first: a walk without generated siblings (constructor metadata
  -- over bindings that are never executed) poisons the occurrence and must
  -- never turn a site-shape failure into a rejection.
  sibling <- maybe (Left MissingSibling) Right (Map.lookup (verbKey spec) siblings)
  -- Recognition and sibling resolution check their declared modules. A verb
  -- and sibling may live in different modules, but must share a defining unit.
  case (nameModule_maybe (idName surface), nameModule_maybe (idName sibling)) of
    (Just surfaceOwner, Just siblingOwner)
      | moduleUnit surfaceOwner == moduleUnit siblingOwner -> Right ()
      | otherwise -> Left MismatchedSiblingUnit
    _ -> Left MissingSiteOwner
  let (binders, result) = splitInvisPiTys (idType surface)
      nTypes = length (takeWhile isNamed binders)
      nEvidence = length binders - nTypes
  suppliedTypes <- traverse getType (zip [0..] (take nTypes args))
  let missingTypes = fst (splitForAllTyCoVars (piResultTys (idType surface) suppliedTypes))
      types = suppliedTypes ++ map mkTyVarTy missingTypes
  let (evidence, rest) = splitAt nEvidence (drop nTypes args)
  mapM_ checkEvidence (zip [0..] evidence)
  let missingEvidence = take (nEvidence - length evidence)
        (drop (length evidence) (fst (splitFunTys (piResultTys (idType surface) types))))
  let instantiatedArgs = fst (splitFunTys (piResultTys (idType sibling) types))
  answer <- case vsAnswerSource spec of
    FirstTypeArgument -> typeAt types 0 >>= closed SiteResult
    TypeArgument i -> typeAt types i >>= closed SiteResult
    CarrierReply -> case drop nEvidence instantiatedArgs of
      Scaled _ carrierType : _ -> case splitTyConApp_maybe carrierType of
        Just (_, [_, reply]) -> closed SiteResult reply
        _ -> Left IncompatibleSibling
      _ -> Left IncompatibleSibling
    EffectResult -> do
      let (_, effectType) = splitFunTys (piResultTys (idType surface) types)
      case splitTyConApp_maybe effectType of
        Just (_, [_effects, value]) -> closed SiteResult value
        _ -> Left MissingEffectResult
  inputs <- traverse (\i -> typeAt types i >>= closed SiteInput) (vsInputTypeArgs spec)
  let (siblingBinders, siblingBody) = splitInvisPiTys (idType sibling)
      (siblingArgs, siblingResult) = splitFunTys siblingBody
      rebuild = foldr (\(Scaled mult arg) body -> mkVisFunTy mult arg body) siblingResult
  case (siblingArgs, drop nEvidence instantiatedArgs) of
    (_ : remaining, Scaled _ carrierType : _)
      | eqType (mkPiTys siblingBinders (rebuild remaining)) (mkPiTys binders result) ->
          Right (SitePlan sibling types missingTypes evidence missingEvidence rest answer inputs carrierType)
    _ -> Left IncompatibleSibling
  where
    isNamed Named{} = True
    isNamed _ = False
    getType (_, Type ty) = Right ty
    getType (i, _) = Left (MissingTypeArgument i)
    checkEvidence (_, argument) | isValArg argument = Right ()
    checkEvidence (i, _) = Left (MissingEvidence i)
    typeAt types i = case drop i types of
      ty : _ -> Right ty
      [] -> Left (MissingTypeArgument i)
    closed position ty = let stable = stabilizeEffectRows ty in
      if isEmptyVarSet (tyCoVarsOfType stable)
        then Right stable else Left (OpenSiteType position stable)

renderSiteFailure :: String -> VerbSpec -> SiteFailure -> String
renderSiteFailure origin spec failure = case failure of
  OpenSiteType position ty -> polymorphicSiteMessage (vsName spec) position origin
    (renderWithContext defaultSDocContext (ppr ty))
  _ -> vsName spec ++ " site in " ++ origin ++ ": " ++ case failure of
    MissingTypeArgument i -> "missing type argument " ++ show i
    MissingEvidence i -> "missing constraint evidence " ++ show i
    MissingSibling -> "missing generated site-aware sibling"
    MissingSiteOwner -> "surface verb or generated sibling lacks a defining module"
    MismatchedSiblingUnit -> "surface verb and generated sibling have different defining units"
    IncompatibleSibling -> "generated site-aware sibling has an incompatible type"
    MissingEffectResult -> "expected an Eff result type"

-- | Preserve every argument, including type applications following nospec.
-- Casts are deliberately retained: erasing a cast changes typed Core.
stripNospecSpine :: (CoreExpr, [CoreExpr]) -> (CoreExpr, [CoreExpr])
stripNospecSpine (Tick _ body, arguments) =
  let (headExpr, prefix) = collectArgs body
  in stripNospecSpine (headExpr, prefix ++ arguments)
stripNospecSpine (Var binder, Type _ : function : rest)
  | isNospecVar binder =
      let (headExpr, prefix) = collectArgs function
      in stripNospecSpine (headExpr, prefix ++ rest)
stripNospecSpine spine = spine

isNospecVar :: Id -> Bool
isNospecVar binder = occNameString (nameOccName (idName binder)) == "nospec"
  && maybe False ((== "GHC.Magic") . normalizeMod . moduleNameString . moduleName)
    (nameModule_maybe (idName binder))
