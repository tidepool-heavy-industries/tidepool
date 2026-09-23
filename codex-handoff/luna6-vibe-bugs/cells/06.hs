{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
fibSrc <- fmap (either (const "") id . Cmd.stdout) (Cmd.run (Cmd.argv ["cat", "fib.py"]))
notes <- fmap (either (const "") id . Cmd.stdout) (Cmd.run (Cmd.argv ["cat", "notes.md"]))
let files = [("fib.py", fibSrc), ("notes.md", notes)]
let packet =
      #per_file := J.each fst (\(name, text) ->
            #slow := J.noul ("Does " <> name <> " contain an algorithm with exponential running time? " <> text)
              :& #doc := J.noul ("Is " <> name <> " documentation rather than code? " <> text))
        files
answer <- J.ask (J.state (#task := ("Find performance problems in this repository" :: Text))) packet
fmap (\r -> [(name, a.slow.yes, a.doc.yes) | ((name, _), a) <- r.per_file]) answer
