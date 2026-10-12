{
  description = "Haskell this workspace compiles but does not carry: jev-dsl, pinned.";

  inputs.jev-dsl = {
    url = "github:inanna-malick/jev-dsl/38e8fe361b3437363e8cef7b684d4a9af7a526ef";
    flake = false;
  };

  # Nothing is built from here. `[haskell.flake_sources]` in `.exomonad/config.toml`
  # names the directories inside the input that hold modules, and Exomonad captures
  # them into the run as ordinary source roots.
  outputs = { ... }: { };
}
