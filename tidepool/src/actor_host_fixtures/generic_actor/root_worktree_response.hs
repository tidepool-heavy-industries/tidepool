complete $ nextTurn $ AgentAction $ do { createdTree <- createWorktree (fromCurrentRepository "response-metadata"); pure (Right createdTree) }
