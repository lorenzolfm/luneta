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

      # Exposed as a package so that `nix run .#dev -- build` works without a
      # devshell, which is what CI uses.
      packages.${system}.dev = dev;
    };
}
