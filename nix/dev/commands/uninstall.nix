{ pkgs, ... }:
let
  name = "uninstall";
  installed = import ../installed.nix { inherit pkgs name; };
in
{
  inherit name;
  usage = "[--mux]";
  help = "take dev install's build out of ~/.cargo/bin and end the luned that runs; --mux lune-mux too, saying it all in ~/.cargo/lune-dev.log too when that ends this terminal";
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

    lune="$builds/current/lune"
    if [ -f "$lune" ]; then
      rm -f "$builds/current/luned" "$builds/current/lune-mux"
      info "$lune"
      case "$ran" in
        0)
          echo "dev uninstall: ending luned $build (pid $pid), so the next lune starts its own" >&2
          end_daemon "$pid"
          ;;
        5) ;;
        *)
          end_holder "so the next lune starts its own"
          ;;
      esac
    fi
    rm -rf "$builds"

    if [ -n "$mux" ]; then
      end_mux
    fi
    lune="$(PATH="$DEV_PATH" type -P lune || true)"
    echo "dev uninstall: your PATH finds ''${lune:-no lune}" >&2
  '';
}
