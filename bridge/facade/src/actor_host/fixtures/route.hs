import qualified Tidepool.Agent.Contract as A
Right captured <- checkpoint "route workers"
Right producerActor <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "producer" })
Right consumerActor <- spawnSubagent (ForkCtx captured) (ForkWorktree projectHead)
  ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnLabel = Just "consumer" })
Right producer <- request @Text producerActor ("candidate" :: Text) defaultRequestOptions
Right consumer <- request @Text consumerActor ("reviewer ready" :: Text) defaultRequestOptions
forwarding <- route (settlement producer) (\settled -> case settled of { Right answer -> do { Right _ <- request @Text consumerActor answer defaultRequestOptions; pure () }; Left failure -> error (T.pack (show failure)) })
broken <- route (settlement producer) (\_ -> error "deliberate route failure")
reviewLaunch <- route (settlement producer) (\settled -> case settled of
  Right answer -> do
    Right reviewer <- spawnSubagent (FreshCtx answer) (ForkWorktree projectHead)
      ((defaultSpawnOptions (A.defaultWorkbenchSpec @'[Replies])) { spawnModel = Just (Literal "gpt-6-sol"), spawnLabel = Just "review" })
    Right _ <- request @Text reviewer answer defaultRequestOptions
    pure ()
  Left _ -> pure ())
