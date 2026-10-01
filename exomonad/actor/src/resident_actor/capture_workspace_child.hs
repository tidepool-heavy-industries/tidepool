do
  let childEntry :: Int -> Eff '[Core.ActorKernel, Core.AgentSession, Core.ActorLocal CaptureProtocol] ()
      childEntry _ = do
        send (Core.AgentAttachWith Nothing)
        send Core.ActorReadyWith
        Mailbox.serve @() @CaptureProtocol () (\() CaptureNoop -> pure ((), ()))
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
