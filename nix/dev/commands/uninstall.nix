{ pkgs, ... }:
let
  installed = import ../installed.nix { inherit pkgs; };
in
{
  name = "uninstall";
  usage = "[--mux]";
  help = "take dev install's build out of ~/.cargo/bin and stop its lunetad; --mux luneta-mux too";
  inherit (installed) tools;
  inheritPath = true;
  text = ''
    said="dev uninstall"
    ${installed.text}

    bin="$(cd "$home" 2>/dev/null && pwd -P)" || bin="$home"
    removed=()
    for program in "''${programs[@]}"; do
      if [ -e "$bin/$program" ]; then
        rm -f "$bin/$program"
        removed+=("$program")
      fi
    done
    echo "$said: took ''${removed[*]:-nothing} out of $bin" >&2

    mapfile -t daemons < <(listed | awk -v program="$bin/lunetad" '$2 == program { print $1 }')
    if [ "''${#daemons[@]}" -gt 0 ]; then
      echo "$said: stopping lunetad (pid ''${daemons[*]}), so the next luneta starts its own" >&2
      stop "''${daemons[@]}"
    fi

    if [ -n "$mux" ]; then
      end_mux
    fi
    luneta="$(type -P luneta || true)"
    echo "$said: your PATH finds ''${luneta:-no luneta}" >&2
  '';
}
