module Tidepool.TypePolicy
  ( isGhcCompilerName
  , isGhcCompilerTyCon
  , modulesOfType
  , typeMentionsEffectMonad
  ) where

import Data.Char (isDigit)
import Data.Text (Text)
import qualified Data.List as List
import qualified Data.Text as T
import GHC.Core.DataCon (dataConOrigArgTys)
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Core.TyCo.Rep (Scaled(..), Type(TyConApp))
import GHC.Core.TyCon
  ( TyCon, tyConDataCons_maybe, tyConName, unwrapNewTyCon_maybe )
import GHC.Core.Type (splitFunTy_maybe, splitTyConApp_maybe)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Unique.Set
  ( UniqSet, addOneToUniqSet, elementOfUniqSet, emptyUniqSet )
import qualified GHC.Types.Unique.Set as USet
import GHC.Unit.Module (moduleName, moduleNameString)
import GHC.Unit.Types (moduleUnitId, unitIdString)

-- | Whether a name belongs to the @ghc@ compiler package rather than a
-- runtime package such as @ghc-prim@ or @ghc-internal@. Compiler API values
-- can enter Core through Template Haskell helpers, but cannot be executed by
-- Tidepool's runtime.
isGhcCompilerName :: Name -> Bool
isGhcCompilerName name = case nameModule_maybe name of
  Just m -> case List.stripPrefix "ghc-" (unitIdString (moduleUnitId m)) of
    Just (c : _) -> isDigit c
    _            -> False
  Nothing -> False

isGhcCompilerTyCon :: TyCon -> Bool
isGhcCompilerTyCon = isGhcCompilerName . tyConName

-- | Defining modules required to name a type in generated source.
--
-- The raw head is included alongside GHC's synonym-expanding traversal. This
-- preserves the module of a user-written type synonym while still discovering
-- the modules of types nested beneath it. Results are stable and deduplicated.
modulesOfType :: Type -> [Text]
modulesOfType ty =
  List.sort . List.nub $ headModule ty ++
    [ T.pack (moduleNameString (moduleName m))
    | tc <- USet.nonDetEltsUniqSet (tyConsOfType ty)
    , Just m <- [nameModule_maybe (tyConName tc)]
    ]
  where
    headModule (TyConApp tc _)
      | Just m <- nameModule_maybe (tyConName tc) =
          [T.pack (moduleNameString (moduleName m))]
    headModule _ = []

-- | Whether a type contains freer-simple's @Eff@, including through function
-- arguments, type arguments, newtypes, and data-constructor fields.
--
-- A concrete @Eff@ row is local to one generated program and cannot safely
-- cross into a later compilation. Effect vocabulary types themselves are not
-- rejected: they live in the stable @Tidepool.Effects.Core@ module and have a
-- shared identity across programs using the same vocabulary.
typeMentionsEffectMonad :: Type -> Bool
typeMentionsEffectMonad = goType emptyUniqSet
  where
    goType :: UniqSet TyCon -> Type -> Bool
    goType visited ty
      | Just (_, _, argTy, resultTy) <- splitFunTy_maybe ty =
          goType visited argTy || goType visited resultTy
      | Just (tc, args) <- splitTyConApp_maybe ty =
          isEffTyCon tc || any (goType visited) args || goTyCon visited tc
      | otherwise = False

    isEffTyCon tc =
      occNameString (nameOccName (tyConName tc)) == "Eff"
        && definedIn "Control.Monad.Freer.Internal" tc

    definedIn expected tc =
      maybe False ((== expected) . moduleNameString . moduleName)
        (nameModule_maybe (tyConName tc))

    goTyCon :: UniqSet TyCon -> TyCon -> Bool
    goTyCon visited tc
      | tc `elementOfUniqSet` visited = False
      | isGhcCompilerTyCon tc = False
      | otherwise =
          let visited' = addOneToUniqSet visited tc
              newtypeHit = case unwrapNewTyCon_maybe tc of
                Just (_, representation, _) -> goType visited' representation
                Nothing -> False
              fieldHit = case tyConDataCons_maybe tc of
                Just constructors -> any
                  (any (\(Scaled _ fieldType) -> goType visited' fieldType)
                    . dataConOrigArgTys)
                  constructors
                Nothing -> False
          in newtypeHit || fieldHit
