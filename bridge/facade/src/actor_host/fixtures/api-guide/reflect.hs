-- Your own recent turns, or none when this context has no conversation.
recentContext :: Member Reflect effects => Int -> Eff effects [ConversationTurn]
recentContext n = do
  seen <- reflect n
  case seen of
    Right turns -> pure turns
    -- Continue without history rather than borrowing someone else's.
    Left _ -> pure []

-- The instructions you were given and the tool output you already paid for.
instructionsAndResults :: [ConversationTurn] -> [Text]
instructionsAndResults turns =
  [ text
  | turn <- turns
  , item <- turnItems turn
  , text <- case item of
      TurnMessage RoleUser instruction -> [instruction]
      TurnToolResult _ output -> [output]
      _ -> []
  ]

background <- instructionsAndResults <$> recentContext 5
display background
