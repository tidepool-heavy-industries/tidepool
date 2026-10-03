data NotebookPlain = NotebookPlain Int (Int -> Int)

display (NotebookPlain 3 (+ 1))
