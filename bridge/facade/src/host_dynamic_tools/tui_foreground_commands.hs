launchRead <- Cmd.run [bash|cat .agents/skills/shoal-command/SKILL.md; cat plans/parallel-dogfood/next-wave/{resume.md,README.md}; cat plans/parallel-dogfood/planner.md|]
-- fixture-step
foregroundResult <- Cmd.run [bash|
printf 'once\n' >> foreground-start-count
printf 'foreground-started\n'
while [ ! -e release-foreground ]; do
  sleep .02
done
printf 'foreground-finished\n'
|]
Cmd.run [bash|touch forbidden-suffix|]
