{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
let nextStep =
      J.choice "Given the task and the files, which next step best advances the task?"
        ( J.alt #profile "Time the program to confirm the slowness" ["python3", "-c", "import time,runpy; t=time.time(); runpy.run_path('fib.py'); print('seconds', round(time.time()-t,3))"]
            J..| J.alt #readDocs "Read the documentation for context" ["cat", "notes.md"]
            J..| J.alt #nothing "Nothing needs doing" ["true"] )
picked <- J.ask (J.state (#task := ("Confirm whether fib.py is slow" :: Text) :& #files := ("fib.py: naive recursive fib; notes.md: project blurb" :: Text))) (#next := nextStep)
case picked of
  Left e -> pure (T.pack (show e))
  Right a -> do
    let argv = J.handle a.next (#profile id J..| #readDocs id J..| #nothing id)
    out <- Cmd.quiet (Cmd.run (Cmd.argv argv))
    pure (T.unwords argv <> " => " <> either (const "") id (Cmd.stdout out))
