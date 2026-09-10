let Right campaign = campaignLabel "workspace-acceptance"
let Right group = forkGroupLabel "root"
let Right label = branchLabel "child"
childWork <- unfold (batch campaign group) (child @Text (coding label projectHead ("fixture-child" :: Text)))
