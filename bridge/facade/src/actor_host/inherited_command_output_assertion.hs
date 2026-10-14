page <- Cmd.output job
if Cmd.pageText page == "first-line\nsecond-line\n"
  then pure ()
  else error "inherited command output mismatch"
