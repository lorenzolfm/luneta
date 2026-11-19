{ pkgs, ... }:
let
  name = "uninstall";
  installed = import ../installed.nix { inherit pkgs name; };
in
{
  inherit name;
  usage = "[--mux]";
  help = "take dev install's build out of ~/.cargo/bin and end the lunetad that runs; --mux luneta-mux too, saying it all in ~/.cargo/luneta-dev.log too when that ends this terminal";
  inherit (installed) tools;
  inheritPath = true;
  text = ''
    ${installed.text}

    if [ -d "$bin" ]; then
      bin="$(cd "$bin" && pwd -P)"
    fi
    if [ -d "$builds" ]; then
      builds="$(cd "$builds" && pwd -P)"
      exec 9>"$builds/.lock"
      flock 9
    fi

    removed=()
    for program in "''${programs[@]}"; do
      if [ "$(readlink "$bin/$program")" = "$builds/current/$program" ]; then
        rm -f "$bin/$program"
        removed+=("$program")
      fi
    done
    echo "dev uninstall: took ''${removed[*]:-nothing} out of $bin" >&2

    luneta="$builds/current/luneta"
    if [ -f "$luneta" ]; then
      rm -f "$builds/current/lunetad" "$builds/current/luneta-mux"
      info "$luneta"
      case "$ran" in
        0)
          echo "dev uninstall: ending lunetad $build (pid $pid), so the next luneta starts its own" >&2
          end_daemon "$pid"
          ;;
        5) ;;
        *)
          end_holder "so the next luneta starts its own"
          ;;
      esac
    fi
    rm -rf "$builds"

    if [ -n "$mux" ]; then
      end_mux
    fi
    luneta="$(PATH="$DEV_PATH" type -P luneta || true)"
    echo "dev uninstall: your PATH finds ''${luneta:-no luneta}" >&2
  '';
}
