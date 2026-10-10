let resultRegistryReceiver :: AgentProtocol () -> Eff '[Core.ActorLocal AgentProtocol, Core.AgentTools, Core.AgentSession, Core.Actor, Core.FsRead, Core.Worktree, Core.Notifications, Core.Console, Core.Sleep] ()
    resultRegistryReceiver (RunRequest action) = action
    resultRegistryScalar :: Int
    resultRegistryScalar = 41
    resultRegistryVerifier :: Replies.ResponseResult Int -> Eff '[] Int
    resultRegistryVerifier response = pure (Replies.responseValue response)
