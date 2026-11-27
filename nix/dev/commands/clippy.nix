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
  hostTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget;
  target =
    if pkgs.stdenv.hostPlatform.isDarwin then "x86_64-unknown-linux-gnu" else "aarch64-apple-darwin";
  tools = [ (toolchain.override { targets = [ target ]; }) ] ++ buildTools ++ [ pkgs.git ];
  skipNative =
    triple:
    ''
      [target.${triple}.transcribe]
      rustc-link-lib = ["transcribe"]
      [target.${triple}.alsa]
      rustc-link-lib = ["asound"]
      [target.${triple}.ghostty-vt]
      rustc-link-lib = ["ghostty-vt"]
    '';
  buildScriptsSkipped = pkgs.writeText "clippy-skip-native.toml" ''
    ${skipNative hostTarget}
    ${skipNative target}
  '';
  hostRelease = "cargo clippy --config ${buildScriptsSkipped} --locked ${daemonPackages} --release -- -D warnings";
  crossClippy = ''cargo clippy --config ${buildScriptsSkipped} --locked ${clippyRun} --target ${target} "$@" -- -D warnings'';
  crossRelease = "cargo clippy --config ${buildScriptsSkipped} --locked ${daemonPackages} --target ${target} --release -- -D warnings";
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
    ${hostRelease} || status=$?
    if [ "$status" -eq 0 ]; then
      case " $* " in
        *" --allow-dirty "*) ;;
        *" --fix "*) set -- --allow-dirty "$@" ;;
      esac
    fi
    echo "dev clippy: linting for ${target}" >&2
    ${crossClippy} || status=$?
    ${crossRelease} || status=$?
    exit "$status"
  '';
  cross = {
    name = "clippy-cross";
    inherit tools;
    text = ''
      ${compileEnv}
      status=0
      ${crossClippy} || status=$?
      ${crossRelease} || status=$?
      exit "$status"
    '';
  };
  release = {
    name = "clippy-release";
    tools = buildTools ++ [ pkgs.git ];
    text = ''
      ${compileEnv}
      ${hostRelease}
    '';
  };
}
