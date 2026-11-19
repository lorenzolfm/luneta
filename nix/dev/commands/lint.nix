{ pkgs, ... }:
let
  comments = import ../../comments.nix { inherit pkgs; };
in
{
  name = "lint";
  usage = "";
  help = "statix, deadnix and no comments";
  tools = [
    pkgs.deadnix
    pkgs.findutils
    pkgs.git
    pkgs.statix
    comments
  ];
  text = ''
    if [ "$#" -ne 0 ]; then
      echo "usage: dev lint" >&2
      exit 2
    fi
    status=0
    statix check . || status=1
    git ls-files -z --cached --others --exclude-standard '*.nix' |
      xargs -0 deadnix --fail || status=1
    git ls-files -z --cached --others --exclude-standard | luneta-comments || status=1
    exit "$status"
  '';
}
