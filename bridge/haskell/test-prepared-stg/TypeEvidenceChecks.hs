module TypeEvidenceChecks (runTypeEvidenceChecks) where

import Control.Monad (unless)
import Data.Text qualified as Text
import System.Directory (createDirectoryIfMissing)
import System.FilePath ((</>))
import Tidepool.ExecutionProjection (ProjectionError)
import Tidepool.ExecutionSchema
import Tidepool.GhcPipeline
  ( PipelineSelection(PreparedStg), PreparedPipelineResult, runPipelineSelected )

-- Compile once, then select each entry independently to exercise the same
-- reachable-owner filtering used by notebook artifact production.
runTypeEvidenceChecks :: FilePath
  -> (PreparedPipelineResult -> String -> Either ProjectionError WireProgram)
  -> (PreparedPipelineResult -> String -> [String] -> Either ProjectionError WireProgram)
  -> IO ()
runTypeEvidenceChecks directory project projectWithAux = do
  let target = directory </> "TypeEvidence.hs"
  createDirectoryIfMissing True (directory </> "Tidepool" </> "Effects")
  writeFile (directory </> "Tidepool" </> "Effects" </> "Core.hs") (unlines
    [ "{-# LANGUAGE ExplicitForAll #-}"
    , "{-# LANGUAGE GADTs #-}"
    , "module Tidepool.Effects.Core where"
    , "data AgentSession a where"
    , "  AgentAttachWith :: Maybe String -> AgentSession ()"
    , "data AgentTools a where"
    , "  AgentToolsInstallWith :: AgentTools ()"
    ])
  createDirectoryIfMissing True (directory </> "Tidepool" </> "Internal")
  readFile "test-prepared-stg/site-fixtures/RequestSite.hs" >>= writeFile (directory </> "Tidepool" </> "Internal" </> "RequestSite.hs")
  writeFile (directory </> "Tidepool" </> "Actor.hs") (unlines
    [ "{-# LANGUAGE DataKinds, ExplicitForAll #-}"
    , "module Tidepool.Actor where"
    , "import Tidepool.Internal.RequestSite (RequestSite)"
    , "{-# OPAQUE receive #-}"
    , "receive :: forall answer. String -> Maybe answer"
    , "receive _ = Nothing"
    , "{-# OPAQUE receiveSited #-}"
    , "receiveSited :: forall answer. RequestSite '[] answer -> String -> Maybe answer"
    , "receiveSited _ _ = Nothing"
    ])
  readFile "test-prepared-stg/site-fixtures/TypeEvidence.hs" >>= writeFile target
  result <- runPipelineSelected PreparedStg target [directory]
  secondResult <- runPipelineSelected PreparedStg target [directory]
  let program entry = either
        (ioError . userError . ((entry ++ ": ") ++) . show) pure (project result entry)
      answer entry = do
        wire <- program entry
        case programSites wire of
          [site] -> pure (wire, nodeAt wire (siteWire site))
          sites -> ioError (userError (entry ++ ": expected one selected site, got "
            ++ show (length sites)))
      dataAnswer entry expected = do
        (wire, node) <- answer entry
        case node of
          TypeData family arguments rows -> do
            assert (symbolOccurrence family == expected)
              (entry ++ ": incorrect normalized family " ++ show family)
            pure (wire, arguments, rows)
          other -> ioError (userError (entry ++ ": expected data evidence, got " ++ show other))
      leafAnswer entry expected constructors = do
        (wire, node) <- answer entry
        assert (node == expected) (entry ++ ": wrong leaf evidence " ++ show node)
        let names = map (symbolOccurrence . constructorIdentity) (programConstructors wire)
        assert (all (`elem` names) constructors)
          (entry ++ ": missing closure-only leaf constructors " ++ show names)

  (_, _, boolRows) <- dataAnswer "boolAnswer" "Bool"
  assert (length boolRows == 2) "Bool evidence must declare both constructors"
  _ <- dataAnswer "nestedIdentity" "Int"
  (_, _, maybeRows) <- dataAnswer "higherKinded" "Maybe"
  assert (length maybeRows == 2) "higher-kinded newtype erased to incomplete Maybe"
  (_, gadt) <- answer "impossibleGadt"
  assert (isRefusal gadt) "GADT evidence admitted OnlyInt into Choice Bool"
  (_, loop) <- answer "recursiveNewtype"
  assert (isRefusal loop) "recursive newtype did not produce a bounded refusal"

  (chain, _, chainRows) <- dataAnswer "recursiveData" "Chain"
  chainRoot <- case programSites chain of
    [site] -> pure (siteWire site)
    _ -> ioError (userError "recursiveData: expected one selected site")
  assert (map rowFields chainRows == [[], [chainRoot]])
    "recursive data evidence did not close its constructor-field cycle"
  (nest, _) <- answer "expandingData"
  assert (length (programTypes nest) <= 65536
    && any isRefusal (programTypes nest))
    "nonregular recursive evidence did not stop at a bounded refusal"

  (phantomIntWire, phantomIntArgs, _) <- dataAnswer "phantomInt" "Phantom"
  (phantomBoolWire, phantomBoolArgs, _) <- dataAnswer "phantomBool" "Phantom"
  assert (map (familyAt phantomIntWire) phantomIntArgs == [Just "Int"]
    && map (familyAt phantomBoolWire) phantomBoolArgs == [Just "Bool"])
    "phantom type arguments disappeared from evidence"
  (leftWire, leftArgs, leftRows) <- dataAnswer "eitherIntBool" "Either"
  (rightWire, rightArgs, _) <- dataAnswer "eitherBoolInt" "Either"
  assert (map (familyAt leftWire) leftArgs == [Just "Int", Just "Bool"]
    && map (familyAt rightWire) rightArgs == [Just "Bool", Just "Int"]
    && length leftRows == 2)
    "Either evidence lost argument order or the never-matched constructor"

  leafAnswer "textAnswer" TypeText ["Text"]
  leafAnswer "integerAnswer" TypeInteger ["IS", "IP", "IN"]
  leafAnswer "naturalAnswer" TypeNatural ["NS", "NB"]
  (_, packed) <- answer "packedAnswer"
  assert (isRefusal packed) "UNPACK layout was admitted as source-field layout"
  printWire <- program "printRequest"
  printNode <- verbAnswer printWire "Print"
  assert (familyOf printNode `elem` [Just "Unit", Just "()"])
    ("Print's intrinsic reply is not unit: " ++ show printNode)
  fetchWire <- program "fetchRequest"
  fetchNode <- verbAnswer fetchWire "Fetch"
  case fetchNode of
    TypeData family arguments rows -> assert
      (symbolOccurrence family == "Either" && length rows == 2
        && map (familyAt fetchWire) arguments == [Just "Bool", Nothing]
        && map (nodeAt fetchWire) (drop 1 arguments) == [TypeText])
      ("Fetch's intrinsic reply lost its Either Bool Text evidence: " ++ show fetchNode)
    other -> ioError (userError ("Fetch reply is not data evidence: " ++ show other))
  echoWire <- program "echoRequest"
  echoNode <- verbAnswer echoWire "Echo"
  assert (isRefusal echoNode) "an open reply index acquired structural construction authority"
  functionWire <- program "functionRequest"
  functionNode <- verbAnswer functionWire "FunctionReply"
  assert (isRefusal functionNode) "a function reply index acquired structural construction authority"
  profileWire <- program "profileWitness"
  assert (null (programConstructorReplies profileWire))
    "an effect-list witness acquired a lifted reply graph"
  firstChoices <- mapM program ["polyChoice", "polyChoiceNested"]
  secondChoices <- mapM (\entry -> either (ioError . userError . show) pure (project secondResult entry))
    ["polyChoice", "polyChoiceNested"]
  mapM_ (\wire -> do
      choice <- verbAnswer wire ":|"
      assert (case choice of TypeData family _ _ -> symbolOccurrence family == "Either"; _ -> False)
        "ordinary saturated alternatives constructor lacks its intrinsic partial reply graph")
    (firstChoices <> secondChoices)
  customWire <- program "customSend"
  customNode <- verbAnswer customWire "Print"
  assert (familyOf customNode `elem` [Just "Unit", Just "()"])
    "custom Member/send effect without KnownEffect lost intrinsic reply evidence"
  partialWire <- program "partialReply"
  partialNode <- verbAnswer partialWire "PartialReply"
  case partialNode of
    TypeData family [payload] rows -> do
      assert (symbolOccurrence family == "Maybe" && map rowFields rows == [[], [payload]])
        "partial Maybe reply lost its fieldless Nothing branch"
      assert (isRefusal (nodeAt partialWire payload)) "partial Maybe payload became constructible"
    other -> ioError (userError ("partial Maybe reply lacks its graph: " ++ show other))
  carrierWire <- program "genuineCarrier"
  assert (replyFor carrierWire "TypeEvidence" "GenuineCarrier" == [ReplyAtSite])
    "genuine carrier with GADT result equality did not select AtSite"
  mapM_ (\(entry, constructor) -> do
      wire <- program entry
      node <- verbAnswer wire constructor
      assert (familyOf node == Just "Int")
        (constructor ++ ": reply/input layout mismatch was admitted as AtSite"))
    [("mismatchedCarrier", "MismatchedCarrier"), ("strictCarrier", "StrictCarrier"),
     ("dictionaryCarrier", "DictionaryCarrier"), ("integerPayload", "IntegerPayload")]
  progressWire <- program "progressRequest"
  progressNode <- verbAnswer progressWire "ObserveProgress"
  case progressNode of
    TypeData family [payload] rows -> do
      assert (symbolOccurrence family == "Progress"
        && map rowFields rows == [[], [payload], []])
        ("polymorphic progress reply lost its fieldless constructors: " ++ show progressNode)
      assert (isRefusal (nodeAt progressWire payload))
        "polymorphic progress payload was treated as constructible"
    other -> ioError (userError ("polymorphic progress reply lacks algebraic evidence: " ++ show other))

  mapM_ (\(entry, constructor) -> do
      wire <- program entry
      node <- verbAnswerFrom wire "Tidepool.Effects.Core" constructor
      assert (familyOf node `elem` [Just "Unit", Just "()"])
        (constructor ++ ": private protocol request lost its unit reply row"))
    [ ("agentAttachRequest", "AgentAttachWith")
    , ("agentToolsInstallRequest", "AgentToolsInstallWith")
    ]

  empty <- program "unrelated"
  assert (null (programSites empty) && null (programTypes empty))
    "unreachable typed sites leaked into the selected artifact"

  -- An admitted auxiliary root is not a declared site: its own result type
  -- must still be interned, even though
  -- 'unrelated' -- the selected entry here -- never otherwise constructs or
  -- observes an 'Either'.
  auxWire <- either
    (ioError . userError . ("auxiliaryRootDecode: " ++) . show) pure
    (projectWithAux result "unrelated" ["auxiliaryRootDecode"])
  let auxConstructorNames =
        map (symbolOccurrence . constructorIdentity) (programConstructors auxWire)
  assert (all (`elem` auxConstructorNames) ["Left", "Right"])
    ("admitted auxiliary root's own Either evidence is missing: "
      ++ show auxConstructorNames)
 where
  assert condition message = unless condition (ioError (userError message))
  isRefusal TypeUnconstructible{} = True
  isRefusal _ = False
  nodeAt wire (TypeNodeId index) = programTypes wire !! fromIntegral index
  familyAt wire index = familyOf (nodeAt wire index)
  familyOf node = case node of
    TypeData family _ _ -> Just (symbolOccurrence family)
    _ -> Nothing
  replyFor wire moduleName occurrence =
    let named = [ ConstructorId index
                | (index, declaration) <- zip [0 ..] (programConstructors wire)
                , symbolModule (constructorIdentity declaration) == Text.pack moduleName
                , symbolOccurrence (constructorIdentity declaration) == Text.pack occurrence ]
    in [reply | (constructor, reply) <- programConstructorReplies wire, constructor `elem` named]
  verbAnswer wire occurrence = verbAnswerFrom wire "TypeEvidence" occurrence
  verbAnswerFrom wire moduleName occurrence = case replyFor wire moduleName occurrence of
    [StaticReply node] -> pure (nodeAt wire node)
    other -> ioError (userError (occurrence ++ ": expected one intrinsic static reply, got " ++ show other))
