let nestedGroup = case forkGroupLabel "nested" of { Right value -> value; Left _ -> error "fixture label" }
let nestedLabel = case branchLabel "leaf" of { Right value -> value; Left _ -> error "fixture label" }
nested <- unfold (subgroup nestedGroup) (child (researching @Text nestedLabel boundHead ()))
