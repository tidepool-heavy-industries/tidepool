import GHC.Generics (Generic)
data BoxState = BoxState { boxCalls :: Int } deriving Show
data Box mode = Box { boxState :: mode :- State BoxState, boxRun :: mode :- Call Text (R.Reply Int), boxView :: mode :- Call () (R.Reply BoxState) } deriving Generic
type BoxEffects = LocalEffects Box '[Replies, Actor]
let boxDefinition :: ActorSpec Box BoxEffects; boxDefinition = R.definition "box" (Actor.Selected knownEffects) Box { boxState = BoxState 0, boxView = \() -> get, boxRun = \program -> do { modify' (\s -> s { boxCalls = boxCalls s + 1 }); pure (T.length program) } }
data CallerState = CallerState { callerReplies :: [Int] } deriving Show
data Caller mode = Caller { callerState :: mode :- State CallerState, callerAsk :: mode :- Call (ActorHandle Box, Text) NoReply, callerView :: mode :- Call () (R.Reply CallerState) } deriving Generic
type CallerEffects = LocalEffects Caller '[Replies, Actor]
let callerDefinition :: ActorSpec Caller CallerEffects; callerDefinition = R.definition "caller" (Actor.Selected knownEffects) Caller { callerState = CallerState [], callerView = \() -> get, callerAsk = \(box, program) -> do { code <- R.call (boxRun (R.client box)) program; modify' (\s -> s { callerReplies = callerReplies s ++ [code] }) } }
box <- R.start boxDefinition
caller <- R.start callerDefinition
R.send (callerAsk (R.client caller)) (box, "exit 3")
