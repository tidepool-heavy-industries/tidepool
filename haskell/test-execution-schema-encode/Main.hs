module Main (main) where

import Control.Monad (unless)
import Data.ByteString qualified as BS
import Tidepool.ExecutionEncode (encodeWireProgram)
import Tidepool.ExecutionSchema

assert :: Bool -> String -> IO ()
assert condition message = unless condition (ioError (userError message))

main :: IO ()
main = do
  let first = encodeWireProgram representative
      second = encodeWireProgram representative
  assert (first == second) "prepared execution encoding is not deterministic"
  assert (BS.take 7 first == BS.pack [0x8c, 0x65, 0x54, 0x50, 0x53, 0x54, 0x47])
    "prepared execution root does not start with [\"TPSTG\", ...]"

representative :: WireProgram
representative = WireProgram envelope signatures globals constructors operations bindings (ValueId 0)
 where
  exact modul occurrence = SymbolIdentity "m3-fixture" modul "value" occurrence
  target = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
  envelope = ProgramEnvelope schemaVersion "ghc-9.12-prepared-stg" "ghc-9.12.2"
    executionAbiVersion target
  signatures =
    [ Signature [LiftedRefRep] [LiftedRefRep]
    , Signature [] [IntRep 64]
    ]
  globals = [GlobalDecl (exact "Fixture.Dependency" "imported") LiftedRefRep
    (Just (SignatureId 0)) False (Just 7)]
  layout = CheckedLayout [FieldLayout (IntRep 64) 0] 8 8 [False]
  constructors = [ConstructorDecl (exact "Fixture.Vertical" "Box")
    (exact "Fixture.Vertical" "Box") LiftedRefRep [IntRep 64] [True] layout 1 1]
  operations = [OperationDecl "sub-int64" (SignatureId 0)]
  result = Return [Scalar (IntLiteral 64 (BS.pack [0,0,0,0,0,0,0,42]))]
  binding = HeapBinding (ValueId 0)
    (Thunk (SignatureId 1) Memoize [Global (GlobalId 0)] result)
  bindings = [Recursive [TopBinding (exact "Fixture.Vertical" "entry") binding]]
