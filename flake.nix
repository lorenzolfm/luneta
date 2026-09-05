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
      # The script is a real file rather than a nix string: it stays shellcheck-able
      # and editable without `''$` escaping, and writeShellApplication runs
      # shellcheck over it at build time either way.
      dev = pkgs.writeShellApplication {
        name = "dev";
        meta.description = "luneta build recipes on the flake-pinned toolchain";
        # zellij is absent on purpose — see the reload comment in scripts/dev.sh.
        runtimeInputs = [
          toolchain
          pkgs.coreutils
          pkgs.git
        ];
        text = builtins.readFile ./scripts/dev.sh;
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
