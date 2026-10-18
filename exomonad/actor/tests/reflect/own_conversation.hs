do
      answer <- send (ReflectWith 2)
      case answer of
        Right [ConversationTurn { turnStartedAt = Just "opened", turnCompletedAt = Just "closed", turnItems = firstItems }, ConversationTurn { turnStartedAt = Nothing, turnCompletedAt = Nothing, turnItems = secondItems }]
          | firstItems ==
              [ TurnMessage RoleUser "read the brief"
              , TurnToolCall "c1" "haskell" "briefQuery"
              , TurnToolResult "c1" "the brief"
              ]
          , secondItems == [TurnMessage RoleAssistant "acknowledged"]
          -> pure (37 :: Int)
        _ -> error "reflect did not return this actor's own two completed turns"
