offlineEditorStopped <- stopAgent offlineEditor
offlineTesterStopped <- stopAgent offlineTester
let offlineStopped result = case result of
      StoppedNow -> True
      AlreadyStopped -> True
      _ -> False
display (offlineStopped offlineEditorStopped && offlineStopped offlineTesterStopped)
