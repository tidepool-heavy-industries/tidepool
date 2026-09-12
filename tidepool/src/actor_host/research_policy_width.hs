let wideGroup = "too-wide" :: ForkGroupLabel
let firstLabel = "first" :: BranchLabel
let secondLabel = "second" :: BranchLabel
widthResult <- attemptUnfold (subgroup wideGroup) ((,) <$> child (researching @Text firstLabel boundHead ()) <*> child (researching @Text secondLabel boundHead ()))
case widthResult of { Left _ -> True; Right _ -> False }
