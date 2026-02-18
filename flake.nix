{
  description = "tidepool - compile freer-simple effect stacks into Cranelift-backed state machines";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rust
            pkgs.haskell.compiler.ghc912
            pkgs.cabal-install
            pkgs.pkg-config
          ];

          shellHook = ''
            echo "tidepool dev shell"
            echo "  Rust: $(rustc --version)"
            echo "  GHC:  $(ghc --version)"
          '';
        };

        packages.tidepool-extract = let
          # freer-simple 1.2.1.2 needs a patch for GHC 9.12: MonadBase instance
          # requires explicit Applicative+Monad constraints due to superclass changes.
          hsPkgs = pkgs.haskell.packages.ghc912.override {
            overrides = self': super': {
              freer-simple = (pkgs.haskell.lib.unmarkBroken (
                pkgs.haskell.lib.doJailbreak super'.freer-simple
              )).overrideAttrs (old: {
                postPatch = (old.postPatch or "") + ''
                  sed -i 's/instance (MonadBase b m, LastMember m effs) => MonadBase b (Eff effs)/instance (MonadBase b m, LastMember m effs, Applicative b, Monad b) => MonadBase b (Eff effs)/' src/Control/Monad/Freer/Internal.hs
                '';
              });
            };
          };
          ghcEnv = hsPkgs.ghcWithPackages (ps: with ps; [
            freer-simple
          ]);
          harness = hsPkgs.callCabal2nix "tidepool-harness" ./haskell {};
        in pkgs.writeShellScriptBin "tidepool-extract" ''
          export PATH="${ghcEnv}/bin:$PATH"
          exec ${harness}/bin/tidepool-harness "$@"
        '';

        packages.default = self.packages.${system}.tidepool-extract;
      }
    );
}
