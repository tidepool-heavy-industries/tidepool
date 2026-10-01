let nestedGroup = "nested" :: ForkGroupLabel
let nestedLabel = [label|leaf|]
nested <- unfoldDeferred (subgroup nestedGroup) (child (researching @Text currentCheckout (assignment nestedLabel ())))
