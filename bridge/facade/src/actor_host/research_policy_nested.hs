let nestedGroup = "nested" :: ForkGroupLabel
let nestedLabel = [label|leaf|]
nested <- unfold (subgroup nestedGroup) (child (researching @Text currentCheckout (assignment nestedLabel ())))
