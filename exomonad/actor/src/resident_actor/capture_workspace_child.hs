do
  let childEntry :: Int -> Eff '[Core.ActorKernel, Core.AgentTools] ()
      childEntry _ = do
        send Core.ActorReadyWith
        serveToolsWith () $ \_ -> CaptureTools
          { ping = tool "Keep the child workbench available." $ \() -> pure (0 :: Int) }
  started <- send (Core.ForksStartWith
    "CHILD_PATH" childEntry Nothing GROUP_ID
    Core.ActorResearchRole Core.ActorReadOnlyProfile []
    Nothing Core.RequireClean [] Nothing Nothing Nothing
    Core.InheritedContext (Just "CHECKPOINT_TOKEN") Nothing Core.ParentOwned)
  case started of
    Left failure -> error (tshow failure) >> pure True
    Right _ -> do
      committed <- send (Core.ForksCommitWith GROUP_ID)
      case committed of
        Right () -> pure True
        Left failure -> error (tshow failure) >> pure False
