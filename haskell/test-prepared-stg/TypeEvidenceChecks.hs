module TypeEvidenceChecks (runTypeEvidenceChecks) where

import Control.Monad (unless)
import Data.Bits ((.&.))
import Data.Text qualified as Text
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
  readFile "test-prepared-stg/site-fixtures/TypeEvidence.hs" >>= writeFile target
  result <- runPipelineSelected PreparedStg target [directory]
  let program entry = either
        (ioError . userError . ((entry ++ ": ") ++) . show) pure (project result entry)
      answer entry = do
        wire <- program entry
        case filter (not . synthetic) (programSites wire) of
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
  chainRoot <- case filter (not . synthetic) (programSites chain) of
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
    ("Print's synthetic reply is not unit: " ++ show printNode)
  fetchWire <- program "fetchRequest"
  fetchNode <- verbAnswer fetchWire "Fetch"
  case fetchNode of
    TypeData family arguments rows -> assert
      (symbolOccurrence family == "Either" && length rows == 2
        && map (familyAt fetchWire) arguments == [Just "Bool", Nothing]
        && map (nodeAt fetchWire) (drop 1 arguments) == [TypeText])
      ("Fetch's synthetic reply lost its Either Bool Text evidence: " ++ show fetchNode)
    other -> ioError (userError ("Fetch reply is not data evidence: " ++ show other))
  echoWire <- program "echoRequest"
  assert (null (programVerbSites echoWire))
    "an open reply index acquired a synthetic site"

  empty <- program "unrelated"
  assert (null (programSites empty) && null (programTypes empty))
    "unreachable typed sites leaked into the selected artifact"

  -- An admitted auxiliary root is not a declared site (mirrors
  -- 'preparedDecodeTargetName'/'__decodeValue' beside a turn's resume
  -- entry): its own result type must still be interned, even though
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
  synthetic site = siteId site .&. 0x8000000000000000 /= 0
  -- The request constructor's verb-site entry names exactly one synthetic,
  -- input-free host-answer row; return that row's wire evidence.
  verbAnswer wire occurrence = do
    let named = [ ConstructorId index
                | (index, declaration) <- zip [0 ..] (programConstructors wire)
                , symbolOccurrence (constructorIdentity declaration) == Text.pack occurrence ]
        entries = [ entry | entry@(constructor, _) <- programVerbSites wire
                  , constructor `elem` named ]
    case entries of
      [(_, sid)] -> case filter ((== sid) . siteId) (programSites wire) of
        [row] -> do
          assert (synthetic row && siteDelivery row == HostAnswer
              && null (siteInputs row) && siteOrdinal row == 0
              && siteOrigin row == Text.pack ("TypeEvidence." ++ occurrence))
            (occurrence ++ ": malformed synthetic row " ++ show row)
          pure (nodeAt wire (siteWire row))
        rows -> ioError (userError (occurrence ++ ": verb site names rows " ++ show rows))
      other -> ioError (userError (occurrence ++ ": expected one verb site, got "
        ++ show other ++ " in " ++ show (programVerbSites wire)))
