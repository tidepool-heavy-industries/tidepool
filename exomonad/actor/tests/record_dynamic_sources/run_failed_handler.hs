do
  let { watcherSpec = R.definition "dynamic-source-failing-watcher" (Actor.Selected (knownEffects @'[Actor]))
      (Watcher
        0
        (\target -> do
          own <- R.self @Watcher
          R.attach (watcherEvent own) (R.lifecycle target))
        (\() -> R.get)
        (\() -> P.error "intentional handler failure")
        (R.on mempty (\_ -> R.modify' (+ 1))))
    ; replacementSpec = R.definition "dynamic-source-repaired-watcher" (Actor.Selected (knownEffects @'[Actor]))
      (Watcher
        0
        (\_ -> pure (Right ()))
        (\() -> R.get)
        (\() -> pure ())
        (R.on mempty (\_ -> R.modify' (+ 10))))
    ; tools current = Tools
      { runCase = finishTool "Repair a failed handler while retaining its attached source." $ \_ -> do
          watcher <- R.start watcherSpec
          attached <- R.call (watcherBegin (R.client watcher)) watcher
          before <- R.call (watcherCount (R.client watcher)) ()
          R.send (watcherFail (R.client watcher)) ()
          successor <- R.replace watcher replacementSpec
          observed <- R.call (watcherCount (R.client successor)) ()
          finished <- R.finish successor
          pure (CaseOutput (attached == Right () && before == 1 && observed == 21 && finished == Actor.Completed 21)
            (T.pack (show attached)) observed, current)
      }
    }
  serveToolsWith 0 tools
