{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE OverloadedStrings #-}
module FormViewTest (formViewTests) where
import Prelude
import Control.Exception (evaluate)
import Data.IORef (newIORef, modifyIORef', readIORef)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import Data.List.NonEmpty (NonEmpty(..))
import GHC.Generics (Generic)
import Tidepool.Test.Runner (TestTree, testGroup, testCase)
import Test.Tasty.HUnit (assertEqual, assertBool)
import Test.Tasty (askOption, withResource)
import Test.Tasty.QuickCheck (testProperty, (===), counterexample, Property)
import Test.Tasty.QuickCheck (QuickCheckTests(..))
import qualified Test.QuickCheck as QC
import Test.QuickCheck.Property (Callback(..), CallbackKind(..), callback)
import System.IO (hPutStrLn, stderr)
import Tidepool.Aeson.Value
import Tidepool.Form.Algebra
import Tidepool.Form.GForm
import Tidepool.Form.Wire
import Tidepool.View
import Tidepool.View.Wire (encodeView)
import Tidepool.Inspection.Display (Display(..))
import Tidepool.Inspection.Tree (renderTree)

formViewTests :: TestTree
formViewTests = testGroup "form-view"
  [ testProperty "applicative independent controls agree with reference" independentControls
  , generatedFormCampaign
  , testCase "form model reaches nested branches reused controls and refinements" generatedFormPartitions
  , testProperty "applicative identity retains decode" applicativeIdentity
  , testProperty "applicative composition retains decode" applicativeComposition
  , testProperty "reused subform occurrences stay independent" reusedOccurrences
  , testCase "duplicate previews retain distinct closure payloads" duplicatePreviews
  , testCase "empty many descriptor and submission retain the empty list" emptyMany
  , testCase "blank numbers stay typed field errors" blankNumbers
  , testCase "many returns original values in offered order" manyOriginalOrder
  , testCase "many rejects unknown and duplicate identities" manyInvalidIdentities
  , testCase "inactive alternative validators are never run" inactiveAlternative
  , testCase "field and form refinement errors retain locality" refinementLocality
  , testCase "independent invalid controls accumulate errors" independentErrors
  , testCase "integer lexemes preserve machine bounds and browser precision" integerBounds
  , testCase "integer lexemes refuse fractions out-of-bounds and numeric coercion" integerRefusals
  , testCase "integer edit seed round-trips without floating point" integerSeed
  , testCase "generic form needs no JSON instances" genericWithoutJson
  , testCase "seeded edit selects constructor and primitive initial values" seededEdit
  , testCase "nested optional unit has distinct states" optionalUnit
  , testCase "rich rendering bounds infinite layout and lazy inspection" lazyRichView
  , testCase "single Display defaults are acyclic" acyclicDisplay
  , testCase "rich-only Display gets a bounded text interpretation" richDisplayText
  , testCase "standalone text is raw and nested text is quoted" textInterpretations
  , testCase "SVG source remains explicit and is forwarded intact" svgSource
  ]

draft :: [(Text,Value)] -> Value
draft = Object . Map.fromList
field :: Int -> Text
field i = "f" <> T.pack (show i)
independentControls :: [Bool] -> Property
independentControls xs =
  let controls = sequenceA [boolInput "Flag" Nothing | _ <- xs]
      submission = draft [(field i,Bool b) | (i,b) <- zip [0..] xs]
  in decodeSubmission (prepareForm controls) submission === Right xs
applicativeIdentity :: Int -> Property
applicativeIdentity x =
  let a = intInput "A" Nothing; v = draft [("f0",integer x)]
  in decodeSubmission (prepareForm (pure id <*> a)) v === decodeSubmission (prepareForm a) v
applicativeComposition :: Int -> Property
applicativeComposition x =
  let a = intInput "A" Nothing; v = draft [("f0",integer x)]
      left = pure (.) <*> pure (+1) <*> pure (*2) <*> a
      right = pure (+1) <*> (pure (*2) <*> a)
  in decodeSubmission (prepareForm left) v === decodeSubmission (prepareForm right) v
reusedOccurrences :: Int -> Int -> Property
reusedOccurrences x y =
  let shared = intInput "same label" Nothing
      p = prepareForm ((,) <$> shared <*> shared)
  in counterexample "Reusing a Form must allocate two mounted occurrences"
     (decodeSubmission p (draft [("f0",integer x),("f1",integer y)]) === Right (x,y))

-- The model owns the answer values and mounted occurrence order. It never
-- obtains either from the descriptor or from a successful production decode.
data Answer = IntAnswer Int | BoolAnswer Bool | TextAnswer Text deriving (Eq, Show)
data FormAst
  = Constant Int | IntegerLeaf Int | BooleanLeaf Bool | TextLeaf Text | ChoiceLeaf Bool
  | Together FormAst FormAst | Branch Bool FormAst FormAst | Reused Int Int
  | SectionAst FormAst | Positive Int | PositiveTotal Int Int
  deriving (Show)

instance QC.Arbitrary FormAst where
  arbitrary = QC.sized (genForm . min 3)
  shrink (Constant n) = map Constant (QC.shrink n)
  shrink (IntegerLeaf n) = map IntegerLeaf (QC.shrink n)
  shrink (BooleanLeaf b) = map BooleanLeaf (QC.shrink b)
  shrink (TextLeaf t) = map (TextLeaf . T.pack) (QC.shrink (T.unpack t))
  shrink (ChoiceLeaf b) = map ChoiceLeaf (QC.shrink b)
  shrink (Together a b) = [a,b] ++ [Together x b | x <- QC.shrink a] ++ [Together a x | x <- QC.shrink b]
  shrink (Branch selected a b) = [a,b] ++ [Branch selected x b | x <- QC.shrink a] ++ [Branch selected a x | x <- QC.shrink b]
  shrink (Reused x y) = [Reused a b | (a,b) <- QC.shrink (x,y)]
  shrink (SectionAst a) = a : map SectionAst (QC.shrink a)
  shrink (Positive n) = [Positive x | x <- QC.shrink n, x > 0]
  shrink (PositiveTotal x y) = [PositiveTotal a b | (a,b) <- QC.shrink (x,y), a > 0, b > 0]

genForm :: Int -> QC.Gen FormAst
genForm depth = QC.frequency $ leaves ++ if depth <= 0 then [] else
  [ (2,Together <$> child <*> child)
  , (2,Branch <$> QC.arbitrary <*> child <*> child)
  , (1,SectionAst <$> child)
  ]
  where
    child = genForm (depth-1)
    small = QC.chooseInt (-20,20)
    positive = QC.chooseInt (1,20)
    leaves =
      [ (1,Constant <$> small), (2,IntegerLeaf <$> small)
      , (2,BooleanLeaf <$> QC.arbitrary)
      , (2,TextLeaf . T.pack <$> QC.elements ["", "same", "hello", "λ"])
      , (1,ChoiceLeaf <$> QC.arbitrary), (2,Reused <$> small <*> small)
      , (1,Positive <$> positive), (1,PositiveTotal <$> positive <*> positive)
      ]

authoredForm :: FormAst -> Form [Answer]
authoredForm ast = case ast of
  Constant n -> pure [IntAnswer n]
  IntegerLeaf _ -> (\n -> [IntAnswer n]) <$> intInput "same" Nothing
  BooleanLeaf _ -> (\b -> [BoolAnswer b]) <$> boolInput "same" Nothing
  TextLeaf _ -> (\t -> [TextAnswer t]) <$> textInput "same" Nothing
  ChoiceLeaf _ -> choice "same" (option (text "same") [IntAnswer 101] :| [option (text "same") [IntAnswer 202]])
  Together a b -> (++) <$> authoredForm a <*> authoredForm b
  Branch _ a b -> branches "same" (option (text "same") (authoredForm a) :| [option (text "same") (authoredForm b)])
  Reused _ _ -> let shared = intInput "same" Nothing
                in (\x y -> [IntAnswer x,IntAnswer y]) <$> shared <*> shared
  SectionAst a -> section "same" (authoredForm a)
  Positive _ -> (\n -> [IntAnswer n]) <$> validate (\n -> ["positive required" | n <= 0]) (intInput "same" Nothing)
  PositiveTotal _ _ ->
    validateForm (\answers -> ["positive total required" | sum [n | IntAnswer n <- answers] <= 0])
      ((\x y -> [IntAnswer x,IntAnswer y]) <$> intInput "same" Nothing <*> intInput "same" Nothing)

-- Width counts all mounted controls, including inactive alternatives. The
-- independent values below choose one branch; they do not decode wire data.
formWidth :: FormAst -> Int
formWidth ast = case ast of
  Constant _ -> 0
  Together a b -> formWidth a + formWidth b
  Branch _ a b -> 1 + formWidth a + formWidth b
  Reused _ _ -> 2
  SectionAst a -> formWidth a
  PositiveTotal _ _ -> 2
  _ -> 1

data FormModel = FormModel
  { expectedAnswers :: [Answer]
  , activeInputs :: [(Text,Value)]
  , missingFields :: [Maybe Text]
  , refinementFailures :: [(Text,Value,ValidationError)]
  } deriving (Show)

modelForm :: Int -> FormAst -> FormModel
modelForm offset ast = case ast of
  Constant n -> FormModel [IntAnswer n] [] [] []
  IntegerLeaf n -> leaf (IntAnswer n) (integer n)
  BooleanLeaf b -> leaf (BoolAnswer b) (Bool b)
  TextLeaf t -> leaf (TextAnswer t) (String t)
  ChoiceLeaf selected -> leaf (IntAnswer (if selected then 202 else 101)) (selection selected)
  Together a b -> combine (modelForm offset a) (modelForm (offset + formWidth a) b)
  Branch selected a b ->
    let chosen = if selected then modelForm (offset+1+formWidth a) b else modelForm (offset+1) a
    in chosen { activeInputs = (key,selection selected) : activeInputs chosen, missingFields = [Just key] }
  Reused x y -> combine (modelForm offset (IntegerLeaf x)) (modelForm (offset+1) (IntegerLeaf y))
  SectionAst a -> modelForm offset a
  Positive n -> (leaf (IntAnswer n) (integer n))
    { refinementFailures = [(key,integer 0,ValidationError (Just key) "positive required")] }
  PositiveTotal x y -> (combine (modelForm offset (IntegerLeaf x)) (modelForm (offset+1) (IntegerLeaf y)))
    { refinementFailures = [(key,integer (-y),ValidationError Nothing "positive total required")] }
  where
    key = field offset
    selection selected = String (if selected then "o1" else "o0")
    leaf answer input = FormModel [answer] [(key,input)] [Just key] []
    combine a b = FormModel (expectedAnswers a ++ expectedAnswers b)
      (activeInputs a ++ activeInputs b) (missingFields a ++ missingFields b)
      (refinementFailures a ++ refinementFailures b)

modelAgreement :: FormAst -> Property
modelAgreement ast =
  let model = modelForm 0 ast
      prepared = prepareForm (authoredForm ast)
      -- Every inactive control has a deliberately invalid value. Acceptance
      -- therefore witnesses activation instead of merely well-typed branches.
      inputs = Map.fromList ([(field i,Null) | i <- [0..formWidth ast-1]] ++ activeInputs model)
      decode = decodeSubmission prepared . Object
      fieldsEqual expected result = case result of
        Left errors -> map errorField errors === expected
        Right answers -> counterexample ("invalid submission accepted: " ++ show answers) False
      negatives = QC.conjoin $
        [ fieldsEqual [Just key] (decode (Map.delete key inputs))
        | (key,_) <- activeInputs model ] ++
        [ fieldsEqual [Just key] (decode (Map.insert key (Array []) inputs))
        | (key,_) <- activeInputs model ] ++
        [ decode (Map.insert key value inputs) === Left [expected]
        | (key,value,expected) <- refinementFailures model ] ++
        [ if null (missingFields model) then decode Map.empty === Right (expectedAnswers model)
          else fieldsEqual (missingFields model) (decode Map.empty) ]
  in counterexample (show ast ++ "\n" ++ show model) $ case decode inputs of
      Right answers | answers == expectedAnswers model -> negatives
      result -> counterexample "valid fixture must agree before negative perturbations" (result === Right (expectedAnswers model))

data FormCampaign = FormCampaign
  { formCallbacks :: Int, formCompletedCallbacks :: Int
  , formBranchCallbacks :: Int, formReusedCallbacks :: Int, formNestedBranchCallbacks :: Int
  } deriving (Show)

generatedFormCampaign :: TestTree
generatedFormCampaign = askOption $ \(QuickCheckTests configured) ->
  withResource (newIORef (FormCampaign 0 0 0 0 0))
    (\counts -> do
      observed <- readIORef counts
      hPutStrLn stderr ("form_model_campaign configured_cases=" ++ show configured ++
        " observed_callbacks_include_shrinking=" ++ show observed)) $
    \getCounts -> testProperty "generated forms agree with independent occurrence value and activation model" $ \ast ->
      QC.ioProperty $ do
        counts <- getCounts
        modifyIORef' counts $ \seen -> seen
          { formCallbacks = formCallbacks seen + 1
          , formBranchCallbacks = formBranchCallbacks seen + fromEnum (hasBranch ast)
          , formReusedCallbacks = formReusedCallbacks seen + fromEnum (hasReuse ast)
          , formNestedBranchCallbacks = formNestedBranchCallbacks seen + fromEnum (selectedDepth ast > 1)
          }
        pure $ callback (PostTest NotCounterexample $ \_ _ ->
          modifyIORef' counts $ \seen -> seen { formCompletedCallbacks = formCompletedCallbacks seen + 1 }) (modelAgreement ast)
  where
    selectedDepth :: FormAst -> Int
    selectedDepth (Branch selected a b) = 1 + selectedDepth (if selected then b else a)
    selectedDepth (Together a b) = max (selectedDepth a) (selectedDepth b)
    selectedDepth (SectionAst a) = selectedDepth a
    selectedDepth _ = 0
    hasBranch (Branch _ _ _) = True
    hasBranch (Together a b) = hasBranch a || hasBranch b
    hasBranch (SectionAst a) = hasBranch a
    hasBranch _ = False
    hasReuse (Reused _ _) = True
    hasReuse (Together a b) = hasReuse a || hasReuse b
    hasReuse (Branch _ a b) = hasReuse a || hasReuse b
    hasReuse (SectionAst a) = hasReuse a
    hasReuse _ = False

generatedFormPartitions :: IO ()
generatedFormPartitions = mapM_ check
  [ Constant 3, IntegerLeaf 7, BooleanLeaf False, TextLeaf "λ", ChoiceLeaf True
  , Reused 11 29, Positive 2, PositiveTotal 3 5
  , Branch False (IntegerLeaf 7) (PositiveTotal 3 5)
  , Branch True (Positive 2) (Reused 11 29)
  , Together (Branch True (BooleanLeaf True) (TextLeaf "selected")) (SectionAst (PositiveTotal 2 3))
  , Branch False (Branch True (Positive 2) (Reused 11 29)) (IntegerLeaf 7)
  , Branch True (IntegerLeaf 7) (Branch False (SectionAst (PositiveTotal 2 3)) (TextLeaf "unused"))
  ]
  where
    check ast = do
      result <- QC.quickCheckWithResult QC.stdArgs { QC.maxSuccess = 1, QC.chatty = False } (modelAgreement ast)
      assertBool (show ast ++ "\n" ++ QC.output result) (QC.isSuccess result)

duplicatePreviews :: IO ()
duplicatePreviews = do
  let p = prepareForm (choice "Action" (option (text "same") (+1) :| [option (text "same") (*2)]))
  case decodeSubmission p (draft [("f0",String "o1")]) of
    Right action -> assertEqual "second original function survives" (20::Int) (action 10)
    Left es -> fail (show es)
manyOriginalOrder :: IO ()
manyOriginalOrder = do
  let p = prepareForm (choices "Actions" [option (text "same") (+1),option (text "same") (*2)])
  case decodeSubmission p (draft [("f0",Array [String "o1",String "o0"])]) of
    Right actions -> assertEqual "offer order, original payloads" [11,20::Int] (map ($ 10) actions)
    Left es -> fail (show es)
manyInvalidIdentities :: IO ()
manyInvalidIdentities = do
  let p = prepareForm (choices "Values" [option (text "same") (1::Int)])
  assertBool "duplicate option identity rejected" (isLeft (decodeSubmission p (draft [("f0",Array [String "o0",String "o0"])])))
  assertBool "unknown option identity rejected" (isLeft (decodeSubmission p (draft [("f0",Array [String "o9"])])))
inactiveAlternative :: IO ()
inactiveAlternative = do
  let active = intInput "Active" Nothing
      inactive = validate (\_ -> error "inactive validation ran") (intInput "Inactive" Nothing)
      p = prepareForm (branches "Mode" (option (text "mode") active :| [option (text "mode") inactive]))
  assertEqual "only selected branch decodes" (Right (7::Int))
    (decodeSubmission p (draft [("f0",String "o0"),("f1",integer (7::Int)),("f2",String "not an integer")]))
refinementLocality :: IO ()
refinementLocality = do
  let a = validate (\n -> ["Must be positive" | n <= 0]) (intInput "A" Nothing)
  assertEqual "single control refinement points to occurrence" (Left [ValidationError (Just "f0") "Must be positive"])
    (decodeSubmission (prepareForm a) (draft [("f0",integer (0::Int))]))
  let composite = validateForm (\(x,y) -> ["A must precede B" | x >= y]) ((,) <$> intInput "A" Nothing <*> intInput "B" Nothing)
  assertEqual "cross-field refinement points to form" (Left [ValidationError Nothing "A must precede B"])
    (decodeSubmission (prepareForm composite) (draft [("f0",integer (2::Int)),("f1",integer (1::Int))]))
independentErrors :: IO ()
independentErrors = case decodeSubmission (prepareForm ((,) <$> intInput "A" Nothing <*> boolInput "B" Nothing)) (draft []) of
  Left es -> assertEqual "both missing controls reported" [Just "f0",Just "f1"] (map errorField es)
  Right _ -> fail "invalid fields decoded"

data Request = Local | Remote { endpoint :: Text, count :: Int } deriving (Eq,Show,Generic)
genericWithoutJson :: IO ()
genericWithoutJson = assertEqual "generic constructor functions reconstruct original record" (Right (Remote "host" 3))
  (decodeSubmission (prepareForm (autoForm @Request)) (draft [("f0",String "o1"),("f1",String "host"),("f2",integer (3::Int))]))
seededEdit :: IO ()
seededEdit = do
  let descriptor = formDescriptor (prepareForm (edit (Remote "host" 3)))
      nodes = allNodes descriptor
      initialValues = [(key,nodeValue "initial" node) | node <- nodes, Just (String key) <- [nodeValue "id" node]]
  assertEqual "constructor selected by occurrence" (Just (Just (String "o1"))) (lookup "f0" initialValues)
  assertEqual "text seed belongs to selected constructor" (Just (Just (String "host"))) (lookup "f1" initialValues)
  assertEqual "integer seed belongs to selected constructor" (Just (Just (integer (3::Int)))) (lookup "f2" initialValues)
optionalUnit :: IO ()
optionalUnit = do
  let p = prepareForm (autoForm @(Maybe (Maybe ())))
  assertEqual "outer absent" (Right Nothing) (decodeSubmission p (draft [("f0",String "o0")]))
  assertEqual "inner absent" (Right (Just Nothing)) (decodeSubmission p (draft [("f0",String "o1"),("f1",String "o0")]))
  assertEqual "present unit" (Right (Just (Just ()))) (decodeSubmission p (draft [("f0",String "o1"),("f1",String "o1")]))
lazyRichView :: IO ()
lazyRichView = do
  let encoded = encodeView 10 (column (repeat (text "abc")))
  _ <- evaluate (valueWeight encoded)
  assertBool "layout frontier is finite" (valueWeight encoded < 1000)
  _ <- evaluate (valueWeight (encodeView 4 (row [inspect ([1..]::[Int]),error "unconsumed layout forced"])))
  let clipped = encodeView 4 (inspect ([1..]::[Int]))
  assertEqual "nested inspection explicitly lacks callback detail" (Just (Bool True)) (nodeValue "unavailable" clipped)

data EmptyDisplay = EmptyDisplay
instance Display EmptyDisplay
acyclicDisplay :: IO ()
acyclicDisplay = do
  assertEqual "no mutual default recursion" ("<opaque>",False) (displayWith 100 EmptyDisplay)
  _ <- evaluate (encodeView 100 (inspect EmptyDisplay))
  pure ()
textInterpretations :: IO ()
textInterpretations = do
  assertEqual "standalone raw text" (Just (String "hello")) (nodeValue "text" (encodeView 100 (displayView ("hello"::Text))))
  assertEqual "inspect is structural even for standalone text" (Just (String "\"hello\"")) (nodeValue "text" (encodeView 100 (inspect ("hello"::Text))))
  let (rendered,_,_) = renderTree 100 (displayTree (("hello"::Text),1::Int))
  assertEqual "nested quoted text" "(\"hello\", 1)" rendered
svgSource :: IO ()
svgSource = do
  let source = "<svg xmlns=\"http://www.w3.org/2000/svg\"><g transform=\"scale(2)\"><path d=\"M0 0L1 1\"/></g></svg>"
  assertEqual "standard SVG vocabulary retained for host parser" (Just (String source)) (nodeValue "source" (encodeView 100 (svg (SvgDocument source))))

nodeValue :: Text -> Value -> Maybe Value
nodeValue key (Object values) = Map.lookup key values
nodeValue _ _ = Nothing
allNodes :: Value -> [Value]
allNodes v@(Object values) = v : concatMap allNodes (Map.elems values)
allNodes (Array xs) = concatMap allNodes xs
allNodes _ = []
isLeft :: Either a b -> Bool
isLeft (Left _) = True
isLeft _ = False

valueWeight :: Value -> Int
valueWeight (Object fields) = sum [T.length k + valueWeight v | (k,v) <- Map.toList fields]
valueWeight (Array xs) = sum (map valueWeight xs)
valueWeight (String t) = T.length t
valueWeight _ = 1

integer :: Int -> Value
integer = String . T.pack . show
integerBounds :: IO ()
integerBounds = mapM_ (\value -> assertEqual "exact decimal Int" (Right value)
  (decodeSubmission (prepareForm (intInput "Integer" Nothing)) (draft [("f0",integer value)])))
  [minBound,maxBound,9007199254740993]
integerRefusals :: IO ()
integerRefusals = do
  let p = prepareForm (intInput "Integer" Nothing)
      invalid = [String (T.pack (show (toInteger (minBound::Int)-1))),String (T.pack (show (toInteger (maxBound::Int)+1))),String "1.5",String "1e3",String " 1",String "1 ",String "-",toJSON (1::Int)]
  mapM_ (\value -> assertBool ("refused " ++ show value) (isLeft (decodeSubmission p (draft [("f0",value)])))) invalid
integerSeed :: IO ()
integerSeed = do
  let original = 9007199254740993 :: Int
      p = prepareForm (edit original)
      leaf = case [node | node <- allNodes (formDescriptor p), nodeValue "id" node == Just (String "f0")] of
        [node] -> node
        _ -> error "seeded integer did not have one control"
  assertEqual "seed is decimal text" (Just (String "9007199254740993")) (nodeValue "initial" leaf)
  assertEqual "browser passes the unchanged initial lexeme" (Right original)
    (decodeSubmission p (draft [("f0",maybe Null id (nodeValue "initial" leaf))]))

data RichOnly = RichOnly
instance Display RichOnly where
  displayView _ = text "rich preview"
richDisplayText :: IO ()
richDisplayText = assertEqual "text interpretation follows rich view without a new class" ("rich",True) (displayWith 4 RichOnly)

emptyMany :: IO ()
emptyMany = do
  let p = prepareForm (choices "Actions" ([] :: [Option Int]))
      descriptor = case nodeValue "root" (formDescriptor p) of Just root -> root; Nothing -> error "prepared form lost root"
  assertEqual "empty many remains many" (Just (String "many")) (nodeValue "kind" descriptor)
  assertEqual "zero options are a valid collection" (Just (Array [])) (nodeValue "options" descriptor)
  assertEqual "empty selection seed" (Just (Array [])) (nodeValue "initial" descriptor)
  assertEqual "empty choice list decodes without inventing a value" (Right []) (decodeSubmission p (draft [("f0",Array [])]))
blankNumbers :: IO ()
blankNumbers = do
  let p = prepareForm (numberInput "Number" Nothing)
  mapM_ (\value -> case decodeSubmission p value of
    Left errors -> assertEqual "blank value reaches owning field validator" [Just "f0"] (map errorField errors)
    Right _ -> error "blank number was accepted") [draft [("f0",Null)],draft []]
