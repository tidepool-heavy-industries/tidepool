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
        # Overlay: rebuild GHC 9.12 with fat interface files for boot libraries.
        # -fwrite-if-simplified-core writes ALL Core (including workers, loop-breakers)
        # into mi_extra_decls in .hi files, bypassing unfolding heuristics entirely.
        # -fexpose-all-unfoldings + high threshold retained as secondary defense.
        # Targets: ghc-internal (stdlib impl) + ghc-bignum (Integer/Natural).
        # base is just re-exports; ghc-prim has no Haskell Core.
        ghcInternalOverlay = final: prev: {
          haskell = prev.haskell // {
            compiler = prev.haskell.compiler // {
              ghc912 = prev.haskell.compiler.ghc912.overrideAttrs (old: {
                postPatch = (old.postPatch or "") + ''
                  TIDEPOOL_GHC_OPTS="-fexpose-all-unfoldings -funfolding-creation-threshold=100000 -fwrite-if-simplified-core"

                  # Patch ghc-internal and ghc-bignum .cabal/.cabal.in files
                  for f in libraries/ghc-internal/ghc-internal.cabal.in libraries/ghc-internal/ghc-internal.cabal \
                           libraries/ghc-bignum/ghc-bignum.cabal.in libraries/ghc-bignum/ghc-bignum.cabal; do
                    if [ -f "$f" ]; then
                      sed -i -e "/^library/a \\    ghc-options: $TIDEPOOL_GHC_OPTS" "$f"
                      echo "tidepool: patched $f with fat interface flags"
                    fi
                  done

                  # Also inject OPTIONS_GHC pragmas into every .hs source file in case
                  # Hadrian ignores .cabal ghc-options for boot libraries
                  for dir in libraries/ghc-internal/src libraries/ghc-bignum/src; do
                    if [ -d "$dir" ]; then
                      find "$dir" -name '*.hs' -exec sed -i "1s/^/{-# OPTIONS_GHC $TIDEPOOL_GHC_OPTS #-}\n/" {} +
                      echo "tidepool: injected OPTIONS_GHC into all .hs files in $dir"
                    fi
                  done
                '';
              });
            };
            # Rebuild the package set against the patched compiler
            packages = prev.haskell.packages // {
              ghc912 = prev.haskell.packages.ghc912.override {
                ghc = final.haskell.compiler.ghc912;
              };
            };
          };
        };

        overlays = [ (import rust-overlay) ghcInternalOverlay ];
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
