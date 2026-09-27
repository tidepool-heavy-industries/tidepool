{
  description = "Pinned jev-dsl source and Node environment for web verification.";

  inputs.jev-dsl = {
    url = "github:inanna-malick/jev-dsl/f16f1363b4d389d6e34f9d695fbd254ca0735f2e";
    flake = false;
  };

  # Match Tidepool's pinned nixpkgs revision so web verification does not
  # depend on whichever Node installation happens to be on the host.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/bfc1b8a4574108ceef22f02bafcf6611380c100d";

  # `[haskell.flake_sources]` in `.exomonad/config.toml` names the directories
  # inside jev-dsl that Exomonad captures as ordinary source roots.
  outputs = { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in {
      devShells = forSystems (pkgs: {
        web = pkgs.mkShell { packages = [ pkgs.nodejs_24 ]; };
      });
    };
}
