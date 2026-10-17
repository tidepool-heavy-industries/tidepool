do
  C.ContextStaged _ <- C.stageContext (C.contextPatch [C.appendNote "child-curated" []])
  C.setNextModel (C.CustomModel C.OpenAIPlan "executor")
  if curatedHelper retainedValue == 43
    then pure ()
    else error "curated child did not inherit the final value 43"
