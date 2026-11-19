{ pkgs, ... }:
let
  name = "uninstall";
  installed = import ../installed.nix { inherit pkgs name; };
in
{
  inherit name;
  usage = "[--mux]";
  help = "take dev install's build out of ~/.cargo/bin and stop its lunetad; --mux luneta-mux too";
  inherit (installed) tools;
  inheritPath = true;
  text = ''
    ${installed.text}

    if [ -d "$bin" ]; then
      bin="$(cd "$bin" && pwd -P)"
    fi
    removed=()
    for ((index = ''${#programs[@]} - 1; index >= 0; index--)); do
      program="''${programs[index]}"
      if [ -e "$bin/$program" ]; then
        rm -f "$bin/$program"
        removed+=("$program")
      fi
    done
    echo "dev uninstall: took ''${removed[*]:-nothing} out of $bin" >&2

    mapfile -t daemons < <(processes | awk -v program="$bin/lunetad" '$2 == program { print $1 }')
    wait "$!"
    if [ "''${#daemons[@]}" -gt 0 ]; then
      echo "dev uninstall: stopping lunetad (pid ''${daemons[*]}), so the next luneta starts its own" >&2
      stop "''${daemons[@]}"
    fi

    if [ -n "$mux" ]; then
      end_mux
    fi
    luneta="$(PATH="$DEV_PATH" type -P luneta || true)"
    echo "dev uninstall: your PATH finds ''${luneta:-no luneta}" >&2
  '';
}
