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
      url = "github:inanna-malick/jev-dsl/2883fdc38cc7a64572e76ea43bd38e1df3a5e28b";
      flake = false;
    };
    # Browser assets must come from the exact harness source used by Cargo.
    harnessWeb = {
      url = "github:tidepool-heavy-industries/exomonad-harness/59b342b96a921f4d9b0e044fe8ef61f6fc62bc71";
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
        # ghc-internal owns most stdlib implementation, but base also defines
        # executable functions. Every selected boot implementation needs Core.
        # Hadrian package names, not source directories: CompileHs includes
        # generated Haskell modules and hsc2hs outputs in these libraries.
        nativeBootPackages = [
          "ghc-internal"
          "ghc-bignum"
          "ghc-prim"
          "base"
          "containers"
          "bytestring"
          "array"
          "deepseq"
          "directory"
          "filepath"
          "process"
          "unix"
          "parsec"
          "mtl"
          "transformers"
          "stm"
          "template-haskell"
          "binary"
          "exceptions"
          "time"
          "hpc"
          "Cabal"
          "Cabal-syntax"
          "text"
        ];
        nativeBootOptions = [
          "-fexpose-all-unfoldings"
          "-funfolding-creation-threshold=100000"
          "-fwrite-if-simplified-core"
        ];
        nativeBootSettings = map (
          package: "*.${package}.ghc.hs.opts+=${builtins.concatStringsSep " " nativeBootOptions}"
        ) nativeBootPackages;
        nativeBootArgvCheck = lib: ''
          for expected in ${lib.escapeShellArgs nativeBootSettings}; do
            count=0
            for actual in "''${hadrianFlagsArray[@]}"; do
              if [ "$actual" = "$expected" ]; then count=$((count + 1)); fi
            done
            if [ "$count" -ne 1 ]; then
              echo "native boot setting must be one whole argument exactly once: $expected" >&2
              exit 1
            fi
          done
        '';
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
                  patches = (old.patches or [ ]) ++ [
                    ./nix/ghc-make-cache-filter.patch
                    ./nix/ghc-stg-external-scope.patch
                  ];
                  # Append complete settings as individual argv elements. Nixpkgs
                  # initializes this array in preConfigure and quotes it at Hadrian
                  # invocation. Source OPTIONS_GHC still supplies module-specific flags.
                  preConfigure = (old.preConfigure or "") + ''
                    hadrianFlagsArray+=( ${prev.lib.escapeShellArgs nativeBootSettings} )
                  '' + nativeBootArgvCheck prev.lib;
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
          hash = "sha256-gXYGez5cJIcLl6KoAiG3bH1wrEmyf+kax+uoMtj+99g=";
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
            tidepool-resume = self'.callCabal2nix "tidepool-resume" ./bridge/haskell/resume { };
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
            tidepool-resume
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
          version = "59b342b96a921f4d9b0e044fe8ef61f6fc62bc71";
          src = "${harnessWeb}/web";
          nodejs = harnessPkgs.nodejs_24;
          npmDeps = embeddedWebNpmCache;
          installPhase = ''
            runHook preInstall
            mkdir -p "$out/share/exomonad/web"
            cp -R dist/. "$out/share/exomonad/web/"
            runHook postInstall
          '';
          passthru.sourceRevision = "59b342b96a921f4d9b0e044fe8ef61f6fc62bc71";
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
            pkgs.ripgrep
            pkgs.findutils
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
          exomonad-runtime-search-tools = pkgs.runCommand "exomonad-runtime-search-tools-check" { } ''
            export PATH=${self.packages.${system}.buck-exomonad-runtime-tools}/bin
            mkdir -p fixture/nested
            printf 'packaged-search-witness\n' > fixture/nested/input.txt
            test "$(rg --files fixture)" = fixture/nested/input.txt
            test "$(rg --fixed-strings --line-number packaged-search-witness fixture)" = fixture/nested/input.txt:1:packaged-search-witness
            test "$(find fixture -type f -name input.txt)" = fixture/nested/input.txt
            touch "$out"
          '';

          # Hadrian validates package keys against its own package registry.
          # This check exercises only argv transport; preConfigure applies the
          # same guard to the actual inherited array before Hadrian starts.
          ghc-native-boot-settings = pkgs.runCommand "ghc-native-boot-settings-check" { } (''
            hadrianFlagsArray=( ${pkgs.lib.escapeShellArgs nativeBootSettings} )
          '' + nativeBootArgvCheck pkgs.lib + ''
            if (
              hadrianFlagsArray=( ${pkgs.lib.escapeShellArgs (builtins.tail nativeBootSettings)} )
              ${nativeBootArgvCheck pkgs.lib}
            ); then echo "missing setting accepted" >&2; exit 1; fi
            if (
              hadrianFlagsArray+=( ${pkgs.lib.escapeShellArg (builtins.head nativeBootSettings)} )
              ${nativeBootArgvCheck pkgs.lib}
            ); then echo "duplicate setting accepted" >&2; exit 1; fi
            if (
              read -ra splitSetting <<< ${pkgs.lib.escapeShellArg (builtins.head nativeBootSettings)}
              hadrianFlagsArray=( "''${splitSetting[@]}" ${pkgs.lib.escapeShellArgs (builtins.tail nativeBootSettings)} )
              ${nativeBootArgvCheck pkgs.lib}
            ); then echo "split setting accepted" >&2; exit 1; fi
            touch "$out"
          '');

          embedded-web-provenance = pkgs.runCommand "embedded-web-provenance" { } ''
            ${pkgs.python3}/bin/python3 ${./scripts/embedded_web_provenance.py} --self-test --check --root ${./.}
            test -s ${embeddedWebAssets}/share/exomonad/web/index.html
            touch "$out"
          '';
        };
      }
    );
}
