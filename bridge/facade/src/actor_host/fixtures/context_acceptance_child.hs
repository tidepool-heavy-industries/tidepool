do
  current <- C.getContext
  C.putContext (over C.editableTexts (ContextText.replace "parent-curated" "child-curated") current)
  C.setNextModel "executor"
  pure (retainedHelper retainedValue)
