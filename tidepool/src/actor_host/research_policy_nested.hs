let nestedGroup = "nested" :: ForkGroupLabel
let nestedLabel = "leaf" :: BranchLabel
nested <- unfold (subgroup nestedGroup) (child (researching @Text nestedLabel boundHead ()))
