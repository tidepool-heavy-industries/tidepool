do
  let slurp p = do
        r <- Cmd.run (Cmd.argv ["cat", p])
        pure (either (const "") id (Cmd.stdout r))
  rawA <- slurp "/home/inanna/.claude/jobs/4940a626/tmp/fix/a.out"
  rawB <- slurp "/home/inanna/.claude/jobs/4940a626/tmp/fix/b.out"
  pure (object [ "a_bytes" .= T.length rawA, "a_lines" .= length (T.lines rawA)
               , "b_bytes" .= T.length rawB, "b_lines" .= length (T.lines rawB) ])
