# Defining compiled tools

Use this when a project needs a direct tool backed by Haskell. The same record
DSL declares raw-text endpoints and structured JSON inputs. Handlers run in the
actor's normal effect stack; runtime authority still controls resources.

```haskell
{-# LANGUAGE DeriveGeneric, FlexibleContexts, OverloadedStrings, TypeOperators #-}
module Project.Tools where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell

data ProjectTools mode = ProjectTools
  { script :: mode :- RawCall Text
  , execute :: mode :- Call Shell.Execute Text
  , continueProcess :: mode :- Call Shell.WriteInput Text
  , diagnostics :: mode :- Call Shell.ReadOutput Text
  } deriving Generic

tools :: Member Cmd.Commands effects => ProjectTools (AsServerT (Eff effects))
tools = ProjectTools
  { script = rawTool "Run literal Bash." $ \text ->
      Cmd.run (Cmd.bashCommand text) >> pure ""
  , execute = tool "Execute with command options." Shell.execute
  , continueProcess = tool "Send input or poll a session." Shell.writeInput
  , diagnostics = tool "Read retained output." Shell.readRetained
  }
```

Select the record in the next workspace package:

```toml
[haskell]
source_roots = ["."]
modules = ["Project.Tools"]
tools = "Project.Tools.tools"
```

The shared `haskell` tool remains available. Field names become snake_case tool
names; `RawCall` receives literal Text, while `Call` derives its input schema
from the same types used for decoding. Returned Text displays literally.
Schemas and handler code freeze at actor startup; calls apply retained compiled
code to input data. Changes require a new package/run, not a live file edit.

`Shell.execute` composes `Cmd.start` and bounded `Cmd.observe`, allowing the
handler to continue even when the command is still running. The resulting
session identifier supports direct input and output tools. `Cmd.run` preserves
its foreground-overrun semantics instead; use it when that handoff is intended.
No tool name needs a Rust dispatcher case.
