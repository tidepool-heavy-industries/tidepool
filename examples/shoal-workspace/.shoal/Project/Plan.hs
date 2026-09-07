{-# LANGUAGE OverloadedStrings #-}

-- An authored application plan, not a scheduler. Edit these choices alongside
-- the Markdown tree; loading this module starts no work.
module Project.Plan
  ( GraphComponent (..)
  , componentTask
  , componentLane
  , componentLead
  , relationDesign
  ) where

import Data.Text (Text)
import qualified Data.Text as Text
import Data.Bifunctor (first)
import Tidepool.Actors.Shoal
import Tidepool.Effects.Core (GitRef (..))
import Shoal.Workspace (workspacePrompt)
import Project.Types
import Project.Work (taskContext)

data GraphComponent = RelationContract | RelationProjection | RelationControls
  deriving (Show, Eq)

componentName :: GraphComponent -> Text
componentName RelationContract = "contract"
componentName RelationProjection = "projection"
componentName RelationControls = "controls"

componentTask :: GraphComponent -> Task
componentTask component = Task
  (".shoal/plans/graph/" <> componentName component <> "/README.md")
  (case component of
    RelationContract -> "Land the shared graph relation contract and incorporate the tagged design decision."
    RelationProjection -> "Implement faithful, deterministic forests for the selected relation over exact actor identities."
    RelationControls -> "Expose relation selection and all three raw relationships without changing composer or authority behavior.")
  (case component of
    RelationContract -> "Buildable shared API; optional creator decodes old snapshots; pure relation selection works; no successful placeholder."
    RelationProjection -> "Every visible actor appears once; cycles, missing parents, reordering and incarnations preserve evidence; focused graph tests pass."
    RelationControls -> "Keyboard and mouse controls, narrow layouts, selection and focus remain coherent; no POST from graph interaction; focused UI checks pass.")

-- The baseline must include accepted prerequisites named by this component's
-- plan. Each lane publishes its own checked integration commit for the owner.
componentLane :: CampaignLabel -> GraphComponent -> GitRef -> Either NameError DeliveryLane
componentLane campaign component baseline = do
  implementation <- forkGroupLabel (componentName component <> "-implementation")
  review <- forkGroupLabel (componentName component <> "-review")
  integration <- forkGroupLabel (componentName component <> "-integration")
  implementer <- branchLabel "implement"
  reviewer <- branchLabel "review"
  integrator <- branchLabel "integrate"
  pure $ DeliveryLane (componentTask component)
    (batch campaign implementation) implementer (atRef baseline)
    (batch campaign review) reviewer
    (batch campaign integration) integrator (atRef baseline)

componentLead :: BranchLabel -> GitRef -> DeliveryLane -> Branch CodingEffects DeliveryLane Delivery
componentLead label baseline lane =
  withInstructions leadInstructions $ withContext (selected (taskContext . laneTask)) $
  withModel "gpt-5.6-sol" $ withEffort Low $ coding label (atRef baseline) lane
  where
    leadInstructions = case workspacePrompt "lead" of
      Just body -> body
      Nothing -> error "Missing configured project prompt: lead"

-- The only planned Astra execution placement in this application wave.
relationDesign :: CampaignLabel -> Either Text DesignSlot
relationDesign campaign = do
  group <- first (Text.pack . show) (forkGroupLabel "relation-design")
  label <- first (Text.pack . show) (branchLabel "forest-semantics")
  ready <- first (Text.pack . show) (watchLabel "relation-design-ready")
  pure $ DesignSlot ".shoal/plans/graph/contract/design.md"
    (batch campaign group) label ready "gpt-6-astra" Medium
