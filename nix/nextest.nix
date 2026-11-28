{ pkgs, testRun }:
let
  inherit (pkgs) lib;
  awk = lib.getExe pkgs.gawk;
  chmod = lib.getExe' pkgs.coreutils "chmod";
  df = lib.getExe' pkgs.coreutils "df";
  jq = lib.getExe pkgs.jq;
  mktemp = lib.getExe' pkgs.coreutils "mktemp";
  ps = lib.getExe' pkgs.procps "ps";
  realpath = lib.getExe' pkgs.coreutils "realpath";
  rm = lib.getExe' pkgs.coreutils "rm";
  seq = lib.getExe' pkgs.coreutils "seq";
  sleep = lib.getExe' pkgs.coreutils "sleep";
  shmMinKib = 524288;
  inMemory = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
    shm_tmpdir=""
    cleanup_shm_tmpdir() {
      if [ -n "$shm_tmpdir" ]; then
        case "$shm_tmpdir" in
          /dev/shm/lune-nextest.*)
            ${chmod} -R u+rwx -- "$shm_tmpdir" 2>/dev/null || true
            ${rm} -rf -- "$shm_tmpdir"
            ;;
          *)
            printf 'lune-nextest: refusing to remove unexpected shm_tmpdir %s\n' "$shm_tmpdir" >&2
            return 1
            ;;
        esac
        shm_tmpdir=""
      fi
    }
    if [ -d /dev/shm ] && [ -w /dev/shm ]; then
      shm_avail_kib="$(${df} -Pk /dev/shm | ${awk} 'NR == 2 { print $4 }')"
      case "$shm_avail_kib" in
        "" | *[!0-9]*)
          printf 'lune-nextest: could not read free space on /dev/shm (got %s)\n' "$shm_avail_kib" >&2
          exit 2
          ;;
      esac
      if [ "$shm_avail_kib" -ge ${toString shmMinKib} ]; then
        shm_tmpdir="$(${mktemp} -d /dev/shm/lune-nextest.XXXXXX)"
        case "$shm_tmpdir" in
          /dev/shm/lune-nextest.*) ;;
          *)
            printf 'lune-nextest: mktemp returned unexpected path %s\n' "$shm_tmpdir" >&2
            exit 2
            ;;
        esac
        trap cleanup_shm_tmpdir EXIT
        trap 'cleanup_shm_tmpdir || true; trap - INT EXIT; kill -s INT "$$"' INT
        trap 'cleanup_shm_tmpdir || true; trap - TERM EXIT; kill -s TERM "$$"' TERM
        trap 'cleanup_shm_tmpdir || true; trap - HUP EXIT; kill -s HUP "$$"' HUP
        TMPDIR="$shm_tmpdir"
        export TMPDIR
      fi
    fi
  '';
  cleanupInMemory = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
    cleanup_shm_tmpdir || [ "$tested" -ne 0 ] || tested=1
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
    ${cleanupInMemory}
    exit "$tested"
  '';
}
