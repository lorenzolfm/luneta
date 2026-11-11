{
  pkgs,
  toolchain,
  buildTools,
  compileEnv,
  clippyRun,
  ...
}:
let
  target =
    if pkgs.stdenv.hostPlatform.isDarwin then "x86_64-unknown-linux-gnu" else "aarch64-apple-darwin";
  tools = [ (toolchain.override { targets = [ target ]; }) ] ++ buildTools ++ [ pkgs.git ];
  buildScriptsSkipped = pkgs.writeText "clippy-cross.toml" ''
    [target.${target}.transcribe]
    rustc-link-lib = ["transcribe"]
    [target.${target}.alsa]
    rustc-link-lib = ["asound"]
    [target.${target}.ghostty-vt]
    rustc-link-lib = ["ghostty-vt"]
  '';
  crossRun = ''cargo clippy --config ${buildScriptsSkipped} --locked ${clippyRun} --target ${target} "$@" -- -D warnings'';
in
{
  name = "clippy";
  usage = "[args...]";
  help = "cargo clippy over every target, for Linux and the Mac from either, warnings denied";
  inherit tools;
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
    echo "dev clippy: linting for ${target}" >&2
    ${crossRun} || status=$?
    exit "$status"
  '';
  cross = {
    name = "clippy-cross";
    inherit tools;
    text = ''
      ${compileEnv}
      ${crossRun}
    '';
  };
}
