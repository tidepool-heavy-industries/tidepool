holder <- Cmd.start $ withMemory (MiB 512) [bash|
touch holder-started
while [ ! -e release-holder ]; do
  sleep .02
done
printf holder-done
|]
-- fixture-step
queued <- Cmd.start $ withMemory (MiB 512) [bash|printf admitted-after-release|]
Cmd.status queued
-- fixture-step
:{
do
  small <- Cmd.start [bash|printf protected-slot-usable|]
  result <- Cmd.await small
  output <- Cmd.tailOutput Cmd.Stdout small
  pure (inspectFull (result, output))
:}
-- fixture-step
:{
do
  holderResult <- Cmd.await holder
  queuedResult <- Cmd.await queued
  output <- Cmd.tailOutput Cmd.Stdout queued
  pure (inspectFull (holderResult, queuedResult, output))
:}
-- fixture-step
:{
do
  stdinJob <- Cmd.start $ Cmd.withArguments ["a b;$HOME\n'quoted'"] $ Cmd.withStdin [bash|printf 'arg:%s\n' "$1"; cat; printf input-closed >&2|]
  Cmd.sendInput stdinJob "a b;$HOME\n'quoted'"
  Cmd.closeInput stdinJob
  result <- Cmd.await stdinJob
  output <- Cmd.tailOutput Cmd.Stdout stdinJob
  pure (inspectFull (result, output))
:}
-- fixture-step
:{
do
  oomJob <- Cmd.start $ withMemory (MiB 64) [bash|python3 -c 'a=bytearray(128*1024*1024)'|]
  Cmd.await oomJob
:}
-- fixture-step
cancelJob <- Cmd.start [bash|touch cancel-started; exec sleep infinity|]
-- fixture-step
:{
do
  Cmd.cancel cancelJob
  Cmd.await cancelJob
:}
-- fixture-step
noisy <- Cmd.start [bash|python3 -c 'print("x"*600000); print("TAIL-MARKER")'|]
:{
do
  result <- Cmd.await noisy
  output <- Cmd.tailOutput Cmd.Stdout noisy
  first <- Cmd.readOutput Cmd.Stdout noisy
  next <- Cmd.nextPage first
  let advances = Cmd.outputStart (Cmd.pageDetails next) == Cmd.outputEnd (Cmd.pageDetails first)
  pure (inspectFull (if advances then "page-contiguous" else "bad-page-offset", result, output))
:}
-- fixture-step
import GHC.Generics (Generic)
import qualified Tidepool.Actor as Actor
data Completed mode = Completed { completedCount :: mode :- State Int, completedJob :: mode :- Event Cmd.CommandResult, readCompleted :: mode :- Call () (R.Reply Int) } deriving Generic
let collector = R.definition "completed-command" Actor.ReadOnly Completed { completedCount = 0, completedJob = R.on (Cmd.completion noisy) (\_ -> modify' (+1)), readCompleted = \() -> get }
:{
do
  listener <- R.start collector
  count <- R.call (readCompleted (R.client listener)) ()
  exit <- R.finish listener
  pure (if count == 1 then "completion-once" else "bad-completion-count", exit)
:}
-- fixture-step
terminal <- Cmd.start $ Cmd.withTerminal [bash|stty size > terminal-size; touch terminal-started; IFS= read -r line; printf 'terminal:%s\n' "$line"|]
-- fixture-step
:{
do
  Cmd.resize terminal 32 100
  Cmd.sendInput terminal "hello\n"
  result <- Cmd.await terminal
  output <- Cmd.tailOutput Cmd.Stdout terminal
  pure (inspectFull (result, output))
:}
