let Right campaign = campaignLabel "workspace-descendants"
let Right group = forkGroupLabel "child"
let Right label = branchLabel "grandchild"
grandchildWork <- unfold (batch campaign group) (child @Text (coding label boundHead ("fixture-grandchild" :: Text)))
