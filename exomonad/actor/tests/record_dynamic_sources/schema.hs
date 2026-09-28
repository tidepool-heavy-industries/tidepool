type ActorEffects = '[AgentTools, Actor]

data CaseInput = CaseInput deriving (Generic, FromJSON, JsonSchema)
data CaseOutput = CaseOutput
  { passed :: Bool
  , attachment :: Text
  , observed :: Int
  } deriving (Generic, ToJSON, JsonSchema)
data Tools mode = Tools
  { runCase :: mode :- Finish CaseInput CaseOutput
  } deriving (Generic)
data Watcher mode = Watcher
  { watcherState :: mode R.:- R.State Int
  , watcherBegin :: mode R.:- R.Call (R.ActorHandle Watcher) (R.Reply (Either R.AttachmentError ()))
  , watcherCount :: mode R.:- R.Call () (R.Reply Int)
  , watcherEvent :: mode R.:- R.Event Actor.ActorLifecycle
  } deriving (Generic)
