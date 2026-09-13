-- | Compiler-owned identities, not spelling-based recovery fallbacks.
-- Projection synthesizes callable execution tops while preserving and checking
-- their authoritative saturated signatures.
module Tidepool.PreparedBuiltins
  ( DeferredFunction(..)
  , deferredFunction
  , wiredInErrorKind
  ) where

import Data.Text (Text)
import GHC.Builtin.Names qualified as Names
import GHC.Types.Id (Id)
import GHC.Types.Name (nameModule_maybe, nameOccName)
import GHC.Types.Name.Occurrence (occNameString)
import GHC.Types.Unique (getUnique)
import GHC.Types.Var (varName)
import GHC.Unit.Module (moduleName, moduleNameString, moduleUnit)
import GHC.Unit.Types (unitString)
import Tidepool.ExecutionSchema
  ( ResultContract(Returns), RuntimeRep(..), Signature(..)
  , WiredInErrorKind(..) )

-- | Exact compiler-library functions whose boundary is owned by the runtime.
-- Their GHC entry signature remains authoritative and is checked at every
-- recovered or imported use before projection substitutes the body.
data DeferredFunction = DeferredFunction
  { deferredCapability :: Text
  , deferredSignature :: Signature
  } deriving stock (Eq, Show)

deferredFunction :: Id -> Maybe DeferredFunction
deferredFunction binder = do
  owner <- nameModule_maybe (varName binder)
  lookup
    ( unitString (moduleUnit owner)
    , moduleNameString (moduleName owner)
    , occNameString (nameOccName (varName binder))
    ) deferredFunctions

deferredFunctions :: [((String, String, String), DeferredFunction)]
deferredFunctions =
  [ ( ("ghc-internal", "GHC.Internal.ExecutionStack.Internal", "stackFrames")
    , DeferredFunction "ghc:stackFrames"
        (Signature [LiftedRefRep] (Returns [LiftedRefRep]))
    )
  , ( ("ghc-internal", "GHC.Internal.ExecutionStack.Internal", "collectStackTrace")
    , DeferredFunction "ghc:collectStackTrace"
        (Signature [VoidRep] (Returns [LiftedRefRep]))
    )
  , ( ("ghc-internal", "GHC.Internal.ExecutionStack.Internal", "collectStackTrace1")
    , DeferredFunction "ghc:collectStackTrace"
        (Signature [VoidRep] (Returns [LiftedRefRep]))
    )
  , ( ("ghc-internal", "GHC.Internal.Stack.CCS", "$wgo")
    , DeferredFunction "ghc:ccsToStrings"
        (Signature [AddressRep, LiftedRefRep, VoidRep] (Returns [LiftedRefRep]))
    )
  ]

wiredInErrorKind :: Id -> Maybe WiredInErrorKind
wiredInErrorKind binder = lookup (getUnique binder)
  [ (Names.patErrorIdKey, WiredPatternMatch)
  , (Names.nonExhaustiveGuardsErrorIdKey, WiredNonExhaustiveGuards)
  , (Names.recSelErrorIdKey, WiredRecordSelector)
  , (Names.recConErrorIdKey, WiredRecordConstruction)
  , (Names.noMethodBindingErrorIdKey, WiredNoMethodBinding)
  , (Names.typeErrorIdKey, WiredDeferredType)
  , (Names.impossibleErrorIdKey, WiredImpossible)
  , (Names.impossibleConstraintErrorIdKey, WiredImpossibleConstraint)
  , (Names.absentErrorIdKey, WiredAbsent)
  , (Names.absentConstraintErrorIdKey, WiredAbsentConstraint)
  , (Names.absentSumFieldErrorIdKey, WiredAbsentSumField)
  ]
