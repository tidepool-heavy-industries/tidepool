{-# LANGUAGE TypeApplications #-}
let assigned = case reviewBasis (sessionInput :: ReviewRequest) of { AssignedTask value -> value; _ -> error "expected assigned review" }
let reviewerMarker = "reviewer-private-history" :: Text
Right consumer <- spawnSubagent (FreshCtx "Read the assigned Task in a fresh independent context.") (ForkWorktree currentCheckout) ((defaultSpawnOptions workspaceAgentSpec) { spawnModel = Just (OpenAIModel Gpt6Luna), spawnLifetime = ActorOwned })
Right consumerRequest <- request @(Outcome Candidate) consumer assigned defaultRequestOptions
answer <- await (result consumerRequest)
display (case answer of { Right (Blocked "fresh task read" []) -> True; _ -> False })
