let Right campaign = campaignLabel "final-handoffs"
let Right wave = forkGroupLabel "lanes"
let Right leftLabel = branchLabel "left"
let Right rightLabel = branchLabel "right"
let task = Task (batch campaign wave) "plans/component.md" sourceHead "Deliver the component" "Exercise typed subtree handoff" ["component source"] "read final source" []
(left, leftProgress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @Delivery (coding leftLabel projectHead task))
