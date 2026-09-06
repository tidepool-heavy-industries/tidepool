let wideGroup = case forkGroupLabel "too-wide" of { Right value -> value; Left _ -> error "fixture label" }
let firstLabel = case branchLabel "first" of { Right value -> value; Left _ -> error "fixture label" }
let secondLabel = case branchLabel "second" of { Right value -> value; Left _ -> error "fixture label" }
widthResult <- attemptUnfold (subgroup wideGroup) ((,) <$> child (researching @Text firstLabel boundHead ()) <*> child (researching @Text secondLabel boundHead ()))
case widthResult of { Left _ -> True; Right _ -> False }
