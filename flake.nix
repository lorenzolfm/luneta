{
  description = "luneta — a personal zellij session picker plugin";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      system = "x86_64-linux";
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

      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

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
      #
      # The source is filtered to what cargo reads, so editing the README or a tape
      # is a cache hit rather than a rebuild.
      luneta = pkgs.stdenv.mkDerivation {
        pname = "luneta";
        inherit (cargoToml.package) version;

        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./src
            ./Cargo.toml
            ./Cargo.lock
          ];
        };

        cargoDeps = pkgs.rustPlatform.importCargoLock { lockFile = ./Cargo.lock; };

        nativeBuildInputs = [
          toolchain
          pkgs.rustPlatform.cargoSetupHook
        ];

        buildPhase = ''
          runHook preBuild
          cargo build --release --offline --target wasm32-wasip1
          runHook postBuild
        '';

        # There is no bin/ to install: zellij loads the .wasm by path, so the
        # derivation is the file and nothing else.
        installPhase = ''
          runHook preInstall
          install -Dm0644 \
            target/wasm32-wasip1/release/luneta.wasm \
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
          platforms = [ system ];
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
        runtimeInputs = [
          toolchain
          pkgs.coreutils
          pkgs.git
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
            dev install                build, then install to ~/.local/share/zellij/plugins
            dev reload [session]       install, then put the new bytes in the running picker
            dev clean                  cargo clean
          HELP
          }

          cmd="''${1:-help}"
          shift || true

          case "$cmd" in
            help | -h | --help)
              usage
              exit 0
              ;;
            build | install | reload | clean) ;;
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
          cd "$root"

          wasm="target/wasm32-wasip1/release/luneta.wasm"

          # Zellij caches permissions against the absolute path of the .wasm. Keep this path
          # stable — including the literal ~/.local/share, rather than XDG_DATA_HOME, which
          # would move the file for anyone who sets it — or zellij asks for the permissions
          # again after each install.
          install_dir="$HOME/.local/share/zellij/plugins"
          installed="$install_dir/luneta.wasm"

          build() {
            cargo build --release --target wasm32-wasip1
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
          esac
        '';
      };
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = [
          toolchain
          dev
        ];

        shellHook = ''
          echo "luneta — rust $(rustc --version | cut -d' ' -f2); run 'dev help' for the command set"
        '';
      };

      # `dev` is exposed so that `nix run .#dev -- build` works without a devshell,
      # which is what CI uses.
      packages.${system} = {
        inherit dev luneta;
        default = luneta;
      };
    };
}
