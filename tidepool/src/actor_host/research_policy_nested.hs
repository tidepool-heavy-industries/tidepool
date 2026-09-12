let nestedGroup = "nested" :: ForkGroupLabel
let nestedLabel = "leaf" :: Label
nested <- unfold (subgroup nestedGroup) (child (researching @Text boundHead (assignment nestedLabel ())))
