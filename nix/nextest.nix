{ pkgs, testRun }:
let
  inherit (pkgs) lib;
  awk = lib.getExe pkgs.gawk;
  bash = lib.getExe pkgs.bash;
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
  mkProbeFn =
    { chmodBin, rmBin }:
    ''
      probe_shm_exec() {
        local dir="$1"
        if [ -z "$dir" ] || [ ! -d "$dir" ]; then
          return 1
        fi
        local probe="$dir/probe"
        if ! printf '#!%s\nexit 0\n' ${lib.escapeShellArg bash} > "$probe"; then
          ${rmBin} -f -- "$probe" || true
          return 1
        fi
        if ! ${chmodBin} 0700 -- "$probe"; then
          ${rmBin} -f -- "$probe" || true
          return 1
        fi
        local status=0
        ( "$probe" ) 2>/dev/null || status=$?
        ${rmBin} -f -- "$probe" || true
        [ "$status" -eq 0 ]
      }
    '';
  mkCleanupFn =
    { chmodBin, rmBin }:
    ''
      shm_state="none"
      shm_tmpdir=""
      cleanup_shm_tmpdir() {
        case "$shm_state" in
          none | cleaned)
            return 0
            ;;
          failed)
            return 1
            ;;
          allocated)
            if [[ ! $shm_tmpdir =~ ^/dev/shm/lune-nextest\.[A-Za-z0-9]{6}$ ]]; then
              printf 'lune-nextest: refusing to remove unexpected shm_tmpdir %s\n' "$shm_tmpdir" >&2
              shm_state="failed"
              return 1
            fi
            ${chmodBin} -R u+rwx -- "$shm_tmpdir" 2>/dev/null || true
            if ${rmBin} -rf -- "$shm_tmpdir" && [ ! -e "$shm_tmpdir" ]; then
              shm_tmpdir=""
              shm_state="cleaned"
              return 0
            fi
            printf 'lune-nextest: failed to remove shm_tmpdir %s\n' "$shm_tmpdir" >&2
            shm_state="failed"
            return 1
            ;;
          *)
            printf 'lune-nextest: unexpected shm_state %s\n' "$shm_state" >&2
            return 1
            ;;
        esac
      }
    '';
  inMemory = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
    ${mkCleanupFn {
      chmodBin = chmod;
      rmBin = rm;
    }}
    ${mkProbeFn {
      chmodBin = chmod;
      rmBin = rm;
    }}
    if [ -d /dev/shm ] && [ -w /dev/shm ]; then
      shm_avail_kib="$(${df} -B 1024 --output=avail /dev/shm | ${awk} 'NR == 2 { print $1 }')"
      case "$shm_avail_kib" in
        "" | *[!0-9]*)
          printf 'lune-nextest: could not read free space on /dev/shm (got %s)\n' "$shm_avail_kib" >&2
          exit 2
          ;;
      esac
      if [ "$shm_avail_kib" -ge ${toString shmMinKib} ]; then
        trap 'cleanup_shm_tmpdir || true' EXIT
        trap 'cleanup_shm_tmpdir || true; trap - INT EXIT; kill -s INT "$$"' INT
        trap 'cleanup_shm_tmpdir || true; trap - TERM EXIT; kill -s TERM "$$"' TERM
        trap 'cleanup_shm_tmpdir || true; trap - HUP EXIT; kill -s HUP "$$"' HUP
        shm_tmpdir="$(${mktemp} -d /dev/shm/lune-nextest.XXXXXX)"
        shm_state="allocated"
        if [[ ! $shm_tmpdir =~ ^/dev/shm/lune-nextest\.[A-Za-z0-9]{6}$ ]]; then
          printf 'lune-nextest: mktemp returned unexpected path %s\n' "$shm_tmpdir" >&2
          shm_state="failed"
          exit 2
        fi
        if probe_shm_exec "$shm_tmpdir"; then
          TMPDIR="$shm_tmpdir"
          export TMPDIR
        else
          if ! cleanup_shm_tmpdir; then
            exit 2
          fi
          trap - EXIT INT TERM HUP
        fi
      fi
    fi
  '';
  cleanupInMemory = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
    cleanup_shm_tmpdir || [ "$tested" -ne 0 ] || tested=1
  '';
  cleanupCheck = pkgs.runCommand "lune-nextest-cleanup" { allowedReferences = [ ]; } ''
    ${bash} -euo pipefail <<'EOF'
    ${mkCleanupFn {
      chmodBin = "true";
      rmBin = "false";
    }}
    shm_state="allocated"
    shm_tmpdir="/dev/shm/lune-nextest.Ab12Cd"
    tested=0
    cleanup_shm_tmpdir 2>/dev/null || [ "$tested" -ne 0 ] || tested=1
    [ "$tested" -eq 1 ]
    [ "$shm_state" = "failed" ]
    [ "$shm_tmpdir" = "/dev/shm/lune-nextest.Ab12Cd" ]
    if cleanup_shm_tmpdir 2>/dev/null; then
      exit 1
    fi
    EOF

    ${bash} -euo pipefail <<'EOF'
    ${mkCleanupFn {
      chmodBin = "true";
      rmBin = "true";
    }}
    cleanup_shm_tmpdir
    [ "$shm_state" = "none" ]

    shm_state="allocated"
    shm_tmpdir="/dev/shm/lune-nextest.Ab12Cd/../../etc"
    if cleanup_shm_tmpdir 2>/dev/null; then
      exit 1
    fi
    [ "$shm_state" = "failed" ]

    shm_state="allocated"
    shm_tmpdir="/dev/shm/lune-nextest.Ab12Cd"
    cleanup_shm_tmpdir
    [ "$shm_state" = "cleaned" ]
    [ -z "$shm_tmpdir" ]
    cleanup_shm_tmpdir
    [ "$shm_state" = "cleaned" ]

    shm_state="bogus"
    if cleanup_shm_tmpdir 2>/dev/null; then
      exit 1
    fi
    EOF

    ${bash} -euo pipefail <<'EOF'
    ${mkProbeFn {
      chmodBin = chmod;
      rmBin = rm;
    }}
    probe_dir="$(${mktemp} -d)"
    trap '${rm} -rf -- "$probe_dir"' EXIT
    probe_shm_exec "$probe_dir"
    [ ! -e "$probe_dir/probe" ]
    EOF

    ${bash} -euo pipefail <<'EOF'
    ${mkCleanupFn {
      chmodBin = "true";
      rmBin = "true";
    }}
    ${mkProbeFn {
      chmodBin = "true";
      rmBin = rm;
    }}
    probe_dir="$(${mktemp} -d)"
    trap '${rm} -rf -- "$probe_dir"' EXIT
    orig_tmpdir="/tmp/original-tmpdir"
    TMPDIR="$orig_tmpdir"
    shm_state="allocated"
    shm_tmpdir="/dev/shm/lune-nextest.Ab12Cd"
    probe_err="$(probe_shm_exec "$probe_dir" 2>&1 >/dev/null && echo unexpected || true)"
    [ -z "$probe_err" ]
    [ ! -e "$probe_dir/probe" ]
    if probe_shm_exec "$probe_dir"; then
      exit 1
    else
      cleanup_shm_tmpdir
    fi
    [ ! -e "$probe_dir/probe" ]
    [ "$TMPDIR" = "$orig_tmpdir" ]
    [ "$shm_state" = "cleaned" ]
    [ -z "$shm_tmpdir" ]
    EOF
    touch "$out"
  '';
in
pkgs.writeShellApplication {
  name = "lune-nextest";
  passthru.check = cleanupCheck;
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
    unset NEXTEST_RETRIES NEXTEST_CONFIG_FILE
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
