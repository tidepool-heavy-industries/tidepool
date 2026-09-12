let campaign = "final-handoffs" :: CampaignLabel
let wave = "lanes" :: ForkGroupLabel
let leftLabel = "left" :: BranchLabel
let rightLabel = "right" :: BranchLabel
let task = Task (batch campaign wave) "plans/component.md" sourceHead "Deliver the component" "Exercise typed subtree handoff" ["component source"] "read final source" []
(left, leftProgress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @Delivery (coding leftLabel projectHead task))
