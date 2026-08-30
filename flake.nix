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
      # Pinned to the channel zellij's own rust-toolchain.toml names, with the
      # wasm target added. The system rustc cannot build wasm32-wasip1: nix ships
      # std for the host triple only, and there is no rustup to add targets with.
      toolchain = pkgs.rust-bin.stable."1.95.0".default.override {
        targets = [ "wasm32-wasip1" ];
      };
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = [ toolchain ];
      };
    };
}
