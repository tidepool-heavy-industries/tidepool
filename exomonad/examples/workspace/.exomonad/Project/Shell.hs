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
presentSelected _command _purpose focus observation retained = do
  (_, prepared) <- Cmd.observeWith observation retained (prepare focus)
  pure prepared

-- | Without a focus, output that fits 'Cmd.presentedByteBudget' is shown
-- whole -- no Jev call, no line-count gate. Output that does not fit is
-- truncated to a head and tail with an omitted-byte-range marker per
-- stream. With a focus, 'Project.Sift.sift' scores and selects sections
-- relevant to it, bounded by the same byte budget; text that already fits
-- is still shown whole, without scoring.
prepare ::
  (Member Commands effects, Member Jev effects, Member Reflect effects) =>
  Maybe Text ->
  Cmd.PresentedObservation ->
  Eff effects Text
prepare focus observed = case Cmd.presentedOutput observed of
  Left issue -> pure (statusHeading observed <> "\nOutput unavailable: " <> Cmd.renderCommandError issue <> recovery observed)
  Right output -> do
    let frozen =
          OutputSnapshot
            (Cmd.presentedJob observed)
            (Cmd.outputAvailableEnd (Cmd.commandStdout output))
            (Cmd.outputAvailableEnd (Cmd.commandStderr output))
    loaded <- loadStreams frozen (Just output)
    case loaded of
      Left issue -> pure (statusHeading observed <> "\n" <> renderIssue issue <> recoveryFor frozen)
      Right (stdoutText, stderrText) -> do
        let heading = statusHeading observed
            budget = Cmd.presentedByteBudget observed
            bodyBudget = max 0 (budget - utf8Bytes (heading <> "\n"))
            plain = stdoutText <> stderrText
        case focus of
          Nothing ->
            pure $
              heading <> "\n"
                <> if utf8Bytes plain <= bodyBudget
                     then plain
                     else plainOverBudget frozen bodyBudget stdoutText stderrText
          Just focusText -> do
            let labeled = labelStream Cmd.Stdout stdoutText <> labelStream Cmd.Stderr stderrText
            if utf8Bytes labeled <= bodyBudget
              then pure (heading <> "\n" <> plain)
              else do
                selected <- Sift.sift focusText bodyBudget labeled
                pure (heading <> "\n" <> selected <> recoveryFor frozen)

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

-- | A head of about three quarters of the given stream's budget share and a
-- tail of the rest, with a marker naming the omitted byte range -- only for
-- a stream that was actually cut. Budget is split between stdout and
-- stderr proportionally to their sizes.
plainOverBudget :: OutputSnapshot -> Int -> Text -> Text -> Text
plainOverBudget frozen budget stdoutText stderrText =
  let stdoutBytes = utf8Bytes stdoutText
      stderrBytes = utf8Bytes stderrText
      total = max 1 (stdoutBytes + stderrBytes)
      share bytes = (budget * bytes) `div` total
      (stdoutShown, stdoutMarker) = truncateStream "stdout" stdoutText (share stdoutBytes)
      (stderrShown, stderrMarker) = truncateStream "stderr" stderrText (share stderrBytes)
      markers = T.concat [marker <> "\n" | Just marker <- [stdoutMarker, stderrMarker]]
   in stdoutShown <> stderrShown <> markers <> recoveryFor frozen

truncateStream :: Text -> Text -> Int -> (Text, Maybe Text)
truncateStream label text budget
  | bytes <= max 0 budget = (text, Nothing)
  | otherwise =
      let headBudget = (max 0 budget * 3) `div` 4
          tailBudget = max 0 (budget - headBudget)
          headText = takeUtf8 headBudget text
          tailText = takeUtf8End tailBudget text
          omittedStart = utf8Bytes headText
          omittedEnd = max omittedStart (bytes - utf8Bytes tailText)
          marker = "omitted " <> label <> " bytes " <> number omittedStart <> ".." <> number omittedEnd <> " of " <> number bytes
       in (headText <> tailText, Just marker)
  where
    bytes = utf8Bytes text

statusHeading :: Cmd.PresentedObservation -> Text
statusHeading observed =
  "session_id: " <> jobText (Cmd.presentedJob observed) <> "\n" <> case Cmd.presentedStatus observed of
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

-- | Substituted with the actual retained-binding name (e.g. @job1@) by the
-- host once it mints that binding — the same fact it already names in the
-- "retained as ... :: Cmd.Job" line above this tool's output. The tool body
-- runs, and this text is built, before the host assigns a binding, so it
-- cannot be named here directly.
jobBindingPlaceholder :: Text
jobBindingPlaceholder = "{{job_binding}}"

recoveryFor :: OutputSnapshot -> Text
recoveryFor frozen =
  "Recover without rerunning: let snap = Project.Shell.outputSnapshot "
    <> jobBindingPlaceholder
    <> " "
    <> number (stdoutEndpoint frozen)
    <> " "
    <> number (stderrEndpoint frozen)
    <> "."

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
