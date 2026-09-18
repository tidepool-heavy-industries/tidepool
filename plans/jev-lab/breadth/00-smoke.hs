do
  let slurp p = do
        r <- Cmd.run (Cmd.argv ["cat", p])
        pure (either (const "") id (Cmd.stdout r))
  let fx n = "/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/" <> n
  chk <- slurp (fx "53ad43c-check.out")
  a46 <- slurp (fx "assign-46.txt")
  tasks <- slurp (fx "TASKS.md")
  pure (object [ "check_lines" .= length (T.lines chk)
         , "assign46_lines" .= length (T.lines a46)
         , "tasks_lines" .= length (T.lines tasks) ])
