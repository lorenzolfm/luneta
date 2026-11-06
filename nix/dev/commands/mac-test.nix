{ pkgs, ... }:
{
  name = "mac-test";
  usage = "<ssh destination> [args...]";
  help = "dev test on a Mac over ssh, on a copy of this tree; touches no checkout there";
  tools = [
    pkgs.openssh
    pkgs.rsync
  ];
  text = ''
    case "''${1:-}" in
      "" | -*)
        echo "usage: dev mac-test <ssh destination> [args...]" >&2
        exit 2
        ;;
    esac
    destination="$1"
    shift

    name="''${PWD##*/}"
    folder=".cache/luneta-mac-test/''${name//[^A-Za-z0-9._-]/_}"
    echo "dev mac-test: $destination:$folder" >&2

    ssh "$destination" mkdir -p "$folder/tree"
    rsync -a --delete --filter=':- .gitignore' --exclude=/.git ./ "$destination:$folder/tree/"

    ssh "$destination" bash -s <<<"{
      cd $folder/tree || exit
      CARGO_TARGET_DIR=\$HOME/$folder/target exec nix run 'path:.#dev.commands.test' -- ''${*@Q}
    } </dev/null"
  '';
}
