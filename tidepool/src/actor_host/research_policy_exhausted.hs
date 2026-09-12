exhausted <- attemptUnfold (subgroup nestedGroup) (child (researching @Text boundHead (assignment nestedLabel ())))
case exhausted of { Left (UnfoldBeginRejected _) -> True; _ -> False }
