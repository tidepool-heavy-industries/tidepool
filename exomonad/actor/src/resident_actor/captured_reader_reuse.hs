do
  let childEntry :: Int -> Eff '[Core.ActorKernel, Core.AgentTools] ()
      childEntry _ = do
        send Core.ActorReadyWith
        serveToolsWith () $ \_ -> CaptureTools
          { ping = tool "Read the completed private capture after parent cell failure." $ \() -> pure (capturedValue + 1) }
  begun <- send (Core.ForksBeginWith False "captured/reused" ["reader"])
  case begun of
    Right (group, _, [path]) -> do
      started <- send (Core.ForksStartWith
        path childEntry Nothing group
        Core.ActorResearchRole Core.ActorReadOnlyProfile []
        Nothing Core.RequireClean [] Nothing Nothing Nothing
        Core.InheritedContext (Just "CHECKPOINT_TOKEN") Nothing Core.ParentOwned)
      case started of
        Right _ -> do
          committed <- send (Core.ForksCommitCapturedWith group)
          case committed of
            Right () -> pure True
            Left refusal -> error (tshow refusal) >> pure False
        Left refusal -> error (tshow refusal) >> pure False
    _ -> error "capture reuse group admission failed" >> pure False
