page <- Cmd.output (Cmd.job normal)
display (Cmd.stdout normal == Right "result"
  && Cmd.stdout silent == Right "result"
  && Cmd.stdout again == Right "result"
  && Cmd.pageText page == "result")
