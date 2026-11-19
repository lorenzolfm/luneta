{ pkgs, name }:
{
  tools = [
    pkgs.coreutils
    pkgs.fish
    pkgs.flock
    pkgs.gawk
    pkgs.procps
  ];
  text = ''
    programs=(luneta-mux lunetad luneta)
    home="''${CARGO_HOME:-$HOME/.cargo}"
    bin="$home/bin"
    builds="$home/luneta-dev"
    left=()

    usage() {
      echo "usage: dev ${name} [--mux]" >&2
      exit 2
    }

    case "$#:$*" in
      0:) mux="" ;;
      1:--mux) mux=yes ;;
      *) usage ;;
    esac

    info() {
      build=""
      pid=""
      ran=0
      said="$("$1" info 2>&1)" || ran=$?
      if [ "$ran" -eq 0 ]; then
        [[ $said =~ ^lunetad\ ([^ ]+)\ \(pid\ ([1-9][0-9]*)\) ]] || {
          echo "dev ${name}: luneta info says '$said', which names no lunetad and pid" >&2
          exit 1
        }
        build="''${BASH_REMATCH[1]}"
        pid="''${BASH_REMATCH[2]}"
      fi
    }

    identified() {
      local pid start
      for pid in "$@"; do
        if start="$(ps -o lstart= -p "$pid")"; then
          echo "$pid $start"
        fi
      done
    }

    same() {
      local pid="''${1%% *}" stat
      stat="$(ps -o stat= -p "$pid")" || return 1
      [[ $stat != *Z* ]] && [ "$(ps -o lstart= -p "$pid")" = "''${1#* }" ]
    }

    signalled() {
      local signal="$1" tries="$2" entry
      shift 2
      for entry in "$@"; do
        if same "$entry"; then
          kill "-$signal" "''${entry%% *}" 2>/dev/null || true
        fi
      done
      for _ in $(seq "$tries"); do
        left=()
        for entry in "$@"; do
          if same "$entry"; then
            left+=("$entry")
          fi
        done
        [ "''${#left[@]}" -gt 0 ] || return 0
        sleep 0.1
      done
    }

    stop() {
      left=()
      [ "$#" -gt 0 ] || return 0
      signalled TERM 50 "$@"
      [ "''${#left[@]}" -gt 0 ] || return 0
      signalled KILL 20 "''${left[@]}"
      [ "''${#left[@]}" -gt 0 ] || return 0
      echo "dev ${name}: pid ''${left[*]%% *} outlived its TERM by 5s and its KILL by 2s" >&2
      return 1
    }

    end_daemon() {
      local ending
      mapfile -t ending < <(identified "$pid")
      wait "$!"
      stop "''${ending[@]}"
    }

    muxes() {
      local folder
      folder="$(fish --no-config -c "source shell/luneta.fish; __luneta_folder")"
      [[ $folder == /* ]] || {
        echo "dev ${name}: __luneta_folder names '$folder', no absolute folder" >&2
        return 1
      }
      ps -ww -u "$(id -u)" -o pid= -o args= |
        awk -v folder="$folder" '$2 ~ /(^|\/)luneta-mux$/ && NF == 3 && $3 == folder { print $1 }'
    }

    end_mux() {
      local running ending
      mapfile -t running < <(muxes)
      wait "$!"
      if [ "''${#running[@]}" -eq 0 ]; then
        echo "dev ${name}: no luneta-mux runs" >&2
        return 0
      fi
      mapfile -t ending < <(identified "''${running[@]}")
      wait "$!"
      echo "dev ${name}: ending luneta-mux (pid ''${running[*]}) and what runs in its terminals; the next lunetad starts the one beside it and restores them" >&2
      stop "''${ending[@]}"
    }
  '';
}
