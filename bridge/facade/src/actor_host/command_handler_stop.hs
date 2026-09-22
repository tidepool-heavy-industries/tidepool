import GHC.Generics (Generic)
import qualified Tidepool.Effects.Row as Row
import qualified Tidepool.Effects.Core as Core
job <- Cmd.start [bash|printf handler|]
data CommandHandler mode = CommandHandler { saved :: mode :- State (), done :: mode :- Event Cmd.CommandResult, execute :: mode :- Call () (R.Reply ()) } deriving Generic
let commandHandler = R.definition "command-handler" (Actor.Selected (Row.knownEffects @'[Core.Commands])) CommandHandler { saved = (), done = R.on (Cmd.completion job) (\_ -> pure ()), execute = \() -> do { result <- Cmd.await job; pure () } }
handler <- R.start commandHandler
