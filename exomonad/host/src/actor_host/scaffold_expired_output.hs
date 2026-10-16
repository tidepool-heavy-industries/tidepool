expired <- Project.Shell.sectionPage snap (Project.Shell.SectionId 1)
display (case expired of { Left (Project.Shell.SnapshotExpired Cmd.Stdout _) -> True; _ -> False })
