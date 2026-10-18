offlineEditorStopped <- stopAgent offlineEditor
offlineTesterStopped <- stopAgent offlineTester
let offlineStopped result = case result of
      StoppedNow -> True
      AlreadyStopped -> True
      _ -> False
if offlineStopped offlineEditorStopped && offlineStopped offlineTesterStopped
  then display True
  else display (offlineEditorStopped, offlineTesterStopped)
