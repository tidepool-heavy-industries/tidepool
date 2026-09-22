-- | Exact GHC-owned facts retained with a prepared module.
--
-- These values are internal compiler data, not a wire format. The M3 projector
-- can consume them without reconstructing signatures or layouts from rendered
-- names, and can reject forms it does not support.
module Tidepool.PreparedFacts
  ( PreparedFacts(..)
  , PreparedOperation(..)
  , PreparedRepresentation(..)
  , PreparedSupport(..)
  , extractPreparedFacts
  ) where

import GHC.Core (AltCon(..))
import GHC.Core.DataCon (DataCon, dataConRepArgTys)
import GHC.Core.TyCo.Rep (Scaled(..), Type)
import GHC.Stg.Syntax
import GHC.Stg.Pipeline (StgCgInfos)
import GHC.Types.Literal (Literal(..), literalType)
import GHC.Types.Name (nameModule_maybe)
import GHC.Types.Name.Env (emptyNameEnv, plusNameEnv)
import GHC.Types.RepType (PrimRep, typePrimRep_maybe)
import GHC.Types.Var (Id, varName, varType)
import GHC.Types.Var.Set (dVarSetElems)
import GHC.Unit.Types (Module)

data PreparedRepresentation
  = PreparedKnownReps [PrimRep]
  | PreparedRuntimePolymorphic Type

data PreparedOperation = PreparedOperation
  { preparedOperation :: StgOp
  , preparedArgumentReps :: [PreparedRepresentation]
  , preparedResultReps :: PreparedRepresentation
  }

data PreparedSupport
  = CharacterLiteralsSupported
  | EmbeddedNulStringsRetainedAsModifiedUtf8
  deriving stock (Eq, Ord, Show)

data PreparedFacts = PreparedFacts
  { preparedReferencedIds :: [Id]
  , preparedImportedIds :: [Id]
  , preparedClosureCaptures :: [(Id, [Id])]
  , preparedConstructors :: [(DataCon, [PreparedRepresentation])]
  , preparedOperations :: [PreparedOperation]
  , preparedLiterals :: [Literal]
  , preparedTagSigs :: StgCgInfos
  , preparedSupport :: [PreparedSupport]
  }

instance Semigroup PreparedFacts where
  PreparedFacts a b c d e f g h <> PreparedFacts i j k l m n o p =
    PreparedFacts (a <> i) (b <> j) (c <> k) (d <> l) (e <> m)
      (f <> n) (plusNameEnv g o) (h <> p)

instance Monoid PreparedFacts where
  mempty = PreparedFacts [] [] [] [] [] [] emptyNameEnv []

extractPreparedFacts :: Module -> StgCgInfos -> [CgStgTopBinding] -> PreparedFacts
extractPreparedFacts thisModule tagSigs bindings = collected
  { preparedImportedIds = filter isImport (preparedReferencedIds collected)
  , preparedTagSigs = tagSigs
  , preparedSupport =
      [CharacterLiteralsSupported, EmbeddedNulStringsRetainedAsModifiedUtf8]
  }
 where
  collected = foldMap walkTop bindings
  isImport binder = case nameModule_maybe (varName binder) of
    Just definingModule -> definingModule /= thisModule
    Nothing -> False

walkTop :: CgStgTopBinding -> PreparedFacts
walkTop (StgTopStringLit _ bytes) = literalFact (LitString bytes)
walkTop (StgTopLifted binding) = walkBinding binding

walkBinding :: CgStgBinding -> PreparedFacts
walkBinding (StgNonRec binder rhs) = walkRhs binder rhs
walkBinding (StgRec pairs) = foldMap (uncurry walkRhs) pairs

walkRhs :: Id -> CgStgRhs -> PreparedFacts
walkRhs binder (StgRhsClosure captures _ _ _ body _) =
  mempty { preparedClosureCaptures = [(binder, dVarSetElems captures)] } <> walkExpr body
walkRhs _ (StgRhsCon _ con _ _ args _) =
  constructorFact con <> foldMap walkArg args

walkExpr :: CgStgExpr -> PreparedFacts
walkExpr (StgApp function args) = reference function <> foldMap walkArg args
walkExpr (StgLit literal) = literalFact literal
walkExpr (StgConApp con _ args _) = constructorFact con <> foldMap walkArg args
walkExpr (StgOpApp op args resultType) =
  mempty
    { preparedOperations =
        [ PreparedOperation op (map argumentReps args) (representation resultType) ]
    }
    <> foldMap walkArg args
walkExpr (StgCase scrutinee _ _ alternatives) =
  walkExpr scrutinee <> foldMap walkAlt alternatives
walkExpr (StgLet _ binding body) = walkBinding binding <> walkExpr body
walkExpr (StgLetNoEscape _ binding body) = walkBinding binding <> walkExpr body
walkExpr (StgTick _ body) = walkExpr body

walkAlt :: CgStgAlt -> PreparedFacts
walkAlt (GenStgAlt con _ body) = altFact con <> walkExpr body

altFact :: AltCon -> PreparedFacts
altFact (DataAlt con) = constructorFact con
altFact (LitAlt literal) = literalFact literal
altFact DEFAULT = mempty

walkArg :: StgArg -> PreparedFacts
walkArg (StgVarArg binder) = reference binder
walkArg (StgLitArg literal) = literalFact literal

reference :: Id -> PreparedFacts
reference binder = mempty { preparedReferencedIds = [binder] }

literalFact :: Literal -> PreparedFacts
literalFact literal = mempty { preparedLiterals = [literal] }


constructorFact :: DataCon -> PreparedFacts
constructorFact con = mempty
  { preparedConstructors =
      [(con, map (representation . scaledThing) (dataConRepArgTys con))]
  }
 where
  scaledThing (Scaled _ ty) = ty

argumentReps :: StgArg -> PreparedRepresentation
argumentReps (StgVarArg binder) = representation (varType binder)
argumentReps (StgLitArg literal) = representation (literalType literal)

-- | Preserve levity-polymorphic evidence without forcing GHC's partial
-- 'typePrimRep'. M3 can resolve or reject the retained 'Type'; the provisional
-- inventory must remain total on real prepared STG.
representation :: Type -> PreparedRepresentation
representation ty = case typePrimRep_maybe ty of
  Just reps -> PreparedKnownReps reps
  Nothing -> PreparedRuntimePolymorphic ty
