module Tidepool.TypePolicy
  ( isGhcCompilerName
  , isGhcCompilerTyCon
  , modulesOfType
  , stabilizeEffectRows
  ) where

import Data.Char (isDigit)
import Data.Text (Text)
import qualified Data.List as List
import qualified Data.Text as T
import GHC.Core.TyCo.FVs (tyConsOfType)
import GHC.Core.TyCo.Rep (Type(..))
import GHC.Core.TyCon
  ( TyCon, isTypeSynonymTyCon, tyConName )
import GHC.Core.Type (coreView, expandTypeSynonyms)
import GHC.Types.Name (Name, nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
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

-- | Replace effect-row aliases with their exact underlying @Eff '[...]@ type.
--
-- Per-incarnation aliases such as @M@ are convenient authored syntax but are
-- the wrong persisted contract: a later compilation could resolve the same
-- spelling to a different row. We therefore expand a synonym only when its
-- fully expanded meaning contains freer-simple's @Eff@. Ordinary domain
-- aliases remain intact, while aliases nested inside functions and records'
-- type arguments are handled recursively.
--
-- This is normalization, not a prohibition. Effectful functions and values
-- may cross a session or typed-suspension boundary; GHC checks their exact row
-- when the receiving program uses them.
stabilizeEffectRows :: Type -> Type
stabilizeEffectRows = go
  where
    go ty@(TyConApp tc args)
      | isTypeSynonymTyCon tc
      , containsEff (expandTypeSynonyms ty)
      , Just expanded <- coreView ty
      = go expanded
      | otherwise = TyConApp tc (map go args)
    go (AppTy f x) = AppTy (go f) (go x)
    go (ForAllTy binder body) = ForAllTy binder (go body)
    go (FunTy flag mult arg result) =
      FunTy flag (go mult) (go arg) (go result)
    go (CastTy ty coercion) = CastTy (go ty) coercion
    go other = other

    containsEff = any isEffTyCon . USet.nonDetEltsUniqSet . tyConsOfType

    isEffTyCon tc =
      occNameString (nameOccName (tyConName tc)) == "Eff"
        && definedIn "Control.Monad.Freer.Internal" tc

    definedIn expected tc =
      maybe False ((== expected) . moduleNameString . moduleName)
        (nameModule_maybe (tyConName tc))
