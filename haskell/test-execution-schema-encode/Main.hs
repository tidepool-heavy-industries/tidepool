module Main (main) where

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Term (Term(..), decodeTerm)
import Control.Monad (unless)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import System.Environment (getArgs)
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionSchema

assert :: Bool -> String -> IO ()
assert condition message = unless condition (ioError (userError message))

main :: IO ()
main = do
  arguments <- getArgs
  let first = encodeWireProgram representative
      second = encodeWireProgram representative
  assert (first == second) "prepared execution encoding is not deterministic"
  assert (BS.take 7 first == BS.pack [0x8d, 0x65, 0x54, 0x50, 0x53, 0x54, 0x47])
    "prepared execution root does not start with [\"TPSTG\", ...]"
  assert (termNumber (termList (decode first) !! 1) == fromIntegral schemaVersion)
    "prepared execution schema is not v6"
  let globalFields = termList (head (termList (termList (decode first) !! 7)))
  assert (drop 3 globalFields == [TBool False, TList [TInt 1, TInt 7], TBool False])
    "global wire fields must end with evaluated, tagged generation, dead-end"
  let schema6Fields = termList (decode (encodeWireProgram schema6Representative))
      schema6Constructor = termList (schema6Fields !! 8) !! 0
      schema6Parent = termList (termList schema6Constructor !! 0) !! 4
      schema6Operation = termList (schema6Fields !! 9) !! 0
      schema6Identity = termList (termList schema6Operation !! 0)
  assert (schema6Parent == TList [TInt 1, TString "FixtureRecord"])
    "record-parent identity did not use the tagged parent form"
  assert (schema6Identity == [TInt 1, TString "rintDouble", TList [TInt 0]])
    "intrinsic operation identity did not use the CCall form"

  case arguments of
    [] -> pure ()
    ["--write-schema6-fixture", output] ->
      BS.writeFile output (encodeWireProgram schema6Representative)
    _ -> ioError (userError
      "usage: execution-schema-encode [--write-schema6-fixture output.cbor]")

  let localBody = Let
        (NonRecursive (HeapBinding (ValueId 8)
          (Thunk (SignatureId 1) Memoize [] (Return []))))
        (Case (Return []) (ValueId 7) [] PolymorphicCase
          [Alternative DefaultPattern [] (Return [])])
      localFrames = bodyFrames (representativeWith localBody)
  assert (length localFrames == 5) "local expressions were not flattened into one arena"
  let caseFrame = termList (localFrames !! 3)
      letFrame = termList (localFrames !! 4)
      localGroup = termList (letFrame !! 1)
      localBinding = termList (localGroup !! 1)
      localRhs = termList (localBinding !! 1)
      alternative = termList (termList (caseFrame !! 5) !! 0)
  assert (termNumber (caseFrame !! 1) == 1 && termNumber (alternative !! 2) == 2)
    "case children do not reference postorder frames"
  assert (termNumber (localRhs !! 4) == 0 && termNumber (letFrame !! 2) == 3)
    "local closure and let body do not reference the shared arena"

  let joinBody = LetJoins (Recursive
        [ JoinBinding (JoinId 0) (SignatureId 1) [] (Return [])
        , JoinBinding (JoinId 1) (SignatureId 1) [] (Jump (JoinId 0) [])
        ]) (Return [])
      joinFrames = bodyFrames (representativeWith joinBody)
      joinFrame = termList (joinFrames !! 3)
      joinBindings = termList (termList (joinFrame !! 1) !! 1)
      joinRoots = [termNumber (termList binding !! 3) | binding <- joinBindings]
  assert (length joinFrames == 4 && joinRoots == [0, 1]
    && termNumber (joinFrame !! 2) == 2)
    "join bodies do not reference the shared arena in group order"

  let deepBody = foldl' (\body n -> Let
        (NonRecursive (HeapBinding (ValueId (fromIntegral n))
          (Constructor (ConstructorId 0) []))) body)
        (Return []) [1 :: Int .. 20000]
      deepFrames = bodyFrames (representativeWith deepBody)
      deepRoot = termList (last deepFrames)
  assert (length deepFrames == 20001 && termNumber (deepRoot !! 2) == 19999)
    "deep expression encoding did not produce a flat postorder body"
  assert (maxTermDepth (decode (encodeWireProgram (representativeWith deepBody))) <= 16)
    "deep expression encoding produced nested CBOR"

  let twoBodies = (representativeWith (Return []))
        { programBindings = [Recursive
            [ TopBinding (SymbolIdentity "m3-fixture" "Fixture" "value" "first" Nothing)
                (HeapBinding (ValueId 0) (Thunk (SignatureId 1) Memoize [] (Return [])))
            , TopBinding (SymbolIdentity "m3-fixture" "Fixture" "value" "second" Nothing)
                (HeapBinding (ValueId 1) (Function (SignatureId 1) [] [] (Return [])))
            ]]
        }
      topRoots = topBodyIndices twoBodies
  assert (length (bodyFrames twoBodies) == 2 && topRoots == [0, 1])
    "top-level bodies do not share the program arena"

decode :: BS.ByteString -> Term
decode bytes = case deserialiseFromBytes decodeTerm (BL.fromStrict bytes) of
  Right (rest, term) | BL.null rest -> term
  _ -> error "prepared execution CBOR did not decode completely"

termList :: Term -> [Term]
termList (TList values) = values
termList _ = error "expected CBOR array"

termNumber :: Term -> Integer
termNumber (TInt value) = fromIntegral value
termNumber (TInteger value) = value
termNumber _ = error "expected CBOR integer"

maxTermDepth :: Term -> Int
maxTermDepth (TList children) = 1 + foldl' max 0 (map maxTermDepth children)
maxTermDepth _ = 0

bodyFrames :: WireProgram -> [Term]
bodyFrames program =
  let fields = termList (decode (encodeWireProgram program))
  in termList (fields !! 10)

topBodyIndices :: WireProgram -> [Integer]
topBodyIndices program =
  let fields = termList (decode (encodeWireProgram program))
      groups = termList (fields !! 11)
      group = termList (firstTerm groups)
      bindings = termList (group !! 1)
  in [termNumber (termList (termList (termList binding !! 1) !! 1) !! 4)
     | binding <- bindings]

firstTerm :: [a] -> a
firstTerm (value : _) = value
firstTerm [] = error "expected nonempty CBOR array"

representative :: WireProgram
representative = representativeWith result
 where
  result = Return [Scalar (IntLiteral 64 (BS.pack [0,0,0,0,0,0,0,42]))]

representativeWith :: Expr -> WireProgram
representativeWith body = WireProgram envelope signatures globals constructors operations bindings (ValueId 0)
 where
  exact modul occurrence = SymbolIdentity "m3-fixture" modul "value" occurrence Nothing
  target = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
  envelope = ProgramEnvelope schemaVersion "ghc-9.12-prepared-stg" "ghc-9.12.2"
    executionAbiVersion target
  signatures =
    [ Signature [LiftedRefRep] [LiftedRefRep]
    , Signature [] [IntRep 64]
    ]
  globals = [GlobalDecl (exact "Fixture.Dependency" "imported") LiftedRefRep
    (Just (SignatureId 0)) False False (Just 7)]
  layout = CheckedLayout [FieldLayout (IntRep 64) 0] 8 8 [False]
  constructors = [ConstructorDecl (exact "Fixture.Vertical" "Box")
    (exact "Fixture.Vertical" "Box") LiftedRefRep [IntRep 64] [True] layout 1 1 0]
  operations = [OperationDecl (PrimOpIdentity "sub-int64") (SignatureId 0)]
  binding = HeapBinding (ValueId 0)
    (Thunk (SignatureId 1) Memoize [Global (GlobalId 0)] body)
  bindings = [Recursive [TopBinding (exact "Fixture.Vertical" "entry") binding]]

schema6Representative :: WireProgram
schema6Representative = representative
  { programConstructors =
      [ConstructorDecl
        (SymbolIdentity "m3-fixture" "Fixture" "value" "RecordField" (Just "FixtureRecord"))
        (exact "Fixture.Vertical" "Box") LiftedRefRep [IntRep 64] [True] layout 1 1 0]
  , programOperations =
      [OperationDecl (IntrinsicIdentity "rintDouble" CCall) (SignatureId 2)]
  , programSignatures = programSignatures representative
      <> [Signature [FloatRep 64] [FloatRep 64]]
  }
 where
  layout = CheckedLayout [FieldLayout (IntRep 64) 0] 8 8 [False]
  exact modul occurrence = SymbolIdentity "m3-fixture" modul "value" occurrence Nothing
