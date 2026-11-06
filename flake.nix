{
  description = "luneta — zellij beside a sidebar of its sessions and agents, and its daemon";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
      ...
    }:
    let
      # Where luneta runs: a Linux desktop, an arm64 Linux server and an Apple
      # Silicon Mac, each with a daemon of its own.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      inherit (cargoToml.workspace.package) version;

      # Everything the flake makes, for one system.
      build =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          # Pinned in rust-toolchain.toml, which rustup reads too, so that every build and
          # every run of clippy is of one rust, whatever the system has: a newer clippy
          # brings lints of its own, and fails a tree this one passes. Read from there
          # and not written again here, so that the two cannot name two rusts.
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          # Every crate is built through crane, on the toolchain above, because it
          # builds a crate's dependencies in a derivation of their own. That
          # derivation reads the manifests and Cargo.lock, with each source file of
          # ours swapped for an empty one, so a change to luneta's code leaves it as it
          # was: a cache hit, which CI restores from what main built instead of
          # compiling every crate in Cargo.lock again (#187).
          #
          # It writes TOML with remarshal, and not with crane-utils, a tool of its own
          # that it builds on nixpkgs' rust: a second rust, which with its llvm was a
          # third of what CI keeps of the store.
          craneLib = ((crane.mkLib pkgs).overrideToolchain toolchain).overrideScope (
            _: prev: { writeTOML = prev.writeTOMLViaRemarshal; }
          );

          # The source is filtered to what cargo reads, so editing the README or a tape
          # is a cache hit for the dependencies and for clippy rather than a rebuild.
          # Not for `lunetad`, which is stamped with when its commit was made, and so
          # builds and tests again on each commit, whatever it changed. luneta's skill
          # is in it for the ask box, which tells its agent the skill.
          cargoFiles = pkgs.lib.fileset.unions [
            ./.cargo/config.toml
            ./crates
            ./proto
            ./skills/luneta/SKILL.md
            ./Cargo.toml
            ./Cargo.lock
          ];

          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = cargoFiles;
          };

          # clippy reads clippy.toml too, which no build does. It has a source of its
          # own, so that editing it rebuilds no package.
          lintSrc = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              cargoFiles
              ./clippy.toml
            ];
          };

          testSrc = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              cargoFiles
              ./.config/nextest.toml
            ];
          };

          # What luneta's speech model is built with, but for what each system
          # brings: cmake, which builds transcribe.cpp and its ggml, and on Linux
          # ALSA, which the microphone is read through, found with pkg-config.
          speechBuild = [
            pkgs.cmake
            pkgs.pkg-config
          ];
          speechLibs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.alsa-lib ];

          # ggml builds for the CPU it is built on, with -march=native, which nix's
          # compiler drops, as what a derivation makes must not hang on the machine
          # that made it. It then builds for the first x86_64 CPUs, and the speech
          # model runs four times slower on a Ryzen than with the instructions of
          # x86-64-v3, which every x86_64 CPU of the last ten years has: those are
          # named instead. On arm64 the base it builds for has the vectors ggml
          # runs on, and on macOS transcribe.cpp names its own, and runs on Metal.
          ggmlCpu =
            {
              x86_64-linux = "-DGGML_NATIVE=OFF -DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DGGML_BMI2=ON";
            }
            .${system} or "-DGGML_NATIVE=OFF";

          buildTools = [
            toolchain
            pkgs.coreutils
            pkgs.gnumake
            pkgs.protobuf
            pkgs.stdenv.cc
          ]
          ++ speechBuild
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
            pkgs.gnused
            pkgs.stdenv.cc.bintools.bintools
          ];

          macBuildEnv = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
            export SDKROOT="${pkgs.apple-sdk.sdkroot}"
            export LIBRARY_PATH="${pkgs.lib.makeLibraryPath [ pkgs.libiconv ]}''${LIBRARY_PATH:+:$LIBRARY_PATH}"
          '';

          testRun = "-p lunetad -p luneta-protocol";

          # What the daemon's derivations share.
          daemon = {
            pname = "lunetad";
            inherit version src;
            strictDeps = true;

            cargoExtraArgs = "--locked -p lunetad";

            # The build scripts write the protocol out as Rust, through protoc, and
            # build the speech model's library.
            nativeBuildInputs = [ pkgs.protobuf ] ++ speechBuild;
            buildInputs = speechLibs;
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;

            # The daemon asks git how each work tree stands, and the tests make work
            # trees of their own for it to ask about. It asks `ps` which processes are
            # in front of the terminal a command runs on, and the tests ask it too.
            # And the tests ask ssh how it reads the options the daemon has each ssh
            # to a machine share one connection with. node runs luneta's extension for
            # pi, as pi would, for the daemon to read what it writes.
            nativeCheckInputs = [
              pkgs.cargo-nextest
              pkgs.git
              pkgs.nodejs
              pkgs.openssh
              pkgs.procps
            ];

            # On Linux the tests run in the sandbox as part of the build, so a Linux
            # whose daemon builds is one whose daemon passes them.
            #
            # Not on macOS. The tests run `/bin/ps` for when a process started, as
            # Claude Code does, and its build sandbox will not let them: a build there
            # with the tests on fails wherever the sandbox is on, as it is in CI. CI
            # runs them on a Mac outside the sandbox instead, through the devshell,
            # where they see the system as the daemon does when you run it.
            doCheck = !pkgs.stdenv.hostPlatform.isDarwin;
          };

          daemonDeps = craneLib.buildDepsOnly daemon;

          # The daemon and its CLI, `lunetad` and `luneta`, for the system this is.
          lunetad = craneLib.buildPackage (
            daemon
            // {
              src = testSrc;
              cargoArtifacts = daemonDeps;

              NEXTEST_SHOW_PROGRESS = "counter";
              checkPhaseCargoCommand = ''
                cargo nextest run --cargo-profile "$CARGO_PROFILE" --profile ci \
                  --test-threads "$NIX_BUILD_CORES" --locked ${testRun}
                cargoWithProfile test --doc --locked ${testRun}
              '';

              # fish's hook, where fish looks for a package's: NixOS and nix-darwin
              # link the folder of every system package into one that fish reads, so
              # that installing luneta hooks the shell, as the menu's `c f` and `c e`
              # need. A path of its own, outside `src`, so that editing it builds no
              # crate again.
              postInstall = ''
                install -Dm444 ${./shell/luneta.fish} $out/share/fish/vendor_conf.d/luneta.fish
              '';

              meta = {
                description = "The luneta daemon, and the CLI that talks to it";
                homepage = "https://github.com/lorenzolfm/luneta";
                license = pkgs.lib.licenses.mit;
                mainProgram = "lunetad";
                platforms = systems;
              };
            }
            # When the commit this builds was made, which luneta orders two builds of one
            # release by: every commit is of one release until the next, and a luneta
            # replaces only a daemon older than it. Here and not in `daemon`, which the
            # dependencies' derivation shares: a stamp there would build every crate in
            # Cargo.lock again on each commit.
            #
            # Only for a commit, which `self.rev` names. A tree with changes not yet
            # committed has its HEAD's time, and a path its newest file's, neither of
            # which says when what is built was made: built from either, it is unstamped,
            # as a cargo build of a checkout is.
            // pkgs.lib.optionalAttrs (self ? rev) {
              LUNETA_BUILT = toString self.lastModified;
            }
          );

          # The run of clippy, which `dev clippy` makes in the tree you are editing and
          # the check below makes in the sandbox, for CI. It is written down here once,
          # for both. `--all-targets`, as the tests are code too, which clippy.toml
          # lets unwrap.
          clippyRun = "--all-targets --workspace";

          # clippy in the sandbox, against the dependencies the daemon's build
          # compiled, so it compiles none of them again. That is a release build, where
          # `dev clippy` lints in dev: the profile changes nothing clippy reads but
          # `cfg(debug_assertions)`, and no code of ours is behind it.
          #
          # `-D warnings`, as every lint in Cargo.toml is set to `warn`, so without it
          # the run always passes and gates nothing.
          clippy = craneLib.cargoClippy (
            daemon
            // {
              src = lintSrc;
              cargoArtifacts = daemonDeps;
              cargoExtraArgs = "--locked";
              cargoClippyExtraArgs = "${clippyRun} -- -D warnings";
              # A check: nothing to test, and no `target/` that anything builds on.
              doCheck = false;
              doInstallCargoArtifacts = false;
            }
          );

          # The project CLI, and the only place the build recipes are written down. It
          # replaced a Makefile whose every target was .PHONY: there was no dependency
          # graph over files for make to walk — cargo does that — so what was left was a
          # task runner, and this one carries its own pinned tools.
          #
          # The recipes live in nix/dev/commands, one file for each command, as nix strings.
          # A `scripts/dev.sh` on disk is a second way to run them — one that bypasses the
          # pinned toolchain and builds with whatever rustc the caller has — so there is no
          # such file, and `nix develop` or `nix run .#dev` is the only way in.
          # writeShellApplication still runs shellcheck over each command at build time.
          dev = import ./nix/dev {
            inherit
              pkgs
              toolchain
              buildTools
              clippyRun
              testRun
              ;
            testTools = daemon.nativeCheckInputs;
            compileEnv = ''
              export TRANSCRIBE_CMAKE_ARGS="${ggmlCpu}"
              export PKG_CONFIG_PATH="${
                pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" speechLibs
              }''${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
              ${macBuildEnv}
            '';
          };

          nixFiles = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.fileFilter (file: file.hasExt "nix") ./.;
          };

          nixLint =
            tool: args:
            pkgs.runCommand "luneta-${tool.pname}" { } ''
              cd ${nixFiles}
              ${pkgs.lib.getExe tool} ${args}
              touch "$out"
            '';
        in
        {
          inherit dev lunetad;

          checks = {
            inherit clippy;
            statix = nixLint pkgs.statix "check .";
            deadnix = nixLint pkgs.deadnix "--hidden --fail .";
          };

          shell = pkgs.mkShell {
            packages = buildTools ++ [
              dev
              pkgs.cargo-nextest
              pkgs.nodejs
            ];
            buildInputs = speechLibs;
            TRANSCRIBE_CMAKE_ARGS = ggmlCpu;
            LUNETA_TEST_RUN = testRun;

            shellHook = ''
              echo "luneta — rust $(rustc --version | cut -d' ' -f2); run 'dev help' for the command set"
            '';
          };
        };

      bySystem = nixpkgs.lib.genAttrs systems build;
    in
    {
      devShells = nixpkgs.lib.mapAttrs (_: built: { default = built.shell; }) bySystem;

      # `dev` is exposed so that `nix run .#dev -- clippy` works without a devshell,
      # and so that CI builds it, which runs shellcheck over the recipes.
      packages = nixpkgs.lib.mapAttrs (_: built: {
        inherit (built) dev lunetad;
        default = built.lunetad;
      }) bySystem;

      # clippy over the workspace and the linters over the nix files, as CI runs them.
      checks = nixpkgs.lib.mapAttrs (_: built: built.checks) bySystem;
    };
}
