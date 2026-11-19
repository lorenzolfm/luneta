{
  compileEnv,
  testRun,
  testKit,
  ...
}:
{
  name = "test";
  usage = "[args...]";
  help = "the tests on nextest, then the doctests; -P quick leaves out the two that wait 30s and more; fails if the tests leave a luneta-mux running";
  tools = testKit;
  text = ''
    ${compileEnv}
    case "''${TERM:-dumb}" in
      dumb) export NEXTEST_SHOW_PROGRESS="''${NEXTEST_SHOW_PROGRESS:-counter}" ;;
    esac
    target="''${CARGO_TARGET_DIR:-target}"
    resolved="$(realpath -m -- "$target")/"
    given="$(realpath -ms -- "$target")/"
    muxes() {
      ps -A -o pid= -o args= | awk -v resolved="$resolved" -v given="$given" -v before="$1" '
        function from(folder) {
          if (index(args, folder) != 1) return 0
          return substr(args, length(folder) + 1) ~ /^([^ ]*\/)?luneta-mux( |$)/
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
    cargo nextest run --locked ${testRun} "$@" || tested=$?
    left="$(muxes "$before")"
    for _ in $(seq 100); do
      [ -z "$left" ] && break
      sleep 0.1
      left="$(muxes "$before")"
    done
    if [ -n "$left" ]; then
      shown=()
      for pid in $left; do
        shown+=(-p "$pid")
      done
      printf 'dev test: the tests left luneta-mux running 10s after they ended:\n' >&2
      ps -o pid= -o etime= -o args= "''${shown[@]}" >&2 || true
      [ "$tested" -ne 0 ] || tested=1
    fi
    [ "$tested" -eq 0 ] || exit "$tested"
    cargo test --doc --locked ${testRun}
  '';
}
