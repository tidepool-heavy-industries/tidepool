page <- Cmd.output (Cmd.job normal)
if Cmd.stdout normal == Right "result"
  && Cmd.stdout silent == Right "result"
  && Cmd.stdout again == Right "result"
  && Cmd.pageText page == "result"
  then pure ()
  else error "retained command values mismatch"
