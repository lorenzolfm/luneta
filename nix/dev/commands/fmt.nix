{ pkgs, toolchain, ... }:
let
  rustfmt = pkgs.writeShellScript "rustfmt" ''
    ${toolchain}/bin/rustfmt "$@" || exit
  '';
in
{
  name = "fmt";
  usage = "[--check]";
  help = "format the rust, nix and proto files; --check only says what would change";
  tools = [
    toolchain
    pkgs.buf
    pkgs.diffutils
    pkgs.findutils
    pkgs.git
    pkgs.nixfmt
  ];
  text = ''
    case "$#:$*" in
      0:)
        rust=()
        nix=()
        proto=(--write)
        ;;
      1:--check)
        rust=(--check)
        nix=(--check)
        proto=(--diff --exit-code)
        ;;
      *)
        echo "usage: dev fmt [--check]" >&2
        exit 2
        ;;
    esac

    RUSTFMT=${rustfmt} cargo fmt --all "''${rust[@]}"
    git ls-files -z --cached --others --exclude-standard '*.nix' | xargs -0 nixfmt "''${nix[@]}"
    buf format "''${proto[@]}" proto
  '';
}
