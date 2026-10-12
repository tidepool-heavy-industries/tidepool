{
  description = "Haskell this workspace compiles but does not carry: jev-dsl, pinned.";

  inputs.jev-dsl = {
    url = "github:inanna-malick/jev-dsl/74ecea515617906d1a126289709ae9fc4c32cbc4";
    flake = false;
  };

  # Nothing is built from here. `[haskell.flake_sources]` in `.exomonad/config.toml`
  # names the directories inside the input that hold modules, and Exomonad captures
  # them into the run as ordinary source roots.
  outputs = { ... }: { };
}
