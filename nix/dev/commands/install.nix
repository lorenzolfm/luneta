{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
let
  name = "install";
  installed = import ../installed.nix { inherit pkgs name; };
in
{
  inherit name;
  usage = "[--mux]";
  help = "build this checkout into ~/.cargo/bin, and end the running luned for a lune to start yours; --mux lune-mux too, saying it all in ~/.cargo/lune-dev.log too when that ends this terminal";
  tools = buildTools ++ installed.tools ++ [ pkgs.jq ];
  inheritPath = true;
  text = ''
    ${installed.text}

    ${compileEnv}
    stamp="$(date +%s)"
    LUNE_BUILT="$stamp" cargo build --locked -p lune -p luned -p lune-mux
    target="$(cargo metadata --locked --no-deps --format-version 1 | jq -r .target_directory)/debug"

    mkdir -p "$bin" "$builds"
    bin="$(cd "$bin" && pwd -P)"
    builds="$(cd "$builds" && pwd -P)"
    exec 9>"$builds/.lock"
    flock 9
    build="$(mktemp -d "$builds/$stamp.XXXXXX")"
    for program in "''${programs[@]}"; do
      install -m 755 "$target/$program" "$build/$program"
    done
    for program in lune luned; do
      version="$("$build/$program" --version)"
      [[ $version == "$program "*"+$stamp" ]] || {
        rm -rf "$build"
        echo "dev install: $target/$program says $version, not the +$stamp it was built with" >&2
        exit 1
      }
    done
    built="$("$build/luned" --version)"
    built="''${built#luned }"
    ln -sfn "$build" "$builds/.current"
    mv -T "$builds/.current" "$builds/current"
    for program in "''${programs[@]}"; do
      if [ "$(readlink "$bin/$program")" != "$builds/current/$program" ]; then
        ln -sfn "$builds/current/$program" "$bin/.$program.new"
        mv -T "$bin/.$program.new" "$bin/$program"
      fi
    done
    for old in "$builds"/*/; do
      [ "$old" -ef "$builds/current" ] || rm -rf "$old"
    done
    exec 9>&-
    echo "dev install: luned $built, with its lune and lune-mux, in $build, which $bin links to" >&2

    lune="$(PATH="$DEV_PATH" type -P lune || true)"
    if [ -z "$lune" ]; then
      echo "dev install: your PATH finds no lune, so put $bin in it" >&2
    elif [ ! "$lune" -ef "$bin/lune" ]; then
      echo "dev install: your PATH finds $lune before $bin/lune, so put $bin ahead of it" >&2
    fi

    ended=""
    info "$bin/lune"
    case "$ran" in
      0)
        echo "dev install: ending luned $build (pid $pid), for a lune to start yours" >&2
        end_daemon "$pid"
        ended=yes
        ;;
      5)
        echo "dev install: ''${said#lune: }, so the next lune starts yours" >&2
        ;;
      *)
        end_holder "for a lune to start yours"
        ended=yes
        ;;
    esac

    if [ -n "$mux" ]; then
      end_mux
    else
      mapfile -t running < <(muxes)
      wait "$!"
      if [ "''${#running[@]}" -gt 0 ]; then
        echo "dev install: lune-mux (pid ''${running[*]}) runs on as it was, and dev install --mux ends it for yours" >&2
      fi
    fi

    if [ -n "$ended" ]; then
      for _ in $(seq 50); do
        info "$bin/lune"
        [ "$ran" -ne 0 ] || break
        sleep 0.1
      done
      case "$ran" in
        0)
          [ "$build" = "$built" ] || {
            echo "dev install: luned $build (pid $pid) runs, not yours ($built), as a lune of another build started it; start yours, and it ends one older than it" >&2
            exit 1
          }
          echo "dev install: luned $build (pid $pid) runs, started by a lune that was open" >&2
          ;;
        5)
          echo "dev install: no lune started a luned in 5s, so the next lune starts yours" >&2
          ;;
        *)
          echo "dev install: lune info, 5s after: ''${said#lune: }" >&2
          exit 1
          ;;
      esac
    fi
    echo "dev install: a lune already open runs its old build until you start it again" >&2
  '';
}
