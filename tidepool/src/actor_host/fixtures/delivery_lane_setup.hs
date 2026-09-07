let task = Task "plans/current/feature.md" "Deliver the feature" "Preserve the open product gate"
let Right campaign = campaignLabel "delivery-package"
let Right leadWave = forkGroupLabel "lead"
let Right leadLabel = branchLabel "feature-lead"
let Just leadPrompt = workspacePrompt "lead"
lead <- unfold (batch campaign leadWave) (child (withInstructions leadPrompt (solTask @Delivery leadLabel projectHead task)))
