module CallerResultProjectionTest (verifyCallerResultProjection) where

import Control.Monad (unless)
import Data.Map.Strict qualified as Map
import Data.Text qualified as Text
import Tidepool.ExecutionProjection
import Tidepool.ExecutionSchema
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult(..), runPipelineSelected )

verifyCallerResultProjection :: IO ()
verifyCallerResultProjection = do
  prepared <- runPipelineSelected PreparedStg "test-prepared-stg/RepPoly.hs" ["test-prepared-stg"]
  program <- project "RepPoly" "result" mempty (pprModules prepared)
  let callable = [signature | signature <- programSignatures program
                            , signatureResults signature == CallerResult]
  assert (not (null callable) && all (not . null . signatureArguments) callable)
    "representation-polymorphic function lost its caller-chosen result"
  let applyIds = [heapBindingId binding | group <- programBindings program
                 , TopBinding symbol binding <- groupItems group
                 , symbolOccurrence symbol == "applyTo"]
      calls = [expression | body <- topBodies program, expression@Call{} <- expressions body]
      demands = [signatureResults (signatureAt program signature)
                | Call (Ref (Local callee)) signature _ <- calls, callee `elem` applyIds]
  assert (Returns [LiftedRefRep] `elem` demands && Returns [IntRep 64] `elem` demands)
    "lifted and unboxed demands for the same applyTo did not survive preparation"
  assert (any (\expression -> case expression of
    Call _ signature _ -> signatureResults (signatureAt program signature) == CallerResult
    _ -> False) calls) "polymorphic body did not forward the caller demand"
  case projectPreparedTarget (context "RepPoly" "applyTo" mempty) (pprModules prepared) of
    Left (InvalidPreparedRepresentation _) -> pure ()
    other -> fail ("polymorphic program entry was not rejected: " ++ show other)
  joinedProgram <- project "RepPoly" "joined" mempty (pprModules prepared)
  let joins = [signatureResults (signatureAt joinedProgram signature)
              | body <- topBodies joinedProgram, LetJoins group _ <- expressions body
              , JoinBinding _ signature _ _ <- groupItems group]
  assert (CallerResult `elem` joins) "representation-polymorphic join was optimized away or misprojected"
  imported <- runPipelineSelected PreparedStg "test-prepared-stg/RepPolyImport.hs" ["test-prepared-stg"]
  let provider = SymbolIdentity "main" "RepPoly" "value" "applyTo" Nothing
  consumer <- project "RepPolyImport" "result" (Map.singleton provider 7) (pprModules imported)
  -- Home-source references may have LFUnknown. Preserve that absence of
  -- evidence; the retained provider's descriptor supplies the callable ABI.
  assert (any (\global -> globalIdentity global == provider
      && globalRequiredGeneration global == Just 7
      && maybe True ((== CallerResult) . signatureResults . signatureAt consumer)
           (globalEntrySignature global)) (programGlobals consumer))
    "retained callable import lost its generation or contradicted its result contract"
  assert (any (\expression -> case expression of
      Call (Ref (Global _)) signature _ ->
        signatureResults (signatureAt consumer signature) == Returns [IntRep 64]
      _ -> False) (concatMap expressions (topBodies consumer)))
    "cross-module call lost its concrete unboxed result demand"
  where
    project modul entry retained modules = either (fail . show) pure
      (projectPreparedTarget (context modul entry retained) modules)
    context modul entry retained = ProjectionContext
      { projectionProfile = "ghc-9.12-prepared-stg"
      , projectionToolchain = "ghc-9.12.2"
      , projectionTarget = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
      , projectionRetainedGenerations = retained
      , projectionEntry = SymbolIdentity "main" (Text.pack modul) "value" (Text.pack entry) Nothing
      , projectionAuxiliaryRoots = []
      , projectionFormattingAuthority = Nothing
      , projectionTimeAuthority = Nothing
      , projectionTextUnit = Nothing
      }
    assert condition message = unless condition (fail message)

signatureAt :: WireProgram -> SignatureId -> Signature
signatureAt program (SignatureId index) = case drop (fromIntegral index) (programSignatures program) of
  signature : _ -> signature
  [] -> error "projected signature reference is out of bounds"

groupItems :: Group a -> [a]
groupItems (NonRecursive binding) = [binding]
groupItems (Recursive bindings) = bindings

topBodies :: WireProgram -> [Expr]
topBodies program = [body | group <- programBindings program
                         , TopBinding _ (HeapBinding _ rhs) <- groupItems group
                         , body <- rhsBodies rhs]

rhsBodies :: HeapRhs -> [Expr]
rhsBodies (Function _ _ _ body) = [body]
rhsBodies (Thunk _ _ _ body) = [body]
rhsBodies Constructor{} = []
rhsBodies Bytes{} = []

expressions :: Expr -> [Expr]
expressions expression = expression : case expression of
  Case scrutinee _ _ _ alternatives -> expressions scrutinee
    ++ concat [expressions body | Alternative _ _ body <- alternatives]
  Let group body -> expressions body ++ concat
    [concatMap expressions (rhsBodies rhs) | HeapBinding _ rhs <- groupItems group]
  LetJoins group body -> expressions body ++ concat
    [expressions rhs | JoinBinding _ _ _ rhs <- groupItems group]
  _ -> []
