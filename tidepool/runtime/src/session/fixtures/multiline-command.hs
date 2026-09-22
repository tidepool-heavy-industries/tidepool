let command = [bash|
set -eu
cat <<'END'
:info notHaskell
:{

  literal $HOME `date` λ
:}
END
|]
