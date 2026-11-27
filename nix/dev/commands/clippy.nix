{
  pkgs,
  buildTools,
  compileEnv,
  hostBuildScriptsSkipped,
  cross,
  clippyRun,
  daemonPackages,
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
    cargo clippy --config ${hostBuildScriptsSkipped} --locked ${daemonPackages} --release -- -D warnings || status=$?
    if [ "$status" -eq 0 ]; then
      case " $* " in
        *" --allow-dirty "*) ;;
        *" --fix "*) set -- --allow-dirty "$@" ;;
      esac
    fi
    echo "dev clippy: linting for ${cross.target}" >&2
    cargo clippy --config ${cross.buildScriptsSkipped} --locked ${clippyRun} --target ${cross.target} "$@" -- -D warnings || status=$?
    cargo clippy --config ${cross.buildScriptsSkipped} --locked ${daemonPackages} --target ${cross.target} --release -- -D warnings || status=$?
    exit "$status"
  '';
}
