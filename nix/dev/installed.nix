{ pkgs, name }:
{
  tools = [
    pkgs.coreutils
    pkgs.flock
    pkgs.gawk
    pkgs.lsof
    pkgs.procps
  ];
  text = ''
    programs=(luneta-mux lunetad luneta)
    home="''${CARGO_HOME:-$HOME/.cargo}"
    bin="$home/bin"
    builds="$home/luneta-dev"
    report="$home/luneta-dev.log"
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
      mapfile -t ending < <(identified "$@")
      wait "$!"
      stop "''${ending[@]}"
    }

    luneta_folder() {
      local root
      if [[ ''${XDG_RUNTIME_DIR-} == /* ]]; then
        echo "''${XDG_RUNTIME_DIR%/}/luneta"
      elif [ "$(uname)" = Darwin ]; then
        root="$(/usr/bin/getconf DARWIN_USER_TEMP_DIR)"
        root="''${root%/}"
        [[ $root == /* ]] || {
          echo "dev ${name}: DARWIN_USER_TEMP_DIR names '$root', no absolute folder" >&2
          return 1
        }
        echo "$root/luneta"
      else
        echo "/tmp/luneta-$(id -u)"
      fi
    }

    holders() {
      lsof -w -t -a -c lunetad -- "$1/lunetad.lock" || true
    }

    end_holder() {
      local folder holding
      folder="$(luneta_folder)"
      mapfile -t holding < <(holders "$folder")
      wait "$!"
      if [ "''${#holding[@]}" -eq 0 ]; then
        echo "dev ${name}: luneta info: ''${said#luneta: }" >&2
        exit 1
      fi
      echo "dev ${name}: luneta info: ''${said#luneta: }, but lunetad (pid ''${holding[*]}) holds $folder: ending it, $1" >&2
      end_daemon "''${holding[@]}"
    }

    muxes() {
      local folder
      folder="$(luneta_folder)"
      ps -ww -u "$(id -u)" -o pid= -o args= |
        awk -v folder="$folder" '$2 ~ /(^|\/)luneta-mux$/ && NF == 3 && $3 == folder { print $1 }'
    }

    descends() {
      local at="$$" parent of
      while [ "$at" -gt 1 ]; do
        for of in "$@"; do
          [ "$at" != "$of" ] || return 0
        done
        parent="$(ps -o ppid= -p "$at")" || return 1
        at="''${parent//[[:space:]]/}"
      done
      return 1
    }

    outlive_terminal() {
      local running
      mapfile -t running < <(muxes)
      wait "$!"
      descends "''${running[@]}" || return 0
      echo "dev ${name}: --mux ends this terminal with luneta-mux (pid ''${running[*]}), so all dev ${name} says from here goes to $report too" >&2
      mkdir -p "$home"
      : >"$report"
      trap "" HUP
      exec > >(tee "$report") 2>&1
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

    if [ -n "$mux" ]; then
      outlive_terminal
    fi
  '';
}
