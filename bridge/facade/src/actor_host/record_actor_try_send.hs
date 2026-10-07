import qualified Tidepool.Actor as Actor
import Data.Text (Text)
import GHC.Generics (Generic)
data Receiver mode = Receiver
  { receiverState :: mode :- State Int
  , add :: mode :- Call Int NoReply
  , selfEnqueue :: mode :- Call () (R.Reply (Either Text ()))
  , readReceiver :: mode :- Call () (R.Reply Int)
  } deriving Generic
let receiverDefinition = R.definition "try-send-receiver" (Actor.Selected (knownEffects @'[Actor])) Receiver
      { receiverState = 0
      , add = \amount -> R.modify' (+ amount)
      , selfEnqueue = \() -> do { own <- R.self @Receiver; R.trySend (add own) 5 }
      , readReceiver = \() -> R.get
      }
receiver <- R.start receiverDefinition
let receiverClient = R.client receiver
accepted <- R.trySend (add receiverClient) 2
selfAccepted <- R.call (selfEnqueue receiverClient) ()
observed <- R.call (readReceiver receiverClient) ()
receiverExit <- R.finish receiver
refused <- R.trySend (add receiverClient) 1
data Manager mode = Manager
  { managerState :: mode :- State Int
  , terminalRoute :: mode :- Call () NoReply
  } deriving Generic
let managerDefinition = R.definition "try-send-manager" (Actor.Selected (knownEffects @'[Actor])) Manager
      { managerState = 0
      , terminalRoute = \() -> do
          { result <- R.trySend (add receiverClient) 1
          ; R.put (case result of { Left _ -> 9; Right () -> 0 })
          }
      }
manager <- R.start managerDefinition
R.send (terminalRoute (R.client manager)) ()
managerExit <- R.finish manager
accepted == Right () && selfAccepted == Right () && observed == 7 &&
  receiverExit == Actor.Completed 7 &&
  (case refused of { Left _ -> True; Right () -> False }) &&
  managerExit == Actor.Completed 9
