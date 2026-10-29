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
          # is a cache hit rather than a rebuild. luneta's skill is in it for the ask
          # box, which tells its agent the skill.
          cargoFiles = pkgs.lib.fileset.unions [
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

          # What the daemon's derivations share.
          daemon = {
            pname = "lunetad";
            inherit version src;
            strictDeps = true;

            cargoExtraArgs = "--locked -p lunetad";

            # The build scripts write the protocol out as Rust, through protoc.
            nativeBuildInputs = [ pkgs.protobuf ];

            # The daemon asks git how each work tree stands, and the tests make work
            # trees of their own for it to ask about. It asks `ps` which processes are
            # in front of the terminal a command runs on, and the tests ask it too.
            nativeCheckInputs = [
              pkgs.git
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
              cargoArtifacts = daemonDeps;

              # When the commit this builds was made, which luneta orders two builds of one
              # release by: every commit is of one release until the next, and a luneta
              # replaces only a daemon older than it. Here and not in `daemon`, which the
              # dependencies' derivation shares: a stamp there would build every crate in
              # Cargo.lock again on each commit.
              LUNETA_BUILT = toString self.lastModified;

              # The protocol's crate too: what the daemon reads of git is read there,
              # and tested there.
              cargoTestExtraArgs = "-p luneta-protocol";

              meta = {
                description = "The luneta daemon, and the CLI that talks to it";
                homepage = "https://github.com/lorenzolfm/luneta";
                license = pkgs.lib.licenses.mit;
                mainProgram = "lunetad";
                platforms = systems;
              };
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
          # The recipes live in this file and nowhere else. A `scripts/dev.sh` on disk is
          # a second way to run them — one that bypasses the pinned toolchain below and
          # builds with whatever rustc the caller has — so there is no such file, and
          # `nix develop` or `nix run .#dev` is the only way in. writeShellApplication
          # still runs shellcheck over this text at build time.
          dev = pkgs.writeShellApplication {
            name = "dev";
            meta.description = "luneta build recipes on the flake-pinned toolchain";
            # protoc is for the build scripts of the protocol's crate and of the
            # daemon, which `dev clippy` and `dev skills` run.
            runtimeInputs = [
              toolchain
              pkgs.coreutils
              pkgs.git
              pkgs.protobuf
            ];
            text = ''
              # luneta build recipes, as one command.
              #
              # Lives in the devshell and behind `nix run .#dev`, and nowhere else. The rust
              # toolchain comes from runtimeInputs, so every subcommand builds with the version
              # this flake pins whether or not the caller entered the shell first; see the
              # toolchain comment above.
              #
              # writeShellApplication supplies the shebang and `set -euo pipefail`, and lints
              # this text with shellcheck at build time.

              usage() {
                cat <<'HELP'
              dev — luneta build recipes (pinned nix tooling)

                dev clippy [args...]       cargo clippy over every target, warnings denied
                dev clean                  cargo clean
                dev skills [dir]           check the menu's skill finder against what claude runs in dir
              HELP
              }

              cmd="''${1:-help}"
              shift || true

              case "$cmd" in
                help | -h | --help)
                  usage
                  exit 0
                  ;;
                clippy | clean | skills) ;;
                *)
                  # Explicit failure, not a silent fall-through to help: `dev buidl` in a script
                  # or a CI step must not exit 0.
                  printf 'dev: unknown command %s\n\n' "$cmd" >&2
                  usage >&2
                  exit 2
                  ;;
              esac

              if ! root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
                echo "dev: run this inside the luneta checkout (no git worktree here)" >&2
                exit 2
              fi
              # Where `dev skills` looks when it is given no directory: where you ran it.
              here="$PWD"
              cd "$root"

              case "$cmd" in
                # The lint set lives in Cargo.toml and clippy.toml, so this is only the
                # invocation: `clippyRun` in flake.nix, which the check CI builds makes too.
                # `-D warnings`, as every lint in Cargo.toml is set to `warn`, so without it
                # the command always exits 0 and gates nothing.
                #
                # Anything after `dev clippy` goes to cargo, ahead of the `--`, so
                # `dev clippy --fix --allow-dirty` applies the machine-applicable ones.
                clippy)
                  cargo clippy --locked ${clippyRun} "$@" -- -D warnings
                  ;;

                clean)
                  cargo clean
                  ;;

                # The menu's search finds skills where Claude Code looks for them, which
                # Claude Code can change in any release. This asks Claude Code itself, by the
                # `init` a headless start prints on a model that is none, and fails with the
                # names only one side has. claude comes from the ambient PATH, and not from
                # the flake: the check is against the claude you run.
                skills)
                  dir="$(cd "''${1:-$here}" && pwd -P)"
                  LUNETA_SKILLS_DIR="$dir" cargo test --locked -p lunetad --bin luneta \
                    skills::tests::the_finder_finds_what_claude_code_runs -- --ignored --exact
                  ;;
              esac
            '';
          };
        in
        {
          inherit dev lunetad;

          checks = { inherit clippy; };

          shell = pkgs.mkShell {
            # protoc for the daemon's build script, as `cargo build -p lunetad` runs it.
            packages = [
              toolchain
              dev
              pkgs.protobuf
            ];

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

      # clippy over the workspace, as CI runs it.
      checks = nixpkgs.lib.mapAttrs (_: built: built.checks) bySystem;
    };
}
