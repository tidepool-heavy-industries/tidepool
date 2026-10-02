do
  current <- C.getContext
  C.putContext (over C.contextBlocks (<> [C.Text Nothing C.User "child-curated" []]) current)
  C.setNextModel "executor"
  pure (curatedHelper retainedValue)
