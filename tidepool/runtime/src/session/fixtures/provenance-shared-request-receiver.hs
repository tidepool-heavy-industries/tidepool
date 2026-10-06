let activationReceiver :: AgentProtocol () -> Eff '[Core.ActorLocal AgentProtocol, Core.AgentTools, Core.AgentSession, Core.Actor, Core.FsRead, Core.Worktree, Core.Notifications, Core.Console, Core.Sleep] ()
    activationReceiver (RunRequest action) = action
    activationUnitReply :: ()
    activationUnitReply = ()
    unusedSharedRequest :: Text -> Eff '[Replies] ()
    unusedSharedRequest = Shared.emit
