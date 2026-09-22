{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Shared command tools whose automatic observations are selected by Jev.
-- Execution, input, cancellation, retained jobs and raw paging stay owned by
-- 'Tidepool.Command'; this module changes only the text prepared for display.
module Project.Shell
  ( tools,
    Snapshot,
    snapshot,
    snapshotJob,
    stdoutEndpoint,
    stderrEndpoint,
    SectionId (..),
    SnapshotIssue (..),
    section,
    sectionPage,
    estimatedTokens,
    splitSections,
  )
where

import Control.Monad (forM)
import Control.Monad.Freer (Eff, Member)
import Data.Char (ord)
import Data.List (sortBy)
import Data.Ord (comparing)
import Data.Text (Text)
import qualified Data.Text as T
import Jev.Operators (Packet ((:=)))
import qualified Jev.Operators as J
import Tidepool.Agent.Contract (AsServerT)
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Command
import Tidepool.Command.Types (Job (..))
import Tidepool.Aeson.Value (Value (String))
import Tidepool.Effects.Core
  ( CommandCleanup (..),
    CommandError (..),
    CommandOutcome (..),
    CommandOutput (..),
    CommandPage (..),
    CommandResult (..),
    CommandStatus (..),
    ConversationRole (..),
    ConversationTurn (..),
    Commands,
    Jev,
    Reflect,
    TurnItem (..),
    reflect,
  )

-- Configuration is ordinary Haskell on purpose: a workspace can tune these
-- values and reload its spec without changing a runtime protocol.
jevTriggerTokens, sectionTokens, recentConversationTokens, selectedOutputTokens, maximumScoredTokens :: Int
jevTriggerTokens = 100
sectionTokens = 512
recentConversationTokens = 4096
selectedOutputTokens = 2048
maximumScoredTokens = 262144

questionsPerRequest :: Int
questionsPerRequest = 128

data Snapshot = Snapshot
  { snapshotJob :: Cmd.Job,
    stdoutEndpoint :: Int,
    stderrEndpoint :: Int
  }
  deriving (Eq, Show)

snapshot :: Cmd.Job -> Int -> Int -> Snapshot
snapshot = Snapshot

newtype SectionId = SectionId Int
  deriving (Eq, Ord, Show)

data SnapshotIssue
  = SnapshotExpired Cmd.CommandStream Int
  | SnapshotDecodingLoss Cmd.CommandStream Int
  | SnapshotUnavailable Cmd.CommandStream CommandError
  | UnknownSection SectionId
  deriving (Eq, Show)

data Section = Section
  { sectionId :: SectionId,
    sectionStream :: Cmd.CommandStream,
    sectionStart :: Int,
    sectionEnd :: Int,
    sectionText :: Text,
    sectionScored :: Bool
  }
  deriving (Eq, Show)

data Ranked = Ranked Section Double

tools ::
  (Member Commands effects, Member Jev effects, Member Reflect effects) =>
  Command.ShellTools (AsServerT (Eff effects))
tools = Command.toolsWith presentSelected

-- | Unicode scalar count times 0.25, rounded up.
estimatedTokens :: Text -> Int
estimatedTokens text = (T.length text + 3) `div` 4

-- | Deterministic line-aware partitioning. A line longer than one section is
-- split at the scalar boundary; otherwise the last newline within the budget
-- closes the section.
splitSections :: Int -> Text -> [Text]
splitSections tokens = go
  where
    scalars = max 1 (tokens * 4)
    go text
      | T.null text = []
      | T.length text <= scalars = [text]
      | otherwise =
          let prefix = T.take scalars text
              suffix = lastSplit (T.splitOn "\n" prefix)
              lastNewline = scalars - T.length suffix
              cut = if lastNewline == 0 then scalars else lastNewline
              (part, rest) = T.splitAt cut text
           in part : go rest

    lastSplit [] = ""
    lastSplit [part] = part
    lastSplit (_ : parts) = lastSplit parts

presentSelected ::
  (Member Commands effects, Member Jev effects, Member Reflect effects) =>
  Command.ObservationPresenter effects
presentSelected command purpose observation retained = do
  recent <- recentConversation
  (_, prepared) <- Cmd.observeWith observation retained (prepare command purpose recent)
  pure prepared

prepare ::
  (Member Commands effects, Member Jev effects, Member Reflect effects) =>
  Maybe Text ->
  Maybe Text ->
  Text ->
  Cmd.PresentedObservation ->
  Eff effects Text
prepare command purpose recent observed = case Cmd.presentedOutput observed of
  Left issue -> pure (statusHeading observed <> "\nOutput unavailable: " <> Cmd.renderCommandError issue <> recovery observed)
  Right output -> do
    let frozen =
          Snapshot
            (Cmd.presentedJob observed)
            (Cmd.outputAvailableEnd (Cmd.commandStdout output))
            (Cmd.outputAvailableEnd (Cmd.commandStderr output))
    loaded <- loadSections frozen (Just output)
    case loaded of
      Left issue -> pure (statusHeading observed <> "\n" <> renderIssue issue <> recoveryFor frozen)
      Right sections -> do
        if estimatedTokens (T.concat (map sectionText sections)) <= jevTriggerTokens
          then pure (renderRaw observed frozen sections)
          else do
            scored <- scoreSections command purpose recent sections
            pure $ case scored of
              Left failure -> renderFallback observed frozen failure sections
              Right ranked -> renderSelected observed frozen ranked

statusHeading :: Cmd.PresentedObservation -> Text
statusHeading observed =
  "session_id: " <> jobText (Cmd.presentedJob observed) <> "\n" <> case Cmd.presentedStatus observed of
    CommandQueued -> "terminal: no · queued (process not started)"
    CommandStarting -> "terminal: no · starting"
    CommandRunning -> "terminal: no · running"
    CommandStopping -> "terminal: no · stopping; cancellation not yet confirmed"
    CommandFinished result ->
      "terminal: yes · " <> T.pack (show (commandOutcome result)) <> " · cleanup: " <> cleanupText (commandCleanup result)
  where
    cleanupText CommandClean = "clean"
    cleanupText other = T.pack (show other)

jobText :: Cmd.Job -> Text
jobText (Job key) = key

snapshotFromObservation :: Cmd.PresentedObservation -> Maybe Snapshot
snapshotFromObservation observed = case Cmd.presentedOutput observed of
  Left _ -> Nothing
  Right output ->
    Just
      ( Snapshot
          (Cmd.presentedJob observed)
          (Cmd.outputAvailableEnd (Cmd.commandStdout output))
          (Cmd.outputAvailableEnd (Cmd.commandStderr output))
      )

loadSections :: Member Commands effects => Snapshot -> Maybe CommandOutput -> Eff effects (Either SnapshotIssue [Section])
loadSections frozen initial = do
  out <- readFrozen frozen Cmd.Stdout (stdoutEndpoint frozen) (Cmd.commandStdout <$> initial)
  err <- readFrozen frozen Cmd.Stderr (stderrEndpoint frozen) (Cmd.commandStderr <$> initial)
  pure $ do
    stdoutText <- out
    stderrText <- err
    let pieces = [(Cmd.Stdout, stdoutText), (Cmd.Stderr, stderrText)]
        numbered = assignSections pieces
    pure (applyScoringCeiling (maximumScoredTokens * 4) numbered)

applyScoringCeiling :: Int -> [Section] -> [Section]
applyScoringCeiling limit sections = zipWith renumber [1 ..] (snd (mapAccum split 0 sections) >>= id)
  where
    renumber ident section' = section' {sectionId = SectionId ident}
    split used section'
      | used >= limit = (used, [section' {sectionScored = False}])
      | T.length (sectionText section') <= limit - used =
          (used + T.length (sectionText section'), [section' {sectionScored = True}])
      | otherwise =
          let scalarCount = limit - used
              (scoredText, remainingText) = T.splitAt scalarCount (sectionText section')
              boundary = sectionStart section' + utf8Bytes scoredText
              scored = section' {sectionEnd = boundary, sectionText = scoredText, sectionScored = True}
              remainder = section' {sectionStart = boundary, sectionText = remainingText, sectionScored = False}
           in (limit, [scored, remainder])

mapAccum :: (s -> a -> (s, b)) -> s -> [a] -> (s, [b])
mapAccum _ state [] = (state, [])
mapAccum step state (value : values) =
  let (next, result) = step state value
      (final, rest) = mapAccum step next values
   in (final, result : rest)

assignSections :: [(Cmd.CommandStream, Text)] -> [Section]
assignSections streams = snd (foldl addStream (1, []) streams)
  where
    addStream (nextId, prior) (stream, text) =
      let (_, built) = foldl (addPart stream) (0, []) (splitSections sectionTokens text)
          numbered = zipWith (number stream) [nextId ..] built
       in (nextId + length built, prior <> numbered)
    addPart _ (offset, parts) part =
      let bytes = utf8Bytes part
       in (offset + bytes, parts <> [(offset, offset + bytes, part)])
    number stream ident (start, end, text) = Section (SectionId ident) stream start end text True

readFrozen :: Member Commands effects => Snapshot -> Cmd.CommandStream -> Int -> Maybe CommandPage -> Eff effects (Either SnapshotIssue Text)
readFrozen frozen stream endpoint initial = case initial of
  Nothing -> collect 0 []
  Just page
    | Cmd.outputLossy page -> pure (Left (SnapshotDecodingLoss stream (Cmd.outputStart page)))
    | Cmd.outputStart page /= 0 || Cmd.outputLostBytes page /= 0 -> pure (Left (SnapshotExpired stream 0))
    | otherwise -> collect (min endpoint (Cmd.outputEnd page)) [Cmd.outputText page]
  where
    collect cursor chunks
      | cursor >= endpoint = pure (Right (T.concat (reverse chunks)))
      | otherwise = do
          let wanted = min 65536 (endpoint - cursor)
          page <- Cmd.tryPage (snapshotJob frozen) stream (Cmd.OutputSlice cursor wanted)
          case page of
            Left issue -> pure (Left (SnapshotUnavailable stream issue))
            Right raw ->
              let details = Cmd.pageDetails raw
               in if Cmd.outputLossy details
                    then pure (Left (SnapshotDecodingLoss stream cursor))
                    else
                      if Cmd.outputStart details /= cursor || Cmd.outputLostBytes details /= 0
                        then pure (Left (SnapshotExpired stream cursor))
                        else
                          if Cmd.outputEnd details <= cursor
                            then pure (Left (SnapshotExpired stream cursor))
                            else collect (min endpoint (Cmd.outputEnd details)) (Cmd.outputText details : chunks)

scoreSections ::
  Member Jev effects =>
  Maybe Text ->
  Maybe Text ->
  Text ->
  [Section] ->
  Eff effects (Either Text [Ranked])
scoreSections command purpose recent sections = do
  let scoreable = filter sectionScored sections
      batches = chunksOf questionsPerRequest scoreable
  answers <- forM batches (scoreBatch command purpose recent)
  pure $
    fmap
      (<> map (\section' -> Ranked section' (-1)) (filter (not . sectionScored) sections))
      (fmap concat (sequence answers))

scoreBatch :: Member Jev effects => Maybe Text -> Maybe Text -> Text -> [Section] -> Eff effects (Either Text [Ranked])
scoreBatch command purpose recent rows = do
  let world =
        J.rawState
          ( String
              ( "command: "
                  <> maybe "(subsequent observation)" id command
                  <> "\nintent: "
                  <> maybe "(unspecified)" id purpose
                  <> "\nrecent conversation:\n"
                  <> recent
              )
          )
      rubric =
        J.level #irrelevant "Unrelated to the command's purpose and current work." (0 :: Int)
          J..| J.level #background "Context that may orient a reader but is not needed for the next action." 1
          J..| J.level #useful "Evidence that helps diagnose, verify, or decide the next action." 2
          J..| J.level #essential "A result, failure, warning, or fact the next action depends on." 3
      packet =
        #sections :=
          J.each
            (sectionKey . sectionId)
            (\row ->
               #relevance :=
                 J.score
                   ( "How relevant is this command-output section to the command's purpose and recent conversation? Preserve decisive diagnostics.\nSection "
                       <> sectionKey (sectionId row)
                       <> " ("
                       <> streamName (sectionStream row)
                       <> "):\n"
                       <> sectionText row
                   )
                   rubric
            )
            rows
  answer <- J.ask world packet
  pure $ case answer of
    Left failure -> Left (T.pack (show failure))
    Right response ->
      Right
        [ Ranked row judged.relevance.expectation
          | (row, judged) <- response.sections
        ]

renderRaw :: Cmd.PresentedObservation -> Snapshot -> [Section] -> Text
renderRaw observed frozen sections =
  let raw = statusHeading observed <> "\n" <> T.concat (map sectionText sections)
   in if utf8Bytes raw <= Cmd.presentedByteBudget observed
        then raw
        else renderFallback observed frozen "short output exceeds the configured byte display budget" sections

renderFallback :: Cmd.PresentedObservation -> Snapshot -> Text -> [Section] -> Text
renderFallback observed frozen failure sections =
  let chosen = takeWithin (selectedOutputTokens * 4) (max 0 (Cmd.presentedByteBudget observed - 1536)) sections
   in boundBytes (Cmd.presentedByteBudget observed) $
        statusHeading observed
          <> "\nJev unavailable; bounded raw output follows ("
          <> failure
          <> ").\n"
          <> renderMarked chosen
          <> recoveryIfOmitted frozen sections chosen

renderSelected :: Cmd.PresentedObservation -> Snapshot -> [Ranked] -> Text
renderSelected observed frozen ranked =
  let allSections = map (\(Ranked section' _) -> section') ranked
      orderedByRank = sortBy (flip (comparing rankKey)) ranked
      selected =
        takeRanked
          (selectedOutputTokens * 4)
          (max 0 (Cmd.presentedByteBudget observed - 1536))
          (filter (sectionScored . rankedSection) orderedByRank)
      original = sortBy (comparing sectionId) selected
   in boundBytes (Cmd.presentedByteBudget observed) $
        statusHeading observed
          <> "\n"
          <> renderMarked original
          <> recoveryIfOmitted frozen allSections original
          <> unscoredNotice ranked
  where
    rankKey (Ranked section' score) = (score, negateId (sectionId section'))
    negateId (SectionId value) = negate value
    rankedSection (Ranked section' _) = section'

takeRanked :: Int -> Int -> [Ranked] -> [Section]
takeRanked scalarBudget byteBudget = go scalarBudget byteBudget []
  where
    go _ _ kept [] = reverse kept
    go scalars bytes kept (Ranked candidate _ : rest)
      | costScalars <= scalars && costBytes <= bytes = go (scalars - costScalars) (bytes - costBytes) (candidate : kept) rest
      | otherwise = go scalars bytes kept rest
      where
        rendered = renderMarked [candidate]
        costScalars = T.length (sectionText candidate)
        costBytes = utf8Bytes rendered

takeWithin :: Int -> Int -> [Section] -> [Section]
takeWithin scalarBudget byteBudget = go scalarBudget byteBudget
  where
    go _ _ [] = []
    go scalars bytes (candidate : rest)
      | T.length (sectionText candidate) <= scalars && utf8Bytes rendered <= bytes = candidate : go (scalars - T.length (sectionText candidate)) (bytes - utf8Bytes rendered) rest
      | otherwise = []
      where
        rendered = renderMarked [candidate]

renderMarked :: [Section] -> Text
renderMarked = T.concat . map render
  where
    render section' =
      let key = sectionKey (sectionId section')
       in streamName (sectionStream section') <> " <" <> key <> ">\n" <> sectionText section' <> "\n</" <> key <> ">\n"

recoveryIfOmitted :: Snapshot -> [Section] -> [Section] -> Text
recoveryIfOmitted frozen allSections selected =
  let kept = map sectionId selected
      omitted = [sectionId section' | section' <- allSections, sectionId section' `notElem` kept]
   in if null omitted
        then ""
        else "omitted: " <> ranges omitted <> "\n" <> recoveryFor frozen

recovery :: Cmd.PresentedObservation -> Text
recovery observed = maybe "" recoveryFor (snapshotFromObservation observed)

recoveryFor :: Snapshot -> Text
recoveryFor frozen =
  "Recover without rerunning (use the Cmd.Job binding shown above): let snap = Project.Shell.snapshot jobN"
    <> " "
    <> number (stdoutEndpoint frozen)
    <> " "
    <> number (stderrEndpoint frozen)
    <> "; raw <- Project.Shell.section snap (Project.Shell.SectionId N)."

unscoredNotice :: [Ranked] -> Text
unscoredNotice ranked =
  let ids = [sectionId section' | Ranked section' _ <- ranked, not (sectionScored section')]
   in if null ids then "" else "unscored beyond the scoring ceiling: " <> ranges ids <> "\n"

sectionKey :: SectionId -> Text
sectionKey (SectionId value) = "s" <> number value

ranges :: [SectionId] -> Text
ranges = T.intercalate "," . map renderRange . runs . map value
  where
    value (SectionId ident) = ident
    runs [] = []
    runs (first : rest) = collect first first rest
    collect first last' [] = [(first, last')]
    collect first last' (next : rest)
      | next == last' + 1 = collect first next rest
      | otherwise = (first, last') : collect next next rest
    renderRange (first, last')
      | first == last' = sectionKey (SectionId first)
      | otherwise = sectionKey (SectionId first) <> "–" <> sectionKey (SectionId last')

streamName :: Cmd.CommandStream -> Text
streamName Cmd.Stdout = "stdout"
streamName Cmd.Stderr = "stderr"

renderIssue :: SnapshotIssue -> Text
renderIssue issue = case issue of
  SnapshotExpired stream offset -> streamName stream <> " retained output expired or has a gap at byte " <> number offset <> "."
  SnapshotDecodingLoss stream offset -> streamName stream <> " has decoding loss at byte " <> number offset <> "."
  SnapshotUnavailable stream failure -> streamName stream <> " unavailable: " <> Cmd.renderCommandError failure
  UnknownSection ident -> "No section " <> sectionKey ident <> " belongs to this snapshot."

section :: Member Commands effects => Snapshot -> SectionId -> Eff effects (Either SnapshotIssue Text)
section frozen ident = do
  loaded <- loadSections frozen Nothing
  pure $ do
    sections <- loaded
    maybe (Left (UnknownSection ident)) (Right . sectionText) (findSection ident sections)

sectionPage :: Member Commands effects => Snapshot -> SectionId -> Eff effects (Either SnapshotIssue Cmd.OutputPage)
sectionPage frozen ident = do
  loaded <- loadSections frozen Nothing
  case loaded >>= maybe (Left (UnknownSection ident)) Right . findSection ident of
    Left issue -> pure (Left issue)
    Right found -> do
      page <- Cmd.tryPage (snapshotJob frozen) (sectionStream found) (Cmd.OutputSlice (sectionStart found) (sectionEnd found - sectionStart found))
      pure $ case page of
        Left issue -> Left (SnapshotUnavailable (sectionStream found) issue)
        Right value
          | Cmd.outputLossy (Cmd.pageDetails value) -> Left (SnapshotDecodingLoss (sectionStream found) (sectionStart found))
          | Cmd.outputStart (Cmd.pageDetails value) /= sectionStart found || Cmd.outputLostBytes (Cmd.pageDetails value) /= 0 -> Left (SnapshotExpired (sectionStream found) (sectionStart found))
          | otherwise -> Right value

findSection :: SectionId -> [Section] -> Maybe Section
findSection ident = go
  where
    go [] = Nothing
    go (candidate : rest)
      | sectionId candidate == ident = Just candidate
      | otherwise = go rest

recentConversation :: Member Reflect effects => Eff effects Text
recentConversation = do
  reflected <- reflect 100
  pure $ case reflected of
    Left _ -> ""
    Right turns -> T.takeEnd (recentConversationTokens * 4) (T.concat (map renderTurn turns))

renderTurn :: ConversationTurn -> Text
renderTurn turn = T.concat (map renderItem (turnItems turn))

renderItem :: TurnItem -> Text
renderItem item = case item of
  TurnMessage role text -> roleName role <> ": " <> text <> "\n"
  TurnToolCall _ tool arguments -> "tool " <> tool <> " call: " <> arguments <> "\n"
  TurnToolResult _ output -> "tool result: " <> output <> "\n"

roleName :: ConversationRole -> Text
roleName RoleSystem = "system"
roleName RoleDeveloper = "developer"
roleName RoleUser = "user"
roleName RoleAssistant = "assistant"

chunksOf :: Int -> [a] -> [[a]]
chunksOf _ [] = []
chunksOf size values = let (front, rest) = splitAt size values in front : chunksOf size rest

boundBytes :: Int -> Text -> Text
boundBytes limit text
  | utf8Bytes text <= limit = text
  | otherwise = takeUtf8 limit text

takeUtf8 :: Int -> Text -> Text
takeUtf8 budget = T.pack . go budget . T.unpack
  where
    go _ [] = []
    go remaining (character : rest)
      | width character <= remaining = character : go (remaining - width character) rest
      | otherwise = []

utf8Bytes :: Text -> Int
utf8Bytes = sum . map width . T.unpack

width :: Char -> Int
width character
  | ord character < 0x80 = 1
  | ord character < 0x800 = 2
  | ord character < 0x10000 = 3
  | otherwise = 4

number :: Int -> Text
number = T.pack . show
