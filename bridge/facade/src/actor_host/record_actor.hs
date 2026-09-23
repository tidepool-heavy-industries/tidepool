import qualified Tidepool.Actor as Actor
import GHC.Generics (Generic)
data Counter mode = Counter { count :: mode :- State Int, add :: mode :- Call Int NoReply, readCount :: mode :- Call () (R.Reply Int), relay :: mode :- Call Int (R.Reply ()), origin :: mode :- Call () (R.Reply ActorInputOrigin) } deriving Generic
let counter = R.definition "record-counter" Actor.ReadOnly Counter { count = 7, add = \delta -> R.modify' (+ delta), readCount = \() -> R.get, relay = \delta -> do { own <- R.self @Counter; R.send (add own) delta }, origin = \() -> R.sender @Counter }
server <- R.start counter
data Origins mode = Origins { originsState :: mode :- State [ActorInputOrigin], lifecycleInput :: mode :- Event Actor.ActorLifecycle, savedOrigins :: mode :- Call () (R.Reply [ActorInputOrigin]) } deriving Generic
let originsDefinition = R.definition "source-origins" Actor.ReadOnly Origins { originsState = [], lifecycleInput = R.on (R.lifecycle server) (\_ -> do { source <- R.sender @Origins; modify' (++ [source]) }), savedOrigins = \() -> get }
originWatcher <- R.start originsDefinition
let endpoints = R.client server
mapM_ (R.send (add endpoints)) [1..20]
R.call (relay endpoints) 7
from <- R.call (origin endpoints) ()
observed <- R.call (readCount endpoints) ()
finished <- R.finish server
again <- R.finish server
sourceOrigins <- R.call (savedOrigins (R.client originWatcher)) ()
originsFinished <- R.finish originWatcher
let address = read (drop (length "ActorHandle ") (show server)) :: (Int, Int)
show server == "ActorHandle " ++ show address && observed == 224 && finished == Actor.Completed 224 && again == finished && (case from of { ActorMessageFrom sender -> sender /= address; _ -> False }) && sourceOrigins == [ActorLifecycleFrom address, ActorLifecycleFrom address] && originsFinished == Actor.Completed sourceOrigins
