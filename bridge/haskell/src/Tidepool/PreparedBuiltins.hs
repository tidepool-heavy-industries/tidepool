-- | Compiler-owned identities, not spelling-based recovery fallbacks.
-- Projection synthesizes callable execution tops while preserving and checking
-- their authoritative saturated signatures.
module Tidepool.PreparedBuiltins
  ( DeferredFunction(..)
  , deferredFunction
  , wiredInErrorKind
  , nullaryWorkerConstructor
  ) where

import Data.Text (Text)
import GHC.Builtin.Names qualified as Names
import GHC.Types.Id (Id, isDataConWorkId_maybe)
import GHC.Core.DataCon (DataCon, dataConRepArity, dataConRepArgTys, isUnboxedTupleDataCon)
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
  -- The pinned getDecodedStackArray worker owns IPE lookup and decoding;
  -- deferring only its foreign leaf leaves the encoding cleanup closure live.
  , ( ("ghc-internal", "GHC.Internal.Stack.CloneStack", "$wgo")
    , DeferredFunction "ghc:decodeStackEntries"
        (Signature [UnliftedRefRep, IntRep 64, VoidRep] (Returns [LiftedRefRep]))
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

-- | A genuinely nullary data-con worker denotes an evaluated object, not an
-- executable import. Requiring no representation arguments also excludes
-- workers whose logical Void arguments still require application.
nullaryWorkerConstructor :: Id -> Maybe DataCon
nullaryWorkerConstructor binder = do
  con <- isDataConWorkId_maybe binder
  if dataConRepArity con == 0 && null (dataConRepArgTys con)
      && not (isUnboxedTupleDataCon con)
    then Just con
    else Nothing
