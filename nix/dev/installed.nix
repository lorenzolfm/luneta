{ pkgs }:
{
  tools = [
    pkgs.coreutils
    pkgs.fish
    pkgs.gawk
    pkgs.procps
  ];
  text = ''
    programs=(luneta lunetad luneta-mux)
    home="''${CARGO_HOME:-$HOME/.cargo}/bin"

    usage() {
      echo "usage: $said [--mux]" >&2
      exit 2
    }

    case "$#:$*" in
      0:) mux="" ;;
      1:--mux) mux=yes ;;
      *) usage ;;
    esac

    listed() {
      ps -ww -u "$(id -u)" -o pid= -o args=
    }

    muxes() {
      local folder
      folder="$(fish --no-config -c "source shell/luneta.fish; __luneta_folder")"
      listed | awk -v folder="$folder" '$2 ~ /(^|\/)luneta-mux$/ && NF == 3 && $3 == folder { print $1 }'
    }

    stop() {
      local pid left
      kill -TERM "$@" 2>/dev/null || true
      for _ in $(seq 50); do
        left=()
        for pid in "$@"; do
          if kill -0 "$pid" 2>/dev/null; then
            left+=("$pid")
          fi
        done
        [ "''${#left[@]}" -gt 0 ] || return 0
        sleep 0.1
      done
      echo "$said: pid ''${left[*]} did not end in 5s" >&2
      return 1
    }

    end_mux() {
      local running
      mapfile -t running < <(muxes)
      if [ "''${#running[@]}" -eq 0 ]; then
        echo "$said: no luneta-mux runs" >&2
        return 0
      fi
      echo "$said: ending luneta-mux (pid ''${running[*]}) and what runs in its terminals; lunetad starts the one beside it and restores them" >&2
      stop "''${running[@]}"
    }
  '';
}
