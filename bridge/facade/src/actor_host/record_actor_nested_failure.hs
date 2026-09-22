import GHC.Generics (Generic)
data Leaf mode = Leaf { leafState :: mode :- State Int, explode :: mode :- Call () NoReply } deriving Generic
data Manager mode = Manager { managerState :: mode :- State Int, createLeaf :: mode :- Call () (R.Reply (ActorHandle Leaf)), managerValue :: mode :- Call () (R.Reply Int) } deriving Generic
let leafDefinition = R.definition "nested-leaf" Actor.ReadOnly Leaf { leafState = 0, explode = \() -> error "nested-handler-probe" }
let managerDefinition = R.definition "machine-manager" Actor.ReadOnly Manager { managerState = 7, createLeaf = \() -> R.start leafDefinition, managerValue = \() -> get }
manager <- R.start managerDefinition
leaf <- R.call (createLeaf (R.client manager)) ()
R.send (explode (R.client leaf)) ()
