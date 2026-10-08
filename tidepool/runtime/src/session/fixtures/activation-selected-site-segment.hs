let selectedReceiver :: AgentProtocol () -> Eff '[Core.ActorLocal AgentProtocol, Core.AgentTools, Core.AgentSession, Core.Actor, Core.FsRead, Core.Worktree, Core.Notifications, Core.Console, Core.Sleep] ()
    selectedReceiver (RunRequest action) = action
let selectedAction :: Int -> Eff '[Replies] ()
    selectedAction _ = Shared.emit "selected input"
let unrelatedAction :: Int -> Eff '[Replies] ()
    unrelatedAction n = do
      let target = Ref.internalAgentRef 17 1
          options = Agents.defaultRequestOptions { Agents.requestLabel = Just "unrelated-site" }
      _ <- Agents.request @() target n options
      pure ()
let unrelatedValue = (7 :: Int)
