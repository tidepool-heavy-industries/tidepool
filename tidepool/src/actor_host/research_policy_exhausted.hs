exhausted <- attemptUnfold (subgroup nestedGroup) (child (researching @Text nestedLabel boundHead ()))
case exhausted of { Left (UnfoldBeginRejected _) -> True; _ -> False }
