{
  pkgs,
  toolchain,
  buildTools,
  compileEnv,
  clippyRun,
  daemonPackages,
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
  crossClippy = ''cargo clippy --config ${buildScriptsSkipped} --locked ${clippyRun} --target ${target} "$@" -- -D warnings'';
  crossCheck = "cargo check --config ${buildScriptsSkipped} --locked ${daemonPackages} --target ${target} --release";
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
    cargo check --locked ${daemonPackages} || status=$?
    if [ "$status" -eq 0 ]; then
      case " $* " in
        *" --allow-dirty "*) ;;
        *" --fix "*) set -- --allow-dirty "$@" ;;
      esac
    fi
    echo "dev clippy: linting for ${target}" >&2
    ${crossClippy} || status=$?
    ${crossCheck} || status=$?
    exit "$status"
  '';
  cross = {
    name = "clippy-cross";
    inherit tools;
    text = ''
      ${compileEnv}
      status=0
      ${crossClippy} || status=$?
      ${crossCheck} || status=$?
      exit "$status"
    '';
  };
}
