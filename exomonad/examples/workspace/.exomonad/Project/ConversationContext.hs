{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Small conversational context for Jev judgments. Tool evidence belongs in
-- the judgment's explicit input, not in a general conversation transcript.
module Project.ConversationContext
  ( recentConversationContext
  , renderConversationContext
  ) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as T
import Tidepool.Effects.Core
  ( ConversationRole (..)
  , ConversationTurn (..)
  , Reflect
  , TurnItem (..)
  , reflect
  )

historyTurns, messageLimit, scalarLimit :: Int
historyTurns = 8
messageLimit = 8
scalarLimit = 3000

-- | Eight latest turns bound the read itself; the active turn can be among
-- them. Only conversational messages enter the rendered context. The pure
-- assembler applies independent message and text limits.
recentConversationContext :: Member Reflect effects => Eff effects Text
recentConversationContext = do
  reflected <- reflect historyTurns
  pure (renderConversationContext messageLimit scalarLimit (either (const Nothing) Just reflected))

-- | Render newest user and assistant messages in chronological order. The
-- bound counts Unicode scalars, including labels and omission markers. Missing
-- history and an available history with no messages remain distinct.
renderConversationContext :: Int -> Int -> Maybe [ConversationTurn] -> Text
renderConversationContext maxMessages maxScalars history =
  case history of
    Nothing -> bounded "[conversation history unavailable]\n"
    Just turns -> case messages turns of
      [] -> bounded "[no recent user or assistant messages in reflected turns]\n"
      allMessages ->
        let newest = take (max 0 maxMessages) (reverse allMessages)
            omittedByCount = length allMessages > length newest
            room = max 0 (maxScalars - T.length heading - T.length omission)
            (fragments, omittedBySize) = collect room newest
            prefix = if omittedByCount || omittedBySize then omission else ""
        in bounded (heading <> prefix <> T.concat (reverse fragments))
  where
    bounded = T.take (max 0 maxScalars)
    heading = "Recent user and assistant messages:\n"
    omission = "[earlier conversation omitted]\n"

messages :: [ConversationTurn] -> [(Text, Text)]
messages turns =
  [ (label, body)
  | turn <- turns
  , TurnMessage role body <- turnItems turn
  , Just label <- [roleLabel role]
  ]

roleLabel :: ConversationRole -> Maybe Text
roleLabel RoleUser = Just "user"
roleLabel RoleAssistant = Just "assistant"
roleLabel _ = Nothing

collect :: Int -> [(Text, Text)] -> ([Text], Bool)
collect _ [] = ([], False)
collect room ((label, body) : rest)
  | T.length line <= room =
      let (older, omitted) = collect (room - T.length line) rest
      in (line : older, omitted)
  | room >= T.length prefix + T.length truncated + 1 =
      ([prefix <> truncated <> T.takeEnd (room - T.length prefix - T.length truncated - 1) body <> "\n"], True)
  | otherwise = ([], True)
  where
    prefix = label <> ": "
    line = prefix <> body <> "\n"
    truncated = "[start omitted] "
