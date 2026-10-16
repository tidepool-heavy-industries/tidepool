{-# LANGUAGE DuplicateRecordFields #-}
{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}
module Persona.Spec (agentSpec, helperSpec, PersonaEffects, HelperEffects) where
import GHC.Generics (Generic)
import Control.Monad.Freer (Members)
import Tidepool.Aeson (FromJSON)
import Tidepool.Agent.Contract
import Tidepool.Records (FsError)
import qualified Tidepool.File as File
import qualified Persona.FileTools as Files
import qualified Tidepool.Lookup.Tools as LookupTools
import Tidepool.Effects.Core
  ( AskUser, Console, FsRead, FsWrite, Jev, Lookup, Reflect, Source, Web
  , Actor, ActorContext, AgentLaunch, AgentInspection
  , AgentControl, Notifications, ResourceScopes, BoundWorktree )
import Tidepool.Agent.Reply (Replies)
import Tidepool.Agent.Watch (Watches)
import qualified Persona.Orientation as Orientation

-- These base rows are delegable. Sync supplies invocation-only context editing
-- and reload; child grants retain only the effects listed here.
type PersonaEffects =
  '[AskUser, Console, FsRead, FsWrite, Jev, Lookup, Reflect, Source, Web
   , Actor, ActorContext, AgentLaunch, AgentInspection
   , AgentControl, Notifications, ResourceScopes, BoundWorktree, Replies, Watches]
-- Helpers can explore, compose and curate their own context. Findings return
-- to the companion, which owns human-facing forms and durable memory edits.
type HelperEffects =
  '[Console, FsRead, Jev, Lookup, Reflect, Source, Web
   , Actor, ActorContext, AgentLaunch, AgentInspection, AgentControl
   , Notifications, ResourceScopes, BoundWorktree, Replies, Watches]
-- Hosted functions use an object input. The operation retains typed
-- create/replace intent inside that object.
data WriteFileInput = WriteFileInput { operation :: Files.WriteInput }
  deriving (Generic)
instance FromJSON WriteFileInput
instance JsonSchema WriteFileInput

data PersonaTools mode = PersonaTools
  { read_file :: mode :- Sync (Call Files.ReadInput (Either Files.FileToolError Files.ReadReceipt))
  , write_file :: mode :- Sync (Call WriteFileInput (Either FsError File.CommitOutcome))
  , edit_file :: mode :- Sync (Call Files.EditInput (Either Files.FileToolError File.CommitOutcome))
  , preview_edit :: mode :- Sync (Call Files.EditInput (Either Files.FileToolError Files.PreviewReceipt))
  , lookup :: LookupTools.LookupTools mode
  , haskell :: mode :- Sync (HaskellCell PersonaEffects)
  , reload :: mode :- Sync (Call ReloadInput (Either ReloadError ReloadOutcome))
  }
  deriving (Generic)
-- Try adding an afterTool hook here to notice something useful after a call.
-- A hook receives completed ToolCall and ToolResult values; compile, schema and
-- other preexecution refusals do not invoke it. Annotation: Annotated
-- adds a note beside the result; NoAnnotation leaves it alone. With this spec,
-- a notebook call is the whole cell, rather than each helper invoked inside it.
-- A Jev/Reflect-using hook needs those effects in this signature's constraints.
-- Save the implementation in a module, connect it here, then reload. You can
-- add or replace the hook without adding a new registered tool.
agentSpec :: (Subset PersonaEffects (SyncEffects effects), Members '[FsRead, FsWrite, Lookup, Console] effects)
  => AgentSpec PersonaTools effects
agentSpec = defaultSpec { specTools = PersonaTools
  { read_file = presentWith presentJson $ syncTool
      "Read a workspace file and its version. Optional expectedVersion detects drift; range uses inclusive 1-based lines."
      Files.readFileTool
  , write_file = presentWith presentJson $ syncTool
      "Create a new file, or replace an earlier-read file reference. Stale versions refuse publication."
      (\(WriteFileInput intent) -> Files.writeFileTool intent)
  , edit_file = presentWith presentJson $ syncTool
      "Apply exact text replacements to an earlier-read file reference. Each oldText must occur exactly once; stale versions refuse publication."
      Files.editFileTool
  , preview_edit = presentWith presentJson $ syncTool
      "Preview exact replacements without writing. Use the file reference returned by read_file."
      Files.previewEditTool
  , lookup = LookupTools.tools
  , haskell = haskellTool "Use the persistent Haskell notebook to recall and save memory, search the web, use forms, and compose reusable functions, Jev judgments, actors and workflows. Use the installed file tools for direct source reads and edits, including when a notebook cell will not compile. Inspect active source separately from drafts; explicitly validate and reload edits before relying on them. Bind evidence before displaying selections."
  , reload = standardReload }
  , initialContext = Just Orientation.startup }

data HelperTools mode = HelperTools
  { haskell :: mode :- Sync (HaskellCell HelperEffects) }
  deriving (Generic)
helperSpec :: AgentSpec HelperTools HelperEffects
helperSpec = defaultSpec { specTools = HelperTools
  { haskell = haskellTool "Compose an investigation in Haskell. Retain evidence, distinguish uncertainty, and return findings through the typed request. Curate your own context as useful; the companion owns durable memory edits and human-facing forms." } }
