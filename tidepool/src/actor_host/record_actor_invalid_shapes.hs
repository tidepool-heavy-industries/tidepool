import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
data NoState mode = NoState { noStateMessage :: mode :- Call () NoReply } deriving Generic
data TwoStates mode = TwoStates { firstState :: mode :- State Int, secondState :: mode :- State Bool } deriving Generic
