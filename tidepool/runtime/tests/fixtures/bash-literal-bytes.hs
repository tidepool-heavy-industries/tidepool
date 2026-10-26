literal <- pure $! if commandArgv (Q.describe [Q.bash|$VAR `printf ignored` $(ignored)
λ \ backslash "quoted"|]) == ["bash", "--noprofile", "--norc", "-c", "$VAR `printf ignored` $(ignored)\nλ \\ backslash \"quoted\"", "exomonad-bash"] then (42 :: Int) else error "literal Bash bytes changed"
