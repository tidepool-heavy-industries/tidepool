{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module Project.ConversationContextChecks (boundedHistory) where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Project.ConversationContext (renderConversationContext)
import Tidepool.Check (RecipeCheck, check)
import Tidepool.Effects.Core
  ( ConversationRole (..)
  , ConversationTurn (..)
  , TurnItem (..)
  )

boundedHistory :: Member RecipeCheck effects => Eff effects ()
boundedHistory = do
  let turns =
        [ turn
            [ TurnMessage RoleSystem "system secret"
            , TurnMessage RoleUser "first"
            , TurnToolCall "c1" "bash" "tool secret"
            , TurnToolResult "c1" "result secret"
            ]
        , turn [TurnMessage RoleAssistant "second", TurnMessage RoleDeveloper "developer secret"]
        , turn [TurnMessage RoleUser "third"]
        ]
      rendered = renderConversationContext 2 300 (Just turns)
  check "conversation context excludes tool and privileged message bodies"
    (all (not . (`T.isInfixOf` rendered)) ["secret", "system", "developer"])
  check "newest messages retain role labels and chronological order"
    ("assistant: second\nuser: third\n" `T.isSuffixOf` rendered
      && not ("user: first" `T.isInfixOf` rendered))
  check "dropped older messages are explicit"
    ("[earlier conversation omitted]" `T.isInfixOf` rendered)
  let wide = renderConversationContext 8 120
        (Just [turn [TurnMessage RoleUser (T.replicate 200 "🦀")]])
  check "Unicode scalar bound retains complete characters and a role label"
    (T.length wide <= 120 && "user: [start omitted] " `T.isInfixOf` wide
      && "🦀" `T.isInfixOf` wide)
  check "unavailable and empty history are distinguishable"
    (renderConversationContext 8 120 Nothing
      /= renderConversationContext 8 120 (Just [])
      && "unavailable" `T.isInfixOf` renderConversationContext 8 120 Nothing
      && "no recent" `T.isInfixOf` renderConversationContext 8 120 (Just []))

turn :: [TurnItem] -> ConversationTurn
turn items = ConversationTurn "t" Nothing Nothing items
