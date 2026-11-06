{ pkgs, ... }:
{
  name = "lint";
  usage = "[--breaking]";
  help = "statix, deadnix and buf breaking; --breaking runs that one alone";
  tools = [
    pkgs.buf
    pkgs.deadnix
    pkgs.findutils
    pkgs.git
    pkgs.statix
  ];
  text = ''
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
