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
  help = "build this checkout into ~/.cargo/bin, in place of the running lunetad; --mux of luneta-mux too";
  tools = buildTools ++ installed.tools ++ [ pkgs.jq ];
  inheritPath = true;
  text = ''
    ${installed.text}

    ${compileEnv}
    stamp="$(date +%s)"
    LUNETA_BUILT="$stamp" cargo build --locked -p lunetad -p luneta-mux
    target="$(cargo metadata --locked --no-deps --format-version 1 | jq -r .target_directory)/debug"

    mkdir -p "$bin"
    bin="$(cd "$bin" && pwd -P)"
    for program in "''${programs[@]}"; do
      install -m 755 "$target/$program" "$bin/.$program.new"
      mv -f "$bin/.$program.new" "$bin/$program"
    done
    for program in luneta lunetad; do
      version="$("$bin/$program" --version)"
      [[ $version == "$program "*"+$stamp" ]] || {
        echo "dev install: $bin/$program says $version, not the +$stamp it was built with" >&2
        exit 1
      }
    done
    built="$("$bin/lunetad" --version)"
    echo "dev install: $built, with its luneta and luneta-mux, in $bin" >&2

    luneta="$(PATH="$DEV_PATH" type -P luneta || true)"
    if [ -z "$luneta" ]; then
      echo "dev install: your PATH finds no luneta, so put $bin in it" >&2
    elif [ ! "$luneta" -ef "$bin/luneta" ]; then
      echo "dev install: your PATH finds $luneta before $bin/luneta, so put $bin ahead of it" >&2
    fi

    if before="$("$bin/luneta" info 2>&1)"; then
      "$bin/luneta" watch >/dev/null
      after="$("$bin/luneta" info)"
      [[ $after == "$built ("* ]] || {
        echo "dev install: $after, runs on, as luneta replaces only a lunetad older than yours (''${built#lunetad }); end it, and the next luneta starts yours" >&2
        exit 1
      }
      echo "dev install: $after, in place of $before" >&2
    else
      case "$before" in
        "luneta: no lunetad runs in "*)
          echo "dev install: ''${before#luneta: }, so the next luneta starts yours" >&2
          ;;
        *)
          echo "dev install: luneta info: ''${before#luneta: }" >&2
          exit 1
          ;;
      esac
    fi

    if [ -n "$mux" ]; then
      end_mux
    else
      mapfile -t running < <(muxes)
      wait "$!"
      if [ "''${#running[@]}" -gt 0 ]; then
        echo "dev install: luneta-mux (pid ''${running[*]}) runs on as it was, and dev install --mux ends it for yours" >&2
      fi
    fi
    echo "dev install: a luneta already open runs its old build until you start it again" >&2
  '';
}
