import qualified Tidepool.Actor as Actor
import qualified Tidepool.Effects.Core as Core
import qualified Tidepool.Effects.Row as Row
import GHC.Generics (Generic)
data PersistentCounter mode = PersistentCounter { counterState :: mode :- State Int, advanceCounter :: mode :- Call Int (R.Reply Int), runHandlerCommand :: mode :- Call () (R.Reply Text) } deriving Generic
let counterDefinition = R.definition "invocation-persistent-counter" (Actor.Selected (Row.knownEffects @'[Core.Commands])) PersistentCounter { counterState = 7, advanceCounter = \delta -> do { R.modify' (+ delta); R.get }, runHandlerCommand = \() -> do { finished <- Cmd.quiet (Cmd.run (Cmd.argv ["printf", "record-handler-command"])); case Cmd.stdout finished of { Right output -> pure output; Left issue -> error ("handler command failed: " <> T.pack (show issue)) } } }
counterService <- R.start counterDefinition
read (drop (length "ActorHandle ") (show counterService)) :: (Int, Int)
