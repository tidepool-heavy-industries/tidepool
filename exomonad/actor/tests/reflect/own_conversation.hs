do
      answer <- send (ReflectWith 2)
      case answer of
        Right [ConversationTurn _ (Just "opened") (Just "closed") firstItems, ConversationTurn _ Nothing Nothing secondItems]
          | firstItems ==
              [ TurnMessage RoleUser "read the brief"
              , TurnToolCall "c1" "haskell" "briefQuery"
              , TurnToolResult "c1" "the brief"
              ]
          , secondItems == [TurnMessage RoleAssistant "acknowledged"]
          -> pure (37 :: Int)
        _ -> error "reflect did not return this actor's own two completed turns"
