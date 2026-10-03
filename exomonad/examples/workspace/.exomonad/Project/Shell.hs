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
    OutputSnapshot,
    outputSnapshot,
    snapshotJob,
    stdoutEndpoint,
    stderrEndpoint,
    SectionId (..),
    OutputSnapshotIssue (..),
    section,
    sectionPage,
    estimatedTokens,
    splitSections,
    rawLineThreshold,
  )
where

import Control.Monad.Freer (Eff, Member)
import Data.Char (ord)
import Data.Text (Text)
import qualified Data.Text as T
import qualified Project.Sift as Sift
import Tidepool.Agent.Contract (AsServerT)
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Command
import Tidepool.Command.Types (Job (..))
import Tidepool.Effects.Core
  ( CommandCleanup (..),
    CommandError (..),
    CommandOutcome (..),
    CommandOutput (..),
    CommandPage (..),
    CommandResult (..),
    CommandStatus (..),
    Commands,
    Jev,
    Reflect,
  )

-- Configuration is ordinary Haskell on purpose: a workspace can tune these
-- values and reload its spec without changing a runtime protocol.
rawLineThreshold, sectionTokens, maximumScoredTokens :: Int
-- 'Project.Watchdog.trivialCall' abstains before asking Jev when a
-- finished, non-destructive call's displayed output is no longer than this
-- many lines. Presentation itself no longer gates on line count: output
-- that fits 'max_output_bytes' is shown whole regardless of line count.
rawLineThreshold = 15
sectionTokens = 512
maximumScoredTokens = 262144

data OutputSnapshot = OutputSnapshot
  { snapshotJob :: Cmd.Job,
    stdoutEndpoint :: Int,
    stderrEndpoint :: Int
  }
  deriving (Eq, Show)

outputSnapshot :: Cmd.Job -> Int -> Int -> OutputSnapshot
outputSnapshot = OutputSnapshot

newtype SectionId = SectionId Int
  deriving (Eq, Ord, Show)

data OutputSnapshotIssue
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
presentSelected command _purpose focus observation retained = do
  let introduceSession = maybe False (const True) command
  (_, prepared) <- Cmd.observeWith observation retained (prepare introduceSession focus retained)
  pure prepared

-- | Without a focus, output that fits 'Cmd.presentedByteBudget' is shown
-- whole -- no Jev call, no line-count gate. Output that does not fit is
-- truncated to a head and tail with an omitted-byte-range marker per
-- stream. With a focus, 'Project.Sift.sift' scores and selects sections
-- relevant to it, bounded by the same byte budget; text that already fits
-- is still shown whole, without scoring.
prepare ::
  (Member Commands effects, Member Jev effects, Member Reflect effects) =>
  Bool ->
  Maybe Text ->
  Cmd.Job ->
  Cmd.PresentedObservation ->
  Eff effects Command.CommandToolResult
prepare introduceSession focus job observed = case Cmd.presentedOutput observed of
  Left issue -> pure (reply 0 False (statusHeading introduceSession observed <> "\nOutput unavailable: " <> Cmd.renderCommandError issue <> recovery observed))
  Right output -> do
    let frozen =
          OutputSnapshot
            (Cmd.presentedJob observed)
            (Cmd.outputAvailableEnd (Cmd.commandStdout output))
            (Cmd.outputAvailableEnd (Cmd.commandStderr output))
    case focus of
      Nothing -> do
        (text, payloadLines, isComplete) <- prepareUnfocused introduceSession observed frozen output
        pure (reply payloadLines isComplete text)
      Just focusText -> do
        loaded <- loadStreams frozen (Just output)
        case loaded of
          Left issue -> pure (reply 0 False (statusHeading introduceSession observed <> "\n" <> renderIssue issue <> recoveryFor frozen))
          Right (stdoutText, stderrText) -> do
            let heading = statusHeading introduceSession observed
                budget = Cmd.presentedByteBudget observed
                bodyBudget = max 0 (budget - utf8Bytes (heading <> "\n"))
                plain = stdoutText <> stderrText
                labeled = labelStream Cmd.Stdout stdoutText <> labelStream Cmd.Stderr stderrText
            if utf8Bytes labeled <= bodyBudget
              then pure (reply (lineCount stdoutText + lineCount stderrText) True (heading <> "\n" <> labeled))
              else do
                let recoveryText = "\n" <> recoveryFor frozen
                    selectedBudget = max 0 (bodyBudget - utf8Bytes recoveryText)
                selected <- Sift.siftWithFacts payloadLineCount focusText selectedBudget labeled
                pure (reply (Sift.siftedPayloadLines selected) (Sift.siftedComplete selected) (heading <> "\n" <> Sift.siftedText selected <> recoveryText))
  where
    reply linesShown complete text =
      Command.observedResult job (Cmd.presentedStatus observed) (stdoutEndpoint <$> snapshotFromObservation observed) (stderrEndpoint <$> snapshotFromObservation observed) linesShown complete text

-- The ordinary unfocused view only reads both complete streams when their
-- frozen endpoints fit. Otherwise it requests bounded head and tail slices.
prepareUnfocused :: Member Commands effects => Bool -> Cmd.PresentedObservation -> OutputSnapshot -> CommandOutput -> Eff effects (Text, Int, Bool)
prepareUnfocused introduceSession observed frozen output =
  case initialIssue of
    Just issue -> pure (prefix <> renderIssue issue <> recovery, 0, False)
    Nothing
      | totalBytes <= fullBudget -> do
          loaded <- loadStreams frozen (Just output)
          pure $ case loaded of
            Left issue -> (prefix <> renderIssue issue <> recovery, 0, False)
            Right (stdoutText, stderrText) -> (prefix <> labelStream Cmd.Stdout stdoutText <> labelStream Cmd.Stderr stderrText, lineCount stdoutText + lineCount stderrText, True)
      | otherwise -> do
          let markerReserve = sum (map (reservedMarker . fst) nonemptyStreams)
              labelReserve = sum (map (labelBytes . fst) nonemptyStreams)
              selectedBudget = max 0 (truncatedBudget - markerReserve - labelReserve)
              share bytes = proportional selectedBudget bytes totalBytes
          stdout <- readStreamWindow frozen Cmd.Stdout (stdoutEndpoint frozen) (share (stdoutEndpoint frozen))
          stderr <- readStreamWindow frozen Cmd.Stderr (stderrEndpoint frozen) (share (stderrEndpoint frozen))
          pure $ case (stdout, stderr) of
            (Left issue, _) -> (prefix <> renderIssue issue <> recovery, 0, False)
            (_, Left issue) -> (prefix <> renderIssue issue <> recovery, 0, False)
            (Right stdoutText, Right stderrText) ->
              ( prefix <> renderWindow Cmd.Stdout stdoutText <> renderWindow Cmd.Stderr stderrText <> recovery,
                windowPayloadLines stdoutText + windowPayloadLines stderrText,
                False
              )
  where
    prefix = statusHeading introduceSession observed <> "\n"
    budget = max 0 (Cmd.presentedByteBudget observed)
    recovery = "\n" <> recoveryFor frozen
    totalBytes = stdoutEndpoint frozen + stderrEndpoint frozen
    fullBudget = max 0 (budget - utf8Bytes prefix - sum (map (labelBytes . fst) nonemptyStreams))
    truncatedBudget = max 0 (budget - utf8Bytes prefix - utf8Bytes recovery)
    nonemptyStreams = [(Cmd.Stdout, stdoutEndpoint frozen) | stdoutEndpoint frozen > 0] <> [(Cmd.Stderr, stderrEndpoint frozen) | stderrEndpoint frozen > 0]
    initialIssue = firstIssue (pageIssue Cmd.Stdout (Cmd.commandStdout output), pageIssue Cmd.Stderr (Cmd.commandStderr output))
    firstIssue (Just issue, _) = Just issue
    firstIssue (Nothing, issue) = issue
    pageIssue stream page
      | Cmd.outputLossy page = Just (SnapshotDecodingLoss stream (Cmd.outputStart page))
      | Cmd.outputStart page /= 0 || Cmd.outputLostBytes page /= 0 = Just (SnapshotExpired stream 0)
      | otherwise = Nothing
    labelBytes stream = utf8Bytes (streamName stream <> ":\n") + 1
    reservedMarker stream = utf8Bytes ("\n<omitted " <> streamName stream <> " bytes " <> number (endpoint stream) <> ".." <> number (endpoint stream) <> " of " <> number (endpoint stream) <> ">\n")
    endpoint Cmd.Stdout = stdoutEndpoint frozen
    endpoint Cmd.Stderr = stderrEndpoint frozen

windowPayloadLines :: StreamWindow -> Int
windowPayloadLines window = lineCount (windowHead window) + lineCount (windowTail window)

payloadLineCount :: Text -> Int
payloadLineCount text
  | "stdout:\n" `T.isPrefixOf` text || "stderr:\n" `T.isPrefixOf` text = lineCount (T.drop 1 (T.dropWhile (/= '\n') text))
  | otherwise = lineCount text

lineCount :: Text -> Int
lineCount text
  | T.null text = 0
  | otherwise = length (T.lines text)

proportional :: Int -> Int -> Int -> Int
proportional budget bytes total
  | total <= 0 = 0
  | otherwise = fromInteger (toInteger budget * toInteger bytes `div` toInteger total)

data StreamWindow = StreamWindow
  { windowHead :: Text,
    windowHeadEnd :: Int,
    windowTail :: Text,
    windowTailStart :: Int,
    windowTotal :: Int
  }

readStreamWindow :: Member Commands effects => OutputSnapshot -> Cmd.CommandStream -> Int -> Int -> Eff effects (Either OutputSnapshotIssue StreamWindow)
readStreamWindow frozen stream endpoint budget
  | endpoint <= budget = do
      whole <- readSelectedRange frozen stream 0 endpoint
      pure $ fmap (\(text, _, end) -> StreamWindow text end "" endpoint endpoint) whole
  | otherwise = do
      let headBudget = budget * 3 `div` 4
          tailBudget = budget - headBudget
          tailOffset = endpoint - tailBudget
      headPage <- readSelectedRange frozen stream 0 headBudget
      tailPage <- readSelectedRange frozen stream tailOffset endpoint
      pure $ do
        (headText, _, headEnd) <- headPage
        (tailText, tailStart, _) <- tailPage
        pure (StreamWindow headText headEnd tailText tailStart endpoint)

readSelectedRange :: Member Commands effects => OutputSnapshot -> Cmd.CommandStream -> Int -> Int -> Eff effects (Either OutputSnapshotIssue (Text, Int, Int))
readSelectedRange frozen stream start end
  | end <= start = pure (Right ("", start, end))
  | otherwise = do
      result <- Cmd.tryPage (snapshotJob frozen) stream (Cmd.OutputSlice start (end - start))
      pure $ case result of
        Left issue -> Left (SnapshotUnavailable stream issue)
        Right page ->
          let details = Cmd.pageDetails page
              actualStart = Cmd.outputStart details
              actualEnd = Cmd.outputEnd details
           in if Cmd.outputLossy details
                then Left (SnapshotDecodingLoss stream start)
                else if Cmd.outputLostBytes details /= 0 || actualStart < start || actualStart > end
                  then Left (SnapshotExpired stream start)
                  else if actualEnd < actualStart || actualEnd > end
                    then Left (SnapshotExpired stream actualEnd)
                    else if actualStart > start && not (Cmd.outputLeadingFragment details)
                      then Left (SnapshotExpired stream start)
                      else if actualEnd < end && not (Cmd.outputTrailingFragment details)
                        then Left (SnapshotExpired stream actualEnd)
                        else Right (Cmd.outputText details, actualStart, actualEnd)

renderWindow :: Cmd.CommandStream -> StreamWindow -> Text
renderWindow _ window | windowTotal window == 0 = ""
renderWindow stream window = streamName stream <> ":\n" <> windowHead window
  <> (if windowHeadEnd window < windowTailStart window
        then "\n<omitted " <> streamName stream <> " bytes " <> number (windowHeadEnd window) <> ".." <> number (windowTailStart window) <> " of " <> number (windowTotal window) <> ">\n"
        else "")
  <> windowTail window <> "\n"

-- | Two independent streams: no section splitting, no scoring -- the
-- head/tail truncation that runs whenever there is no focus to ask Jev
-- about.
loadStreams :: Member Commands effects => OutputSnapshot -> Maybe CommandOutput -> Eff effects (Either OutputSnapshotIssue (Text, Text))
loadStreams frozen initial = do
  out <- readFrozen frozen Cmd.Stdout (stdoutEndpoint frozen) (Cmd.commandStdout <$> initial)
  err <- readFrozen frozen Cmd.Stderr (stderrEndpoint frozen) (Cmd.commandStderr <$> initial)
  pure ((,) <$> out <*> err)

labelStream :: Cmd.CommandStream -> Text -> Text
labelStream _ text | T.null text = ""
labelStream stream text = streamName stream <> ":\n" <> text <> "\n"

statusHeading :: Bool -> Cmd.PresentedObservation -> Text
statusHeading introduceSession observed =
  (if introduceSession then "session_id: " <> jobText (Cmd.presentedJob observed) <> "\n" else "") <> case Cmd.presentedStatus observed of
    CommandQueued -> "terminal: no · queued (process not started)"
    CommandStarting -> "terminal: no · starting"
    CommandRunning -> "terminal: no · running"
    CommandStopping -> "terminal: no · stopping; cancellation not yet confirmed"
    CommandFinished result ->
      "terminal: yes · " <> outcomeText (commandOutcome result) <> " · cleanup: " <> cleanupText (commandCleanup result)
  where
    cleanupText CommandClean = "clean"
    cleanupText other = T.pack (show other)

-- | Model-facing rendering of a command outcome, matching
-- 'Tidepool.Command.outcomeText': an out-of-memory kill and a raw signal are
-- named directly rather than shown as an opaque exit code.
outcomeText :: CommandOutcome -> Text
outcomeText (CommandOutOfMemory limit) =
  "out of memory · memory_mib=" <> T.pack (show limit) <> " exceeded · rerun with a larger memory_mib"
outcomeText (CommandSignalled signal) =
  "killed by signal " <> T.pack (show signal)
outcomeText other = T.pack (show other)

jobText :: Cmd.Job -> Text
jobText (Job key) = key

snapshotFromObservation :: Cmd.PresentedObservation -> Maybe OutputSnapshot
snapshotFromObservation observed = case Cmd.presentedOutput observed of
  Left _ -> Nothing
  Right output ->
    Just
      ( OutputSnapshot
          (Cmd.presentedJob observed)
          (Cmd.outputAvailableEnd (Cmd.commandStdout output))
          (Cmd.outputAvailableEnd (Cmd.commandStderr output))
      )

loadSections :: Member Commands effects => OutputSnapshot -> Maybe CommandOutput -> Eff effects (Either OutputSnapshotIssue [Section])
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

readFrozen :: Member Commands effects => OutputSnapshot -> Cmd.CommandStream -> Int -> Maybe CommandPage -> Eff effects (Either OutputSnapshotIssue Text)
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

recovery :: Cmd.PresentedObservation -> Text
recovery observed = maybe "" recoveryFor (snapshotFromObservation observed)

recoveryFor :: OutputSnapshot -> Text
recoveryFor frozen =
  "Recover retained output with read_output(session_id=\""
    <> jobText (snapshotJob frozen)
    <> "\", stream=\"Stdout\" or \"Stderr\", offset=<next_offset>). Do not rerun the command."

sectionKey :: SectionId -> Text
sectionKey (SectionId value) = "s" <> number value

streamName :: Cmd.CommandStream -> Text
streamName Cmd.Stdout = "stdout"
streamName Cmd.Stderr = "stderr"

renderIssue :: OutputSnapshotIssue -> Text
renderIssue issue = case issue of
  SnapshotExpired stream offset -> streamName stream <> " retained output expired or has a gap at byte " <> number offset <> "."
  SnapshotDecodingLoss stream offset -> streamName stream <> " has decoding loss at byte " <> number offset <> "."
  SnapshotUnavailable stream failure -> streamName stream <> " unavailable: " <> Cmd.renderCommandError failure
  UnknownSection ident -> "No section " <> sectionKey ident <> " belongs to this snapshot."

section :: Member Commands effects => OutputSnapshot -> SectionId -> Eff effects (Either OutputSnapshotIssue Text)
section frozen ident = do
  loaded <- loadSections frozen Nothing
  pure $ do
    sections <- loaded
    maybe (Left (UnknownSection ident)) (Right . sectionText) (findSection ident sections)

sectionPage :: Member Commands effects => OutputSnapshot -> SectionId -> Eff effects (Either OutputSnapshotIssue Cmd.OutputPage)
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
          | Cmd.outputEnd (Cmd.pageDetails value) /= sectionEnd found -> Left (SnapshotExpired (sectionStream found) (Cmd.outputEnd (Cmd.pageDetails value)))
          | otherwise -> Right value

findSection :: SectionId -> [Section] -> Maybe Section
findSection ident = go
  where
    go [] = Nothing
    go (candidate : rest)
      | sectionId candidate == ident = Just candidate
      | otherwise = go rest

takeUtf8 :: Int -> Text -> Text
takeUtf8 budget = T.pack . go budget . T.unpack
  where
    go _ [] = []
    go remaining (character : rest)
      | width character <= remaining = character : go (remaining - width character) rest
      | otherwise = []

-- | Like 'takeUtf8' but keeps a scalar-safe suffix instead of a prefix.
takeUtf8End :: Int -> Text -> Text
takeUtf8End budget text = T.takeEnd (go budget (T.unpack (T.reverse text))) text
  where
    go _ [] = 0
    go remaining (character : rest)
      | width character <= remaining = 1 + go (remaining - width character) rest
      | otherwise = 0

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
