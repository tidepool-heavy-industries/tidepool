-- | Typed application-spine policy shared by both executable backends.
module Tidepool.SiteClassifier
  ( SitePlan(..), SiteFailure(..), classifySiteOccurrence
  , renderSiteFailure, stripNospecSpine, isNospecVar
  ) where

import Data.Map.Strict (Map)
import Data.Map.Strict qualified as Map
import GHC.Builtin.Types (intTy)
import GHC.Core
import GHC.Core.TyCo.Compare (eqType)
import GHC.Core.TyCo.FVs (tyCoVarsOfType)
import GHC.Core.TyCo.Rep (Scaled(..))
import GHC.Core.Type
  ( Type, mkTyVarTy, splitForAllTyCoVars, piResultTys, splitFunTys
  , mkPiTys, mkVisFunTyMany, splitInvisPiTys )
import GHC.Types.Id (Id, idName, idType)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Var (PiTyBinder(..), TyCoVar)
import GHC.Types.Var.Set (isEmptyVarSet)
import GHC.Unit.Module (moduleName, moduleNameString)
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
  }

data SiteFailure
  = MissingTypeArgument Int
  | MissingEvidence Int
  | OpenSiteType SiteTypePosition Type
  | MissingSibling
  | IncompatibleSibling

classifySiteOccurrence :: Map String Id -> VerbSpec -> Id -> [CoreExpr]
  -> Either SiteFailure SitePlan
classifySiteOccurrence siblings spec surface args = do
  -- Siblings first: a walk without generated siblings (constructor metadata
  -- over bindings that are never executed) poisons the occurrence and must
  -- never turn a site-shape failure into a rejection.
  sibling <- maybe (Left MissingSibling) Right (Map.lookup (vsName spec) siblings)
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
  answer <- typeAt types (case vsAnswerSource spec of
    FirstTypeArgument -> 0
    TypeArgument i -> i)
    >>= closed SiteResult
  inputs <- traverse (\i -> typeAt types i >>= closed SiteInput) (vsInputTypeArgs spec)
  if eqType (idType sibling) (mkPiTys binders (mkVisFunTyMany intTy result))
    then Right (SitePlan sibling types missingTypes evidence missingEvidence rest answer inputs)
    else Left IncompatibleSibling
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
    IncompatibleSibling -> "generated site-aware sibling has an incompatible type"

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
