import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
let command = Cmd.withEnvironment [("A", "new")] $ Cmd.withEnvironment [("A", "old"), ("B", "kept")] $ Cmd.withArguments ["a b;$HOME\n'quoted'"] $ withMemory (GiB 8) [bash|printf '%s' "$1"|]
job <- Cmd.start command
data Completions mode = Completions { seen :: mode :- State Int, done :: mode :- Event Cmd.CommandResult, completionCount :: mode :- Call () (R.Reply Int) } deriving Generic
let collector = R.definition "command-completions" Actor.ReadOnly Completions { seen = 0, done = R.on (Cmd.completion job) (\_ -> modify' (+1)), completionCount = \() -> get }
listener <- R.start collector
Cmd.status job
