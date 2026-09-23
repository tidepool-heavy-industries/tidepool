let wideGroup = "too-wide" :: ForkGroupLabel
let firstLabel = [label|first|]
let secondLabel = [label|second|]
widthResult <- attemptUnfold (subgroup wideGroup) ((,) <$> child (researching @Text boundHead (assignment firstLabel ())) <*> child (researching @Text boundHead (assignment secondLabel ())))
case widthResult of { Left _ -> True; Right _ -> False }
