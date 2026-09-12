{-# LANGUAGE OverloadedStrings #-}

-- The application allocation is editable Haskell. It describes meaningful work,
-- not a required actor for every noun or a pipeline executed during import.
module Project.Plan
  ( GraphComponent (..), component, componentLead, componentLeadFrom, relationDesign
  ) where

import Data.Text (Text)
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (GitOid)
import Project.Types
import Project.Work (projectPrompt, solTaskFrom)

data GraphComponent = RelationContract | RelationProjection | RelationControls
  deriving (Show, Eq)

componentName :: GraphComponent -> Text
componentName RelationContract = "contract"
componentName RelationProjection = "projection"
componentName RelationControls = "controls"

component :: CampaignLabel -> GraphComponent -> GitOid -> Either NameError Task
component campaign part source = do
  group <- forkGroupLabel (componentName part)
  pure $ Task (batch campaign group)
    (".shoal/plans/graph/" <> componentName part <> "/README.md") source
    (case part of
      RelationContract -> "Land the shared graph relation contract and incorporate the tagged design decision."
      RelationProjection -> "Implement faithful, deterministic forests for the selected relation over exact actor identities."
      RelationControls -> "Expose relation selection and all three raw relationships without changing composer or authority behavior.")
    (case part of
      RelationContract -> "Creation, supervision and context are different evidence. Resolve their common contract before parallel consumers rely on it."
      RelationProjection -> "Display edges must not fabricate authority or lose actors. One deterministic projection should serve all relation views."
      RelationControls -> "The operator needs to understand relationships while retaining the existing editor, selection and submission guarantees.")
    (case part of
      RelationContract -> ["src/graph_wire.rs", "src/agents.rs: shared types and parent selection", "fixture constructors", ".shoal/plans/graph/contract"]
      RelationProjection -> ["src/agents.rs: graph projection and traversal", "focused pure graph tests"]
      RelationControls -> ["src/ui/agents.rs", "view state, inspector and canvas consumers", "UI and interaction tests", "README controls"])
    (case part of
      RelationContract -> "Buildable shared API; omitted creator remains unknown; pure parent selection; compile consumers; document accepted signatures before consumer forks."
      RelationProjection -> "Every supplied actor appears once; stable under reorder; cycles, missing parents and distinct incarnations preserve evidence; focused graph tests pass."
      RelationControls -> "Keyboard/mouse and narrow layouts work; selection/focus remain coherent; graph interaction never POSTs Haskell or changes the composer; terminal proof and focused UI tests.")
    []

-- The lead implements useful work itself and commissions independent review.
-- withLifetime remains an ordinary caller choice when admitting this branch.
componentLead :: Label -> Task -> Branch CodingEffects Task Delivery
componentLead label = componentLeadFrom label boundHead

componentLeadFrom :: Label -> WorktreeSeed -> Task -> Branch CodingEffects Task Delivery
componentLeadFrom label source task = withEffort Medium $ withInstructions (projectPrompt "lead") $
  solTaskFrom label source task

relationDesign :: CampaignLabel -> DesignSlot
relationDesign campaign = DesignSlot ".shoal/plans/graph/contract/design.md"
  (batch campaign "relation-design") "forest-semantics" "relation-design-ready"
  "planner" Medium
