{-# LANGUAGE DataKinds #-}
{-# LANGUAGE LambdaCase #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The provider's observed request and response contract as position-
-- specific types over an abstract JSON value. Boot packages only. Nothing
-- here knows about packets or modes; it is the wire vocabulary the schema
-- layer targets.
module Jev.Core.Contract
  ( -- * Positions
    Presence (..)
  , Instructions (..)
  , question
  , Criteria (..)
  , noCriteria
  , yesOnly
  , noOnly
  , bothSides
  , State (..)
  , state
  , checkState
  , checkInstructions
  , checkDescription
  , checkLevel
  , renderInstructions
  , extras
    -- * Wire questions
  , WireQuestion (..)
  , questionValue
    -- * Errors
  , PrepError (..)
  , DecodeError (..)
  , Rejection (..)
  , ValidationIssue (..)
    -- * Wire answers and the response envelope
  , NoulAnswer (..)
  , ChoiceAnswer (..)
  , ScoreAnswer (..)
  , parseNoul
  , parseChoice
  , parseScore
  , Envelope (..)
  , parseEnvelope
  , unit
  , distribution
  , driftOf
  ) where

import Data.List (nub)
import Data.Text (Text)
import qualified Data.Text as T
import Jev.Core.Json

-- ---------------------------------------------------------------------------
-- Positions
-- ---------------------------------------------------------------------------

-- | Omission is not representable in JSON, and the provider distinguishes an
-- omitted criteria block or side from an explicit null.
data Presence a = Omitted | Present a deriving (Eq, Show)

-- | Instructions. 'Structured' and 'Premised' keep their structure until
-- preparation so a duplicate key is an error, never a silent merge.
data Instructions v
  = NoInstructions
  | Instructions v                 -- ^ any value the provider admits: string, object, array, or null
  | Structured [(Text, v)]         -- ^ an object, checked for duplicate keys at preparation
  | Premised Text (Instructions v) -- ^ a runtime premise over the original, rendered as @{"premise", "instructions"}@

question :: JsonValue v => Text -> Instructions v
question = Instructions . jString

-- | Add structured members. A plain question becomes @{"question": q, ...}@;
-- an existing object gains the members; a premise keeps wrapping.
extras :: JsonValue v => [(Text, v)] -> Instructions v -> Instructions v
extras kv = \case
  NoInstructions -> Structured kv
  Instructions q -> Structured (("question", q) : kv)
  Structured kv0 -> Structured (kv0 ++ kv)
  Premised p i -> Premised p (extras kv i)

-- | Noul criteria: each side independently omitted, null, or content.
data Criteria v = Criteria
  { yesWhen :: Presence v
  , noWhen :: Presence v
  }

noCriteria :: Presence (Maybe (Criteria v))
noCriteria = Omitted

yesOnly :: v -> Presence (Maybe (Criteria v))
yesOnly y = Present (Just (Criteria (Present y) Omitted))

noOnly :: v -> Presence (Maybe (Criteria v))
noOnly n = Present (Just (Criteria Omitted (Present n)))

bothSides :: v -> v -> Presence (Maybe (Criteria v))
bothSides y n = Present (Just (Criteria (Present y) (Present n)))

-- | The shared input to every question. Rendered as given, or under
-- @context@ beside the declared pools when the packet declares any.
newtype State v = State { stateValue :: v }

-- | Total; the outer shape (string, object, or array) is checked at
-- preparation.
state :: v -> State v
state = State

checkState :: JsonValue v => State v -> Either PrepError ()
checkState st = case jView (stateValue st) of
  VString _ -> Right ()
  VObject _ -> Right ()
  VArray _ -> Right ()
  _ -> Left BadStateShape

admissible :: JsonValue v => v -> Bool
admissible v = case jView v of
  VNull -> True
  VString _ -> True
  VObject _ -> True
  VArray _ -> True
  _ -> False

checkInstructions :: JsonValue v => Text -> Instructions v -> Either PrepError ()
checkInstructions key = \case
  NoInstructions -> Right ()
  Instructions v -> if admissible v then Right () else Left (BadInstructions key)
  Structured kv -> case [k | (k, _) <- kv, length (filter ((== k) . fst) kv) > 1] of
    k : _ -> Left (DuplicateInstructionKey key k)
    [] -> Right ()
  Premised _ inner -> checkInstructions key inner

renderInstructions :: JsonValue v => Instructions v -> [(Text, v)]
renderInstructions = \case
  NoInstructions -> []
  Instructions v -> [("instructions", v)]
  Structured kv -> [("instructions", jObject kv)]
  Premised p inner -> [("instructions", jObject (("premise", jString p) : renderInstructions inner))]

checkDescription :: JsonValue v => Text -> Text -> v -> Either PrepError ()
checkDescription key alt v = if admissible v then Right () else Left (BadDescription key alt)

checkLevel :: JsonValue v => Text -> Int -> v -> Either PrepError ()
checkLevel key ix v = case jView v of
  VString _ -> Right ()
  VObject _ -> Right ()
  VArray _ -> Right ()
  _ -> Left (BadLevel key ix)

-- ---------------------------------------------------------------------------
-- Wire questions
-- ---------------------------------------------------------------------------

data WireQuestion v
  = WNoul (Instructions v) (Presence (Maybe (Criteria v)))
  | WChoice (Instructions v) [(Text, v)]
  | WScore (Instructions v) [v]
  | WRaw v

questionValue :: JsonValue v => WireQuestion v -> v
questionValue = \case
  WNoul i c -> jObject ([("type", jString "noul")] ++ renderInstructions i ++ criteria c)
  WChoice i alts -> jObject ([("type", jString "choice")] ++ renderInstructions i ++ [("criteria", jObject alts)])
  WScore i ls -> jObject ([("type", jString "score")] ++ renderInstructions i ++ [("criteria", jArray ls)])
  WRaw v -> v
  where
    criteria = \case
      Omitted -> []
      Present Nothing -> [("criteria", jNull)]
      Present (Just (Criteria y n)) -> [("criteria", jObject (side "true" y ++ side "false" n))]
    side k = \case
      Omitted -> []
      Present d -> [(k, d)]

-- ---------------------------------------------------------------------------
-- Errors
-- ---------------------------------------------------------------------------

-- | Preparation failures. Question-level errors name the flattened
-- question id.
data PrepError
  = EmptyOffer Text
  | DuplicateKeys Text [Text]
  | KeyCollidesWithLabel Text Text
  | TooManyAlternatives Text Int
  | BadLevelCount Text Int
  | DuplicateQuestionPath Text
  | EmptyQuestionMap
  | EmptyQuestionKey Text
  | BadStateShape
  | BadInstructions Text
  | DuplicateInstructionKey Text Text
  | BadDescription Text Text
  | BadLevel Text Int
  | UndeclaredPool Text
  | ConflictingPool Text
  | DuplicatePool Text
  | DuplicatePoolKey Text Text
  | MultiplePoolsInChoice Text
  | PoolDeclaredInNested Text
  deriving (Show, Eq)

-- | A provider rejection, parsed from the observed 400 and 422 bodies.
data Rejection
  = RejectionMessage Text
  | RejectionError Text (Maybe Text)
  | RejectionValidation [ValidationIssue]
  | RejectionOther
  deriving (Show, Eq)

data ValidationIssue = ValidationIssue
  { issueLocation :: [Text]
  , issueMessage :: Text
  , issueType :: Text
  } deriving (Show, Eq)

-- | Decoding failures. The response is untrusted until every check passes.
data DecodeError
  = ResponseShape Text
  | ProviderRejected Rejection
  | MissingAnswer Text
  | UnexpectedAnswer Text
  | DuplicateAnswer Text
  | WrongKind Text
  | Malformed Text Text
  | UnknownSelection Text Text
  | MissingMass Text Text
  | ExtraMass Text Text
  | LegendMismatch Text
  | ValueOutOfRange Text Text
  deriving (Show, Eq)

-- ---------------------------------------------------------------------------
-- Wire answers
-- ---------------------------------------------------------------------------

newtype NoulAnswer = NoulAnswer { noulYes :: Double } deriving (Eq, Show)

data ChoiceAnswer = ChoiceAnswer
  { choiceSelected :: Text
  , choiceMasses :: [(Text, Double)]
  , choiceConfidence :: Double
  } deriving (Eq, Show)

data ScoreAnswer v = ScoreAnswer
  { wireExpectation :: Double
  , wireLegend :: [(Text, v)]
  , wireMasses :: [(Text, Double)]
  , wireConfidence :: Double
  }

field :: JsonValue v => Text -> Text -> v -> Either DecodeError v
field key name v = maybe (Left (Malformed key ("missing " <> name))) Right (lookupKey name v)

numberField :: JsonValue v => Text -> Text -> v -> Either DecodeError Double
numberField key name v = field key name v >>= \x ->
  maybe (Left (Malformed key (name <> " is not a number"))) Right (viewNumber x)

textField :: JsonValue v => Text -> Text -> v -> Either DecodeError Text
textField key name v = field key name v >>= \x ->
  maybe (Left (Malformed key (name <> " is not a string"))) Right (viewText x)

numberMap :: JsonValue v => Text -> Text -> v -> Either DecodeError [(Text, Double)]
numberMap key name v = field key name v >>= \x -> case viewObject x of
  Just kv -> mapM (\(k, n) -> maybe (Left (Malformed key (name <> "." <> k <> " is not a number"))) (Right . (,) k) (viewNumber n)) kv
  Nothing -> Left (Malformed key (name <> " is not an object"))

kind :: JsonValue v => Text -> Text -> v -> Either DecodeError ()
kind key expected v = textField key "type" v >>= \t ->
  if t == expected then Right () else Left (WrongKind key)

parseNoul :: JsonValue v => Text -> v -> Either DecodeError NoulAnswer
parseNoul key v = do
  kind key "noul" v
  p <- numberField key "noul" v
  unit key "noul" p
  Right (NoulAnswer p)

parseChoice :: JsonValue v => Text -> v -> Either DecodeError ChoiceAnswer
parseChoice key v = do
  kind key "choice" v
  ChoiceAnswer <$> textField key "choice" v <*> numberMap key "probabilities" v <*> numberField key "confidence" v

parseScore :: JsonValue v => Text -> v -> Either DecodeError (ScoreAnswer v)
parseScore key v = do
  kind key "score" v
  legend <- field key "legend" v >>= \x ->
    maybe (Left (Malformed key "legend is not an object")) Right (viewObject x)
  ScoreAnswer <$> numberField key "score" v <*> pure legend <*> numberMap key "probabilities" v <*> numberField key "confidence" v

-- ---------------------------------------------------------------------------
-- Response envelope
-- ---------------------------------------------------------------------------

data Envelope v
  = Evaluated { envelopeModel :: Text, envelopeUsage :: v, envelopeAnswers :: [(Text, v)] }
  | Rejected Rejection

parseEnvelope :: JsonValue v => v -> Either DecodeError (Envelope v)
parseEnvelope v = case lookupKey "answers" v of
  Just answersValue -> do
    answers <- maybe (Left (ResponseShape "answers is not an object")) Right (viewObject answersValue)
    model <- maybe (Left (ResponseShape "model is not a string")) Right (lookupKey "model" v >>= viewText)
    let usage = maybe jNull id (lookupKey "usage" v)
    Right (Evaluated model usage answers)
  Nothing
    | Just d <- lookupKey "detail" v -> Right (Rejected (rejection d))
    | Just _ <- lookupKey "error_type" v -> Right (Rejected (rejection v))
    | otherwise -> Left (ResponseShape "neither an evaluation nor a recognizable rejection")
  where
    rejection d = case jView d of
      VString m -> RejectionMessage m
      VObject _ | Just t <- lookupKey "error_type" d >>= viewText ->
        RejectionError t (lookupKey "message" d >>= viewText)
      VArray issues -> RejectionValidation [ValidationIssue (locOf i) (textOr "msg" i) (textOr "type" i) | i <- issues]
      _ -> RejectionOther
    textOr k i = maybe "" id (lookupKey k i >>= viewText)
    locOf i = case lookupKey "loc" i of
      Just x | VArray parts <- jView x -> map segment parts
      _ -> []
    segment x = case jView x of
      VString t -> t
      VNumber n -> T.pack (show (round n :: Integer))
      _ -> "?"

-- ---------------------------------------------------------------------------
-- Value checks shared by the schema layer
-- ---------------------------------------------------------------------------

unit :: Text -> Text -> Double -> Either DecodeError ()
unit key what x
  | isNaN x || isInfinite x || x < 0 || x > 1 = Left (ValueOutOfRange key what)
  | otherwise = Right ()

-- | Probability keys must equal the submitted key set exactly; every value
-- and the confidence in [0,1]. Sum drift is a diagnostic elsewhere.
distribution :: Text -> [Text] -> [(Text, Double)] -> Double -> Either DecodeError ()
distribution key expected ms conf = do
  unit key "confidence" conf
  mapM_ (\k -> if k `elem` map fst ms then Right () else Left (MissingMass key k)) expected
  mapM_ (\(k, x) -> if k `elem` expected then unit key k x else Left (ExtraMass key k)) ms
  if length (nub (map fst ms)) /= length ms then Left (ExtraMass key "duplicate") else Right ()

driftOf :: JsonValue v => v -> Maybe Double
driftOf v = do
  ps <- lookupKey "probabilities" v >>= viewObject
  Just (sum [n | (_, x) <- ps, Just n <- [viewNumber x]])
