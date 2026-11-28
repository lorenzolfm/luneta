{ pkgs, testRun }:
let
  inherit (pkgs) lib;
  awk = lib.getExe pkgs.gawk;
  jq = lib.getExe pkgs.jq;
  ps = lib.getExe' pkgs.procps "ps";
  realpath = lib.getExe' pkgs.coreutils "realpath";
  seq = lib.getExe' pkgs.coreutils "seq";
  sleep = lib.getExe' pkgs.coreutils "sleep";
  mktemp = lib.getExe' pkgs.coreutils "mktemp";
  rm = lib.getExe' pkgs.coreutils "rm";
  inMemory = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
    if [ -d /dev/shm ] && [ -w /dev/shm ]; then
      TMPDIR="$(${mktemp} -d /dev/shm/lune-nextest.XXXXXX)"
      export TMPDIR
      trap '${rm} -rf -- "$TMPDIR"' EXIT
    fi
  '';
in
pkgs.writeShellApplication {
  name = "lune-nextest";
  meta.description = "cargo nextest run on lune's tests with their temporary folders in memory on Linux, failing if they leave a lune-mux from target/ running";
  text = ''
    target="$(cargo metadata --locked --no-deps --format-version 1 | ${jq} -r .target_directory)"
    case "$target" in
      /*) ;;
      *)
        printf 'lune-nextest: cargo metadata gave %s as the target folder, not an absolute path, so no lune-mux could be told apart\n' "$target" >&2
        exit 2
        ;;
    esac
    resolved="$(${realpath} -m -- "$target")/"
    given="$(${realpath} -ms -- "$target")/"
    muxes() {
      ${ps} -A -o pid= -o args= | ${awk} -v resolved="$resolved" -v given="$given" -v before="$1" '
        function from(folder) {
          if (index(args, folder) != 1) return 0
          return substr(args, length(folder) + 1) ~ /^([^ ]*\/)?lune-mux( |$)/
        }
        {
          pid = $1
          args = $0
          sub(/^[ \t]*[0-9]+[ \t]+/, "", args)
          if ((from(resolved) || from(given)) && index(before, " " pid " ") == 0) printf "%s ", pid
        }'
    }
    before=" $(muxes " ")"
    tested=0
    ${inMemory}
    cargo nextest run --locked ${testRun} "$@" || tested=$?
    left="$(muxes "$before")"
    for _ in $(${seq} 100); do
      [ -z "$left" ] && break
      ${sleep} 0.1
      left="$(muxes "$before")"
    done
    if [ -n "$left" ]; then
      shown=()
      for pid in $left; do
        shown+=(-p "$pid")
      done
      printf 'lune-nextest: the tests left lune-mux running 10s after they ended:\n' >&2
      ${ps} -o pid= -o etime= -o args= "''${shown[@]}" >&2 || true
      [ "$tested" -ne 0 ] || tested=1
    fi
    exit "$tested"
  '';
}
