do
  current <- C.getContext
  C.putContext (over C.contextBlocks (<> [C.Text Nothing C.User "child-curated" []]) current)
  C.setNextModel "executor"
  if curatedHelper retainedValue == 43
    then pure ()
    else error "curated child did not inherit the final value 43"
