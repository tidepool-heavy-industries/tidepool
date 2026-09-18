---
name: shoal-agent-spec
description: Use when you want your own hosted tools, or code that runs after every tool call to annotate or prune its result, and when editing and reloading them inside a live session.
---

Your agent spec is one Haskell module. It names the tools you are offered and
the after-tool slot applied to each finished tool call. It is ordinary source:
edit it with file tools, then call `reload_agent_spec`. Nothing reloads on save.

## Where it lives

The module is `AgentSpec` and the value is `agentSpec`. It is the first
`AgentSpec.hs` in your own source roots, in the order your cells resolve
modules: your checkout's `.shoal/` roots first, then the run's. A workspace may
name another entry with `[haskell] spec`; `[haskell] tools` still names a bare
tools record. `status` with `view: "detailed"` says which rule matched, which
file was read, and which revision is installed.

## Shape

Your tools record is one field per tool — and one field may be another tools
record, whose tools are spliced in at that position under their own names:

```haskell
{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Project.Tools (MyTools (..), Probe (..), tools) where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import GHC.Generics (Generic)
import Tidepool.Aeson.FromJSON (FromJSON)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Tidepool.Command.Tools as Shell

newtype Probe = Probe { topic :: Text }
  deriving (Generic, FromJSON, JsonSchema)

data MyTools mode = MyTools
  { shell :: Shell.ShellTools mode
  , probe :: mode :- Call Probe Text
  }
  deriving (Generic)

tools :: Member Cmd.Commands effects => MyTools (AsServerT (Eff effects))
tools = MyTools
  { shell = Shell.tools
  , probe = tool "Answer one fixed question about a topic." (\_ -> pure "one")
  }
```

That declares `bash`, `exec_command`, `write_stdin`, `read_output`,
`cancel_command`, `probe`, in that order; the `shell` field name is not a tool
name of its own. Naming your own record REPLACES the shell record rather than
adding to it, so a record that does not nest `Shell.ShellTools` leaves you with
no `bash` and no `exec_command` at your next incarnation — nest it unless you
mean to give them up.

And the spec that installs it:

```haskell
{-# LANGUAGE OverloadedStrings #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import Tidepool.Agent.Contract
import qualified Tidepool.Command as Cmd
import qualified Project.Tools as Tools

agentSpec :: Member Cmd.Commands effects => AgentSpec Tools.MyTools effects
agentSpec = defaultSpec
  { specTools = Tools.tools
  , afterTool = Just noted
  }

noted :: ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call /= "probe" = pure NoAnnotation
  | otherwise = pure (Annotated "asked about this topic twice before")
```

Always build from `defaultSpec` with a record update, so a slot added later
leaves your spec compiling. `specTools` is the same tools record as before: one
field per tool (or per nested record), `mode :- Call input output`, the field
name is the tool name, and schemas derive from the types.

## What the slot may answer

| Answer | The model is shown |
|---|---|
| `NoAnnotation` | the result, unchanged |
| `Abstained reason` | the result, unchanged; the reason appears only in `status` |
| `Annotated text` | the result, then `text` marked as derived context |
| `Pruned text (toolResultHandle result)` | `text` marked as a selection; the whole result is bound as that handle, a `Text`, for a later cell |

The slot runs in your own resident machine with your own effects, so it may
read recent turns (`reflect n`, topic `reflect`), ask Jev (`shoal-jev`), or run
a command. Name the effects it uses in its signature, for example
`(Member Reflect effects, Member Jev effects) => ToolCall -> ToolResult -> Eff effects Annotation`,
and give `agentSpec` the same constraints. The slot is shown the result as the
model would see it, already bounded for display, so a command's complete output
is reached through its retained job, not through the slot's input. The result waits for it, up to
five minutes. If it fails or runs out of time the result is delivered unchanged
with one line naming `after-tool#N`; `status` has the rest. A slot's own tool
use never triggers the slot. `lookup`, `status`, `reload_agent_spec` and
authored cells are never shown to it, so a broken slot cannot block its repair.

## Asking Jev from a tool or the slot

A module is not a cell, and three things a cell gives you for free have to be
written out:

- `import qualified Jev.Operators as J`. Only cells get `J` from the workbench.
- `{-# LANGUAGE OverloadedLabels #-}` for `#yes`, and `OverloadedRecordDot` if
  you read answers as `a.key`.
- `Member Jev effects` on every signature that asks Jev or wraps something that
  does: the tool body, `tools`, the slot, and `agentSpec`. `Jev` comes from
  `Tidepool.Effects.Core`. The same goes for `Reflect`, `Cmd.Commands` and any
  other effect a body uses.

This is the slot a hosted test compiles and runs, changed only to abstain when
Jev is unavailable:

```haskell
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
module AgentSpec (agentSpec) where

import Control.Monad.Freer (Eff, Member)
import qualified Data.Text as T
import Tidepool.Aeson.Value (Value (String))
import Tidepool.Agent.Contract
import Tidepool.Effects.Core (Jev)
import qualified Jev.Operators as J
import qualified Project.Tools as Tools

agentSpec :: Member Jev effects => AgentSpec Tools.SpecTools effects
agentSpec =
  defaultSpec
    { specTools = Tools.tools
    , afterTool = Just noted
    }

noted :: Member Jev effects => ToolCall -> ToolResult -> Eff effects Annotation
noted call result
  | toolCallName call /= T.pack "probe" = pure NoAnnotation
  | otherwise = do
      answer <-
        J.ask1
          (J.rawState (String (toolResultOutput result)))
          ( J.choice
              "Does this tool output look complete?"
              ( J.alt #yes "The output looks complete" ()
                  J..| J.alt #no "The output looks incomplete" ()
              )
          )
      pure $ case answer of
        Left _ -> Abstained (T.pack "jev unavailable")
        Right a ->
          Annotated
            ( J.handle
                a
                ( #yes (\_ -> T.pack "looks complete")
                    J..| #no (\_ -> T.pack "looks incomplete")
                )
            )
```

A tool body asks the same way, with the same constraint. When Jev cannot
answer, abstain: a slot that fails is reported to you on every call, and one
that abstains is not.

## Reload

`reload_agent_spec` publishes your source layer, recompiles the spec, and swaps
the implementations between calls. The receipt says where it stopped:

- **layer rejected**: a module did not typecheck. Your files are untouched and
  the previous spec keeps answering.
- **spec did not compile**: the layer is published, so cells can import the new
  modules while you repair the spec.
- **refused**: the rebuilt spec declares a different tool name, description,
  schema, kind or order. The difference is listed. Tool bodies and the slot may
  change freely; a changed surface takes effect at your next incarnation.
- **swapped**: later calls run the new code. A call already accepted keeps the
  implementation it started with.

Reload is yours alone. It never changes a child's or a parent's spec.
