{
  description = "luneta — a personal zellij session picker plugin, and its daemon";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      # Where luneta runs: a Linux desktop, an arm64 Linux server and an Apple
      # Silicon Mac, each with a daemon of its own. The plugin is wasm, and runs
      # on each; each system builds it too, so that each has a devshell. A
      # release is the x86_64-linux build: another system's works the same but
      # is not the same bytes, as the paths it is built in are written into it.
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
          # Pinned to the channel that the rust-toolchain.toml of zellij names, with the
          # wasm target added. The system rustc cannot build wasm32-wasip1, because nix
          # ships std for the host triple only and there is no rustup to add a target.
          toolchain = pkgs.rust-bin.stable."1.95.0".default.override {
            targets = [ "wasm32-wasip1" ];
          };

          # The source is filtered to what cargo reads, so editing the README or a tape
          # is a cache hit rather than a rebuild. The daemon's crate and the protocol are
          # in it for the plugin too: cargo loads every member of the workspace,
          # whichever package it builds.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./src
              ./crates
              ./proto
              ./Cargo.toml
              ./Cargo.lock
            ];
          };

          # The release artifact.
          #
          # `dev build` and this derivation are not redundant, and they run the same
          # cargo invocation on purpose. `dev build` is the development loop:
          # incremental, against whatever cargo resolves, in a tree you are editing.
          # This one is what a tag ships — every dependency at the revision Cargo.lock
          # names, fetched by hash and built in the sandbox with no network and no
          # ambient state, so the bytes the provenance attestation covers do not depend
          # on what the runner had lying around.
          #
          # Not buildRustPackage: its cargoBuildHook hardcodes `--target
          # <host triple>` and exports CARGO_BUILD_TARGET over anything the caller sets,
          # so the crate gets built for x86_64 — where zellij-tile pulls in the whole
          # non-wasm stack, down to openssl-sys — and its check, install and auditable
          # hooks all then want a host binary that does not exist. Only the vendoring is
          # useful here, so only the vendoring is used.
          luneta = pkgs.stdenv.mkDerivation {
            pname = "luneta";
            inherit version src;

            cargoDeps = pkgs.rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };

            # protoc is for the build script of the protocol's crate, which the
            # plugin reads the daemon's answers with.
            nativeBuildInputs = [
              toolchain
              pkgs.rustPlatform.cargoSetupHook
              pkgs.protobuf
            ];

            buildPhase = ''
              runHook preBuild
              cargo build --release --offline --target wasm32-wasip1 -p luneta
              runHook postBuild
            '';

            # There is no bin/ to install: zellij loads the .wasm by path, so the
            # derivation is the file and nothing else. Cargo names it after the plugin's
            # binary; the file goes out under the name zellij is told to load.
            installPhase = ''
              runHook preInstall
              install -Dm0644 \
                target/wasm32-wasip1/release/luneta-plugin.wasm \
                "$out/luneta.wasm"
              runHook postInstall
            '';

            # The unit tests are host-target and run with `cargo test` in the devshell.
            # Nothing in this sandbox can execute a wasm32-wasip1 binary.
            doCheck = false;

            meta = {
              description = "A telescope-style session picker plugin for zellij";
              homepage = "https://github.com/lorenzolfm/luneta";
              license = pkgs.lib.licenses.mit;
              platforms = systems;
            };
          };

          # The daemon and its CLI, `lunetad` and `luneta`, for the system this is.
          #
          # buildRustPackage suits this one, for the reason it does not suit the plugin:
          # its hooks build, test and install for the host triple, which is the target
          # here. It builds on the toolchain the plugin builds on, and on Linux the tests
          # run in the sandbox as part of the build, so a Linux whose daemon builds is
          # one whose daemon passes them.
          #
          # Not on macOS. The tests run `/bin/ps` for when a process started, as Claude
          # Code does, and its build sandbox will not let them: a build there with the
          # tests on fails wherever the sandbox is on, as it is in CI. CI runs them on a
          # Mac outside the sandbox instead, through the devshell, where they see the
          # system as the daemon does when you run it.
          lunetad =
            (pkgs.makeRustPlatform {
              cargo = toolchain;
              rustc = toolchain;
            }).buildRustPackage
              {
                pname = "lunetad";
                inherit version src;

                cargoLock.lockFile = ./Cargo.lock;
                cargoBuildFlags = [
                  "-p"
                  "lunetad"
                ];
                # The protocol's crate too: what the daemon reads of git is read
                # there, and tested there.
                cargoTestFlags = [
                  "-p"
                  "lunetad"
                  "-p"
                  "luneta-protocol"
                ];

                # The build scripts write the protocol out as Rust, through protoc.
                nativeBuildInputs = [ pkgs.protobuf ];

                # The daemon asks git how each work tree stands, and the tests make
                # work trees of their own for it to ask about. It asks `ps` which
                # processes are in front of the terminal a command runs on, and the
                # tests ask it too.
                nativeCheckInputs = [
                  pkgs.git
                  pkgs.procps
                ];

                doCheck = !pkgs.stdenv.hostPlatform.isDarwin;

                meta = {
                  description = "The luneta daemon, and the CLI that talks to it";
                  homepage = "https://github.com/lorenzolfm/luneta";
                  license = pkgs.lib.licenses.mit;
                  mainProgram = "lunetad";
                  platforms = systems;
                };
              };

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
            # zellij is absent on purpose — see the reload comment in `text` below.
            # protoc is for the build scripts of the protocol's crate and of the
            # daemon, which `dev build` and `dev clippy` run.
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
              # this flake pins whether or not the caller entered the shell first. The system
              # rustc cannot build this crate; see the toolchain comment above.
              #
              # writeShellApplication supplies the shebang and `set -euo pipefail`, and lints
              # this text with shellcheck at build time.

              usage() {
                cat <<'HELP'
              dev — luneta build recipes (pinned nix tooling)

                dev build                  cargo build --release --target wasm32-wasip1
                dev clippy [args...]       cargo clippy over every target, warnings denied
                dev install                build, then install to ~/.local/share/zellij/plugins
                dev reload [session]       install, then put the new bytes in the running picker
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
                build | clippy | install | reload | clean | skills) ;;
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

              # Named after the plugin's binary, `luneta-plugin`; it is installed as
              # `luneta.wasm`.
              wasm="target/wasm32-wasip1/release/luneta-plugin.wasm"

              # Zellij caches permissions against the absolute path of the .wasm. Keep this path
              # stable — including the literal ~/.local/share, rather than XDG_DATA_HOME, which
              # would move the file for anyone who sets it — or zellij asks for the permissions
              # again after each install.
              install_dir="$HOME/.local/share/zellij/plugins"
              installed="$install_dir/luneta.wasm"

              build() {
                cargo build --release --target wasm32-wasip1 -p luneta
              }

              do_install() {
                build
                mkdir -p "$install_dir"
                install -m 0644 "$wasm" "$installed"
                echo "installed -> $installed"
              }

              case "$cmd" in
                build)
                  build
                  ;;

                # The lint set lives in Cargo.toml and clippy.toml, so this is only the
                # invocation. It runs once for each side of the workspace, as neither side
                # builds for the other's target: the plugin for the wasm it ships as, and the
                # daemon for this host, which is what it ships for. Three parts of it are
                # load-bearing:
                #
                #   --all-targets   the tests and the profiling example are code too, and
                #                   `unwrap_used` reads differently in each — clippy.toml
                #                   exempts tests, not examples.
                #   --target        the plugin's run names the triple `dev build` uses.
                #                   Linting the plugin for the host would check a build that
                #                   never ships, and `cfg(target_*)` code would be linted on
                #                   the wrong side. The daemon's run names none, and so lints
                #                   the host's.
                #   -D warnings     every lint in Cargo.toml is set to `warn`, so without
                #                   this the command always exits 0 and gates nothing.
                #
                # Anything after `dev clippy` goes to cargo in both runs, ahead of the `--`,
                # so `dev clippy --fix --allow-dirty` applies the machine-applicable ones.
                clippy)
                  cargo clippy --locked --all-targets --target wasm32-wasip1 -p luneta "$@" -- -D warnings
                  cargo clippy --locked --all-targets --workspace --exclude luneta "$@" -- -D warnings
                  ;;

                install)
                  do_install
                  ;;

                # Installing first is not optional here: a reload that reads the old bytes off
                # disk is the failure this command exists to avoid, and the copy is free.
                #
                # zellij is deliberately not a runtimeInput. These calls drive the server that is
                # already running, and a pinned client that drifts from it fails on a version
                # mismatch instead of reloading. It comes from the ambient PATH.
                #
                # Two calls are necessary, because neither one covers both states:
                #
                #   launch-or-focus  Creates the picker if it does not run, and is the only call
                #                    that can pass --floating, which is the geometry the key
                #                    binding gives it. On a picker that runs, it takes focus.
                #   start-or-reload  Reads the .wasm from disk again and puts it into the pane
                #                    that is already there. This is what loads the new bytes.
                #
                # The order matters. Focus or create first, so that there is something to reload.
                #
                # Do not return to a close-and-relaunch pair. That version had to name the pane it
                # closed, and it could not: zellij documents a `plugin_<id>` on the stdout of
                # launch-or-focus-plugin ("Returns: Plugin pane ID"), but 0.45.1 prints nothing.
                # The guard thus saw an empty id and refused to run, which was correct:
                # `close-pane` without an id closes the focused pane, which during development is
                # usually your editor. A reload needs no pane id.
                #
                # Pass a session name to drive another session. Omit it to use the current one.
                reload)
                  do_install

                  zellij=(zellij)
                  if [ "$#" -gt 0 ] && [ -n "$1" ]; then
                    zellij=(zellij -s "$1")
                  fi

                  "''${zellij[@]}" action launch-or-focus-plugin \
                    --skip-plugin-cache --floating "file:$installed"
                  "''${zellij[@]}" action start-or-reload-plugin "file:$installed"
                  ;;

                clean)
                  cargo clean
                  ;;

                # The menu's search finds skills where Claude Code looks for them, which
                # Claude Code can change in any release. This asks Claude Code itself, by the
                # `init` a headless start prints on a model that is none, and fails with the
                # names only one side has. Like zellij for `reload`, claude comes from the
                # ambient PATH: the check is against the claude you run.
                skills)
                  dir="$(cd "''${1:-$here}" && pwd -P)"
                  LUNETA_SKILLS_DIR="$dir" cargo test --locked \
                    skills::tests::the_finder_finds_what_claude_code_runs -- --ignored --exact
                  ;;
              esac
            '';
          };
        in
        {
          inherit dev luneta lunetad;

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

      # `dev` is exposed so that `nix run .#dev -- build` works without a devshell,
      # which is what CI uses.
      packages = nixpkgs.lib.mapAttrs (_: built: {
        inherit (built) dev luneta lunetad;
        default = built.luneta;
      }) bySystem;
    };
}
