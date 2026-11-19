{
  pkgs,
  buildTools,
  compileEnv,
  ...
}:
let
  installed = import ../installed.nix { inherit pkgs; };
in
{
  name = "install";
  usage = "[--mux]";
  help = "build this checkout into ~/.cargo/bin, in place of the running lunetad; --mux of luneta-mux too";
  tools = buildTools ++ installed.tools ++ [ pkgs.jq ];
  inheritPath = true;
  text = ''
    said="dev install"
    ${installed.text}

    ${compileEnv}
    LUNETA_BUILT="$(date +%s)" cargo build --locked -p lunetad -p luneta-mux
    target="$(cargo metadata --locked --no-deps --format-version 1 | jq -r .target_directory)/debug"

    mkdir -p "$home"
    bin="$(cd "$home" && pwd -P)"
    for program in "''${programs[@]}"; do
      install -m 755 "$target/$program" "$bin/.$program.new"
      mv -f "$bin/.$program.new" "$bin/$program"
    done
    echo "$said: $("$bin/lunetad" --version), with its luneta and luneta-mux, in $bin" >&2

    luneta="$(type -P luneta || true)"
    if [ ! "$luneta" -ef "$bin/luneta" ]; then
      echo "$said: your PATH finds ''${luneta:-no luneta} before $bin/luneta, so put $bin ahead of it" >&2
    fi

    if before="$("$bin/luneta" info 2>&1)"; then
      "$bin/luneta" watch >/dev/null
      echo "$said: $("$bin/luneta" info), in place of $before" >&2
    else
      echo "$said: ''${before#luneta: }, so the next luneta starts yours" >&2
    fi

    if [ -n "$mux" ]; then
      end_mux
    else
      mapfile -t running < <(muxes)
      if [ "''${#running[@]}" -gt 0 ]; then
        echo "$said: luneta-mux (pid ''${running[*]}) runs on as it was, and $said --mux ends it for yours" >&2
      fi
    fi
    echo "$said: a luneta already open runs its old build until you start it again" >&2
  '';
}
