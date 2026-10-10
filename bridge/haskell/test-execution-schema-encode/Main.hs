module Main (main, tests) where

import Tidepool.Test.Runner (TestTree, runTests, testCase, testGroup)

import Codec.CBOR.Read (deserialiseFromBytes)
import Codec.CBOR.Encoding (encodeBytes, encodeListLen, encodeString, encodeWord64)
import Codec.CBOR.Term (Term(..), decodeTerm)
import Codec.CBOR.Write (toStrictByteString)
import Control.Exception (bracket)
import Control.Monad (forM_, unless)
import Data.ByteString qualified as BS
import Data.ByteString.Lazy qualified as BL
import Data.Text qualified as T
import Data.IntMap.Strict qualified as IntMap
import Data.Word (Word32)
import System.Directory (getTemporaryDirectory, removeFile)
import System.IO (openBinaryTempFile, hClose)
import Tidepool.ExecutionEncode
  ( encodeWireProgram, encodeProjectedGroup, encodeModuleProducts
  , prepareModuleProductEncoding, moduleProductInput, moduleProductBytes
  , encodeModuleProductInventory )
import Tidepool.ExecutionSchema
import Tidepool.ModuleCandidates (readModuleCandidates)

assert :: Bool -> String -> IO ()
assert condition message = unless condition (ioError (userError message))

candidateManifestChecks :: IO ()
candidateManifestChecks = do
  temp <- getTemporaryDirectory
  bracket (openBinaryTempFile temp "tidepool-candidates.cbor")
    (\(path, _) -> removeFile path)
    (\(path, handle) -> do
      hClose handle
      let digest = T.pack (replicate 64 'a')
          candidate sourcePath productPath = encodeListLen 16
            <> encodeString "main" <> encodeString "Fixture"
            <> encodeString sourcePath <> encodeString digest
            <> encodeString "/tmp/Fixture.hi" <> encodeString digest
            <> encodeString digest <> encodeString digest <> encodeString digest
            <> encodeListLen 0 <> encodeListLen 0
            <> encodeString "/tmp/Fixture.hi.packages" <> encodeString digest
            <> encodeString productPath
            <> encodeListLen 0
            <> encodeListLen 5 <> encodeString "module"
            <> encodeString "/tmp/Fixture.finalized.cbor" <> encodeString digest
            <> encodeString "/tmp/Fixture.core" <> encodeString digest
          manifest version items = toStrictByteString
            (encodeListLen 8 <> encodeString "TPMCAN" <> encodeString version
              <> encodeListLen 0 <> encodeListLen 0
              <> encodeListLen (fromIntegral (length items)) <> mconcat items
              <> encodeListLen 2 <> encodeListLen 0 <> encodeListLen 0 <> encodeString digest <> encodeListLen 1 <> encodeString "fresh-files")
      -- Structural decoding does not authenticate or promote these descriptors.
      let validCandidate = candidate "/tmp/Fixture.hs" "/tmp/Fixture.tpmod"
      BS.writeFile path (manifest "11" [validCandidate])
      valid <- readModuleCandidates path
      assert (case valid of Right [_] -> True; _ -> False)
        "bounded module candidate manifest did not decode"
      forM_ ["6", "7", "8", "9", "10"] $ \version -> do
        BS.writeFile path (manifest version [validCandidate])
        old <- readModuleCandidates path
        assert (case old of Left _ -> True; _ -> False)
          "candidate manifest accepted an unsupported version"
      BS.writeFile path (manifest "11" [candidate "/tmp/Fixture.hs" "relative.tpmod"])
      relativeProduct <- readModuleCandidates path
      assert (case relativeProduct of Left _ -> True; _ -> False)
        "candidate manifest accepted a relative original product path"
      BS.writeFile path (BS.snoc (manifest "11" [validCandidate]) 0)
      trailing <- readModuleCandidates path
      assert (case trailing of Left _ -> True; _ -> False)
        "candidate manifest accepted trailing bytes"
      BS.writeFile path (manifest "11" [validCandidate, validCandidate])
      duplicate <- readModuleCandidates path
      assert (case duplicate of Left _ -> True; _ -> False)
        "candidate manifest accepted duplicate owners"
      BS.writeFile path (manifest "11" [candidate "relative.hs" "/tmp/Fixture.tpmod"])
      relative <- readModuleCandidates path
      assert (case relative of Left _ -> True; _ -> False)
        "candidate manifest accepted a relative source path")

main :: IO ()
main = runTests tests

tests :: TestTree
tests = testGroup "execution-schema-encode"
  [ testCase "bounded candidate descriptor decoding" candidateManifestChecks
  , testCase "module product encoding" moduleProductEncodingChecks
  , testCase "prepared wire structure and identities" encodingChecks
  ]

encodingChecks :: IO ()
encodingChecks = do
  let first = encodeWireProgram representative
      second = encodeWireProgram representative
  assert (first == second) "prepared execution encoding is not deterministic"
  assert (BS.take 7 first == BS.pack [0x91, 0x65, 0x54, 0x50, 0x53, 0x54, 0x47])
    "prepared execution root does not start with [\"TPSTG\", ...]"
  assert (termNumber (termList (decode first) !! 1) == fromIntegral schemaVersion)
    "prepared execution schema version differs from the producer contract"
  let evidenceFields = termList (decode (encodeWireProgram evidenceRepresentative))
      familyTerm = TList
        [ TString "m3-fixture", TString "Fixture", TString "type"
        , TString "Recursive", TList [TInt 0] ]
  assert (length evidenceFields == 17) "prepared schema requires seventeen program fields"
  assert (evidenceFields !! 16 == TList [TInt 0])
    "absent program JSON layout must use the explicit Nothing tag"
  let layout = fmap ConstructorId (JsonLayout 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17)
      jsonFields = termList (decode (encodeWireProgram
        representative { programJsonLayout = Just layout }))
  assert (jsonFields !! 16 == TList [TInt 1, TList (map TInt [0 .. 17])])
    "program JSON layout must preserve all eighteen named roles in wire order"
  let graphFields = termList (evidenceFields !! 13)
      graphNodes = termList (graphFields !! 1)
      graphEdges = termList (graphFields !! 2)
  assert (head graphFields == TInt 1 && length graphFields == 3)
    "finite type graph must carry one versioned node/edge envelope"
  assert (take 3 graphNodes ==
      [ TList [TInt 0, TInt 0, TList [], TString "Recursive"]
      , TList [TInt 1, familyTerm, TList [], TList [TInt 0], TInt 0]
      , TList [TInt 2, TInt 0] ])
    "type graph lost scoped roots, complete nominal symbols or physical constructor references"
  assert (take 4 graphEdges ==
      [ TList [TInt 0, TInt 3, TList [TInt 1]]
      , TList [TInt 1, TInt 2, TList [TInt 10, TInt 1]]
      , TList [TInt 2, TInt 3, TList [TInt 11, TInt 0, TList [TInt 1]]]
      , TList [TInt 2, TInt 5, TList [TInt 11, TInt 1, TList [TInt 4, TInt 64]]] ])
    "type graph lost original recursive fields or paired source representations"
  assert (evidenceFields !! 14 == TList
      [ TList [TInt (41 + ordinal), TString "Fixture.entry", TInt ordinal
          , TInt ordinal, TInt 0, TList [TInt 0, TInt 0]]
      | ordinal <- [0 .. 3]
      ]) "site evidence fields or delivery tags differ from the prepared type graph contract"
  assert (evidenceFields !! 15 == TList
      [TList [TInt 0, TList [TInt 0, TInt 0]]])
    "constructor reply table must pair an exact id with its static node"
  let carrierFields = termList (decode (encodeWireProgram evidenceRepresentative
        { programConstructorReplies = [(ConstructorId 0, ReplyAtSite)] }))
  assert (carrierFields !! 15 == TList [TList [TInt 0, TList [TInt 1]]])
    "constructor reply carrier must use the strict one-field AtSite tag"
  let callerProgram = representative
        { programSignatures = [Signature [LiftedRefRep] CallerResult] }
      callerSignatures = termList (termList (decode (encodeWireProgram callerProgram)) !! 6)
  assert (map (last . termList) callerSignatures == [TList [TInt 2]])
    "caller-chosen result contract did not use tag 2"
  let globalFields = termList (firstTerm (termList (termList (decode first) !! 7)))
  assert (drop 3 globalFields == [TBool False, TList [TInt 1, TInt 7]])
    "global wire fields must end with evaluated, tagged generation"
  let schema6Fields = termList (decode (encodeWireProgram schema6Representative))
      schema6Constructor = termList (schema6Fields !! 8) !! 0
      schema6Parent = termList (termList schema6Constructor !! 0) !! 4
      schema6Operation = termList (schema6Fields !! 9) !! 0
      schema6Identity = termList (termList schema6Operation !! 0)
  assert (schema6Parent == TList [TInt 1, TString "FixtureRecord"])
    "record-parent identity did not use the tagged parent form"
  assert (schema6Identity == [TInt 1, TString "rintDouble", TList [TInt 0]])
    "intrinsic operation identity did not use the CCall form"
  let identityTerms = map (termList . firstTerm . termList)
        (termList (termList (decode (encodeWireProgram identityRepresentative)) !! 9))
  assert (take 2 identityTerms ==
      [ [TInt 2, TString "ffi.lookup"]
      , [TInt 3, TInt 0]
      ])
    "new operation identities did not use capability and wired-in tags"
  assert (drop 2 identityTerms ==
      [[TInt 3, TInt (fromIntegral tagValue)] | tagValue <- [1 :: Int .. 10]])
    "wired-in error kinds did not preserve their stable declaration-order tags"

  let localBody = Let
        (NonRecursive (HeapBinding (ValueId 8)
          (Thunk (SignatureId 1) Memoize [] (Return []))))
        (Case (Return []) (ValueId 7) (Returns []) PolymorphicCase
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

moduleProductEncodingChecks :: IO ()
moduleProductEncodingChecks = do
  let firstGroup = projectedGroup 7 (representativeWith
        (Let (NonRecursive (HeapBinding (ValueId 8)
          (Thunk (SignatureId 1) Memoize [] (Return [])))) (Return [])))
      secondGroup = projectedGroup 2 evidenceRepresentative
      inputs =
        [ ("m3-fixture", "Fixture", BS.pack [0, 255, 1], [firstGroup, secondGroup])
        , ("m3-fixture", "Empty", BS.empty, [])
        , ("other-unit", "Fixture", BS.pack [2, 3], [secondGroup])
        ]
      firstProduct = prepareModuleProductEncoding ("m3-fixture", "Fixture", BS.pack [0, 255, 1], [firstGroup, secondGroup])
      products = firstProduct : map prepareModuleProductEncoding (drop 1 inputs)
      aggregate = encodeModuleProductInventory products
  assert (aggregate == legacyModuleProducts inputs
      && aggregate == encodeModuleProducts inputs)
    "retained module inventory changed canonical aggregate bytes"
  forM_ (zip inputs products) $ \(input, encodedProduct) -> do
    assert (moduleProductInput encodedProduct == input)
      "retained encoding changed original product evidence"
    assert (moduleProductBytes encodedProduct == legacyModuleProducts [input])
      "retained singleton changed canonical module bytes and hash input"
  let modules = termList (termList (decode aggregate) !! 2)
      groupBytes = case modules of
        firstModule : _ -> termList (termList firstModule !! 3)
        [] -> []
  assert (groupBytes == map (TBytes . encodeProjectedGroup) [firstGroup, secondGroup])
    "retained inventory changed original group order or payload bytes"
  assert (aggregate /= moduleProductBytes firstProduct)
    "aggregate module framing was replaced by a singleton document"
  let changedInterface = prepareModuleProductEncoding
        ("m3-fixture", "Fixture", BS.pack [0, 255, 2], [firstGroup, secondGroup])
      changedOrder = prepareModuleProductEncoding
        ("m3-fixture", "Fixture", BS.pack [0, 255, 1], [secondGroup, firstGroup])
  assert (moduleProductBytes changedInterface /= moduleProductBytes firstProduct
      && moduleProductBytes changedOrder /= moduleProductBytes firstProduct)
    "retained encoding lost exact interface or original group order identity"
  assert (encodeModuleProductInventory [] == legacyModuleProducts [])
    "empty retained inventory changed its canonical envelope"
  let lazyProduct = prepareModuleProductEncoding
        ("m3-fixture", "Lazy", BS.singleton 42,
          [firstGroup { projectedBody = error "metadata inspection demanded group encoding" }])
      (unit, name, interface, groups) = moduleProductInput lazyProduct
  assert (unit == "m3-fixture" && name == "Lazy" && interface == BS.singleton 42
      && map projectedOriginalOrdinal groups == [7])
    "retaining canonical bytes eagerly demanded projected bodies"

-- The pre-retention TPMOD framing is an independent byte-equivalence oracle.
legacyModuleProducts :: [(T.Text, T.Text, BS.ByteString, [ProjectedGroup])] -> BS.ByteString
legacyModuleProducts modules = toStrictByteString
  (encodeListLen 3 <> encodeString "TPMOD" <> encodeWord64 1
    <> encodeListLen (fromIntegral (length modules))
    <> foldMap (\(unit, name, interface, groups) ->
      encodeListLen 4 <> encodeString unit <> encodeString name <> encodeBytes interface
        <> encodeListLen (fromIntegral (length groups))
        <> foldMap (encodeBytes . encodeProjectedGroup) groups) modules)

projectedGroup :: Word32 -> WireProgram -> ProjectedGroup
projectedGroup ordinal program = ProjectedGroup ordinal
  [identity | group <- programBindings program
    , TopBinding identity _ <- case group of
        NonRecursive binding -> [binding]
        Recursive bindings -> bindings]
  (ProjectedGroupBody (programEnvelope program) (programSignatures program)
    (programGlobals program) (programConstructors program) (programOperations program)
    (programBindings program) (programTypes program) (programSites program)
    (programConstructorReplies program) (programJsonLayout program))

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
representativeWith body = WireProgram envelope signatures globals constructors operations bindings
  (ValueId 0) (TypeGraph IntMap.empty IntMap.empty) [] [] Nothing
 where
  exact modul occurrence = SymbolIdentity "m3-fixture" modul "value" occurrence Nothing
  target = TargetDescriptor X86_64 LittleEndian 64 64 "sysv64" []
  envelope = ProgramEnvelope schemaVersion "ghc-9.12-prepared-stg" "ghc-9.12.2"
    executionAbiVersion target
  signatures =
    [ Signature [LiftedRefRep] (Returns [LiftedRefRep])
    , Signature [] (Returns [IntRep 64])
    ]
  globals = [GlobalDecl (exact "Fixture.Dependency" "imported") LiftedRefRep
    (Just (SignatureId 0)) False (Just 7)]
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
      [ OperationDecl (IntrinsicIdentity "rintDouble" CCall) (SignatureId 3)
      , OperationDecl (PrimOpIdentity "raise#") (SignatureId 2) ]
  , programSignatures = programSignatures representative
      <> [ Signature [] NoSuccess
         , Signature [FloatRep 64] (Returns [FloatRep 64]) ]
  }
 where
  layout = CheckedLayout [FieldLayout (IntRep 64) 0] 8 8 [False]
  exact modul occurrence = SymbolIdentity "m3-fixture" modul "value" occurrence Nothing

identityRepresentative :: WireProgram
identityRepresentative = representative
  { programOperations =
      OperationDecl (CapabilityIdentity "ffi.lookup") (SignatureId 0)
      : [ OperationDecl (WiredInErrorIdentity kind) (SignatureId 2)
        | kind <- [minBound .. maxBound]
        ]
  , programSignatures = programSignatures representative <> [Signature [AddressRep] NoSuccess]
  }

evidenceRepresentative :: WireProgram
evidenceRepresentative = representative
  { programConstructors =
      [ ConstructorDecl
          (SymbolIdentity "m3-fixture" "Fixture" "value" "Recursive" Nothing)
          family LiftedRefRep [LiftedRefRep, IntRep 64] [False, True]
          (CheckedLayout [FieldLayout LiftedRefRep 0, FieldLayout (IntRep 64) 8]
            8 16 [True, False]) 1 1 0 ]
  , programTypes = TypeGraph (IntMap.fromAscList (zip [0 ..]
      [ TypeRoot ClosedRoot [] "Recursive"
      , TypeDeclaration family [] DataDeclaration UnrestrictedSyntax
      , TypeConstructorTemplate (ConstructorId 0)
      , TypeNominalApplication
      , TypeDeclaration (family { symbolOccurrence = "Int#" }) [] (ScalarDeclaration (IntRep 64)) UnrestrictedSyntax
      , TypeNominalApplication
      , TypeDeclaration (family { symbolOccurrence = "Text" }) [] TextDeclaration UnrestrictedSyntax
      , TypeDeclaration (family { symbolOccurrence = "Integer" }) [] IntegerDeclaration UnrestrictedSyntax
      , TypeDeclaration (family { symbolOccurrence = "Natural" }) [] NaturalDeclaration UnrestrictedSyntax
      , TypeFunction TypeToType
      , TypeRoot ConstructorSchemeRoot [SourceSpecified] "Int -> Int"
      , TypeBound 0 ])) (IntMap.fromAscList
      [ (0, [(TypeBody, TypeNodeId 3)])
      , (1, [(TypeConstructor 1, TypeNodeId 2)])
      , (2, [(TypeField 0 LiftedRefRep, TypeNodeId 3), (TypeField 1 (IntRep 64), TypeNodeId 5)])
      , (3, [(TypeHead, TypeNodeId 1)])
      , (5, [(TypeHead, TypeNodeId 4)])
      , (9, [(TypeMultiplicity, TypeNodeId 5), (TypeDomain, TypeNodeId 5), (TypeCodomain, TypeNodeId 5)])
      , (10, [(TypeBinderKind 0, TypeNodeId 5), (TypeBody, TypeNodeId 9)]) ])
  , programSites =
      [ SiteRow (41 + ordinal) "Fixture.entry" ordinal delivery
          (TypeNodeId 0) [TypeNodeId 0, TypeNodeId 0]
      | (ordinal, delivery) <- zip [0 ..]
          [HostAnswer, LiveReentry, ExitCellFill, TerminalCapture] ]
  , programConstructorReplies = [(ConstructorId 0, StaticReply (TypeNodeId 0))]
  }

 where
  family = SymbolIdentity "m3-fixture" "Fixture" "type" "Recursive" Nothing
