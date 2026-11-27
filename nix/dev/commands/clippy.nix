{
  pkgs,
  buildTools,
  compileEnv,
  cross,
  clippyRun,
  ...
}:
{
  name = "clippy";
  usage = "[args...]";
  help = "cargo clippy over every target, for Linux and the Mac from either, warnings denied";
  tools = [ cross.toolchain ] ++ buildTools ++ [ pkgs.git ];
  text = ''
    ${compileEnv}
    status=0
    cargo clippy --locked ${clippyRun} "$@" -- -D warnings || status=$?
    if [ "$status" -eq 0 ]; then
      case " $* " in
        *" --allow-dirty "*) ;;
        *" --fix "*) set -- --allow-dirty "$@" ;;
      esac
    fi
    echo "dev clippy: linting for ${cross.target}" >&2
    cargo clippy --config ${cross.config} --locked ${clippyRun} --target ${cross.target} "$@" -- -D warnings || status=$?
    exit "$status"
  '';
}
