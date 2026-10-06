{
  description = "tidepool - compile freer-simple effect stacks into Cranelift-backed state machines";

  nixConfig = {
    extra-substituters = [ "https://tidepool.cachix.org" ];
    extra-trusted-public-keys = [
      "tidepool.cachix.org-1:jnYeaWymP+9/MeAECROfi4+/l7X1ilkOqM5Nrr5Lo1w="
    ];
  };

  inputs = {
    # Include the pinned shared workspace sources in recursive and remote builds.
    self.submodules = true;
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Haskell this workspace compiles but does not carry: jev-dsl, pinned to
    # the same revision `exomonad/examples/workspace/flake.nix` pins. Nothing is
    # built from it here; `[haskell.flake_sources]` in `.exomonad/config.toml`
    # names the directory inside it that holds modules, and Exomonad captures
    # them into a run as ordinary source roots.
    jev-dsl = {
      url = "github:inanna-malick/jev-dsl/f16f1363b4d389d6e34f9d695fbd254ca0735f2e";
      flake = false;
    };
    # Browser assets must come from the exact harness source used by Cargo.
    harnessWeb = {
      url = "github:tidepool-heavy-industries/exomonad-harness/2aa685129aa4da4b3ec637abcf17666f087b93c8";
      flake = false;
    };
    # Match the harness web verification shell's pinned Node 24 package.
    harnessNixpkgs.url = "github:NixOS/nixpkgs/bfc1b8a4574108ceef22f02bafcf6611380c100d";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      jev-dsl,
      harnessWeb,
      harnessNixpkgs,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        # Overlay: rebuild GHC 9.12 with fat interface files for boot libraries.
        # -fwrite-if-simplified-core writes ALL Core (including workers, loop-breakers)
        # into mi_extra_decls in .hi files, bypassing unfolding heuristics entirely.
        # -fexpose-all-unfoldings + high threshold retained as secondary defense.
        # Targets: ghc-internal (stdlib impl) + ghc-bignum (Integer/Natural).
        # base is just re-exports; ghc-prim has no Haskell Core.
        ghcInternalOverlay =
          final: prev:
          let
            patchedGhc =
              (prev.haskell.compiler.ghc912.override {
                # Native (pure-Haskell) ghc-bignum backend: Integer/Natural ops desugar
                # to pure Core over Word#/ByteArray# primops (no __gmpn_*/integer_gmp_*
                # FFI), which the JIT compiles directly — correct by construction.
                enableNativeBignum = true;
              }).overrideAttrs
                (old: {
                  postPatch = (old.postPatch or "") + ''
                    TIDEPOOL_GHC_OPTS="-fexpose-all-unfoldings -funfolding-creation-threshold=100000 -fwrite-if-simplified-core"

                    # Inject fat interface flags via OPTIONS_GHC into boot libraries.
                    # ghc-internal, ghc-bignum, ghc-prim: safe to prepend (no exotic extensions).
                    for dir in libraries/ghc-internal/src libraries/ghc-bignum/src libraries/ghc-prim; do
                      if [ -d "$dir" ]; then
                        find "$dir" -name '*.hs' -exec sed -i "1s/^/{-# OPTIONS_GHC $TIDEPOOL_GHC_OPTS #-}\n/" {} +
                        echo "tidepool: injected OPTIONS_GHC into $dir"
                      fi
                    done

                    # For ALL other boot libraries (containers, bytestring, array, text, etc.),
                    # inject OPTIONS_GHC AFTER existing pragmas by appending before the module line.
                    # This avoids breaking files that start with {-# LANGUAGE MagicHash #-} etc.
                    for lib in libraries/containers libraries/bytestring libraries/array \
                               libraries/deepseq libraries/directory libraries/filepath \
                               libraries/process libraries/unix libraries/parsec \
                               libraries/mtl libraries/transformers libraries/stm \
                               libraries/template-haskell libraries/binary \
                               libraries/exceptions libraries/time libraries/hpc \
                               libraries/Cabal libraries/Cabal-syntax libraries/text; do
                      if [ -d "$lib" ]; then
                        find "$lib" -name '*.hs' -exec sed -i '/^module /i {-# OPTIONS_GHC '"$TIDEPOOL_GHC_OPTS"' #-}' {} +
                        echo "tidepool: injected OPTIONS_GHC before module decl in $lib"
                      fi
                    done
                  '';
                });
          in
          {
            haskell = prev.haskell // {
              compiler = prev.haskell.compiler // {
                ghc912 = patchedGhc;
              };
              # Wire patched GHC into the package set so ALL Haskell deps
              # (freer-simple, etc.) are rebuilt from source against the new boot lib ABIs.
              # Both `ghc` and `buildHaskellPackages` must point to patchedGhc to avoid
              # mixing artifacts from the old ABI universe (causes "dependency doesn't exist").
              packages = prev.haskell.packages // {
                ghc912 = prev.haskell.packages.ghc912.override (old: {
                  ghc = patchedGhc;
                  buildHaskellPackages = old.buildHaskellPackages.override (_: {
                    ghc = patchedGhc;
                  });
                });
              };
            };
          };

        overlays = [
          (import rust-overlay)
          ghcInternalOverlay
        ];
        pkgs = import nixpkgs { inherit system overlays; };
        harnessPkgs = import harnessNixpkgs { inherit system; };
        embeddedWebNpmCache = harnessPkgs.fetchNpmDeps {
          src = "${harnessWeb}/web";
          hash = "sha256-R1WPUQzu8+knK7B5mSx7i6sneSBLgKmX7HDCSsWwekI=";
        };
        browserTestNpmCache = harnessPkgs.fetchNpmDeps {
          src = ./nix/browser-test;
          hash = "sha256-OHK/Hwr1gq5XxODSXX+zmRM8v0WQqJkkJQ82zzvoJAM=";
        };
        matchedHarnessSource =
          pkgs.runCommand "tidepool-matched-harness-source"
            {
              nativeBuildInputs = [ pkgs.coreutils ];
            }
            ''
              mkdir -p "$out"
              cp -a "${harnessWeb}/." "$out/"
            '';
        playwrightChromium = harnessPkgs.playwright-driver.browsers.override {
          withFirefox = false;
          withWebkit = false;
          withChromiumHeadlessShell = true;
          withFfmpeg = false;
        };
        # rust-toolchain.toml is the single source of truth for the Rust
        # version + components; the flake reads it rather than pinning
        # `stable.latest` (which drifts silently on every flake.lock update).
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        # Buck2 and Reindeer are pinned orchestration tools. The Rust/GHC/C
        # toolchains below remain the declared inputs to native actions.
        buck2Release =
          pkgs.runCommand "buck2-snapshot-20260926-200119"
            {
              nativeBuildInputs = [
                pkgs.zstd
                pkgs.autoPatchelfHook
              ];
              buildInputs = [
                pkgs.stdenv.cc.cc.lib
                pkgs.openssl
                pkgs.zlib
              ];
              source = pkgs.fetchurl {
                url = "https://github.com/thoughtpolice/buck2/releases/download/snapshot-20260926-200119/buck2-x86_64-unknown-linux-gnu.zst";
                hash = "sha256-hCos2M7wxjrYKXaQdCouhaWvoK6XM5urBtKJTm2tkfQ=";
              };
            }
            ''
              mkdir -p "$out/bin"
              zstd -d -c "$source" > "$out/bin/buck2"
              chmod +x "$out/bin/buck2"
              autoPatchelf "$out"
            '';
        buckReindeer =
          pkgs.runCommand "reindeer-2026.09.14.00"
            {
              nativeBuildInputs = [ pkgs.zstd ];
              source = pkgs.fetchurl {
                url = "https://github.com/facebookincubator/reindeer/releases/download/v2026.09.14.00/reindeer-x86_64-unknown-linux-musl.zst";
                hash = "sha256-YWqPwwLD2yuJ5yKGz3pTlpkRY7CI8BWHyb+c/css2qw=";
              };
            }
            ''
              mkdir -p "$out/bin"
              zstd -d -c "$source" > "$out/bin/reindeer"
              chmod +x "$out/bin/reindeer"
            '';
        # One Haskell package universe for development and native worker actions.
        # The worker loads Tidepool modules at runtime, so a bare compiler is
        # insufficient even when it can compile the worker executable itself.
        hsPkgs = pkgs.haskell.packages.ghc912.override {
          overrides = self': super': {
            mkDerivation =
              args:
              super'.mkDerivation (
                args
                // {
                  configureFlags = (args.configureFlags or [ ]) ++ [
                    "--ghc-options=-fwrite-if-simplified-core"
                    "--ghc-options=-fexpose-all-unfoldings"
                  ];
                }
              );
            freer-simple =
              (pkgs.haskell.lib.unmarkBroken (pkgs.haskell.lib.doJailbreak super'.freer-simple)).overrideAttrs
                (old: {
                  postPatch = (old.postPatch or "") + ''
                    sed -i 's/instance (MonadBase b m, LastMember m effs) => MonadBase b (Eff effs)/instance (MonadBase b m, LastMember m effs, Applicative b, Monad b) => MonadBase b (Eff effs)/' src/Control/Monad/Freer/Internal.hs
                  '';
                });
          };
        };
        ghcPackages =
          ps: with ps; [
            freer-simple
            lens
            errors
            cryptohash-sha256
            cborg
            syb
            witherable
            safe
            random
            splitmix
          ];
        ghcEnv = hsPkgs.ghcWithPackages ghcPackages;
        # Test providers stay outside the deployed worker closure.
        ghcTestEnv = hsPkgs.ghcWithPackages (ps:
          ghcPackages ps ++ [ ps.tasty ps.tasty-hunit ps.tasty-quickcheck ps.QuickCheck ]
        );
        # One package selection owns both the executable test surface and the
        # closure-info resources used by existing native process consumers.
        testToolPackages = [ pkgs.git pkgs.bash pkgs.coreutils ]
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.util-linux ];
        testTools = pkgs.buildEnv {
          name = "buck-test-tools";
          pathsToLink = [ "/bin" ];
          paths = testToolPackages;
        };
        embeddedWebAssets = harnessPkgs.buildNpmPackage {
          pname = "exomonad-harness-web";
          version = "2aa685129aa4da4b3ec637abcf17666f087b93c8";
          src = "${harnessWeb}/web";
          nodejs = harnessPkgs.nodejs_24;
          npmDeps = embeddedWebNpmCache;
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/share/exomonad/web"
            cp -R dist/. "$out/share/exomonad/web/"
            runHook postInstall
          '';
          passthru.sourceRevision = "2aa685129aa4da4b3ec637abcf17666f087b93c8";
        };
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            pkgs.pkg-config
            buck2Release
            buckReindeer
          ];
          buildInputs = [
            rust
            ghcTestEnv
            pkgs.cabal-install
            pkgs.openssl
            pkgs.jq
            pkgs.just
            pkgs.bubblewrap
            # No sccache here, deliberately. It IS active for every build on
            # this box, but via `build.rustc-wrapper` in ~/.cargo/config.toml
            # (host-global, outside this flake), naming an absolute store path
            # — which is already an exact pin, tighter than a version string.
            # This shell used to add `pkgs.sccache` alongside it; under the
            # current flake.lock that resolves to 0.14.0 while the live wrapper
            # is 0.16.0, so it put a DIFFERENT sccache client on PATH than the
            # one doing the caching. Every agent worktree on this box shares
            # one sccache server; a second client version reaching it is at
            # best redundant.
          ];

          shellHook = ''
            export TIDEPOOL_GHC_LIBDIR="$(ghc --print-libdir)"
            echo "tidepool dev shell" >&2
            echo "  Rust: $(rustc --version)" >&2
            echo "  GHC:  $(ghc --version)" >&2
            echo "  sccache (rustc-wrapper, from ~/.cargo/config.toml): $(sccache --version 2>/dev/null || echo 'not on PATH')" >&2
          '';
        };

        # Keep the resident development shell independent from provider clients.
        devShells.exomonad = pkgs.mkShell {
          inputsFrom = [ self.devShells.${system}.default ];
          packages = [
            pkgs.git
            pkgs.tmux
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.systemd ];
          EXOMONAD_NIX_STORE_BIN = "${pkgs.nix}/bin/nix-store";
          EXOMONAD_EMBEDDED_ASSET_ROOT = "${embeddedWebAssets}/share/exomonad/web";
          # Fetches the project's flake inputs when `[haskell.flake_sources]`
          # pins Haskell source outside the workspace.
          EXOMONAD_NIX_BIN = "${pkgs.nix}/bin/nix";
          shellHook = ''
            export TIDEPOOL_GHC_LIBDIR="$(ghc --print-libdir)"
            echo "exomonad dev shell" >&2
          '';
        };

        packages.exomonad-embedded-assets = embeddedWebAssets;

        packages.buck-rust = rust;
        packages.buck-buck2 = buck2Release;
        packages.buck-ghc = ghcEnv;
        packages.buck-test-ghc = ghcTestEnv;
        packages.buck-jev-sources = pkgs.runCommand "buck-jev-sources" { } ''
          mkdir -p "$out"
          cp -a ${jev-dsl}/core "$out/core"
        '';
        packages.buck-haskell-test-closure = pkgs.closureInfo {
          rootPaths = [ ghcTestEnv ghcEnv ];
        };
        packages.buck-cc = pkgs.stdenv.cc;
        packages.buck-binutils = pkgs.binutils;
        packages.buck-node = pkgs.nodejs_24;
        packages.buck-npm-cache = embeddedWebNpmCache;
        packages.buck-browser-node = harnessPkgs.nodejs_24;
        packages.buck-browser-npm-cache = browserTestNpmCache;
        packages.buck-playwright-browsers = playwrightChromium;
        packages.buck-matched-harness-source = matchedHarnessSource;
        packages.buck-bash = pkgs.bash;
        packages.buck-test-git = pkgs.git;
        packages.buck-coreutils = pkgs.coreutils;
        packages.buck-tar = pkgs.gnutar;
        packages.buck-gzip = pkgs.gzip;
        packages.buck-python = pkgs.python3;
        packages.buck-bubblewrap = pkgs.bubblewrap;
        packages.buck-cmake = pkgs.cmake;
        packages.buck-perl = pkgs.perl;
        packages.buck-pkg-config = pkgs.pkg-config;
        packages.buck-openssl = pkgs.openssl;
        packages.buck-reindeer = buckReindeer;
        packages.buck-toolchain-closure = pkgs.closureInfo {
          rootPaths = [
            rust
            ghcEnv
            pkgs.stdenv.cc
            pkgs.binutils
            pkgs.nodejs_24
            embeddedWebNpmCache
            pkgs.bash
            pkgs.coreutils
            pkgs.gnutar
            pkgs.gzip
            pkgs.python3
            pkgs.bubblewrap
            pkgs.cmake
            pkgs.perl
            pkgs.pkg-config
            pkgs.openssl
            pkgs.git
          ];
        };
        packages.buck-exomonad-runtime-tools = pkgs.buildEnv {
          name = "exomonad-runtime-tools";
          pathsToLink = [ "/bin" ];
          paths = [
            pkgs.bash
            pkgs.coreutils
            pkgs.git
            pkgs.bubblewrap
            pkgs.tmux
            pkgs.nix
            pkgs.python3
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.systemd pkgs.util-linux ];
        };
        packages.buck-test-tools = testTools;
        packages.buck-test-tools-closure = pkgs.closureInfo {
          rootPaths = testToolPackages ++ [ testTools ];
        };
        packages.buck-browser-test-closure = pkgs.closureInfo {
          rootPaths = [
            harnessPkgs.nodejs_24
            browserTestNpmCache
            playwrightChromium
            matchedHarnessSource
            pkgs.git
          ];
        };

        formatter = pkgs.nixfmt;

        checks = {
          # Fail-loud replacement for the ghcInternalOverlay postPatch's silent
          # `if [ -d "$dir" ]` / `if [ -d "$lib" ]` guards (flake.nix, above):
          # if GHC ever renames/moves one of these boot-library directories,
          # the patch would otherwise skip injecting the fat-interface flags
          # for it with no error. This check lives OUTSIDE the patchedGhc
          # derivation on purpose — it builds over the plain GHC *source*
          # tarball (cheap fetch, no compile), so it can assert loudly without
          # touching postPatch/mkDerivation and changing their derivation
          # hashes (that would trigger a from-source GHC rebuild).
          #
          # The directory list is duplicated rather than shared with
          # postPatch's `for dir in ...` / `for lib in ...` lines: sharing it
          # would mean editing that string, which is exactly the hash-changing
          # edit this check exists to avoid. Keep both lists in sync by hand;
          # each side comments at the other.
          #
          # `knownMissing`: the patch's second loop names `libraries/Cabal-syntax`,
          # but in the GHC 9.12.2 tree that directory lives at
          # `libraries/Cabal/Cabal-syntax` (nested under Cabal, not a sibling
          # under libraries/), so the postPatch `-d` guard silently skips it —
          # Cabal-syntax never gets the fat-interface OPTIONS_GHC. Correcting
          # the path in postPatch changes the patched-GHC derivation hash, so
          # it isn't done here; it rides the next deliberate lock bump (which
          # rebuilds GHC anyway) instead of costing a from-source rebuild today.
          # One-line fix for that future bump:
          #   for lib in ... libraries/Cabal libraries/Cabal/Cabal-syntax libraries/text
          # (replace the bare `libraries/Cabal-syntax` entry with the nested path).
          #
          # The assertion below is self-retiring: it fails if Cabal-syntax ever
          # starts existing at the top-level name the patch actually uses — that
          # would mean the patch entry has gone live again and this exception
          # (and the one-line fix above) should be deleted.
          ghc-boot-library-dirs = pkgs.runCommand "ghc-boot-library-dirs-check" { } ''
            src=${pkgs.haskell.compiler.ghc912.src}

            # Mirrors the two `for` loops in ghcInternalOverlay's postPatch above,
            # minus the one entry tracked in knownMissing below.
            expected="libraries/ghc-internal/src libraries/ghc-bignum/src libraries/ghc-prim \
                  libraries/containers libraries/bytestring libraries/array \
                  libraries/deepseq libraries/directory libraries/filepath \
                  libraries/process libraries/unix libraries/parsec \
                  libraries/mtl libraries/transformers libraries/stm \
                  libraries/template-haskell libraries/binary \
                  libraries/exceptions libraries/time libraries/hpc \
                  libraries/Cabal libraries/text"

            missing=""
            for d in $expected; do
              if [ ! -d "$src/$d" ]; then
                missing="$missing $d"
              fi
            done

            if [ -n "$missing" ]; then
              echo "ghcInternalOverlay's postPatch loops over these boot-library" >&2
              echo "directories, but they no longer exist in $src:" >&2
              echo "$missing" >&2
              exit 1
            fi

            # knownMissing: libraries/Cabal-syntax (patch's path) -> real path
            # libraries/Cabal/Cabal-syntax. Assert the real path still exists
            # (the gap is real, not stale) and the patch's path still does NOT
            # exist (if it now does, the exception above is out of date).
            if [ ! -d "$src/libraries/Cabal/Cabal-syntax" ]; then
              echo "knownMissing entry libraries/Cabal-syntax (real path" >&2
              echo "libraries/Cabal/Cabal-syntax) is stale: the real path no" >&2
              echo "longer exists either. Update the known-gap entry." >&2
              exit 1
            fi
            if [ -d "$src/libraries/Cabal-syntax" ]; then
              echo "libraries/Cabal-syntax now exists at the top level -" >&2
              echo "the postPatch loop's entry for it is live again. Delete" >&2
              echo "the knownMissing exception for it in this check." >&2
              exit 1
            fi

            touch $out
          '';

          embedded-web-provenance = pkgs.runCommand "embedded-web-provenance" { } ''
            ${pkgs.python3}/bin/python3 ${./scripts/embedded_web_provenance.py} --self-test --check --root ${./.}
            test -s ${embeddedWebAssets}/share/exomonad/web/index.html
            touch "$out"
          '';
        };
      }
    );
}
