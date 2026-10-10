let activationReceiver :: AgentProtocol () -> Eff '[Core.ActorLocal AgentProtocol, Core.AgentTools, Core.AgentSession, Core.Actor, Core.FsRead, Core.Worktree, Core.Notifications, Core.Console, Core.Sleep] ()
    activationReceiver (RunRequest action) = action
    activationUnitReply :: ()
    activationUnitReply = ()
    activationFunctionReply :: Int -> Int
    activationFunctionReply value = value + 1
    ownedResultProbe :: ResponseResult (Int -> Int) -> Eff '[Core.ActorLocal AgentProtocol, Core.AgentTools, Core.AgentSession, Core.Actor, Core.FsRead, Core.Worktree, Core.Notifications, Core.Console, Core.Sleep] Int
    ownedResultProbe (ResponseResult value _ _) = pure (value 41)
