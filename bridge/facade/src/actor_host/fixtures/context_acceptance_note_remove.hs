do
  current <- C.getContext
  let notes = filter ((== "independent-note-roundtrip") . C.contextVisibleTextText)
        (toListOf C.referencedTexts current)
  case notes of
    [first, second]
      | C.contextVisibleTextReference first /= C.contextVisibleTextReference second
        && C.contextVisibleTextRemovable first && C.contextVisibleTextRemovable second ->
          case filter ((== C.contextVisibleTextReference first) . C.unitReference) (C.unitTargets current) of
            [target] -> do
              C.ContextStaged _ <- C.stageContext (C.contextPatch [C.removeUnit target])
              pure ()
            _ -> error "issued authored note refused removal"
    _ -> error "authored notes lost their independent issued identities"
  pure True
