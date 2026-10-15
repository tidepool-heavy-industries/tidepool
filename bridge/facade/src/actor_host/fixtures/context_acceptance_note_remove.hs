do
  current <- C.getContext
  let notes = filter ((== "independent-note-roundtrip") . C.contextVisibleTextText)
        (toListOf C.referencedTexts current)
  case notes of
    [first, second]
      | C.contextVisibleTextReference first /= C.contextVisibleTextReference second
        && C.contextVisibleTextRemovable first && C.contextVisibleTextRemovable second ->
          case C.removeText (C.contextVisibleTextReference first) current of
            Left _ -> error "issued authored note refused removal"
            Right removed -> C.putContext removed
    _ -> error "authored notes lost their independent issued identities"
  pure True
