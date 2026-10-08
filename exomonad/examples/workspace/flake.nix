{
  description = "Haskell this workspace compiles but does not carry: jev-dsl, pinned.";

  inputs.jev-dsl = {
    url = "github:inanna-malick/jev-dsl/2883fdc38cc7a64572e76ea43bd38e1df3a5e28b";
    flake = false;
  };

  # Nothing is built from here. `[haskell.flake_sources]` in `.exomonad/config.toml`
  # names the directories inside the input that hold modules, and Exomonad captures
  # them into the run as ordinary source roots.
  outputs = { ... }: { };
}
