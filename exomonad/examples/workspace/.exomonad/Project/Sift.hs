{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}

-- | A general text-selection widget. Given a focus, a byte budget, and a
-- block of text, 'sift' returns the sections of that text most relevant to
-- the focus, scored by Jev and packed to fit the budget, with a marker
-- naming what was left out. It knows nothing about commands, jobs, or
-- streams -- any cell holding a block of text it wants filtered down can
-- call it directly.
module Project.Sift
  ( sift,
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
import Tidepool.Aeson.Value (Value (String))
import Tidepool.Effects.Core
  ( ConversationRole (..),
    ConversationTurn (..),
    Jev,
    Reflect,
    TurnItem (..),
    reflect,
  )

sectionTokens, focusTokens, recentConversationTokens, maximumScoredTokens, questionsPerRequest :: Int
sectionTokens = 512
focusTokens = 1024
recentConversationTokens = 4096
maximumScoredTokens = 262144
questionsPerRequest = 128

newtype SectionId = SectionId Int
  deriving (Eq, Ord, Show)

data Section = Section
  { sectionId :: SectionId,
    sectionText :: Text,
    sectionScored :: Bool
  }
  deriving (Eq, Show)

data Ranked = Ranked Section Double

-- | Select the sections of @text@ most relevant to @focus@, bounded by
-- @budget@ bytes. Text that already fits the budget is returned whole,
-- without asking Jev.
sift :: (Member Jev effects, Member Reflect effects) => Text -> Int -> Text -> Eff effects Text
sift focus budget text
  | utf8Bytes text <= budget = pure text
  | otherwise = do
      recent <- recentConversation
      let sections = applyScoringCeiling (maximumScoredTokens * 4) (numberSections (splitSections sectionTokens text))
      scored <- scoreSections focus recent sections
      pure $ case scored of
        Left failure -> renderFallback budget failure sections
        Right ranked -> renderSelected budget ranked

numberSections :: [Text] -> [Section]
numberSections = zipWith (\ident piece -> Section (SectionId ident) piece True) [1 ..]

-- | Deterministic line-aware partitioning. A line longer than one section is
-- split at the scalar boundary; otherwise the last newline within the budget
-- closes the section.
splitSections :: Int -> Text -> [Text]
splitSections tokens = go
  where
    scalars = max 1 (tokens * 4)
    go piece
      | T.null piece = []
      | T.length piece <= scalars = [piece]
      | otherwise =
          let prefix = T.take scalars piece
              suffix = lastSplit (T.splitOn "\n" prefix)
              lastNewline = scalars - T.length suffix
              cut = if lastNewline == 0 then scalars else lastNewline
              (part, rest) = T.splitAt cut piece
           in part : go rest

    lastSplit [] = ""
    lastSplit [part] = part
    lastSplit (_ : parts) = lastSplit parts

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
           in (limit, [section' {sectionText = scoredText, sectionScored = True}, section' {sectionText = remainingText, sectionScored = False}])

mapAccum :: (s -> a -> (s, b)) -> s -> [a] -> (s, [b])
mapAccum _ state [] = (state, [])
mapAccum step state (value : values) =
  let (next, result) = step state value
      (final, rest) = mapAccum step next values
   in (final, result : rest)

scoreSections :: Member Jev effects => Text -> Text -> [Section] -> Eff effects (Either Text [Ranked])
scoreSections focus recent sections = do
  let scoreable = filter sectionScored sections
      batches = chunksOf questionsPerRequest scoreable
  answers <- forM batches (scoreBatch focus recent)
  pure $
    fmap
      (<> map (\section' -> Ranked section' (-1)) (filter (not . sectionScored) sections))
      (fmap concat (sequence answers))

scoreBatch :: Member Jev effects => Text -> Text -> [Section] -> Eff effects (Either Text [Ranked])
scoreBatch focus recent rows = do
  let world =
        J.rawState
          ( String
              ( "focus: "
                  <> scalarPrefix focusTokens focus
                  <> "\nrecent conversation:\n"
                  <> recent
              )
          )
      rubric =
        J.level #irrelevant "Unrelated to the focus." (0 :: Int)
          J..| J.level #background "Context that may orient a reader but is not needed for the focus." 1
          J..| J.level #useful "Evidence that helps address the focus." 2
          J..| J.level #essential "A result, failure, warning, or fact the focus depends on." 3
      packet =
        #sections :=
          J.each
            (sectionKey . sectionId)
            ( \row ->
                #relevance :=
                  J.score
                    ( "How relevant is this section to the focus and recent conversation? Preserve decisive diagnostics.\nSection "
                        <> sectionKey (sectionId row)
                        <> ":\n"
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

renderFallback :: Int -> Text -> [Section] -> Text
renderFallback budget failure sections =
  let prefix = "Jev unavailable; bounded raw output follows (" <> takeUtf8 256 failure <> ").\n"
      render chosen = prefix <> renderMarked chosen <> omittedNote sections chosen
      chosen = takeWithin budget render sections
   in render chosen

renderSelected :: Int -> [Ranked] -> Text
renderSelected budget ranked =
  let allSections = map rankedSection ranked
      orderedByRank = sortBy (flip (comparing rankKey)) ranked
      render chosen =
        renderMarked (sortBy (comparing sectionId) chosen)
          <> unscoredNotice ranked
          <> omittedNote allSections chosen
      selected = takeRanked budget render (filter (sectionScored . rankedSection) orderedByRank)
      original = sortBy (comparing sectionId) selected
   in render original
  where
    rankKey (Ranked section' score) = (score, negateId (sectionId section'))
    negateId (SectionId value) = negate value
    rankedSection (Ranked section' _) = section'

-- | Best-effort packing, highest-ranked first: a candidate that would blow
-- the budget is skipped rather than stopping the whole selection, since a
-- smaller, lower-ranked section further down may still fit.
takeRanked :: Int -> ([Section] -> Text) -> [Ranked] -> [Section]
takeRanked byteBudget render = go []
  where
    go kept [] = reverse kept
    go kept (Ranked candidate _ : rest)
      | utf8Bytes (render tentative) <= byteBudget = go (candidate : kept) rest
      | otherwise = go kept rest
      where
        tentative = reverse (candidate : kept)

-- | Sequential packing: stops at the first candidate that would blow the
-- budget, since order (and therefore meaning) matters for the raw fallback.
takeWithin :: Int -> ([Section] -> Text) -> [Section] -> [Section]
takeWithin byteBudget render = go []
  where
    go kept [] = reverse kept
    go kept (candidate : rest)
      | utf8Bytes (render tentative) <= byteBudget = go (candidate : kept) rest
      | otherwise = reverse kept
      where
        tentative = reverse (candidate : kept)

renderMarked :: [Section] -> Text
renderMarked = T.concat . map render
  where
    render section' =
      let key = sectionKey (sectionId section')
       in "<" <> key <> ">\n" <> sectionText section' <> "\n</" <> key <> ">\n"

omittedNote :: [Section] -> [Section] -> Text
omittedNote allSections selected =
  let kept = map sectionId selected
      omitted = [sectionId section' | section' <- allSections, sectionId section' `notElem` kept]
   in case omitted of
        [] -> ""
        _ -> "omitted: " <> ranges omitted <> "\n"

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
      | otherwise = sectionKey (SectionId first) <> "\8211" <> sectionKey (SectionId last')

-- | The last turns, filtered to user and assistant message text only --
-- excluding tool calls and tool results -- so scoring context stays on
-- what was said, not on what was run. Bounded to a fixed byte budget.
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
  TurnMessage role text | isUserOrAssistant role -> roleName role <> ": " <> text <> "\n"
  TurnMessage _ _ -> ""
  TurnToolCall {} -> ""
  TurnToolResult {} -> ""

isUserOrAssistant :: ConversationRole -> Bool
isUserOrAssistant RoleUser = True
isUserOrAssistant RoleAssistant = True
isUserOrAssistant _ = False

roleName :: ConversationRole -> Text
roleName RoleSystem = "system"
roleName RoleDeveloper = "developer"
roleName RoleUser = "user"
roleName RoleAssistant = "assistant"

chunksOf :: Int -> [a] -> [[a]]
chunksOf _ [] = []
chunksOf size values = let (front, rest) = splitAt size values in front : chunksOf size rest

scalarPrefix :: Int -> Text -> Text
scalarPrefix tokens = T.take (tokens * 4)

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
