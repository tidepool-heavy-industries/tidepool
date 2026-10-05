instance Display Original.Input where
  displayWith budget value =
    let text = Text.pack (if Original.project value == 42 then "later display 42" else "wrong later input")
    in (Text.take budget text, Text.length text > budget)

instance WorkbenchDisplay Original.Input where
  workbenchDisplay = displayWith 65536
  workbenchActivationDisplay = displayWith
