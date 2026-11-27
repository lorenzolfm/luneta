{ pkgs, ... }:
let
  comments = import ../../comments.nix { inherit pkgs; };
  swift = import ../../swift.nix { inherit pkgs; };
  typescript = import ../../typescript.nix { inherit pkgs; };
in
{
  name = "lint";
  usage = "[--breaking]";
  help = "statix, deadnix, no comments, matching dev and release crate profiles, iOS CI input filter parity, swift-format and swiftlint over the app, typescript over extensions, no clock waits in tests, no call past its wrapper, no shell script files and buf breaking; --breaking runs that one alone";
  tools = [
    pkgs.buf
    pkgs.deadnix
    pkgs.findutils
    pkgs.git
    pkgs.nix
    pkgs.statix
    comments
    swift.check
    typescript
  ];
  text = ''
    profiles() {
      local failures
      failures="$(
        nix eval --impure --raw --expr \
          'builtins.concatStringsSep "\n" (import ${../../profiles.nix} { root = /. + builtins.getEnv "PWD"; })'
      )" || return 1
      [ -z "$failures" ] || {
        printf '%s\n' "$failures" >&2
        return 1
      }
    }

    ios_inputs() {
      local failures
      failures="$(
        nix eval --impure --raw --expr \
          'builtins.concatStringsSep "\n" (import ${../../ios-inputs.nix} { root = /. + builtins.getEnv "PWD"; })'
      )" || return 1
      [ -z "$failures" ] || {
        printf '%s\n' "$failures" >&2
        return 1
      }
    }

    breaking() {
      local base
      base="$(git merge-base HEAD origin/main)" || {
        echo "dev lint: HEAD has no merge base with origin/main" >&2
        return 1
      }
      (
        cd proto &&
          buf breaking \
            --against "../.git#ref=$base,subdir=proto" \
            --config '{"version":"v2","breaking":{"use":["WIRE","RPC_NO_DELETE","PACKAGE_SERVICE_NO_DELETE"]}}'
      )
    }

    case "$#:$*" in
      0:)
        status=0
        statix check . || status=1
        git ls-files -z --cached --others --exclude-standard '*.nix' |
          xargs -0 deadnix --fail || status=1
        git ls-files -z --cached --others --exclude-standard | lune-comments || status=1
        profiles || status=1
        ios_inputs || status=1
        git ls-files -z --cached --others --exclude-standard '*.swift' |
          xargs -0 -r swift-check || status=1
        git ls-files -z --cached --others --exclude-standard 'crates/*.ts' |
          xargs -0 -r lune-typescript || status=1
        breaking || status=1
        exit "$status"
        ;;
      1:--breaking)
        breaking
        ;;
      *)
        echo "usage: dev lint [--breaking]" >&2
        exit 2
        ;;
    esac
  '';
}
