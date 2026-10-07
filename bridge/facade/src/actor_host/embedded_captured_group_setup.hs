data AgentBox mode = AgentBox
  { agentState :: mode :- State [AgentRef]
  , storeAgents :: mode :- Call [AgentRef] NoReply
  , readAgents :: mode :- Call () (R.Reply [AgentRef])
  } deriving Generic
let groupBox = R.definition "embedded-checkpoint-agents" (Actor.Selected (knownEffects @'[])) AgentBox
      { agentState = []
      , storeAgents = R.put
      , readAgents = \() -> R.get
      }
groupStore <- R.start groupBox
