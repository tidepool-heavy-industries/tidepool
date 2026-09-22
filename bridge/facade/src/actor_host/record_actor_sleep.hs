import qualified Tidepool.Actor as Actor
import GHC.Generics (Generic)
data Sleeper mode = Sleeper { seen :: mode :- State [Int], waitAndRecord :: mode :- Call Int NoReply } deriving Generic
let definition = R.definition "sleeping-handler" Actor.ReadOnly Sleeper { seen = [], waitAndRecord = \value -> do { R.modify' (++ [value]); sleep (milliseconds 1); R.modify' (++ [value + 10]) } }
server <- R.start definition
let endpoints = R.client server
R.send (waitAndRecord endpoints) 1
R.send (waitAndRecord endpoints) 2
finished <- R.finish server
finished == Actor.Completed [1, 11, 2, 12]
