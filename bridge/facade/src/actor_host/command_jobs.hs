import qualified Tidepool.Actor as Actor
import Tidepool.Inspection (print)
let command = Cmd.withEnvironment [("A", "new")] $ Cmd.withEnvironment [("A", "old"), ("B", "kept")] $ Cmd.withArguments ["a b;$HOME\n'quoted'"] $ withMemory (GiB 8) [bash|printf '%s' "$1"|]
job <- Cmd.start command
Cmd.detach job
data Completions mode = Completions { seen :: mode :- State Int, done :: mode :- Event Cmd.CommandResult, completionCount :: mode :- Call () (R.Reply Int) }
let collector = R.definition "command-completions" (Actor.Selected (knownEffects @'[Commands, Console])) Completions { seen = 0, done = R.on (Cmd.completion job) (\_ -> print ("completion observed" :: Text) >> modify' (+1)), completionCount = \() -> get }
listener <- R.start collector
Cmd.status job
